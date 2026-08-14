//! Donat-owned conformance cases for the deployed connector boundary.
//!
//! Connector activity execution has no HTTP caller surface: it is owned by the
//! durable process worker. These cases intentionally exercise only deployment
//! startup and the provider-facing signed ingress route, whose verified
//! deliveries are acknowledged solely through their committed durable audit.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use donat_conformance::{Suite, engine_binary, fixture_root, load_fixture};
use hmac::{Hmac, Mac};
use postgres::NoTls;
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
    write_startup_metadata_from(fixture, "/metadata/connectors", case)
}

/// The same temporary deployment, from any one of a fixture's metadata
/// documents: a connector fixture carries more than one when the connector owes
/// more than one startup refusal.
fn write_startup_metadata_from(fixture: &Json, pointer: &str, case: &str) -> PathBuf {
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
        .pointer(pointer)
        .cloned()
        .unwrap_or_else(|| panic!("connector startup fixture has {pointer}"));
    std::fs::write(
        dir.join("connectors.yaml"),
        serde_yaml::to_string(&connectors).expect("serialize connector metadata"),
    )
    .expect("write connector metadata");
    dir
}

fn startup_output(metadata_dir: &Path, env: &[(&str, Option<&str>)]) -> (bool, String) {
    let mut command = Command::new(engine_binary());
    command
        .arg("--metadata-dir")
        .arg(metadata_dir)
        .env(
            "DONAT_DATABASE_URL",
            "postgresql://unused:unused@127.0.0.1:1/unused",
        )
        // These cases are about connector validation, and a boot that can
        // resolve no session refuses before it gets there. One explicit role
        // for every request is the cheapest of the three ways to satisfy it.
        .env("DONAT_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous");
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

/// The wire-level proof every hand-written connector owes (spec 010 §12): a
/// deployment of it is validated *before* a listener opens, and neither refusal
/// discloses a value the process resolved.
///
/// Two refusals, because a connector can be undeployable for two different
/// reasons and both must land at startup:
///
/// 1. a required environment value is unavailable — the message names that
///    variable and nothing else, while another instance's secret resolves
///    successfully in the same process;
/// 2. the deployment enabled an operation it may not reach — an inventory-only
///    one, an undeclared one, or one whose class this deployment's own target
///    denies it (`knowledgebase/declarative-saas/decisions/046-*`) — and the
///    message names the exact metadata path.
fn assert_connector_startup(module: &str) {
    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");

    let metadata_dir = write_startup_metadata(&fixture, module);
    let (success, output) = startup_output(
        &metadata_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(metadata_dir);
    assert!(
        !success,
        "a missing `{module}` connector secret must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/missing_variable")),
        "startup must name only the unavailable variable: {output}"
    );
    assert!(
        !output.contains(excluded),
        "startup error must not disclose another resolved secret: {output}"
    );

    let refused_dir = write_startup_metadata_from(&fixture, "/refused_metadata/connectors", module);
    let (success, output) = startup_output(
        &refused_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(refused_dir);
    assert!(
        !success,
        "an unreachable `{module}` operation must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/refused_operation")),
        "startup must name the refused operation and its metadata path: {output}"
    );
    assert!(
        !output.contains(excluded),
        "startup error must not disclose a resolved secret: {output}"
    );
}

#[test]
fn airtable_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("airtable");
}

#[test]
fn sendgrid_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("sendgrid");
}

#[test]
fn postmark_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("postmark");
}

#[test]
fn twilio_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("twilio");
}

#[test]
fn openai_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("openai");
}

#[test]
fn typeform_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("typeform");
}

#[test]
fn aws_s3_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("aws_s3");
}

#[test]
fn aws_sqs_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("aws_sqs");
}

#[test]
fn aws_ses_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("aws_ses");
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
fn verified_stripe_event_is_acknowledged_only_with_its_committed_durable_audit() {
    let metadata = connector_metadata(&fixture("stripe_webhook_metadata.yaml"));
    let case = fixture("stripe_verified_event_unmatched.yaml");
    let body = fixture_text(&case, "/request/body").as_bytes();
    let suite = Suite::new("connector_verified_event_unmatched")
        .initial_metadata(metadata)
        .env(API_KEY_ENV, "sk-conformance-api-key")
        .env(WEBHOOK_SECRET_ENV, WEBHOOK_SECRET)
        .start();
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Stripe-Signature".to_string(), stripe_signature(body)),
    ];
    let path = fixture_text(&case, "/request/path");
    let instance = fixture_text(&case, "/expect/connector_instance");
    let provider_event_id = fixture_text(&case, "/expect/provider_event_id");

    let (status, response) = suite.post_bytes(path, body, &headers);

    assert_eq!(status, fixture_status(&case), "verified event status");
    assert_eq!(
        response,
        fixture_text(&case, "/response/body").as_bytes(),
        "verified event response body"
    );

    // The acknowledgement is only trustworthy if the delivery audit and the
    // provider dedupe identity are already visible to an independent client.
    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect after the verified delivery is acknowledged");
    let first = client
        .query_one(
            "
            SELECT outcome, instance_id::text, process_event_id::text
            FROM donat.process_inbound_deliveries
            WHERE source_name = 'default'
              AND connector_instance = $1
              AND provider_event_id = $2
            ",
            &[&instance, &provider_event_id],
        )
        .expect("one committed delivery audit row");
    assert_eq!(
        first.get::<_, String>(0),
        fixture_text(&case, "/expect/first_outcome"),
        "a verified event with no receptive wait is audited, not accepted"
    );
    assert_eq!(first.get::<_, Option<String>>(1), None);
    assert_eq!(first.get::<_, Option<String>>(2), None);

    // A provider retry of the same verified event is acknowledged again, adds a
    // distinct audit row, and never duplicates the dedupe identity.
    let (repeat_status, repeat_response) = suite.post_bytes(path, body, &headers);
    assert_eq!(repeat_status, fixture_status(&case), "repeat event status");
    assert_eq!(
        repeat_response,
        fixture_text(&case, "/response/body").as_bytes()
    );

    let ledger = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE source_name = 'default' AND connector_instance = $1
                   AND provider_event_id = $2),
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE source_name = 'default' AND connector_instance = $1
                   AND provider_event_id = $2 AND outcome = $3),
                (SELECT count(*) FROM donat.process_inbound_events
                 WHERE source_name = 'default' AND connector_instance = $1
                   AND provider_event_id = $2)
            ",
            &[
                &instance,
                &provider_event_id,
                &fixture_text(&case, "/expect/repeat_outcome"),
            ],
        )
        .expect("read the split inbound audit and dedupe ledger");
    assert_eq!(ledger.get::<_, i64>(0), 2, "every attempt is audited");
    assert_eq!(
        ledger.get::<_, i64>(1),
        1,
        "the retry is audited as duplicate"
    );
    assert_eq!(ledger.get::<_, i64>(2), 1, "one provider dedupe identity");
}

// ===========================================================================
// Batch B: the webhook-bearing connectors (spec 013)
// ===========================================================================

/// The Donat-owned credentials every Batch B fixture is deployed with. The
/// inbound secret is also what these tests sign with, so a fixture's signature
/// is generated here rather than copied from anywhere.
const BATCH_B_API_KEY_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_BATCH_B_API_KEY";
const BATCH_B_API_KEY: &str = "donat-batch-b-conformance-api-key";
const BATCH_B_WEBHOOK_SECRET_ENV: &str = "DONAT_CONNECTOR_CONFORMANCE_BATCH_B_WEBHOOK_SECRET";
const BATCH_B_WEBHOOK_SECRET: &str = "donat-batch-b-conformance-webhook-secret";

/// The SDK's shared raw-body ceiling, which is what every Batch B trigger
/// declares. One byte past it is the `413` case.
const RAW_BODY_CEILING: usize = 1024 * 1024;

fn batch_b_digest(message: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(BATCH_B_WEBHOOK_SECRET.as_bytes())
        .expect("the fixed conformance secret is a valid HMAC key");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn batch_b_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn batch_b_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn header(name: &str, value: String) -> (String, String) {
    (name.to_owned(), value)
}

/// The deployed route matrix spec 013 §4 proof 4 demands, for one connector.
///
/// `sign` is this test's own transcription of the provider's published scheme,
/// and `forge` is a signature that is wrong under it. A connector's declaration
/// and these transcriptions are written separately and have to agree, which is
/// what makes a green run evidence rather than a tautology.
fn assert_route_matrix(
    module: &str,
    sign: fn(&[u8]) -> Vec<(String, String)>,
    forge: fn() -> Vec<(String, String)>,
) {
    let fixture = fixture(&format!("{module}_webhook.yaml"));
    let suite = inbound_suite(module, "matrix", &fixture);
    let path = fixture_text(&fixture, "/request/path");
    let unknown_path = fixture_text(&fixture, "/request/unknown_path");
    let body = fixture_text(&fixture, "/request/body").as_bytes().to_vec();
    let expected_body = fixture_text(&fixture, "/expect/body").as_bytes();

    // 1. An instance this deployment never declared. It is answered before the
    //    body is read, so an undeclared name is indistinguishable from any
    //    other absent route even when its body is oversized.
    let (status, response) = suite.post_bytes(unknown_path, &body, &json_header());
    assert_eq!(
        status,
        fixture_status_at(&fixture, "/expect/unknown_instance_status"),
        "{module}: an unknown instance"
    );
    assert_eq!(response, expected_body, "{module}: unknown instance body");

    // 2. One byte past the declared ceiling, carrying a *correct* signature.
    //    An oversized authentic body would verify if the ceiling were applied
    //    after the MAC, so this is where that ordering is proven on the wire.
    let oversized = vec![b'x'; RAW_BODY_CEILING + 1];
    let (status, response) = suite.post_bytes(path, &oversized, &sign(&oversized));
    assert_eq!(
        status,
        fixture_status_at(&fixture, "/expect/oversized_status"),
        "{module}: an oversized body"
    );
    assert_eq!(response, expected_body, "{module}: oversized body");

    // 3. A body that is both malformed JSON and incorrectly signed. It is
    //    answered from the raw bytes alone.
    let malformed = br#"{"id":"evt_1","action":"opened""#.to_vec();
    let mut forged = json_header();
    forged.extend(forge());
    let (status, response) = suite.post_bytes(path, &malformed, &forged);
    assert_eq!(
        status,
        fixture_status_at(&fixture, "/expect/invalid_status"),
        "{module}: an invalid signature"
    );
    assert_eq!(response, expected_body, "{module}: invalid signature body");

    // 4. A delivery that verifies. Spec 013 §0 stands: the answer is `503`.
    let mut authentic = json_header();
    authentic.extend(sign(&body));
    let (status, response) = suite.post_bytes(path, &body, &authentic);
    assert_eq!(
        status,
        fixture_status_at(&fixture, "/expect/verified_status"),
        "{module}: a verified delivery is not acknowledged"
    );
    assert_eq!(response, expected_body, "{module}: verified delivery body");
}

/// Spec 013 §4 proof 6: after a successful verification, no row exists in any
/// inbound table and no process transition is created.
///
/// It is its own deployment rather than a tail of the route matrix, so the
/// tables it reads have seen exactly one request: a delivery that verified.
fn assert_verified_event_is_not_persisted(module: &str, sign: fn(&[u8]) -> Vec<(String, String)>) {
    let fixture = fixture(&format!("{module}_webhook.yaml"));
    let suite = inbound_suite(module, "unpersisted", &fixture);
    let path = fixture_text(&fixture, "/request/path");
    let instance = fixture_text(&fixture, "/request/instance");
    let body = fixture_text(&fixture, "/request/body").as_bytes().to_vec();

    let mut authentic = json_header();
    authentic.extend(sign(&body));
    let (status, _) = suite.post_bytes(path, &body, &authentic);
    assert_eq!(
        status,
        fixture_status_at(&fixture, "/expect/verified_status"),
        "{module}: the delivery verified"
    );

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect after the route answered");
    let ledger = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE connector_instance = $1),
                (SELECT count(*) FROM donat.process_inbound_events
                 WHERE connector_instance = $1),
                (SELECT count(*) FROM donat.process_transition_logs),
                (SELECT count(*) FROM donat.process_events)
            ",
            &[&instance],
        )
        .expect("read the inbound ledger");
    assert_eq!(
        ledger.get::<_, i64>(0),
        fixture_count(&fixture, "/expect/persisted_deliveries"),
        "{module}: a verified delivery must not be audited before its inbound transaction exists"
    );
    assert_eq!(
        ledger.get::<_, i64>(1),
        fixture_count(&fixture, "/expect/persisted_events"),
        "{module}: a verified delivery must not create a dedupe identity"
    );
    assert_eq!(
        (ledger.get::<_, i64>(2), ledger.get::<_, i64>(3)),
        (0, 0),
        "{module}: a verified delivery must not create a process transition or a process event"
    );
}

/// One deployment of one Batch B connector, from its own fixture.
///
/// `case` keeps each test's deployment in its own database, so the tables proof
/// 6 reads have seen only that test's own requests.
fn inbound_suite(module: &str, case: &str, fixture: &Json) -> donat_conformance::Running {
    Suite::new(&format!("connector_{module}_{case}"))
        .initial_metadata(connector_metadata(fixture))
        .env(BATCH_B_API_KEY_ENV, BATCH_B_API_KEY)
        .env(BATCH_B_WEBHOOK_SECRET_ENV, BATCH_B_WEBHOOK_SECRET)
        .start()
}

fn json_header() -> Vec<(String, String)> {
    vec![("Content-Type".to_string(), "application/json".to_string())]
}

fn fixture_status_at(fixture: &Json, pointer: &str) -> u16 {
    fixture
        .pointer(pointer)
        .and_then(Json::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or_else(|| panic!("fixture value {pointer} must be a status"))
}

fn fixture_count(fixture: &Json, pointer: &str) -> i64 {
    fixture
        .pointer(pointer)
        .and_then(Json::as_i64)
        .unwrap_or_else(|| panic!("fixture value {pointer} must be a row count"))
}

/// GitHub: "the HMAC hex digest of the request body … generated using the
/// SHA-256 hash function and the `secret` as the HMAC `key`", behind `sha256=`.
fn github_signature(body: &[u8]) -> Vec<(String, String)> {
    vec![
        header(
            "X-Hub-Signature-256",
            format!("sha256={}", batch_b_hex(&batch_b_digest(body))),
        ),
        header("X-GitHub-Event", "issues".to_owned()),
        header(
            "X-GitHub-Delivery",
            "72d3162e-cc78-11e3-81ab-4c9367dc0958".to_owned(),
        ),
    ]
}

/// Shopify: "a base64-encoded HMAC signature in the `X-Shopify-Hmac-SHA256`
/// header, generated using your app's client secret and the raw request body."
fn shopify_signature(body: &[u8]) -> Vec<(String, String)> {
    vec![
        header(
            "X-Shopify-Hmac-Sha256",
            batch_b_base64(&batch_b_digest(body)),
        ),
        header("X-Shopify-Topic", "orders/create".to_owned()),
    ]
}

/// Telegram: "the request will contain a header
/// 'X-Telegram-Bot-Api-Secret-Token' with the secret token as content."
fn telegram_signature(_body: &[u8]) -> Vec<(String, String)> {
    vec![header(
        "X-Telegram-Bot-Api-Secret-Token",
        BATCH_B_WEBHOOK_SECRET.to_owned(),
    )]
}

/// Calendly: `t=<unix>,v1=<hex>`, over the timestamp, a `.`, and the raw body.
fn calendly_signature(body: &[u8]) -> Vec<(String, String)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs();
    let mut canonical = timestamp.to_string().into_bytes();
    canonical.push(b'.');
    canonical.extend_from_slice(body);
    vec![header(
        "Calendly-Webhook-Signature",
        format!(
            "t={timestamp},v1={}",
            batch_b_hex(&batch_b_digest(&canonical))
        ),
    )]
}

/// Sentry: HMAC-SHA256 of the body under the integration's Client Secret,
/// hexadecimal, in `Sentry-Hook-Signature`.
fn sentry_signature(body: &[u8]) -> Vec<(String, String)> {
    vec![
        header("Sentry-Hook-Signature", batch_b_hex(&batch_b_digest(body))),
        header("Sentry-Hook-Resource", "issue".to_owned()),
        header("Request-ID", "0d0b0b3a1a9b4b0f8f0a1b2c3d4e5f60".to_owned()),
    ]
}

/// Typeform: base64 of the HMAC-SHA256 of the whole payload, behind `sha256=`.
fn typeform_signature(body: &[u8]) -> Vec<(String, String)> {
    vec![header(
        "Typeform-Signature",
        format!("sha256={}", batch_b_base64(&batch_b_digest(body))),
    )]
}

#[test]
fn github_route_matrix() {
    assert_route_matrix("github", github_signature, github_forgery);
}

#[test]
fn github_verified_event_is_not_persisted() {
    assert_verified_event_is_not_persisted("github", github_signature);
}

#[test]
fn shopify_route_matrix() {
    assert_route_matrix("shopify", shopify_signature, shopify_forgery);
}

#[test]
fn shopify_verified_event_is_not_persisted() {
    assert_verified_event_is_not_persisted("shopify", shopify_signature);
}

#[test]
fn telegram_route_matrix() {
    assert_route_matrix("telegram", telegram_signature, telegram_forgery);
}

#[test]
fn telegram_verified_event_is_not_persisted() {
    assert_verified_event_is_not_persisted("telegram", telegram_signature);
}

#[test]
fn calendly_route_matrix() {
    assert_route_matrix("calendly", calendly_signature, calendly_forgery);
}

#[test]
fn calendly_verified_event_is_not_persisted() {
    assert_verified_event_is_not_persisted("calendly", calendly_signature);
}

#[test]
fn sentry_route_matrix() {
    assert_route_matrix("sentry", sentry_signature, sentry_forgery);
}

#[test]
fn sentry_verified_event_is_not_persisted() {
    assert_verified_event_is_not_persisted("sentry", sentry_signature);
}

#[test]
fn typeform_route_matrix() {
    assert_route_matrix("typeform", typeform_signature, typeform_forgery);
}

#[test]
fn typeform_verified_event_is_not_persisted() {
    assert_verified_event_is_not_persisted("typeform", typeform_signature);
}

#[test]
fn github_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("github");
}

#[test]
fn shopify_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("shopify");
}

#[test]
fn telegram_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("telegram");
}

#[test]
fn calendly_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("calendly");
}

#[test]
fn sentry_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("sentry");
}

// ---------------------------------------------------------------------------
// The forged half of each scheme.
//
// It is written per connector rather than derived from `sign`, because
// Telegram's scheme is the one here that does not cover the body: signing a
// different message is a forgery for the five HMAC connectors and is a
// perfectly valid delivery for Telegram, whose header carries only the shared
// secret.
// ---------------------------------------------------------------------------

fn github_forgery() -> Vec<(String, String)> {
    github_signature(b"a different message")
}

fn shopify_forgery() -> Vec<(String, String)> {
    shopify_signature(b"a different message")
}

fn telegram_forgery() -> Vec<(String, String)> {
    vec![header(
        "X-Telegram-Bot-Api-Secret-Token",
        "not-the-configured-secret".to_owned(),
    )]
}

fn calendly_forgery() -> Vec<(String, String)> {
    calendly_signature(b"a different message")
}

fn sentry_forgery() -> Vec<(String, String)> {
    sentry_signature(b"a different message")
}

fn typeform_forgery() -> Vec<(String, String)> {
    typeform_signature(b"a different message")
}

// ===========================================================================
// Batch E: the product SaaS connectors (spec 016)
//
// None of these publishes an inbound route, so each owes exactly the startup
// proof of spec 010 §12: a deployment is validated before a listener opens, a
// missing credential names only its own variable, and an operation the
// deployment may not reach names its metadata path.
// ===========================================================================

#[test]
fn slack_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("slack");
}

#[test]
fn linear_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("linear");
}

#[test]
fn notion_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("notion");
}

#[test]
fn intercom_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("intercom");
}

#[test]
fn hubspot_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("hubspot");
}

#[test]
fn jira_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("jira");
}

// ===========================================================================
// Batch C: the Google Workspace connectors (spec 014)
//
// These four owe the startup proof every connector owes, and one more that is
// theirs alone: the scope set a deployment declares in `config.oauth2.scopes`
// has to authorize the operations it enabled, and a deployment where it does
// not is refused before a listener opens rather than at the first activity
// attempt (spec 014 §3.1). The `google_*` fixtures therefore carry a third
// metadata document.
// ===========================================================================

/// The startup proof of spec 010 §12, plus `<name>_scope_shortfall_fails_closed`
/// against the real binary.
fn assert_google_connector_startup(module: &str) {
    assert_connector_startup(module);

    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let scope_dir = write_startup_metadata_from(&fixture, "/scope_metadata/connectors", module);
    let (success, output) = startup_output(
        &scope_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(scope_dir);
    assert!(
        !success,
        "an operation no declared `{module}` scope authorizes must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/scope_shortfall")),
        "startup must name the unauthorized operation and `config.oauth2.scopes`: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

#[test]
fn google_sheets_deployment_is_validated_before_a_listener_opens() {
    assert_google_connector_startup("google_sheets");
}

#[test]
fn google_drive_deployment_is_validated_before_a_listener_opens() {
    assert_google_connector_startup("google_drive");
}

#[test]
fn google_gmail_deployment_is_validated_before_a_listener_opens() {
    assert_google_connector_startup("google_gmail");
}

#[test]
fn google_calendar_deployment_is_validated_before_a_listener_opens() {
    assert_google_connector_startup("google_calendar");
}

// ===========================================================================
// Batch D: the Microsoft 365 connectors (spec 015)
//
// These four owe the startup proof every connector owes, and the same
// permission proof Batch C owes, for the same reason: the grant a deployment
// declares in `config.oauth2.scopes` has to authorize the operations it
// enabled, and a deployment where it does not is refused before a listener
// opens rather than at the first activity attempt (spec 015 §3). The
// `microsoft_*` fixtures therefore carry a third metadata document, and every
// instance in them declares `offline_access` — the scope Microsoft publishes as
// the precondition for receiving a refresh token, which is the whole credential
// story of this batch.
// ===========================================================================

/// The startup proof of spec 010 §12, plus
/// `<name>_permission_shortfall_fails_closed` against the real binary.
fn assert_microsoft_connector_startup(module: &str) {
    assert_connector_startup(module);

    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let scope_dir = write_startup_metadata_from(&fixture, "/scope_metadata/connectors", module);
    let (success, output) = startup_output(
        &scope_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(scope_dir);
    assert!(
        !success,
        "an operation no declared `{module}` permission authorizes must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/scope_shortfall")),
        "startup must name the unauthorized operation and `config.oauth2.scopes`: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

#[test]
fn microsoft_outlook_deployment_is_validated_before_a_listener_opens() {
    assert_microsoft_connector_startup("microsoft_outlook");
}

#[test]
fn microsoft_teams_deployment_is_validated_before_a_listener_opens() {
    assert_microsoft_connector_startup("microsoft_teams");
}

#[test]
fn microsoft_excel_deployment_is_validated_before_a_listener_opens() {
    assert_microsoft_connector_startup("microsoft_excel");
}

#[test]
fn microsoft_onedrive_deployment_is_validated_before_a_listener_opens() {
    assert_microsoft_connector_startup("microsoft_onedrive");
}

// ===========================================================================
// Batch J: the payments and billing connectors (spec 026)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12: a deployment is validated before a listener opens, a missing
// credential names only its own variable, and an operation the deployment may
// not reach names its metadata path. For this batch the refused operation is
// deliberately the one that moves money — a refund a provider publishes no
// idempotency key for is not executable, and startup is where a deployment
// finds that out.
// ===========================================================================

#[test]
fn paddle_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("paddle");
}

/// Mercado Pago's refusal is its near-miss made operational: the provider
/// publishes `X-Idempotency-Key` and no retention for it, so the refund is
/// inventory-only and a deployment that enables it never serves (spec 026 §2).
#[test]
fn mercado_pago_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("mercado_pago");
}

/// Xero's refusal is the other half of its `ExplicitKey` class: a deployment
/// whose send horizon reaches past the documented six-minute key retention less
/// the clock safety margin is refused before a listener opens, because past that
/// point the same key is a second write rather than a replay (spec 026 §4).
#[test]
fn xero_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("xero");
}

/// PayPal owes both refusals, because it has both shapes.
///
/// The first is the refund: PayPal publishes `PayPal-Request-Id` for it and a
/// replay example, but publishes no retention in the Payments v2 reference its
/// own idempotency guide sends you to, so the class is a near-miss and the
/// operation is not executable (spec 026 §2, §3).
///
/// The second is the send horizon, which is where this connector differs from
/// Xero's: PayPal publishes a *different* retention per API — six hours for
/// Orders v2, seventy-two for Billing Subscriptions — and one instance holds
/// both, so the deployment-wide horizon is bounded by the shortest of them.
#[test]
fn paypal_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("paypal");

    let fixture = fixture("paypal_startup.yaml");
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let horizon_dir =
        write_startup_metadata_from(&fixture, "/horizon_metadata/connectors", "paypal");
    let (success, output) = startup_output(
        &horizon_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(horizon_dir);
    assert!(
        !success,
        "a `paypal` send horizon past the documented key retention must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/refused_horizon")),
        "startup must name `config.settings.send_horizon_ms`: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

// ===========================================================================
// Batch G: the CRM and helpdesk connectors (spec 023)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12. Four of the six also carry a per-tenant host or a value that
// completes their declaration, so their fixtures configure one — a mistyped
// host is a startup refusal here rather than a `404` on the first activity
// attempt. The two whose credential is a stored OAuth2 token owe one refusal
// more, and it is the module's own: Salesforce's declared scope set, and Zoho's
// data centre.
// ===========================================================================

/// The startup proof of spec 010 §12, plus the one module-specific refusal a
/// Batch G stored-credential connector owes, against the real binary.
fn assert_crm_connector_startup(module: &str) {
    assert_connector_startup(module);

    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let refusal_dir = write_startup_metadata_from(&fixture, "/scope_metadata/connectors", module);
    let (success, output) = startup_output(
        &refusal_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(refusal_dir);
    assert!(
        !success,
        "a `{module}` credential declaration its own module refuses must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/module_refusal")),
        "startup must name the configuration key it refused: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

#[test]
fn pipedrive_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("pipedrive");
}

#[test]
fn freshdesk_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("freshdesk");
}

/// Zendesk's refused operation is the one its provider documents as repeat-safe
/// over a `POST`: an operation that wants a class which keeps the retry is not
/// executable, and a deployment that enables it never serves (spec 023 §3).
#[test]
fn zendesk_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("zendesk");
}

#[test]
fn woocommerce_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("woocommerce");
}

/// Salesforce owes the scope refusal beside the startup proof: the `api` scope
/// is what Salesforce publishes for this whole surface, and a deployment that
/// did not declare it could not have authorized a single operation it enabled.
#[test]
fn salesforce_deployment_is_validated_before_a_listener_opens() {
    assert_crm_connector_startup("salesforce");
}

/// Zoho owes the data-centre refusal: it serves one org from one centre, so a
/// deployment whose token endpoint belongs to another one is refused before a
/// listener opens rather than authenticating into an org it cannot reach.
#[test]
fn zoho_crm_deployment_is_validated_before_a_listener_opens() {
    assert_crm_connector_startup("zoho_crm");
}

// ===========================================================================
// Batch H: the project-tracking and collaboration connectors (spec 024)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12: a deployment is validated before a listener opens, a missing
// credential names only its own variable, and an operation the deployment may
// not reach names its metadata path. Two are more than that. Trello's
// credential is *two* secrets, so its missing-variable case withholds one half
// while the other resolves. Basecamp's declaration is one a deployment
// completes — its account id is the first path segment of every URL it renders —
// so it owes a refusal of a value its provider's own grammar does not admit.
// ===========================================================================

/// The startup proof of spec 010 §12, plus the refusal Basecamp owes for the
/// deploy-time value compiled into every path it renders, against the real
/// binary.
fn assert_basecamp_connector_startup(module: &str) {
    assert_connector_startup(module);

    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let refusal_dir = write_startup_metadata_from(&fixture, "/account_metadata/connectors", module);
    let (success, output) = startup_output(
        &refusal_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(refusal_dir);
    assert!(
        !success,
        "a `{module}` account id its own module refuses must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/module_refusal")),
        "startup must name the configuration key it refused: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

#[test]
fn asana_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("asana");
}

/// Trello's missing-variable case is the two-secret one: the key resolves and
/// the token does not, and startup names only the variable it could not read
/// (spec 024 §3).
#[test]
fn trello_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("trello");
}

#[test]
fn clickup_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("clickup");
}

/// monday's refused operation is the other half of this batch's idempotency
/// finding: monday publishes an `Idempotency-Key` whose 30-minute cache carries
/// an unquantified escape clause, so no mutation of it is provider-idempotent
/// and the delete is not reachable at all (spec 024 §2).
#[test]
fn monday_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("monday");
}

/// Todoist's refused operation is the one whose second send its own reference
/// publishes — "Returns `NOT_FOUND` when the task does not exist" — which is a
/// refusal rather than the same one absent task.
#[test]
fn todoist_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("todoist");
}

/// Basecamp owes the account refusal beside the startup proof, and its refused
/// operation is the write its provider marks repeat-safe over a `POST`: a class
/// that keeps the retry does not exist, so `todo.complete` is inventory-only and
/// a deployment that enables it never serves (spec 024 §3).
#[test]
fn basecamp_deployment_is_validated_before_a_listener_opens() {
    assert_basecamp_connector_startup("basecamp");
}

// ===========================================================================
// Batch I: the storage and messaging connectors (spec 025)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12. Two of the seven are one provider on two origins — Dropbox
// serves metadata from `api.dropboxapi.com` and content from
// `content.dropboxapi.com`, and a connector has one compiled origin — so a
// deployment that needs both is two instances here, exactly as it is in
// production. Two more carry a host the deployment names, and two publish the
// scopes their operations need, so those owe one refusal more.
// ===========================================================================

/// The startup proof of spec 010 §12, plus the scope refusal a Batch I
/// connector whose provider publishes its scope set owes, against the real
/// binary.
fn assert_storage_connector_startup(module: &str) {
    assert_connector_startup(module);

    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let refusal_dir = write_startup_metadata_from(&fixture, "/scope_metadata/connectors", module);
    let (success, output) = startup_output(
        &refusal_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(refusal_dir);
    assert!(
        !success,
        "a `{module}` operation its declared scopes do not authorize must prevent serving: \
         {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/module_refusal")),
        "startup must name `config.oauth2.scopes`: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

/// Dropbox's refused operation is the whole shape of its write surface: it
/// serves every endpoint over `POST`, so `NaturalMethod` cannot reach one, and
/// its published error union makes a repeat a refusal rather than a second
/// effect — which wants a class that keeps the retry rather than ADR 063's
/// (spec 025 §4).
#[test]
fn dropbox_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("dropbox");
}

/// The second origin, as a deployment meets it: `dropbox_content` is its own
/// module with its own instance, and the metadata read that lives on the other
/// connector is a name this module was not built with (spec 025 §2).
#[test]
fn dropbox_content_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("dropbox_content");
}

/// Box owes the scope refusal beside the startup proof, and its refused
/// operation is the folder delete: Box publishes "or has already been deleted"
/// for a file and publishes nothing of the kind for a folder, so one delete is
/// `NaturalMethod` and the other is not.
#[test]
fn box_deployment_is_validated_before_a_listener_opens() {
    assert_storage_connector_startup("box");
}

/// Discord's refused operation is this batch's sharpest classification: it
/// publishes `nonce` with `enforce_nonce`, a real deduplication mechanism, and
/// publishes no retention for it — so `ExplicitKey` is refused under ADR 073 and
/// the at-most-once class is refused under ADR 063, which is admitted on an
/// absence there is not one of (spec 025 §4).
#[test]
fn discord_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("discord");
}

/// Mattermost is self-hosted: the deployment names the whole origin, and the
/// module refuses one it may not send a bearer token to.
#[test]
fn mattermost_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("mattermost");
}

/// Mailchimp's data centre is one host label from deploy-time configuration,
/// never from input.
#[test]
fn mailchimp_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("mailchimp");
}

/// Zoom owes the scope refusal beside the startup proof: it publishes
/// `meeting:read` and `meeting:write`, and a deployment that authorized only the
/// first cannot enable the delete.
#[test]
fn zoom_deployment_is_validated_before_a_listener_opens() {
    assert_storage_connector_startup("zoom");
}

// ===========================================================================
// Batch K: the development and monitoring connectors (spec 027)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12: a deployment is validated before a listener opens, a missing
// credential names only its own variable, and an operation the deployment may
// not reach names its metadata path. Four of the six carry a value that
// completes their declaration or names their host, so their fixtures configure
// one — a mistyped instance origin or account address is a startup refusal here
// rather than a bearer token on a cleartext connection.
// ===========================================================================

/// GitLab's instance is the deployment's own, named as a whole origin, and every
/// operation it declares is executable — so the refusal it owes is for a name
/// this binary was never built with.
#[test]
fn gitlab_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("gitlab");
}

/// Grafana's instance origin is deploy-time configuration too, and its refused
/// operation is the `PUT` whose effect the provider never described.
#[test]
fn grafana_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("grafana");
}

/// Bitbucket's HTTP Basic username is the Atlassian account address, which the
/// auth plan carries and no request may choose.
#[test]
fn bitbucket_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("bitbucket");
}

/// PagerDuty's `From` is the account user every write is attributed to, and its
/// refused operation is the partial state change beside the create whose
/// published deduplication key is a rejection rather than an absorption.
#[test]
fn pagerduty_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("pagerduty");
}

/// UptimeRobot's credential is a bearer token because this connector declares
/// the v3 surface; its refused operation is the write the provider documents as
/// repeat-safe over a `POST`, which wants a class that keeps the retry.
#[test]
fn uptimerobot_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("uptimerobot");
}

/// Cloudflare's refused operation is the zone `PATCH`, one line down from the
/// DNS-record `PUT` its provider publishes as "Overwrite".
#[test]
fn cloudflare_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("cloudflare");
}

// ===========================================================================
// Batch L, the forms half: the forms and surveys connectors (spec 028)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12: a deployment is validated before a listener opens, a missing
// credential names only its own variable, and an operation the deployment may
// not reach names its metadata path. Each also configures the non-secret half
// of its deploy-time material beside its secret one, so a mistyped region or
// account is a startup refusal rather than an API key on a host the connector
// was never declared against (spec 028 §3).
// ===========================================================================

/// Jotform's region names one of the three API URLs it publishes, chosen from a
/// compiled table rather than filled into a template; its refused operation is
/// the `DELETE` whose second send the provider is silent about.
#[test]
fn jotform_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("jotform");
}

/// SurveyMonkey's origin is a compiled constant and its access token is the
/// whole credential, so its fixture configures no setting; its refused
/// operation is the `DELETE` whose second send the provider is silent about.
#[test]
fn surveymonkey_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("surveymonkey");
}

/// Cal.com pins its API version per operation, so its declaration needs no
/// deploy-time setting at all; its refused operation is the booking cancel — a
/// `POST` the gate does not admit, for which the provider publishes no
/// consequence of a second send either.
#[test]
fn cal_com_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("cal_com");
}

/// Acuity's HTTP Basic username is the account's numeric User ID, which is
/// deploy-time configuration rather than a secret and is held to the provider's
/// own grammar before a listener opens; its refused operation is the cancel — a
/// `PUT` against a fixed identity that the gate still does not admit, because
/// the method is not the evidence.
#[test]
fn acuity_deployment_is_validated_before_a_listener_opens() {
    assert_connector_startup("acuity");
}

// ===========================================================================
// Batch L, the scheduling and people half (spec 028)
//
// None of these publishes an inbound route, so each owes the startup proof of
// spec 010 §12. What is different about this half of the batch is the split
// between a secret and a configured identity: Harvest sends a Personal Access
// Token *and* an account id on every request, and only the first is a secret.
// The second is `config.settings` material with a grammar of its own, so a
// mistyped account is a startup refusal here rather than a header this
// connector would have sent to somebody else's account.
// ===========================================================================

/// The startup proof of spec 010 §12, plus the refusal a Batch L declaration
/// owes for the non-secret deploy-time value compiled into every request it
/// renders, against the real binary.
fn assert_people_connector_startup(module: &str) {
    assert_connector_startup(module);

    let fixture = fixture(&format!("{module}_startup.yaml"));
    let excluded = fixture_text(&fixture, "/expect/output_excludes");
    let refusal_dir = write_startup_metadata_from(&fixture, "/account_metadata/connectors", module);
    let (success, output) = startup_output(
        &refusal_dir,
        &[
            (PRESENT_SECRET_ENV, Some(PRESENT_SECRET)),
            (PRESENT_WEBHOOK_SECRET_ENV, Some(PRESENT_WEBHOOK_SECRET)),
            (MISSING_API_KEY_ENV, None),
        ],
    );
    let _ = std::fs::remove_dir_all(refusal_dir);
    assert!(
        !success,
        "a `{module}` account identifier its own module refuses must prevent serving: {output}"
    );
    assert!(
        output.contains(fixture_text(&fixture, "/expect/module_refusal")),
        "startup must name the configuration key it refused: {output}"
    );
    assert!(
        !output.contains(excluded),
        "a startup error must not disclose a resolved secret: {output}"
    );
}

/// Harvest owes the account refusal beside the startup proof, because the
/// account id is the non-secret half of what it sends on every request. Its
/// refused operation is the `PATCH` partial update: a method the gate does not
/// admit for `NaturalMethod`, and an operation whose second send its provider
/// never described, so there is no consequence to record for at-most-once
/// either (spec 028 §3).
#[test]
fn harvest_deployment_is_validated_before_a_listener_opens() {
    assert_people_connector_startup("harvest");
}

/// BambooHR owes the same refusal for a different reason: its company subdomain
/// is one host *label*, so a deployment that typed a whole host is refused
/// before a listener opens rather than sending an API key to another authority.
/// Its refused operation is the partial update its provider publishes over a
/// `POST` and says nothing about repeating (spec 028 §3).
#[test]
fn bamboohr_deployment_is_validated_before_a_listener_opens() {
    assert_people_connector_startup("bamboohr");
}

/// Clockify owes the same refusal for a third reason: its workspace is the first
/// scoped path segment of every URL it renders, so a mistyped one is a startup
/// refusal rather than a request into a workspace nobody configured. Its refused
/// operation is the `PUT` whose second send its provider never described — the
/// case that shows `NaturalMethod` is evidence rather than a method (spec 028
/// §3).
#[test]
fn clockify_deployment_is_validated_before_a_listener_opens() {
    assert_people_connector_startup("clockify");
}

/// Eventbrite owes the same refusal for its organization, which is a path
/// segment of the event collection and the event create: a mistyped one would be
/// an event created in somebody else's organization, so it never reaches a
/// listener. Its refused operation is the partial update its provider publishes
/// over a `POST` (spec 028 §3).
#[test]
fn eventbrite_deployment_is_validated_before_a_listener_opens() {
    assert_people_connector_startup("eventbrite");
}
