//! Verifying a caller that cannot present a JWT.
//!
//! A provider posting a callback has no token and no session. The alternatives
//! are the admin secret, which is transport authentication rather than a
//! permission, and the unauthorized role, which makes the route a mutation any
//! stranger may call. So an endpoint declares a credential the engine can
//! check, and names the role a verified request runs as.
//!
//! Everything here runs **before the body is parsed**. A signature covers the
//! exact bytes, and re-serializing a parsed document produces different ones,
//! so a valid signature would fail against anything reconstructed. That
//! ordering is the security property; the rest is bookkeeping.

use axum::http::HeaderMap;
use base64::Engine as _;
use donat_metadata::{
    EndpointAuthentication, EndpointCredential, SharedSecretScheme, SignatureAlgorithm,
    SignatureEncoding, SignatureScheme, TimestampSource,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Why a request was not authenticated.
///
/// Deliberately coarse at the boundary: the caller reports one status for all
/// of them, so a prober cannot learn whether a header was missing, malformed
/// or simply wrong. The variants exist for logs, not for responses.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthRejection {
    /// Refused before hashing, so an oversized body is never digested.
    PayloadTooLarge,
    MissingCredential,
    MalformedCredential,
    /// The secret named by the endpoint is not in the environment. A
    /// configuration fault rather than a caller fault, and worth telling apart
    /// in a log even though the caller sees the same thing.
    SecretUnavailable,
    TimestampOutOfTolerance,
    Invalid,
}

/// What the digest is computed over, for a request that may have no body.
pub struct SignedRequest<'a> {
    pub body: &'a [u8],
    pub path: &'a str,
    pub query: &'a str,
}

/// Verify a request against an endpoint's declared credential.
///
/// `read_env` is injected so the whole path is testable without touching the
/// process environment, and `now` so the tolerance window is testable without
/// waiting.
pub fn verify(
    auth: &EndpointAuthentication,
    headers: &HeaderMap,
    request: &SignedRequest<'_>,
    read_env: &dyn Fn(&str) -> Option<String>,
    now: i64,
) -> Result<(), AuthRejection> {
    if request.body.len() > auth.max_body_bytes {
        return Err(AuthRejection::PayloadTooLarge);
    }
    match &auth.credential {
        EndpointCredential::Signature(scheme) => {
            verify_signature(scheme, headers, request, read_env, now)
        }
        EndpointCredential::SharedSecret(scheme) => verify_shared_secret(scheme, headers, read_env),
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn verify_shared_secret(
    scheme: &SharedSecretScheme,
    headers: &HeaderMap,
    read_env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), AuthRejection> {
    let expected = read_env(&scheme.secret.value_from_env)
        .filter(|secret| !secret.is_empty())
        .ok_or(AuthRejection::SecretUnavailable)?;
    let presented = header(headers, &scheme.header).ok_or(AuthRejection::MissingCredential)?;
    // Compared through HMAC rather than by equality: `==` on strings returns
    // as soon as two bytes differ, which leaks the shared prefix to anyone
    // willing to time it.
    let mut mac = HmacSha256::new_from_slice(expected.as_bytes())
        .expect("HMAC accepts arbitrary non-empty key bytes");
    mac.update(presented.as_bytes());
    let mut check = HmacSha256::new_from_slice(expected.as_bytes())
        .expect("HMAC accepts arbitrary non-empty key bytes");
    check.update(expected.as_bytes());
    if mac.finalize().into_bytes() == check.finalize().into_bytes() {
        Ok(())
    } else {
        Err(AuthRejection::Invalid)
    }
}

fn verify_signature(
    scheme: &SignatureScheme,
    headers: &HeaderMap,
    request: &SignedRequest<'_>,
    read_env: &dyn Fn(&str) -> Option<String>,
    now: i64,
) -> Result<(), AuthRejection> {
    let secret = read_env(&scheme.secret.value_from_env)
        .filter(|secret| !secret.is_empty())
        .ok_or(AuthRejection::SecretUnavailable)?;
    let raw = header(headers, &scheme.header).ok_or(AuthRejection::MissingCredential)?;

    let timestamp = match &scheme.timestamp {
        None => None,
        Some(TimestampSource::Header { header: name }) => Some(
            header(headers, name)
                .ok_or(AuthRejection::MissingCredential)?
                .to_string(),
        ),
        Some(TimestampSource::SignatureHeaderField { field }) => Some(
            field_from_header(raw, field)
                .ok_or(AuthRejection::MalformedCredential)?
                .to_string(),
        ),
    };

    // A declared tolerance with no timestamp to check would be a window
    // nobody watches; a timestamp outside the window is a replay.
    if let (Some(tolerance), Some(stamp)) = (scheme.tolerance_seconds, timestamp.as_deref()) {
        let sent: i64 = stamp
            .parse()
            .map_err(|_| AuthRejection::MalformedCredential)?;
        if now.abs_diff(sent) > tolerance {
            return Err(AuthRejection::TimestampOutOfTolerance);
        }
    }

    let presented = candidate_digests(raw, scheme, &scheme.prefix)?;
    let payload = signed_payload(&scheme.signed_payload, request, timestamp.as_deref());

    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts arbitrary non-empty key bytes");
    mac.update(&payload);
    let SignatureAlgorithm::HmacSha256 = scheme.algorithm;
    if presented
        .iter()
        .any(|candidate| mac.clone().verify_slice(candidate).is_ok())
    {
        Ok(())
    } else {
        Err(AuthRejection::Invalid)
    }
}

/// `t=123,v1=abc` → the value of the named field. Senders that fold the
/// timestamp into the signature header use this shape; senders that give it a
/// header of its own do not.
fn field_from_header<'a>(raw: &'a str, field: &str) -> Option<&'a str> {
    raw.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == field).then_some(value)
    })
}

/// Every digest the header offers, since a sender rotating a secret presents
/// several and any one matching is a match.
fn candidate_digests(
    raw: &str,
    scheme: &SignatureScheme,
    prefix: &Option<String>,
) -> Result<Vec<Vec<u8>>, AuthRejection> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        // A declared prefix is stripped from the whole token first. It has to
        // be: a prefix like `sha256=` contains the separator, so splitting on
        // `=` before stripping mistakes the prefix for a field name and leaves
        // a digest that no longer starts with it.
        let token = match prefix {
            Some(p) => match part.strip_prefix(p.as_str()) {
                Some(rest) => rest,
                None => continue,
            },
            // No prefix declared: take the value of a `k=v` pair, or the whole
            // token when the header carries the digest alone.
            None => match part.split_once('=') {
                Some((_, value)) if !value.is_empty() => value,
                _ => part,
            },
        };
        if let Some(bytes) = decode(token, scheme.encoding) {
            out.push(bytes);
        }
    }
    if out.is_empty() {
        return Err(AuthRejection::MalformedCredential);
    }
    Ok(out)
}

fn decode(token: &str, encoding: SignatureEncoding) -> Option<Vec<u8>> {
    match encoding {
        SignatureEncoding::Hex => {
            if token.len() % 2 != 0 || token.is_empty() {
                return None;
            }
            (0..token.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&token[i..i + 2], 16).ok())
                .collect()
        }
        SignatureEncoding::Base64 => base64::engine::general_purpose::STANDARD.decode(token).ok(),
    }
}

/// Substitute the template. A method with no body signs its path and query,
/// which is why this is a template rather than a flag.
fn signed_payload(template: &str, request: &SignedRequest<'_>, timestamp: Option<&str>) -> Vec<u8> {
    let mut out = Vec::with_capacity(template.len() + request.body.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.extend_from_slice(rest[..open].as_bytes());
        let Some(close) = rest[open..].find('}').map(|i| open + i) else {
            break;
        };
        match &rest[open + 1..close] {
            "body" => out.extend_from_slice(request.body),
            "path" => out.extend_from_slice(request.path.as_bytes()),
            "query" => out.extend_from_slice(request.query.as_bytes()),
            "timestamp" => out.extend_from_slice(timestamp.unwrap_or("").as_bytes()),
            // An unknown placeholder is left verbatim rather than dropped: a
            // typo then fails to verify loudly instead of silently signing
            // less than the author meant.
            other => {
                out.push(b'{');
                out.extend_from_slice(other.as_bytes());
                out.push(b'}');
            }
        }
        rest = &rest[close + 1..];
    }
    out.extend_from_slice(rest.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use donat_metadata::SecretRef;

    const SECRET: &str = "whsec_test";

    fn env(name: &str) -> Option<String> {
        (name == "SECRET").then(|| SECRET.to_string())
    }

    fn scheme(signed_payload: &str, timestamp: Option<TimestampSource>) -> SignatureScheme {
        SignatureScheme {
            header: "X-Sig".into(),
            algorithm: SignatureAlgorithm::HmacSha256,
            encoding: SignatureEncoding::Hex,
            prefix: None,
            signed_payload: signed_payload.into(),
            timestamp,
            tolerance_seconds: None,
            secret: SecretRef {
                value_from_env: "SECRET".into(),
            },
        }
    }

    fn auth(credential: EndpointCredential) -> EndpointAuthentication {
        EndpointAuthentication {
            credential,
            run_as: "billing".into(),
            session_variables: Default::default(),
            max_body_bytes: 64,
            accept: vec![],
        }
    }

    fn digest(payload: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(SECRET.as_bytes()).unwrap();
        mac.update(payload);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (k, v) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                v.parse().unwrap(),
            );
        }
        map
    }

    fn request<'a>(body: &'a [u8]) -> SignedRequest<'a> {
        SignedRequest {
            body,
            path: "/api/rest/hooks/x",
            query: "",
        }
    }

    #[test]
    fn a_correctly_signed_body_verifies() {
        let body = br#"{"type":"invoice.paid"}"#;
        let a = auth(EndpointCredential::Signature(scheme("{body}", None)));
        let h = headers(&[("X-Sig", &digest(body))]);
        assert_eq!(verify(&a, &h, &request(body), &env, 0), Ok(()));
    }

    /// The property the whole ordering exists for: one byte different is a
    /// different request.
    #[test]
    fn a_tampered_body_does_not() {
        let a = auth(EndpointCredential::Signature(scheme("{body}", None)));
        let h = headers(&[("X-Sig", &digest(br#"{"amount":100}"#))]);
        assert_eq!(
            verify(&a, &h, &request(br#"{"amount":900}"#), &env, 0),
            Err(AuthRejection::Invalid)
        );
    }

    #[test]
    fn an_oversized_body_is_refused_before_it_is_hashed() {
        let body = vec![b'x'; 65];
        let a = auth(EndpointCredential::Signature(scheme("{body}", None)));
        // Correctly signed, and still refused: the bound is checked first.
        let h = headers(&[("X-Sig", &digest(&body))]);
        assert_eq!(
            verify(&a, &h, &request(&body), &env, 0),
            Err(AuthRejection::PayloadTooLarge)
        );
    }

    #[test]
    fn a_timestamped_scheme_signs_the_timestamp_too() {
        let body = br#"{"ok":true}"#;
        let mut s = scheme(
            "{timestamp}.{body}",
            Some(TimestampSource::SignatureHeaderField { field: "t".into() }),
        );
        s.tolerance_seconds = Some(300);
        let a = auth(EndpointCredential::Signature(s));

        let mut payload = b"1000.".to_vec();
        payload.extend_from_slice(body);
        let h = headers(&[("X-Sig", &format!("t=1000,v1={}", digest(&payload)))]);

        assert_eq!(verify(&a, &h, &request(body), &env, 1_100), Ok(()));
        assert_eq!(
            verify(&a, &h, &request(body), &env, 9_000),
            Err(AuthRejection::TimestampOutOfTolerance)
        );
    }

    /// A callback with no body still has something to sign.
    #[test]
    fn a_bodiless_method_signs_path_and_query() {
        let a = auth(EndpointCredential::Signature(scheme(
            "{path}?{query}",
            None,
        )));
        let req = SignedRequest {
            body: b"",
            path: "/api/rest/hooks/x",
            query: "id=42",
        };
        let h = headers(&[("X-Sig", &digest(b"/api/rest/hooks/x?id=42"))]);
        assert_eq!(verify(&a, &h, &req, &env, 0), Ok(()));
    }

    #[test]
    fn a_prefixed_digest_is_stripped_before_decoding() {
        let body = br#"{}"#;
        let mut s = scheme("{body}", None);
        s.prefix = Some("sha256=".into());
        let a = auth(EndpointCredential::Signature(s));
        let h = headers(&[("X-Sig", &format!("sha256={}", digest(body)))]);
        assert_eq!(verify(&a, &h, &request(body), &env, 0), Ok(()));
    }

    /// A sender mid-rotation presents both; either matching is a match.
    #[test]
    fn any_offered_digest_may_match() {
        let body = br#"{}"#;
        let a = auth(EndpointCredential::Signature(scheme("{body}", None)));
        let h = headers(&[(
            "X-Sig",
            &format!("v1={},v1={}", digest(b"stale"), digest(body)),
        )]);
        assert_eq!(verify(&a, &h, &request(body), &env, 0), Ok(()));
    }

    #[test]
    fn a_missing_header_is_not_a_pass() {
        let a = auth(EndpointCredential::Signature(scheme("{body}", None)));
        assert_eq!(
            verify(&a, &headers(&[]), &request(b"{}"), &env, 0),
            Err(AuthRejection::MissingCredential)
        );
    }

    /// A secret the deployment never set must not authenticate anybody. This
    /// is the failure that would otherwise turn a misconfiguration into an
    /// open endpoint.
    #[test]
    fn an_unset_secret_authenticates_nobody() {
        let a = auth(EndpointCredential::Signature(scheme("{body}", None)));
        let h = headers(&[("X-Sig", &digest(b"{}"))]);
        assert_eq!(
            verify(&a, &h, &request(b"{}"), &|_| None, 0),
            Err(AuthRejection::SecretUnavailable)
        );
        assert_eq!(
            verify(&a, &h, &request(b"{}"), &|_| Some(String::new()), 0),
            Err(AuthRejection::SecretUnavailable)
        );
    }

    #[test]
    fn a_shared_secret_matches_only_itself() {
        let a = auth(EndpointCredential::SharedSecret(SharedSecretScheme {
            header: "X-Api-Key".into(),
            secret: SecretRef {
                value_from_env: "SECRET".into(),
            },
        }));
        let ok = headers(&[("X-Api-Key", SECRET)]);
        assert_eq!(verify(&a, &ok, &request(b""), &env, 0), Ok(()));

        let wrong = headers(&[("X-Api-Key", "whsec_tes")]);
        assert_eq!(
            verify(&a, &wrong, &request(b""), &env, 0),
            Err(AuthRejection::Invalid)
        );
    }

    #[test]
    fn an_unknown_placeholder_is_left_verbatim() {
        // Signing less than the author meant is worse than failing, so a typo
        // fails to verify rather than quietly narrowing the digest.
        let out = signed_payload("{typo}-{body}", &request(b"x"), None);
        assert_eq!(out, b"{typo}-x");
    }
}
