//! Donat-owned conformance cases for the deployed connector boundary.
//!
//! Connector activity execution has no HTTP caller surface yet: it is owned by
//! the future durable process worker. These cases intentionally exercise only
//! deployment startup and the provider-facing signed ingress route.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use donat_conformance::{Suite, engine_binary, fixture_root, load_fixture};
use hmac::{Hmac, Mac};
use serde_json::Value as Json;
use sha2::Sha256;

const API_KEY_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_STRIPE_API_KEY";
const WEBHOOK_SECRET_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_STRIPE_WEBHOOK_SECRET";
const WEBHOOK_SECRET: &str = "whsec-conformance-webhook-secret";
const PRESENT_SECRET_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_PRESENT_SECRET";
const PRESENT_SECRET: &str = "connector-present-secret-sentinel";
const PRESENT_WEBHOOK_SECRET_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_PRESENT_WEBHOOK_SECRET";
const PRESENT_WEBHOOK_SECRET: &str = "whsec-conformance-present-secret-sentinel";
const MISSING_API_KEY_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_MISSING_API_KEY";

type HmacSha256 = Hmac<Sha256>;

static NEXT_METADATA_DIR: AtomicU32 = AtomicU32::new(0);

fn fixture(name: &str) -> Json {
    load_fixture(&fixture_root().join("connectors").join(name))
        .unwrap_or_else(|error| panic!("load connector fixture {name}: {error}"))
}

fn fixture_text<'a>(fixture: &'a Json, pointer: &str) -> &'a str {
    fixture
        .pointer(pointer)
        .and_then(Json::as_str)
        .unwrap_or_else(|| panic!("fixture value {pointer} must be a string"))
}

fn fixture_status(fixture: &Json) -> u16 {
    fixture
        .pointer("/response/status")
        .and_then(Json::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .expect("fixture response status must fit in u16")
}

fn connector_metadata(fixture: &Json) -> donat_metadata::Metadata {
    serde_json::from_value(
        fixture
            .get("metadata")
            .cloned()
            .expect("connector metadata fixture has metadata"),
    )
    .expect("connector metadata fixture deserializes")
}

fn write_startup_metadata(fixture: &Json, case: &str) -> PathBuf {
    let suffix = NEXT_METADATA_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "donat-connector-conformance-{case}-{}-{suffix}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("databases"))
        .expect("create temporary connector metadata directory");
    std::fs::write(dir.join("version.yaml"), "version: 3\n").expect("write metadata version");
    std::fs::write(dir.join("databases/databases.yaml"), "[]\n")
        .expect("write empty source metadata");
    let connectors = fixture
        .pointer("/metadata/connectors")
        .cloned()
        .expect("connector startup fixture has connectors");
    std::fs::write(
        dir.join("connectors.yaml"),
        serde_yaml::to_string(&connectors).expect("serialize connector metadata"),
    )
    .expect("write connector metadata");
    dir
}

fn startup_output(metadata_dir: &Path, env: &[(&str, Option<&str>)]) -> (bool, String) {
    let mut command = Command::new(engine_binary());
    command.arg("--metadata-dir").arg(metadata_dir).env(
        "DONAT_DATABASE_URL",
        "postgresql://unused:unused@127.0.0.1:1/unused",
    );
    for (name, value) in env {
        match value {
            Some(value) => {
                command.env(name, value);
            }
            None => {
                command.env_remove(name);
            }
        }
    }
    let output = command
        .output()
        .expect("start donat with connector metadata");
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), combined)
}

fn stripe_signature(body: &[u8]) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs();
    let mut mac = HmacSha256::new_from_slice(WEBHOOK_SECRET.as_bytes())
        .expect("fixed webhook secret is a valid HMAC key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let signature = mac.finalize().into_bytes();
    format!("t={timestamp},v1={signature:x}")
}

#[test]
fn connector_static_metadata_rejection_never_discloses_resolved_credential_values() {
    let fixture = fixture("static_unknown_module.yaml");
    let metadata_dir = write_startup_metadata(&fixture, "static");
    let (success, output) =
        startup_output(&metadata_dir, &[(PRESENT_SECRET_ENV, Some(PRESENT_SECRET))]);
    let _ = std::fs::remove_dir_all(metadata_dir);

    assert!(
        !success,
        "invalid connector metadata must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/output_contains")),
        "startup must report the static metadata error: {output}"
    );
    assert!(
        !output.contains(fixture_text(&fixture, "/expect/output_excludes")),
        "startup error must not disclose the resolved credential: {output}"
    );
}

#[test]
fn connector_missing_secret_prevents_startup_without_disclosing_other_secret_values() {
    let fixture = fixture("missing_secret.yaml");
    let metadata_dir = write_startup_metadata(&fixture, "missing-secret");
    let (success, output) = startup_output(
        &metadata_dir,
        &[
            (MISSING_API_KEY_ENV, None),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
        ],
    );
    let _ = std::fs::remove_dir_all(metadata_dir);

    assert!(
        !success,
        "a missing connector secret must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/output_contains")),
        "startup must name only the unavailable variable: {output}"
    );
    assert!(
        !output.contains(fixture_text(&fixture, "/expect/output_excludes")),
        "startup error must not disclose another resolved secret: {output}"
    );
}

#[test]
fn stripe_signature_rejection_has_the_fixture_defined_minimal_http_response() {
    let metadata = connector_metadata(&fixture("stripe_webhook_metadata.yaml"));
    let case = fixture("stripe_signature_rejection.yaml");
    let body = fixture_text(&case, "/request/body").as_bytes();
    let suite = Suite::new("connector_signature_rejection")
        .initial_metadata(metadata)
        .env(API_KEY_ENV, "sk-conformance-api-key")
        .env(WEBHOOK_SECRET_ENV, WEBHOOK_SECRET)
        .start();
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "Stripe-Signature".to_string(),
            format!(
                "t={},v1=00",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after the Unix epoch")
                    .as_secs()
            ),
        ),
    ];

    let (status, response) = suite.post_bytes(fixture_text(&case, "/request/path"), body, &headers);

    assert_eq!(status, fixture_status(&case), "signature rejection status");
    assert_eq!(
        response,
        fixture_text(&case, "/response/body").as_bytes(),
        "signature rejection response body"
    );
}

#[test]
fn verified_stripe_event_has_no_delivery_acknowledgement_before_process_ingress() {
    let metadata = connector_metadata(&fixture("stripe_webhook_metadata.yaml"));
    let case = fixture("stripe_verified_event_unavailable.yaml");
    let body = fixture_text(&case, "/request/body").as_bytes();
    let suite = Suite::new("connector_verified_event_unavailable")
        .initial_metadata(metadata)
        .env(API_KEY_ENV, "sk-conformance-api-key")
        .env(WEBHOOK_SECRET_ENV, WEBHOOK_SECRET)
        .start();
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Stripe-Signature".to_string(), stripe_signature(body)),
    ];

    let (status, response) = suite.post_bytes(fixture_text(&case, "/request/path"), body, &headers);

    assert_eq!(status, fixture_status(&case), "verified event status");
    assert!(
        !(200..300).contains(&status),
        "a verified event must not be acknowledged before durable process ingress"
    );
    assert_eq!(
        response,
        fixture_text(&case, "/response/body").as_bytes(),
        "verified event response body"
    );
}
