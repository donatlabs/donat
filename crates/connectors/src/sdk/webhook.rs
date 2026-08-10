//! Webhook verification over raw bytes (spec 010 §10).
//!
//! Every scheme here authenticates the *exact* bytes a provider sent, before
//! any JSON parser sees them. That ordering is the whole point: a body that
//! fails verification is never parsed, never normalized, and never stored, so
//! an unauthenticated payload cannot reach a decoder at all.
//!
//! The schemes are closed, and the representation is private, exactly as
//! [`crate::sdk::auth::AuthPlan`] is: a connector selects one, and a sixth is
//! an edit to this file with its own test. None of them takes the secret as
//! bytes — a [`Secret`] authenticates a message and answers a comparison, and
//! there is no path from here back to its value.

use std::fmt;
use std::time::Duration;

use base64::Engine;
use reqwest::header::{HeaderMap, HeaderName};

use crate::sdk::auth::Secret;
use crate::sdk::operation::OperationError;

/// A provider-neutral inbound verification failure.
///
/// It carries no provider diagnostic, signature byte, raw body, or
/// secret-derived value: an ingress caller learns only which of these closed
/// reasons applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookRejection {
    MissingSignature,
    InvalidSignature,
    TimestampOutOfTolerance,
    PayloadTooLarge,
    MalformedPayload,
    UnsupportedEvent,
}

impl WebhookRejection {
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingSignature => "webhook_signature_missing",
            Self::InvalidSignature => "webhook_signature_invalid",
            Self::TimestampOutOfTolerance => "webhook_signature_expired",
            Self::PayloadTooLarge => "webhook_payload_too_large",
            Self::MalformedPayload => "webhook_payload_malformed",
            Self::UnsupportedEvent => "webhook_event_unsupported",
        }
    }
}

impl fmt::Display for WebhookRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

/// How a provider spells the digest it sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureEncoding {
    Hex,
    /// Standard base64 with padding, which is what every provider in this set
    /// publishes; an unpadded value simply fails to decode and is not a
    /// candidate.
    Base64,
}

impl SignatureEncoding {
    fn decode(self, value: &str) -> Option<Vec<u8>> {
        match self {
            Self::Hex => decode_hex(value),
            Self::Base64 => base64::engine::general_purpose::STANDARD.decode(value).ok(),
        }
    }
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some(((high << 4) | low) as u8)
        })
        .collect()
}

/// Where the digest itself lives in the request headers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SignatureLocation {
    /// The header value is the digest, after an optional fixed prefix such as
    /// `sha256=`.
    Whole {
        header: HeaderName,
        prefix: Option<String>,
    },
    /// The header is a list of `key=value` elements and the digest is every
    /// element under one key — a provider may publish more than one during a
    /// secret rotation, and any of them verifying is a verified request.
    Element { header: HeaderName, key: String },
}

impl SignatureLocation {
    const fn header(&self) -> &HeaderName {
        match self {
            Self::Whole { header, .. } | Self::Element { header, .. } => header,
        }
    }

    /// The candidate digests, or `None` when the header itself is absent.
    fn candidates(&self, headers: &HeaderMap) -> Option<Vec<String>> {
        let value = header_text(headers, self.header())?;
        Some(match self {
            Self::Whole { prefix, .. } => match prefix {
                Some(prefix) => value
                    .strip_prefix(prefix.as_str())
                    .map(|value| vec![value.to_owned()])
                    .unwrap_or_default(),
                None => vec![value.to_owned()],
            },
            Self::Element { key, .. } => elements(value, key),
        })
    }
}

/// Where the timestamp a canonical string covers comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TimestampLocation {
    Header(HeaderName),
    /// An element of the signature header, which is how a provider that packs
    /// both into one header spells it.
    Element {
        key: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VerificationKind {
    /// HMAC-SHA256 over the exact raw body.
    HmacBody {
        signature: SignatureLocation,
        encoding: SignatureEncoding,
    },
    /// HMAC-SHA256 over `<prefix><timestamp><separator><body>`, accepted only
    /// inside a tolerance window around the receiving clock.
    HmacTimestamped {
        signature: SignatureLocation,
        timestamp: TimestampLocation,
        canonical_prefix: String,
        separator: String,
        encoding: SignatureEncoding,
        tolerance: Duration,
    },
    /// A shared secret sent in a fixed header, compared in constant time.
    SharedSecretHeader { header: HeaderName },
}

/// One declared inbound verification scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookVerifier {
    kind: VerificationKind,
}

impl WebhookVerifier {
    /// HMAC-SHA256 over the exact raw body, compared against a fixed header.
    pub fn hmac_body(header: &str, encoding: SignatureEncoding) -> Result<Self, OperationError> {
        Ok(Self {
            kind: VerificationKind::HmacBody {
                signature: SignatureLocation::Whole {
                    header: static_header(header)?,
                    prefix: None,
                },
                encoding,
            },
        })
    }

    /// The same, for a provider that prefixes its digest (`sha256=<hex>`).
    /// A value that does not carry the declared prefix offers no candidate and
    /// is rejected rather than read as an unprefixed digest.
    pub fn hmac_body_with_prefix(
        header: &str,
        prefix: &str,
        encoding: SignatureEncoding,
    ) -> Result<Self, OperationError> {
        if prefix.is_empty() {
            return Err(OperationError::new(
                "a signature prefix must be static and non-empty",
            ));
        }
        Ok(Self {
            kind: VerificationKind::HmacBody {
                signature: SignatureLocation::Whole {
                    header: static_header(header)?,
                    prefix: Some(prefix.to_owned()),
                },
                encoding,
            },
        })
    }

    /// A shared secret compared against a fixed header in constant time.
    pub fn shared_secret_header(header: &str) -> Result<Self, OperationError> {
        Ok(Self {
            kind: VerificationKind::SharedSecretHeader {
                header: static_header(header)?,
            },
        })
    }

    /// HMAC-SHA256 over a canonical string that includes a timestamp, with a
    /// tolerance window. The builder's defaults are the common shape — the
    /// whole signature header value, hex, no canonical prefix — and each part a
    /// provider spells differently is named explicitly.
    pub fn hmac_timestamped(signature_header: &str) -> Result<TimestampedBuilder, OperationError> {
        Ok(TimestampedBuilder {
            signature_header: static_header(signature_header)?,
            signature_element: None,
            signature_prefix: None,
            timestamp: None,
            canonical_prefix: String::new(),
            separator: String::from("."),
            encoding: SignatureEncoding::Hex,
            tolerance: Duration::from_secs(300),
        })
    }

    /// Verify the exact raw bytes.
    ///
    /// `now_unix_seconds` is the receiving clock, passed in rather than read
    /// here so a test can pin it. Nothing in this function parses the body: it
    /// is a byte string to every scheme.
    pub fn verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
        secret: &Secret,
        now_unix_seconds: i64,
    ) -> Result<(), WebhookRejection> {
        match &self.kind {
            VerificationKind::HmacBody {
                signature,
                encoding,
            } => {
                let candidates = signature
                    .candidates(headers)
                    .ok_or(WebhookRejection::MissingSignature)?;
                admit(&secret.hmac_sha256(raw_body), &candidates, *encoding)
            }
            VerificationKind::HmacTimestamped {
                signature,
                timestamp,
                canonical_prefix,
                separator,
                encoding,
                tolerance,
            } => {
                let header_value = header_text(headers, signature.header())
                    .ok_or(WebhookRejection::MissingSignature)?;
                let sent = match timestamp {
                    TimestampLocation::Element { key } => elements(header_value, key)
                        .first()
                        .and_then(|value| value.parse::<i64>().ok())
                        .ok_or(WebhookRejection::InvalidSignature)?,
                    TimestampLocation::Header(header) => header_text(headers, header)
                        .ok_or(WebhookRejection::MissingSignature)?
                        .trim()
                        .parse::<i64>()
                        .map_err(|_| WebhookRejection::InvalidSignature)?,
                };
                // The window is checked before the digest, so a replay of an
                // authentic-but-old delivery is refused as expired rather than
                // spending a comparison on it.
                if now_unix_seconds.abs_diff(sent) > tolerance.as_secs() {
                    return Err(WebhookRejection::TimestampOutOfTolerance);
                }
                let candidates = signature
                    .candidates(headers)
                    .ok_or(WebhookRejection::MissingSignature)?;
                let mut canonical = Vec::new();
                canonical.extend_from_slice(canonical_prefix.as_bytes());
                canonical.extend_from_slice(sent.to_string().as_bytes());
                canonical.extend_from_slice(separator.as_bytes());
                canonical.extend_from_slice(raw_body);
                admit(&secret.hmac_sha256(&canonical), &candidates, *encoding)
            }
            VerificationKind::SharedSecretHeader { header } => {
                let value =
                    header_text(headers, header).ok_or(WebhookRejection::MissingSignature)?;
                if secret.constant_time_eq(value.as_bytes()) {
                    Ok(())
                } else {
                    Err(WebhookRejection::InvalidSignature)
                }
            }
        }
    }
}

/// The timestamped scheme's declaration.
pub struct TimestampedBuilder {
    signature_header: HeaderName,
    signature_element: Option<String>,
    signature_prefix: Option<String>,
    timestamp: Option<TimestampLocation>,
    canonical_prefix: String,
    separator: String,
    encoding: SignatureEncoding,
    tolerance: Duration,
}

impl TimestampedBuilder {
    /// The signature header is a `key=value` list and the digests live under
    /// this key.
    #[must_use]
    pub fn signature_element(mut self, key: &str) -> Self {
        self.signature_element = Some(key.to_owned());
        self
    }

    /// The signature header value carries this fixed prefix before the digest.
    #[must_use]
    pub fn signature_prefix(mut self, prefix: &str) -> Self {
        self.signature_prefix = Some(prefix.to_owned());
        self
    }

    /// The timestamp is an element of the signature header, under this key.
    #[must_use]
    pub fn timestamp_element(mut self, key: &str) -> Self {
        self.timestamp = Some(TimestampLocation::Element {
            key: key.to_owned(),
        });
        self
    }

    /// The timestamp is its own header.
    pub fn timestamp_header(mut self, header: &str) -> Result<Self, OperationError> {
        self.timestamp = Some(TimestampLocation::Header(static_header(header)?));
        Ok(self)
    }

    /// What separates the timestamp from the body in the canonical string.
    #[must_use]
    pub fn separator(mut self, separator: &str) -> Self {
        self.separator = separator.to_owned();
        self
    }

    /// A fixed string the canonical form starts with, such as a scheme version.
    #[must_use]
    pub fn canonical_prefix(mut self, prefix: &str) -> Self {
        self.canonical_prefix = prefix.to_owned();
        self
    }

    #[must_use]
    pub fn encoding(mut self, encoding: SignatureEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// How far the sent timestamp may be from the receiving clock, in either
    /// direction.
    #[must_use]
    pub fn tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    pub fn build(self) -> Result<WebhookVerifier, OperationError> {
        let timestamp = self.timestamp.ok_or_else(|| {
            OperationError::new("a timestamped webhook scheme must declare where its timestamp is")
        })?;
        if self.tolerance.is_zero() {
            return Err(OperationError::new(
                "a timestamped webhook scheme must declare a positive tolerance",
            ));
        }
        if self.signature_element.is_some() && self.signature_prefix.is_some() {
            return Err(OperationError::new(
                "a signature is read either from a list element or after a prefix, not both",
            ));
        }
        let signature = match self.signature_element {
            Some(key) => SignatureLocation::Element {
                header: self.signature_header,
                key,
            },
            None => SignatureLocation::Whole {
                header: self.signature_header,
                prefix: self.signature_prefix,
            },
        };
        Ok(WebhookVerifier {
            kind: VerificationKind::HmacTimestamped {
                signature,
                timestamp,
                canonical_prefix: self.canonical_prefix,
                separator: self.separator,
                encoding: self.encoding,
                tolerance: self.tolerance,
            },
        })
    }
}

fn static_header(name: &str) -> Result<HeaderName, OperationError> {
    HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| OperationError::new("a webhook header name must be static and valid"))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The values of every `key=value` element carrying this key, in order.
fn elements(value: &str, key: &str) -> Vec<String> {
    value
        .split(',')
        .filter_map(|part| part.split_once('='))
        .filter(|(name, _)| name.trim() == key)
        .map(|(_, value)| value.trim().to_owned())
        .collect()
}

/// Whether any candidate is the expected digest.  The comparison is
/// constant-time and the candidate list is walked in full rather than
/// short-circuiting on the first mismatch.
fn admit(
    expected: &[u8; 32],
    candidates: &[String],
    encoding: SignatureEncoding,
) -> Result<(), WebhookRejection> {
    let mut admitted = false;
    for candidate in candidates {
        if let Some(bytes) = encoding.decode(candidate) {
            admitted |= constant_time_eq(expected, &bytes);
        }
    }
    if admitted {
        Ok(())
    } else {
        Err(WebhookRejection::InvalidSignature)
    }
}

/// Compare two byte strings of a length the algorithm fixes, without an early
/// return on the first differing byte.
///
/// The length is compared first and is not secret here: every caller compares
/// digests, whose width is published by the digest algorithm.  A value whose
/// length is the operator's rather than the algorithm's — a shared secret — is
/// compared through [`Secret::constant_time_eq`], which reduces both sides to a
/// digest before reaching this.
pub(in crate::sdk) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
        #[cfg(test)]
        COMPARED_BYTES.with(|counted| counted.set(counted.get() + 1));
    }
    difference == 0
}

#[cfg(test)]
thread_local! {
    /// Bytes [`constant_time_eq`] has compared on this thread, so a test can
    /// assert that the work a comparison costs does not depend on the length of
    /// the value a caller sent.  Test builds only, and per thread because the
    /// test harness gives each test its own.
    pub(in crate::sdk) static COMPARED_BYTES: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    use super::*;

    type HmacSha256 = Hmac<Sha256>;

    const SECRET: &str = "whsec_donat_sdk_test_secret";
    /// A body with an unterminated object: any parser fails on it, so a test
    /// that reaches a verification answer proves nothing parsed it.
    const MALFORMED_BODY: &[u8] = br#"{"id":"evt_1","type":"thing.happened""#;
    const BODY: &[u8] = br#"{"id":"evt_1","type":"thing.happened"}"#;

    fn digest(message: &[u8]) -> Vec<u8> {
        let mut mac =
            HmacSha256::new_from_slice(SECRET.as_bytes()).expect("a test secret is a valid key");
        mac.update(message);
        mac.finalize().into_bytes().to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).expect("a test header name is valid"),
                value.parse().expect("a test header value is valid"),
            );
        }
        headers
    }

    fn secret() -> Secret {
        Secret::new(SECRET)
    }

    /// `sdk_webhook_verifies_before_parse`: a malformed body with an invalid
    /// signature is rejected without a JSON parse.
    #[test]
    fn sdk_webhook_verifies_before_parse() {
        // Every scheme, over a body no JSON parser would accept.  Each one
        // answers from the raw bytes alone: the body is never decoded, so a
        // hostile payload cannot reach a parser by failing verification.
        let body_hex = WebhookVerifier::hmac_body("X-Signature", SignatureEncoding::Hex)
            .expect("a static header name is valid");
        assert_eq!(
            body_hex
                .verify(
                    &headers(&[("x-signature", &hex(&[0u8; 32]))]),
                    MALFORMED_BODY,
                    &secret(),
                    1_700_000_000,
                )
                .expect_err("an invalid signature over a malformed body is refused"),
            WebhookRejection::InvalidSignature
        );
        // ...and the same malformed body verifies when it is authentic, which
        // is what proves the rejection above came from the signature rather
        // than from a parser.
        assert_eq!(
            body_hex.verify(
                &headers(&[("x-signature", &hex(&digest(MALFORMED_BODY)))]),
                MALFORMED_BODY,
                &secret(),
                1_700_000_000,
            ),
            Ok(())
        );

        // Base64, and a prefixed digest.
        let body_base64 = WebhookVerifier::hmac_body("X-Signature", SignatureEncoding::Base64)
            .expect("a static header name is valid");
        let encoded = base64::engine::general_purpose::STANDARD.encode(digest(BODY));
        assert_eq!(
            body_base64.verify(
                &headers(&[("x-signature", &encoded)]),
                BODY,
                &secret(),
                1_700_000_000,
            ),
            Ok(())
        );
        let prefixed = WebhookVerifier::hmac_body_with_prefix(
            "X-Hub-Signature-256",
            "sha256=",
            SignatureEncoding::Hex,
        )
        .expect("a static header name is valid");
        assert_eq!(
            prefixed.verify(
                &headers(&[(
                    "x-hub-signature-256",
                    &format!("sha256={}", hex(&digest(BODY)))
                )]),
                BODY,
                &secret(),
                1_700_000_000,
            ),
            Ok(())
        );
        assert_eq!(
            prefixed
                .verify(
                    &headers(&[("x-hub-signature-256", &hex(&digest(BODY)))]),
                    BODY,
                    &secret(),
                    1_700_000_000,
                )
                .expect_err("a digest without its declared prefix is not a candidate"),
            WebhookRejection::InvalidSignature
        );

        // An absent header is missing, not invalid: the two are different
        // operator problems.
        assert_eq!(
            body_hex
                .verify(&HeaderMap::new(), BODY, &secret(), 1_700_000_000)
                .expect_err("an absent signature header is missing"),
            WebhookRejection::MissingSignature
        );

        // A shared secret compared in constant time.
        let shared = WebhookVerifier::shared_secret_header("X-Webhook-Token")
            .expect("a static header name is valid");
        assert_eq!(
            shared.verify(
                &headers(&[("x-webhook-token", SECRET)]),
                MALFORMED_BODY,
                &secret(),
                1_700_000_000,
            ),
            Ok(())
        );
        assert_eq!(
            shared
                .verify(
                    &headers(&[("x-webhook-token", "whsec_donat_sdk_test_secre")]),
                    MALFORMED_BODY,
                    &secret(),
                    1_700_000_000,
                )
                .expect_err("a prefix of the secret is not the secret"),
            WebhookRejection::InvalidSignature
        );
    }

    /// The timestamped scheme, in the shape the reference Stripe verifier uses:
    /// one header carrying `t=<unix>` and one or more `v1=<hex>` digests over
    /// `<timestamp>.<body>`, inside a five-minute window.
    #[test]
    fn a_timestamped_canonical_string_covers_the_timestamp_and_the_raw_body() {
        let verifier = WebhookVerifier::hmac_timestamped("Stripe-Signature")
            .expect("a static header name is valid")
            .signature_element("v1")
            .timestamp_element("t")
            .separator(".")
            .encoding(SignatureEncoding::Hex)
            .tolerance(Duration::from_secs(300))
            .build()
            .expect("a complete timestamped declaration is valid");

        let signed = |timestamp: i64, body: &[u8]| {
            let mut canonical = timestamp.to_string().into_bytes();
            canonical.push(b'.');
            canonical.extend_from_slice(body);
            format!("t={timestamp},v1={}", hex(&digest(&canonical)))
        };

        let sent = 1_700_000_000;
        assert_eq!(
            verifier.verify(
                &headers(&[("stripe-signature", &signed(sent, BODY))]),
                BODY,
                &secret(),
                sent + 120,
            ),
            Ok(())
        );

        // One more raw byte is a different message.
        let mut modified = BODY.to_vec();
        modified.push(b' ');
        assert_eq!(
            verifier
                .verify(
                    &headers(&[("stripe-signature", &signed(sent, BODY))]),
                    &modified,
                    &secret(),
                    sent + 120,
                )
                .expect_err("a signature over different raw bytes is rejected"),
            WebhookRejection::InvalidSignature
        );

        // The window is exact, and it closes in both directions.
        for (now, expected) in [
            (sent + 300, Ok(())),
            (sent - 300, Ok(())),
            (sent + 301, Err(WebhookRejection::TimestampOutOfTolerance)),
            (sent - 301, Err(WebhookRejection::TimestampOutOfTolerance)),
        ] {
            assert_eq!(
                verifier.verify(
                    &headers(&[("stripe-signature", &signed(sent, BODY))]),
                    BODY,
                    &secret(),
                    now,
                ),
                expected,
                "now {now} against a signature sent at {sent}"
            );
        }

        // A rotation publishes two digests; either verifying is verified.
        let rotating = format!(
            "t={sent},v1={},v1={}",
            hex(&[0u8; 32]),
            signed(sent, BODY)
                .split_once("v1=")
                .expect("the test signature carries a v1 element")
                .1
        );
        assert_eq!(
            verifier.verify(
                &headers(&[("stripe-signature", &rotating)]),
                BODY,
                &secret(),
                sent,
            ),
            Ok(())
        );

        // A header with no timestamp element at all is invalid, not missing.
        assert_eq!(
            verifier
                .verify(
                    &headers(&[("stripe-signature", &format!("v1={}", hex(&[0u8; 32])))]),
                    BODY,
                    &secret(),
                    sent,
                )
                .expect_err("a signature header without its timestamp is unreadable"),
            WebhookRejection::InvalidSignature
        );
    }

    /// A provider that puts the timestamp in its own header and versions its
    /// canonical string.
    #[test]
    fn a_timestamp_may_live_in_its_own_header_under_a_versioned_canonical_prefix() {
        let verifier = WebhookVerifier::hmac_timestamped("X-Signature")
            .expect("a static header name is valid")
            .signature_prefix("v0=")
            .timestamp_header("X-Request-Timestamp")
            .expect("a static header name is valid")
            .canonical_prefix("v0:")
            .separator(":")
            .tolerance(Duration::from_secs(60))
            .build()
            .expect("a complete timestamped declaration is valid");

        let sent = 1_700_000_000;
        let mut canonical = b"v0:".to_vec();
        canonical.extend_from_slice(sent.to_string().as_bytes());
        canonical.push(b':');
        canonical.extend_from_slice(BODY);
        let signature = format!("v0={}", hex(&digest(&canonical)));

        assert_eq!(
            verifier.verify(
                &headers(&[
                    ("x-signature", signature.as_str()),
                    ("x-request-timestamp", &sent.to_string()),
                ]),
                BODY,
                &secret(),
                sent + 30,
            ),
            Ok(())
        );
        assert_eq!(
            verifier
                .verify(
                    &headers(&[("x-signature", signature.as_str())]),
                    BODY,
                    &secret(),
                    sent + 30,
                )
                .expect_err("an absent timestamp header is missing"),
            WebhookRejection::MissingSignature
        );
    }

    #[test]
    fn a_webhook_declaration_is_static_and_complete() {
        assert!(WebhookVerifier::hmac_body("X-Sig-{tenant}", SignatureEncoding::Hex).is_err());
        assert!(
            WebhookVerifier::hmac_body_with_prefix("X-Sig", "", SignatureEncoding::Hex).is_err()
        );
        assert!(WebhookVerifier::shared_secret_header("X Token").is_err());
        assert!(
            WebhookVerifier::hmac_timestamped("X-Sig")
                .expect("a static header name is valid")
                .build()
                .is_err(),
            "a timestamped scheme without a timestamp location does not build"
        );
        assert!(
            WebhookVerifier::hmac_timestamped("X-Sig")
                .expect("a static header name is valid")
                .timestamp_element("t")
                .tolerance(Duration::ZERO)
                .build()
                .is_err()
        );
        assert!(
            WebhookVerifier::hmac_timestamped("X-Sig")
                .expect("a static header name is valid")
                .timestamp_element("t")
                .signature_element("v1")
                .signature_prefix("v1=")
                .build()
                .is_err(),
            "a signature is read one way or the other, never both"
        );
    }

    /// The shared-secret scheme compares an operator's secret, whose length is
    /// the operator's business — unlike a digest, whose width the algorithm
    /// publishes. A comparison that returns on a length mismatch answers a
    /// wrong-length candidate in a different amount of work than a
    /// right-length one, which is a guessable bit of the secret and the only
    /// one this scheme has to hide.
    ///
    /// The work is counted rather than timed: a timing assertion measures the
    /// machine, and this measures the code.
    #[test]
    fn a_shared_secret_comparison_does_not_measure_the_secret() {
        let shared = WebhookVerifier::shared_secret_header("X-Telegram-Bot-Api-Secret-Token")
            .expect("a static header name is valid");
        let long = "z".repeat(4096);
        let candidates = ["", "z", "whsec_donat_sdk_test_secre", SECRET, &long];

        let work: Vec<usize> = candidates
            .iter()
            .map(|candidate| {
                COMPARED_BYTES.with(|counter| counter.set(0));
                let answer = shared.verify(
                    &headers(&[("x-telegram-bot-api-secret-token", candidate)]),
                    BODY,
                    &secret(),
                    1_700_000_000,
                );
                assert_eq!(
                    answer.is_ok(),
                    *candidate == SECRET,
                    "the comparison still answers equality for {candidate:?}"
                );
                COMPARED_BYTES.with(|counter| counter.get())
            })
            .collect();

        assert!(work[0] > 0, "a candidate is compared, not dismissed");
        assert!(
            work.iter().all(|counted| *counted == work[0]),
            "every candidate costs the same comparison, whatever its length: {work:?}"
        );
    }

    #[test]
    fn a_constant_time_comparison_answers_the_same_question_as_equality() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn a_rejection_carries_a_closed_code_and_nothing_else() {
        for (rejection, code) in [
            (
                WebhookRejection::MissingSignature,
                "webhook_signature_missing",
            ),
            (
                WebhookRejection::InvalidSignature,
                "webhook_signature_invalid",
            ),
            (
                WebhookRejection::TimestampOutOfTolerance,
                "webhook_signature_expired",
            ),
            (
                WebhookRejection::PayloadTooLarge,
                "webhook_payload_too_large",
            ),
            (
                WebhookRejection::MalformedPayload,
                "webhook_payload_malformed",
            ),
            (
                WebhookRejection::UnsupportedEvent,
                "webhook_event_unsupported",
            ),
        ] {
            assert_eq!(rejection.code(), code);
            assert_eq!(rejection.to_string(), code);
        }
    }
}
