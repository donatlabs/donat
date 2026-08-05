//! File attachments (spec 008): the resolved storage registry and the signing
//! primitives shared by every part of the feature.
//!
//! The registry is built before the listener binds, exactly like the connector
//! registry: every secret is resolved from the environment once, and a missing
//! variable stops the boot instead of surfacing as a runtime 500.
//!
//! Signing lives here *and* in SQL (`migrations/…__donat_files.sql`). The two
//! are one algorithm split at a fixed seam: everything constant for a statement
//! is derived here — the day-scoped subkey for engine-served URLs, the
//! `kDate → kRegion → kService → kSigning` chain for S3 — and only the per-row
//! HMAC runs in the database. That is what lets a query return a signed URL per
//! row without the engine walking the response (the M4 one-statement
//! invariant), while the Rust half stays independently testable against the
//! published AWS vectors.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use donat_metadata::{Metadata, StorageBackend, StorageGc};

type HmacSha256 = Hmac<Sha256>;

/// The purpose tag bound into an engine-served token.
///
/// There is one: uploading and downloading are presigned by the object store,
/// so the only capability the engine issues itself is the call reporting an
/// upload finished. The tag stays in the payload so a second capability could
/// never be replayed as this one.
pub const PURPOSE_COMPLETE: &str = "complete";

const SIGNING_KEY_CONTEXT: &[u8] = b"donat-file-v1";

/// A refusal to serve. Every variant names a metadata field or an environment
/// variable, never a resolved value.
#[derive(Debug)]
pub enum StorageRegistryError {
    MissingSecret { backend: String, var: String },
    MissingSigningSecret { var: String },
    Invalid { backend: String, message: String },
}

impl std::fmt::Display for StorageRegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSecret { backend, var } => write!(
                formatter,
                "storage backend `{backend}`: environment variable {var} is not set"
            ),
            Self::MissingSigningSecret { var } => write!(
                formatter,
                "storage signing.secret: environment variable {var} is not set"
            ),
            Self::Invalid { backend, message } => {
                write!(formatter, "storage backend `{backend}`: {message}")
            }
        }
    }
}

impl std::error::Error for StorageRegistryError {}

/// Everything the running engine needs to serve attachments, resolved once.
/// There is no runtime mutation surface: a change means a new deployment.
#[derive(Debug, Default)]
pub struct StorageRegistry {
    backends: BTreeMap<String, Backend>,
    /// `<schema>.<table>.<column>` -> the column's resolved declaration.
    attachments: BTreeMap<String, AttachmentSpec>,
    signing: Option<SigningSecret>,
    gc: StorageGc,
    limits: donat_metadata::StorageLimits,
    cors: donat_metadata::StorageCors,
    identity: String,
}

/// The resolved store. One variant today; see [`donat_metadata::StorageBackend`]
/// for why there is no local-disk one.
#[derive(Debug)]
pub enum Backend {
    S3(S3Backend),
}

#[derive(Debug)]
pub struct S3Backend {
    pub name: String,
    pub bucket: String,
    pub region: String,
    /// Scheme and authority only, no trailing slash.
    pub origin: String,
    pub host: String,
    pub path_style: bool,
    pub access_key_id: String,
    pub public_base_url: Option<String>,
    secret_access_key: String,
}

#[derive(Debug)]
pub struct AttachmentSpec {
    pub key: String,
    pub source: String,
    pub schema: String,
    pub table: String,
    pub column: String,
    pub backend: String,
    pub max_bytes: u64,
    pub media_types: Vec<String>,
    pub public: bool,
}

impl AttachmentSpec {
    pub fn allows_media_type(&self, media_type: &str) -> bool {
        self.media_types.is_empty() || self.media_types.iter().any(|m| m == media_type)
    }

    /// The object key for one upload. Engine-chosen and restricted to
    /// unreserved characters plus '/', which is what lets the SQL half skip
    /// percent-encoding entirely.
    pub fn object_key(&self, id: Uuid) -> String {
        format!("{}/{}", self.key, id)
    }

    /// Where a provider-side upload lands before the engine has seen it.
    ///
    /// A presigned PUT stays usable for its whole lifetime and cannot be
    /// revoked, so bytes the engine has already verified must not live at the
    /// address that URL writes to. Completion copies the staged object to its
    /// final key and drops the staging one; whatever a late PUT writes
    /// afterwards is an orphan nothing references.
    pub fn staging_key(&self, id: Uuid) -> String {
        format!("{}/{}.part", self.key, id)
    }
}

#[derive(Debug)]
struct SigningSecret {
    secret: Vec<u8>,
    upload_ttl_seconds: u32,
    download_ttl_seconds: u32,
}

impl StorageRegistry {
    /// Resolve the deployment's storage configuration from the process
    /// environment. Returns an empty registry when no table declares an
    /// attachment, in which case nothing downstream is mounted.
    pub fn build(metadata: &Metadata) -> Result<Self, StorageRegistryError> {
        Self::build_with(metadata, &|name| std::env::var(name).ok())
    }

    /// The same, with the secrets supplied by the caller instead of read from
    /// the process environment.
    ///
    /// The embedded wasm core has no environment to read: it is handed its
    /// deployment secrets by the host, which keeps them out of the committed
    /// `core-config.json` snapshot. `build` is this with `std::env::var`.
    pub fn build_with(
        metadata: &Metadata,
        secret_of: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, StorageRegistryError> {
        let mut attachments = BTreeMap::new();
        for a in metadata.attachments() {
            let spec = AttachmentSpec {
                key: a.key(),
                source: a.source.to_string(),
                schema: a.table.schema().to_string(),
                table: a.table.name().to_string(),
                column: a.attachment.column.clone(),
                backend: a.attachment.backend.clone(),
                max_bytes: a.attachment.max_bytes,
                media_types: a.attachment.media_types.clone(),
                public: a.attachment.public,
            };
            attachments.insert(spec.key.clone(), spec);
        }

        let mut backends = BTreeMap::new();
        if !attachments.is_empty() {
            // Only the backends some column actually uses are resolved, so an
            // unused S3 declaration cannot fail a boot for a missing secret.
            let used: std::collections::BTreeSet<&str> =
                attachments.values().map(|a| a.backend.as_str()).collect();
            for declared in &metadata.storage.backends {
                if !used.contains(declared.name()) {
                    continue;
                }
                backends.insert(
                    declared.name().to_string(),
                    resolve_backend(declared, secret_of)?,
                );
            }
        }

        let signing = match (&metadata.storage.signing, attachments.is_empty()) {
            (Some(signing), false) => {
                let secret = secret_of(&signing.secret.value_from_env).ok_or_else(|| {
                    StorageRegistryError::MissingSigningSecret {
                        var: signing.secret.value_from_env.clone(),
                    }
                })?;
                Some(SigningSecret {
                    secret: secret.into_bytes(),
                    upload_ttl_seconds: signing.upload_ttl_seconds,
                    download_ttl_seconds: signing.download_ttl_seconds,
                })
            }
            _ => None,
        };

        Ok(Self {
            backends,
            attachments,
            signing,
            gc: metadata.storage.gc.clone(),
            limits: metadata.storage.limits.clone(),
            cors: metadata.storage.cors.clone(),
            identity: metadata.storage.identity.session_variable.clone(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.attachments.is_empty()
    }

    pub fn attachment(&self, key: &str) -> Option<&AttachmentSpec> {
        self.attachments.get(key)
    }

    pub fn attachments(&self) -> impl Iterator<Item = &AttachmentSpec> {
        self.attachments.values()
    }

    pub fn backend(&self, name: &str) -> Option<&Backend> {
        self.backends.get(name)
    }

    pub fn backend_for(&self, attachment: &AttachmentSpec) -> Option<&Backend> {
        self.backends.get(&attachment.backend)
    }

    pub fn gc(&self) -> &StorageGc {
        &self.gc
    }

    pub fn limits(&self) -> &donat_metadata::StorageLimits {
        &self.limits
    }

    pub fn cors(&self) -> &donat_metadata::StorageCors {
        &self.cors
    }

    /// The session variable that identifies the uploader.
    pub fn identity_variable(&self) -> &str {
        &self.identity
    }

    pub fn upload_ttl_seconds(&self) -> u32 {
        self.signing
            .as_ref()
            .map(|s| s.upload_ttl_seconds)
            .unwrap_or(900)
    }

    pub fn download_ttl_seconds(&self) -> u32 {
        self.signing
            .as_ref()
            .map(|s| s.download_ttl_seconds)
            .unwrap_or(300)
    }

    /// The day-scoped subkey behind the engine's own capability. The
    /// deployment secret itself is never handed out, so material that leaks
    /// stops working the next day.
    pub fn day_key(&self, at: DateTime<Utc>) -> Option<[u8; 32]> {
        self.signing
            .as_ref()
            .map(|s| day_key(&s.secret, &day_stamp(at)))
    }

    /// Verify an engine-served token. The issuing day is recovered from the
    /// expiry: a URL cannot outlive its issue day by more than a day (metadata
    /// bounds every TTL at 86400s), so the issuing day is the expiry's day or
    /// the one before it.
    pub fn verify_token(&self, purpose: &str, id: Uuid, expires: i64, presented: &str) -> bool {
        let Some(signing) = &self.signing else {
            return false;
        };
        let Some(expiry) = DateTime::from_timestamp(expires, 0) else {
            return false;
        };
        if Utc::now() > expiry {
            return false;
        }
        [expiry, expiry - chrono::Duration::days(1)]
            .iter()
            .any(|day| {
                let key = day_key(&signing.secret, &day_stamp(*day));
                constant_time_eq(&token(&key, purpose, id, expires), presented)
            })
    }
}

fn resolve_backend(
    declared: &StorageBackend,
    secret_of: &dyn Fn(&str) -> Option<String>,
) -> Result<Backend, StorageRegistryError> {
    match declared {
        StorageBackend::S3(s3) => {
            let access_key_id = secret_of(&s3.access_key_id.value_from_env).ok_or_else(|| {
                StorageRegistryError::MissingSecret {
                    backend: s3.name.clone(),
                    var: s3.access_key_id.value_from_env.clone(),
                }
            })?;
            let secret_access_key =
                secret_of(&s3.secret_access_key.value_from_env).ok_or_else(|| {
                    StorageRegistryError::MissingSecret {
                        backend: s3.name.clone(),
                        var: s3.secret_access_key.value_from_env.clone(),
                    }
                })?;

            let endpoint = s3
                .endpoint
                .clone()
                .unwrap_or_else(|| format!("https://s3.{}.amazonaws.com", s3.region));
            let endpoint = endpoint.trim_end_matches('/').to_string();
            let (scheme, authority) =
                endpoint
                    .split_once("://")
                    .ok_or_else(|| StorageRegistryError::Invalid {
                        backend: s3.name.clone(),
                        message: format!("endpoint '{endpoint}' has no scheme"),
                    })?;
            // Virtual-hosted style puts the bucket in the authority; path style
            // keeps it as the first path segment. Both are decided here so the
            // SQL half only ever concatenates.
            let (origin, host) = if s3.path_style {
                (endpoint.clone(), authority.to_string())
            } else {
                let host = format!("{}.{}", s3.bucket, authority);
                (format!("{scheme}://{host}"), host)
            };

            Ok(Backend::S3(S3Backend {
                name: s3.name.clone(),
                bucket: s3.bucket.clone(),
                region: s3.region.clone(),
                origin,
                host,
                path_style: s3.path_style,
                access_key_id,
                public_base_url: s3
                    .public_base_url
                    .as_ref()
                    .map(|base| base.trim_end_matches('/').to_string()),
                secret_access_key,
            }))
        }
    }
}

impl S3Backend {
    /// The canonical URI of one object key, which is also its path in the URL.
    /// Object keys are engine-chosen and unreserved, so this needs no escaping.
    pub fn canonical_uri(&self, object_key: &str) -> String {
        if self.path_style {
            format!("/{}/{}", self.bucket, object_key)
        } else {
            format!("/{object_key}")
        }
    }

    pub fn credential_scope(&self, at: DateTime<Utc>) -> String {
        format!("{}/{}/s3/aws4_request", day_stamp(at), self.region)
    }

    /// `AKIA…%2F20260801%2Feu-central-1%2Fs3%2Faws4_request` — percent-encoded
    /// here so the SQL half can treat it as an opaque constant.
    pub fn credential_encoded(&self, at: DateTime<Utc>) -> String {
        format!("{}/{}", self.access_key_id, self.credential_scope(at)).replace('/', "%2F")
    }

    /// The `kDate → kRegion → kService → kSigning` chain, constant for a UTC
    /// day. This is the only key material handed to a statement.
    pub fn signing_key(&self, at: DateTime<Utc>) -> [u8; 32] {
        sigv4_signing_key(&self.secret_access_key, &day_stamp(at), &self.region, "s3")
    }

    /// A complete presigned URL, the Rust twin of `donat.s3_presigned_url`.
    /// Requests the engine itself makes — the collector's `DELETE`, the claim
    /// path's `HEAD` — use this; per-row URLs in a query response are built by
    /// the SQL twin from the same inputs.
    pub fn presign(
        &self,
        method: &str,
        object_key: &str,
        at: DateTime<Utc>,
        expires: u32,
    ) -> String {
        let canonical_uri = self.canonical_uri(object_key);
        presign_v4(
            &self.signing_key(at),
            &self.credential_encoded(at),
            &self.credential_scope(at),
            &amz_date(at),
            expires,
            &self.origin,
            &self.host,
            &canonical_uri,
            method,
        )
    }
}

/// `YYYYMMDD`
pub fn day_stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%d").to_string()
}

/// `YYYYMMDDTHHMMSSZ`
pub fn amz_date(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(msg);
    mac.finalize().into_bytes().into()
}

/// The day-scoped subkey for the engine's own capability.
pub fn day_key(secret: &[u8], day: &str) -> [u8; 32] {
    let mut context = SIGNING_KEY_CONTEXT.to_vec();
    context.extend_from_slice(secret);
    hmac_sha256(&context, day.as_bytes())
}

/// base64url, unpadded. The completion capability's signature.
pub fn token(day_key: &[u8; 32], purpose: &str, id: Uuid, expires: i64) -> String {
    let payload = format!("{purpose}:{id}:{expires}");
    URL_SAFE_NO_PAD.encode(hmac_sha256(day_key, payload.as_bytes()))
}

/// A complete engine-served URL carrying that capability.
pub fn file_url(
    day_key: &[u8; 32],
    purpose: &str,
    path_prefix: &str,
    id: Uuid,
    expires: i64,
) -> String {
    format!(
        "{path_prefix}{id}?exp={expires}&sig={}",
        token(day_key, purpose, id, expires)
    )
}

/// The AWS SigV4 key derivation chain.
pub fn sigv4_signing_key(secret: &str, day: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), day.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// The exact canonicalization `donat.s3_presigned_url` implements, with `host`
/// as the only signed header. Kept character-for-character in step with it; the
/// conformance suite asserts the two produce the same URL for the same inputs.
#[allow(clippy::too_many_arguments)]
pub fn presign_v4(
    signing_key: &[u8; 32],
    credential_encoded: &str,
    scope: &str,
    amz_date: &str,
    expires: u32,
    origin: &str,
    host: &str,
    canonical_uri: &str,
    method: &str,
) -> String {
    presign_v4_with(
        signing_key,
        credential_encoded,
        scope,
        amz_date,
        expires,
        origin,
        host,
        canonical_uri,
        method,
        &[],
    )
}

/// The same signature, with additional headers bound into it.
///
/// This is what makes an upload URL self-limiting: signing `content-length` and
/// `content-type` means the provider itself rejects a body of another size or
/// another type, so the declared limits are enforced by the storage the bytes
/// actually go to, not only by a promise the engine records.
#[allow(clippy::too_many_arguments)]
pub fn presign_v4_with(
    signing_key: &[u8; 32],
    credential_encoded: &str,
    scope: &str,
    amz_date: &str,
    expires: u32,
    origin: &str,
    host: &str,
    canonical_uri: &str,
    method: &str,
    extra_headers: &[(String, String)],
) -> String {
    let mut headers: Vec<(String, String)> = extra_headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    headers.push(("host".to_string(), host.to_string()));
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let signed_headers = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers = headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();

    // The separator is percent-encoded in the query, and only there: the
    // canonical headers block still joins on a literal ';'. A raw ';' in a
    // query string is not merely untidy — Go's URL parser rejects the whole
    // query, so a real S3 implementation never even sees the credentials and
    // answers "the authorization mechanism you have provided is not supported".
    let signed_headers_encoded = signed_headers.replace(';', "%3B");
    let query = format!(
        "X-Amz-Algorithm=AWS4-HMAC-SHA256\
         &X-Amz-Credential={credential_encoded}\
         &X-Amz-Date={amz_date}\
         &X-Amz-Expires={expires}\
         &X-Amz-SignedHeaders={signed_headers_encoded}"
    );
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{query}\n{canonical_headers}\n{signed_headers}\nUNSIGNED-PAYLOAD"
    );
    let hashed = hex(&Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{hashed}");
    let signature = hex(&hmac_sha256(signing_key, string_to_sign.as_bytes()));
    format!("{origin}{canonical_uri}?{query}&X-Amz-Signature={signature}")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Length-independent comparison for a presented signature.
fn constant_time_eq(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// The example published in the AWS documentation for query-parameter
    /// (presigned) SigV4 against S3. Pinning it here is what makes the SQL twin
    /// checkable: the conformance suite compares SQL output to this code, and
    /// this test anchors the code to AWS.
    #[test]
    fn presigned_url_matches_the_published_aws_example() {
        let at = Utc.with_ymd_and_hms(2013, 5, 24, 0, 0, 0).unwrap();
        let secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let key = sigv4_signing_key(secret, &day_stamp(at), "us-east-1", "s3");
        let url = presign_v4(
            &key,
            "AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request",
            "20130524/us-east-1/s3/aws4_request",
            &amz_date(at),
            86400,
            "https://examplebucket.s3.amazonaws.com",
            "examplebucket.s3.amazonaws.com",
            "/test.txt",
            "GET",
        );
        assert!(
            url.ends_with(
                "&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
            ),
            "unexpected url: {url}"
        );
        assert!(url.starts_with("https://examplebucket.s3.amazonaws.com/test.txt?"));
    }

    #[test]
    fn a_token_is_bound_to_its_purpose_id_and_expiry() {
        let key = day_key(b"s3cr3t", "20260801");
        let id = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        let t = token(&key, PURPOSE_COMPLETE, id, 1_000);
        assert_ne!(t, token(&key, "other", id, 1_000));
        assert_ne!(t, token(&key, PURPOSE_COMPLETE, other, 1_000));
        assert_ne!(t, token(&key, PURPOSE_COMPLETE, id, 1_001));
        assert_ne!(
            t,
            token(&day_key(b"s3cr3t", "20260802"), PURPOSE_COMPLETE, id, 1_000)
        );
        assert!(!t.contains('+') && !t.contains('/') && !t.contains('='));
    }

    #[test]
    fn a_file_url_carries_the_expiry_and_signature() {
        let key = day_key(b"s3cr3t", "20260801");
        let id = Uuid::from_u128(7);
        let url = file_url(
            &key,
            PURPOSE_COMPLETE,
            "/v1/files/complete/",
            id,
            1_754_000_000,
        );
        assert_eq!(
            url,
            format!(
                "/v1/files/complete/{id}?exp=1754000000&sig={}",
                token(&key, PURPOSE_COMPLETE, id, 1_754_000_000)
            )
        );
    }

    #[test]
    fn virtual_hosted_and_path_style_differ_only_in_authority_and_uri() {
        let backend = S3Backend {
            name: "media".into(),
            bucket: "donat-media".into(),
            region: "eu-central-1".into(),
            origin: "https://donat-media.s3.eu-central-1.amazonaws.com".into(),
            host: "donat-media.s3.eu-central-1.amazonaws.com".into(),
            path_style: false,
            access_key_id: "AKIA".into(),
            public_base_url: None,
            secret_access_key: "secret".into(),
        };
        assert_eq!(
            backend.canonical_uri("public.pet.photo/x"),
            "/public.pet.photo/x"
        );

        let path_style = S3Backend {
            origin: "http://127.0.0.1:9000".into(),
            host: "127.0.0.1:9000".into(),
            path_style: true,
            ..backend
        };
        assert_eq!(
            path_style.canonical_uri("public.pet.photo/x"),
            "/donat-media/public.pet.photo/x"
        );
    }

    #[test]
    fn constant_time_eq_rejects_a_different_length_or_byte() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
    }
}

// ---------------------------------------------------------------------------
// Request-time planning
// ---------------------------------------------------------------------------

/// What one upload request resolves to. Everything here is decided before any
/// SQL is rendered, so the statement that inserts the pending row also returns
/// the finished URL.
#[derive(Debug, Clone)]
pub struct UploadTarget {
    pub upload_id: Uuid,
    pub object_key: String,
    pub method: String,
    /// The complete upload URL. It is a literal rather than a SQL expression
    /// because one request mints exactly one of them — there is nothing
    /// per-row about it.
    pub url: String,
    /// Headers the client must send for the URL to be accepted.
    pub headers: Vec<(String, String)>,
    /// Where the client reports the upload finished, for backends whose bytes
    /// do not pass through the engine. `None` when the engine sees the bytes
    /// itself and measures them as they arrive.
    pub complete_url: Option<String>,
    /// The verified size, when the URL itself constrains it and the provider
    /// enforces that constraint. `None` when the engine must still measure.
    pub byte_size: Option<i64>,
    pub expires_at_epoch: i64,
}

/// The context a request planner needs to turn a file column into SQL.
pub struct RequestContext<'a> {
    pub registry: &'a StorageRegistry,
    pub now: DateTime<Utc>,
    /// Fixed upload id, for tests that need a deterministic statement. Production
    /// leaves this `None` and every request mints a fresh id.
    pub fixed_upload_id: Option<Uuid>,
    /// Absolute prefix for engine-served URLs, e.g. `https://api.example.com`.
    /// Empty means same-origin, which is what a browser needs by default.
    pub external_base_url: String,
}

impl RequestContext<'_> {
    fn day_key(&self, at: DateTime<Utc>) -> Option<[u8; 32]> {
        self.registry.day_key(at)
    }

    fn path(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.external_base_url)
    }

    /// The instant a URL is signed as of: `now` floored to half its lifetime.
    ///
    /// A stored file never changes — its id is consumed once and its bytes are
    /// never rewritten — so the only reason its URL can differ between two
    /// reads is that the signature would otherwise expire. Flooring makes that
    /// the *only* reason: every read inside a window returns byte-identical
    /// bytes, and the URL is re-signed exactly when it must be.
    ///
    /// This is not only about caching. A subscription re-runs its query on a
    /// timer and pushes whenever the response differs, so a URL carrying the
    /// current second would make every poll look like a change. With a window
    /// of half the lifetime, a subscription on an unchanged row pushes at most
    /// once per `ttl / 2` — and a deployment that wants that rarer raises
    /// `download_ttl_seconds`, which is the same knob that decides how long a
    /// leaked URL stays usable.
    fn signed_at(&self, ttl: u32) -> DateTime<Utc> {
        let window = (ttl / 2).max(1) as i64;
        let floored = self.now.timestamp().div_euclid(window) * window;
        DateTime::from_timestamp(floored, 0).unwrap_or(self.now)
    }

    /// The SQL expression that builds a download URL for one row, with `{row}`
    /// standing in for the upload row's alias.
    ///
    /// This is the one URL that has to be built per row, which is exactly why
    /// it is an expression the database evaluates rather than a value Rust
    /// computes after the fact.
    pub fn download_url_sql(&self, attachment: &AttachmentSpec) -> Option<String> {
        let backend = self.registry.backend_for(attachment)?;
        // A public file needs no capability, so its URL carries no signature,
        // no expiry, and no HMAC at all — just the object's stable address.
        // Because a stored object is immutable, that address is correct
        // forever: a CDN and a browser can cache it indefinitely, and a
        // subscription never sees it change.
        let Backend::S3(s3) = backend;
        if attachment.public {
            // Refused at load time when it is missing, so a published
            // attachment always has one.
            let base = format!("{}/", s3.public_base_url.clone()?);
            return Some(format!("({} || {{row}}.object_key)", sql_literal(&base)));
        }
        let ttl = self.registry.download_ttl_seconds();
        let signed_at = self.signed_at(ttl);
        Some(format!(
            "donat.s3_presigned_url({key}, {credential}, {scope}, {amz_date}, {ttl}, \
             {origin}, {host}, {uri_prefix} || {{row}}.object_key, 'GET')",
            key = bytea_literal(&s3.signing_key(signed_at)),
            credential = sql_literal(&s3.credential_encoded(signed_at)),
            scope = sql_literal(&s3.credential_scope(signed_at)),
            amz_date = sql_literal(&amz_date(signed_at)),
            origin = sql_literal(&s3.origin),
            host = sql_literal(&s3.host),
            uri_prefix = sql_literal(&s3.uri_prefix()),
        ))
    }

    /// Resolve one upload request into the URL the caller will receive.
    pub fn upload_target(
        &self,
        attachment: &AttachmentSpec,
        media_type: &str,
        declared_bytes: i64,
    ) -> Option<UploadTarget> {
        let backend = self.registry.backend_for(attachment)?;
        let upload_id = self.fixed_upload_id.unwrap_or_else(Uuid::new_v4);
        let ttl = self.registry.upload_ttl_seconds();
        let signed_at = self.signed_at(ttl);
        let expires_at_epoch = signed_at.timestamp() + ttl as i64;

        match backend {
            Backend::S3(s3) => {
                let key = self.day_key(signed_at)?;
                // The presigned PUT writes to a staging key, never to the
                // address the claimed file will live at.
                let object_key = attachment.staging_key(upload_id);
                let headers = vec![
                    ("Content-Length".to_string(), declared_bytes.to_string()),
                    ("Content-Type".to_string(), media_type.to_string()),
                ];
                let url = presign_v4_with(
                    &s3.signing_key(signed_at),
                    &s3.credential_encoded(signed_at),
                    &s3.credential_scope(signed_at),
                    &amz_date(signed_at),
                    ttl,
                    &s3.origin,
                    &s3.host,
                    &s3.canonical_uri(&object_key),
                    "PUT",
                    &headers,
                );
                Some(UploadTarget {
                    upload_id,
                    object_key,
                    method: "PUT".to_string(),
                    url,
                    headers,
                    // The bytes never pass through the engine, so the size is
                    // not known until it asks the provider — which it does when
                    // the client reports the upload finished.
                    complete_url: Some(self.path(&file_url(
                        &key,
                        PURPOSE_COMPLETE,
                        "/v1/files/complete/",
                        upload_id,
                        expires_at_epoch,
                    ))),
                    byte_size: None,
                    expires_at_epoch,
                })
            }
        }
    }
}

impl S3Backend {
    /// A presigned PUT that copies one object to another key.
    ///
    /// The copy source is a signed header, so a URL minted for one source
    /// cannot be turned against another object.
    pub fn presign_copy(
        &self,
        from_key: &str,
        to_key: &str,
        at: DateTime<Utc>,
        expires: u32,
    ) -> (String, Vec<(String, String)>) {
        let source = format!("/{}/{from_key}", self.bucket);
        let headers = vec![("x-amz-copy-source".to_string(), source)];
        let url = presign_v4_with(
            &self.signing_key(at),
            &self.credential_encoded(at),
            &self.credential_scope(at),
            &amz_date(at),
            expires,
            &self.origin,
            &self.host,
            &self.canonical_uri(to_key),
            "PUT",
            &headers,
        );
        (url, headers)
    }

    /// `/` for virtual-hosted buckets, `/<bucket>/` for path style — the part
    /// the SQL half prepends to a stored object key.
    pub fn uri_prefix(&self) -> String {
        if self.path_style {
            format!("/{}/", self.bucket)
        } else {
            "/".to_string()
        }
    }
}

/// `'\x…'::bytea` — a key literal for a rendered statement.
fn bytea_literal(bytes: &[u8; 32]) -> String {
    format!("'\\x{}'::bytea", hex(bytes))
}

/// A single-quoted SQL string literal. Every value that reaches this is
/// engine-owned (a path prefix, a host, a credential scope), but it is escaped
/// anyway: nothing renders into SQL unescaped.
fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod stability_tests {
    use super::*;
    use chrono::TimeZone;
    use donat_metadata::Metadata;

    const TEST_METADATA: &str = r#"
version: 3
sources:
  - name: default
    kind: postgres
    configuration:
      connection_info:
        database_url: postgresql://localhost/x
    tables:
      - table: {schema: public, name: pet}
        attachments:
          - column: photo
            backend: local
            max_bytes: 1024
storage:
  backends:
    - name: local
      kind: s3
      bucket: donat-test
      region: eu-central-1
      endpoint: http://127.0.0.1:19000
      path_style: true
      access_key_id: { value_from_env: DONAT_TEST_STORAGE_KEY }
      secret_access_key: { value_from_env: DONAT_TEST_STORAGE_SECRET }
  signing:
    secret: { value_from_env: DONAT_TEST_STORAGE_SECRET }
"#;

    fn registry() -> StorageRegistry {
        // Safety: the test process sets its own variables and never unsets them.
        unsafe {
            std::env::set_var("DONAT_TEST_STORAGE_KEY", "test-key");
            std::env::set_var("DONAT_TEST_STORAGE_SECRET", "s3cr3t");
        }
        let metadata: Metadata = serde_yaml::from_str(TEST_METADATA).expect("test metadata");
        StorageRegistry::build(&metadata).expect("registry")
    }

    /// The registry has to be resolvable without a process environment, because
    /// the embedded wasm core has none: it is handed its deployment secrets by
    /// the Go host, which is also what keeps them out of the committed
    /// `core-config.json` snapshot. If this ever went back to reading `env`
    /// directly, file attachments would compile but fail to sign in the
    /// embedded host with no obvious cause.
    #[test]
    fn a_registry_resolves_from_supplied_secrets_without_the_environment() {
        let metadata: Metadata = serde_yaml::from_str(TEST_METADATA).expect("test metadata");
        let supplied = |name: &str| match name {
            "DONAT_TEST_STORAGE_KEY" => Some("key-from-the-host".to_string()),
            "DONAT_TEST_STORAGE_SECRET" => Some("secret-from-the-host".to_string()),
            _ => None,
        };

        let registry = StorageRegistry::build_with(&metadata, &supplied)
            .expect("registry from supplied secrets");
        assert!(
            registry.attachment("public.pet.photo").is_some(),
            "the declared attachment must be resolved"
        );
        assert!(
            registry
                .day_key(Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap())
                .is_some(),
            "a supplied signing secret must produce a day key"
        );
    }

    /// A secret the host does not supply must fail loudly at build, not leave a
    /// registry that silently cannot sign.
    #[test]
    fn a_missing_supplied_secret_fails_the_build() {
        let metadata: Metadata = serde_yaml::from_str(TEST_METADATA).expect("test metadata");
        let err = StorageRegistry::build_with(&metadata, &|_| None)
            .err()
            .expect("a registry with no secrets must not build");
        assert!(
            format!("{err:?}").contains("DONAT_TEST_STORAGE"),
            "the failure must name the secret it wanted: {err:?}"
        );
    }

    /// A request `offset` seconds after a window boundary.
    fn context(registry: &StorageRegistry, offset: i64) -> RequestContext<'_> {
        let start = Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0).unwrap();
        RequestContext {
            registry,
            now: start + chrono::Duration::seconds(offset),
            fixed_upload_id: None,
            external_base_url: String::new(),
        }
    }

    /// The file itself never changes, so its URL may only change when the
    /// signature would otherwise expire — once per half a lifetime, not once
    /// per request.
    #[test]
    fn a_download_url_only_changes_when_it_would_otherwise_expire() {
        let registry = registry();
        let attachment = registry.attachment("public.pet.photo").expect("attachment");
        let window = (registry.download_ttl_seconds() / 2) as i64;

        let first = context(&registry, 1).download_url_sql(attachment).unwrap();
        let same_window = context(&registry, window - 1)
            .download_url_sql(attachment)
            .unwrap();
        assert_eq!(first, same_window, "the URL must not move within a window");

        let next_window = context(&registry, window + 1)
            .download_url_sql(attachment)
            .unwrap();
        assert_ne!(first, next_window, "a new window must re-sign");
    }

    /// The window is capped at half the lifetime, so even a URL minted at the
    /// very end of one is still valid for at least half its declared TTL.
    #[test]
    fn the_window_never_costs_more_than_half_the_lifetime() {
        let registry = registry();
        let ttl = registry.download_ttl_seconds();
        let context = context(&registry, 59);
        let signed_at = context.signed_at(ttl);
        let lost = context.now.timestamp() - signed_at.timestamp();
        assert!(
            lost <= (ttl as i64) / 2,
            "a URL lost {lost}s of a {ttl}s lifetime"
        );
    }
}
