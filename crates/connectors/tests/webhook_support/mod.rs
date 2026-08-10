//! The shared half of the Batch B inbound proofs (spec 013 §4).
//!
//! Every signed fixture in these tests is generated *here*, by this repository's
//! own code, under a Donat-owned test secret. Nothing is copied from a provider,
//! a third-party library, or a documentation sample: each connector's scheme is
//! re-implemented from the provider's published description in this file, and
//! the connector's declaration and this transcription have to agree.

#![allow(dead_code)]

use base64::Engine;
use donat_connectors::providers::inbound::{EventIdentifier, TriggerEvent};
use donat_connectors::sdk::{Connector, Secret, Trigger, WebhookRejection};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderName};
use sha2::Sha256;

/// The Donat-owned inbound secret every one of these tests signs with. It is
/// also the sentinel a redaction assertion looks for.
pub const WEBHOOK_SECRET: &str = "donat-inbound-secret-sentinel-do-not-log";

/// A body that is both unterminated JSON and, on its own, meaningless. Any
/// parser fails on it, so a test that reaches a verification answer over it
/// proves nothing parsed it first.
pub const MALFORMED_BODY: &[u8] = br#"{"id":"evt_1","action":"opened""#;

/// The receiving clock every timestamped fixture is pinned to.
pub const NOW: i64 = 1_700_000_000;

pub fn secret() -> Secret {
    Secret::new(WEBHOOK_SECRET)
}

/// HMAC-SHA256 under the test secret. This is the tests' own transcription of
/// the primitive, deliberately not the SDK's.
pub fn digest(message: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(WEBHOOK_SECRET.as_bytes())
        .expect("a test secret is a valid HMAC key");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("a test header name is valid"),
            value.parse().expect("a test header value is valid"),
        );
    }
    headers
}

/// The one trigger a connector's route verifies with.
///
/// Every trigger of a Batch B connector shares one scheme and one ceiling —
/// which is what makes "one instance, one route" true — so any of them answers
/// for all of them, and [`triggers_share_one_scheme`] proves that rather than
/// assuming it.
pub fn trigger(connector: &'static Connector) -> &'static Trigger {
    connector
        .triggers()
        .first()
        .expect("a Batch B connector declares at least one trigger")
}

/// Verify one delivery at the pinned clock.
pub fn verify(
    connector: &'static Connector,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), WebhookRejection> {
    trigger(connector).verify(headers, body, &secret(), NOW)
}

/// Every declared trigger of this connector applies the same verification to the
/// same ceiling, which is the invariant one HTTP route per instance rests on.
pub fn triggers_share_one_scheme(connector: &'static Connector) {
    let first = trigger(connector);
    for candidate in connector.triggers() {
        assert_eq!(
            candidate.verification(),
            first.verification(),
            "`{}` declares two inbound schemes, so one route could not answer for both",
            connector.name()
        );
        assert_eq!(
            candidate.raw_body_max_bytes(),
            first.raw_body_max_bytes(),
            "`{}` declares two inbound ceilings",
            connector.name()
        );
    }
}

/// `<name>_body_limit_precedes_verification`, in its shared form.
///
/// A body one byte over the declared ceiling is refused as too large even when
/// it carries a *correct* signature, which is what proves the ceiling is applied
/// before the MAC rather than after it: an oversized authentic body would verify
/// if the order were the other way round.
pub fn body_limit_precedes_verification(
    connector: &'static Connector,
    sign: impl Fn(&[u8]) -> HeaderMap,
) {
    let trigger = trigger(connector);
    let ceiling = trigger.raw_body_max_bytes();
    let oversized = vec![b'x'; ceiling + 1];
    assert_eq!(
        verify(connector, &sign(&oversized), &oversized)
            .expect_err("an oversized body is refused before a MAC is computed over it"),
        WebhookRejection::PayloadTooLarge
    );

    // The exact ceiling is admitted, and then answers on its signature — so the
    // refusal above came from the ceiling and not from the scheme.
    let exact = vec![b'x'; ceiling];
    assert_eq!(
        verify(connector, &sign(&exact), &exact),
        Ok(()),
        "the exact ceiling is admitted and verifies"
    );
}

/// `<name>_signature_precedes_parse`, in its shared form.
///
/// The same malformed body is rejected when its signature is wrong and accepted
/// when its signature is right. The acceptance is the load-bearing half: it can
/// only happen if nothing parsed the body, because no parser would accept it.
pub fn signature_precedes_parse(
    connector: &'static Connector,
    sign: impl Fn(&[u8]) -> HeaderMap,
    forged: HeaderMap,
    expected: WebhookRejection,
) {
    assert_eq!(
        verify(connector, &forged, MALFORMED_BODY)
            .expect_err("an incorrectly signed malformed body is refused"),
        expected
    );
    assert_eq!(
        verify(connector, &sign(MALFORMED_BODY), MALFORMED_BODY),
        Ok(()),
        "a correctly signed malformed body verifies, which is only possible if nothing parsed it"
    );
    // An absent signature is `missing`, not `invalid`: they are different
    // operator problems and the closed rejection set keeps them apart.
    assert_eq!(
        verify(connector, &HeaderMap::new(), MALFORMED_BODY)
            .expect_err("an absent signature header is missing"),
        WebhookRejection::MissingSignature
    );
}

/// `<name>_signature_is_exact`, in its shared form: one byte of the body, one
/// byte of the signature, and the secret each decide the answer.
pub fn signature_is_exact(
    connector: &'static Connector,
    body: &[u8],
    sign: impl Fn(&[u8]) -> HeaderMap,
    flip_signature: impl Fn(&HeaderMap) -> HeaderMap,
) {
    let authentic = sign(body);
    assert_eq!(verify(connector, &authentic, body), Ok(()));

    // One more raw byte is a different message.
    let mut modified = body.to_vec();
    modified.push(b' ');
    assert_eq!(
        verify(connector, &authentic, &modified)
            .expect_err("a signature over different raw bytes is rejected"),
        WebhookRejection::InvalidSignature
    );

    // One byte of the signature.
    assert_eq!(
        verify(connector, &flip_signature(&authentic), body)
            .expect_err("a signature that differs in one byte is rejected"),
        WebhookRejection::InvalidSignature
    );

    // A different secret.
    assert_eq!(
        trigger(connector)
            .verify(&authentic, body, &Secret::new("a-different-secret"), NOW)
            .expect_err("a signature under another secret is rejected"),
        WebhookRejection::InvalidSignature
    );
}

/// Every declared event exposes typed fields and names where its identifier
/// lives, and every declared trigger has exactly one event behind it.
pub fn events_match_triggers(connector: &'static Connector, events: &'static [TriggerEvent]) {
    let declared = connector
        .triggers()
        .iter()
        .map(|trigger| trigger.name())
        .collect::<Vec<_>>();
    let published = events
        .iter()
        .map(TriggerEvent::provider_event)
        .collect::<Vec<_>>();
    assert_eq!(
        declared,
        published,
        "`{}` declares one trigger per published event, in order",
        connector.name()
    );
    for event in events {
        assert!(
            !event.fields().is_empty(),
            "`{}` event `{}` exposes no typed field",
            connector.name(),
            event.provider_event()
        );
        for field in event.fields() {
            assert!(
                field.pointer().starts_with('/'),
                "`{}` event `{}` field `{}` is not an absolute JSON pointer",
                connector.name(),
                event.provider_event(),
                field.name()
            );
        }
        match event.event_identifier() {
            EventIdentifier::Header(name) => assert!(!name.is_empty()),
            EventIdentifier::BodyPointer(pointer) => assert!(pointer.starts_with('/')),
            // A provider that publishes no per-delivery identifier is recorded
            // as publishing none. Only Calendly is in that position, and its
            // own test asserts it by name.
            EventIdentifier::Unpublished => {}
        }
    }
}

/// `<name>_comparison_is_constant_time`, in its shared form.
///
/// Two things are checked, because either alone would be weak. The connector
/// module's own source may not compare a signature or a secret with `==`, which
/// is the defect this proof exists to catch; and the SDK's verifier is the only
/// thing that answers, so the comparison it performs is the constant-time one
/// with no early return that `sdk/webhook.rs` implements and tests.
pub fn comparison_is_constant_time(module_source: &str, source: &str) {
    for (index, line) in source.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        let mentions_secret = [
            "signature",
            "Signature",
            "secret",
            "Secret",
            "hmac",
            "digest",
        ]
        .iter()
        .any(|needle| code.contains(needle));
        assert!(
            !(mentions_secret && (code.contains("==") || code.contains("!="))),
            "{module_source}:{}: a signature or secret must never be compared with `==`; the \
             SDK's constant-time comparison is the only admitted one — {line}",
            index + 1
        );
    }
}

/// Read one connector module's source for the check above.
pub fn module_source(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/providers")
        .join(format!("{name}.rs"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Nothing a connector can print carries the inbound secret: not the
/// declaration, not the trigger, not the credential specification, and not a
/// rejection.
pub fn nothing_prints_the_secret(connector: &'static Connector) {
    let rejection = verify(connector, &HeaderMap::new(), MALFORMED_BODY)
        .expect_err("an unsigned delivery is refused");
    let surface = format!(
        "{:?} {:?} {:?} {rejection:?} {rejection}",
        connector.triggers(),
        connector.credential(),
        secret(),
    );
    assert!(
        !surface.contains(WEBHOOK_SECRET),
        "the inbound secret must not appear anywhere: {surface}"
    );
}
