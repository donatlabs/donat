use std::sync::{Arc, Once};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::{Body, to_bytes},
    http::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::Response,
};
use donat_server::{
    connector_webhook,
    connectors::ConnectorRegistry,
    state::{AppState, Engine, SharedState},
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tower::ServiceExt;

const API_KEY_ENV: &str = "DONAT_CONNECTOR_WEBHOOK_TEST_API_KEY";
const WEBHOOK_SECRET_ENV: &str = "DONAT_CONNECTOR_WEBHOOK_TEST_SECRET";
const WEBHOOK_SECRET: &str = "whsec_connector_webhook_test_secret";
const BODY_LIMIT: usize = 1024 * 1024;

type HmacSha256 = Hmac<Sha256>;

fn postgres_sources() -> serde_json::Value {
    serde_json::json!([{
        "name": "default",
        "kind": "postgres",
        "configuration": {}
    }])
}

fn configure_test_environment() {
    static CONFIGURED: Once = Once::new();
    CONFIGURED.call_once(|| {
        // SAFETY: these test-only names are owned by this test target and are
        // installed once before any registry resolves them.
        unsafe {
            std::env::set_var(API_KEY_ENV, "sk_test_connector_webhook_key");
            std::env::set_var(WEBHOOK_SECRET_ENV, WEBHOOK_SECRET);
        }
    });
}

fn state() -> SharedState {
    configure_test_environment();
    let metadata: donat_metadata::Metadata = serde_json::from_value(serde_json::json!({
        "version": 3,
        "sources": postgres_sources(),
        "connectors": [{
            "name": "payments",
            "module": "stripe",
            "config": {
                "endpoint_identity": "stripe_test_2026_07",
                "credential_identity": "stripe_test_credential",
                "secret_key": { "value_from_env": API_KEY_ENV },
                "webhook_secret": { "value_from_env": WEBHOOK_SECRET_ENV },
                "api_version": "2026-07-27"
            },
            "operations": [{
                "name": "checkout.create_session",
                "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                }
            }]
        }, {
            "name": "logistics",
            "module": "http",
            "config": {
                "endpoint_identity": "logistics_test_2026_07",
                "credential_identity": "logistics_test_credential",
                "base_url": "https://logistics.example.test"
            },
            "operations": []
        }]
    }))
    .expect("webhook test metadata deserializes");
    let connectors = Arc::new(
        ConnectorRegistry::build(&metadata).expect("Stripe webhook instance compiles at startup"),
    );
    Arc::new(AppState {
        rate_limiter: Default::default(),
        engine: tokio::sync::RwLock::new(Arc::new(
            Engine::bootstrap_checked(metadata).expect("empty test engine compiles"),
        )),
        connectors,
        default_url: "postgres://unused".to_owned(),
        unauthorized_role: None,
        oidc: None,
        stringify_numerics: false,
        infer_function_permissions: true,
        jwt: None,
        auth_hook: None,
        http: reqwest::Client::new(),
        allowlist_enabled: false,
        subscription_permits: Arc::new(tokio::sync::Semaphore::new(1_000)),
        subscription_poll_permits: Arc::new(tokio::sync::Semaphore::new(16)),
        storage: Arc::new(donat_storage::StorageRegistry::default()),
        external_base_url: String::new(),
    })
}

async fn post(state: SharedState, instance: &str, headers: HeaderMap, body: Vec<u8>) -> Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/v1/connectors/{instance}/webhooks"))
        .body(Body::from(body))
        .expect("fixed webhook test request is valid");
    *request.headers_mut() = headers;
    connector_webhook::router()
        .with_state(state)
        .oneshot(request)
        .await
        .expect("connector ingress router serves the test request")
}

async fn response_body(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), 1024)
        .await
        .expect("minimal ingress responses fit within the test bound")
        .to_vec()
}

fn signed_headers(body: &[u8]) -> HeaderMap {
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
    let value = format!("t={timestamp},v1={signature:x}");
    let mut headers = HeaderMap::new();
    headers.insert(
        "stripe-signature",
        HeaderValue::from_str(&value).expect("fixed signature header is valid"),
    );
    headers
}

#[tokio::test]
async fn unknown_instance_is_rejected_before_the_body_limit_is_considered() {
    // This fails if an undeclared route reads/parses its body before resolving
    // the compiled connector instance, allowing an oversized probe to reveal
    // a different ingress failure.
    let response = post(
        state(),
        "does-not-exist",
        HeaderMap::new(),
        vec![b'x'; BODY_LIMIT + 1],
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(response_body(response).await.is_empty());
}

#[tokio::test]
async fn declared_http_instance_is_indistinguishable_from_an_unknown_webhook_instance() {
    // This fails if the ingress route reveals that a declared HTTP connector
    // exists even though that compiled module has no inbound verifier.
    let unknown = post(
        state(),
        "does-not-exist",
        HeaderMap::new(),
        b"not-a-webhook".to_vec(),
    )
    .await;
    let declared_http = post(
        state(),
        "logistics",
        HeaderMap::new(),
        b"not-a-webhook".to_vec(),
    )
    .await;

    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    assert_eq!(declared_http.status(), StatusCode::NOT_FOUND);
    assert_eq!(declared_http.status(), unknown.status());
    assert_eq!(
        response_body(declared_http).await,
        response_body(unknown).await,
        "a declared module without an inbound verifier must not reveal its capability"
    );
}

#[tokio::test]
async fn declared_webhook_rejects_a_body_larger_than_one_mebibyte() {
    // This fails if the ingress route lets an unbounded raw request reach the
    // verifier or a future process mapper.
    let response = post(
        state(),
        "payments",
        HeaderMap::new(),
        vec![b'x'; BODY_LIMIT + 1],
    )
    .await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(response_body(response).await.is_empty());
}

#[tokio::test]
async fn invalid_signature_has_a_minimal_safe_rejection() {
    // This fails if raw ingress data, verifier diagnostics, or configured
    // credentials can escape in a signature-rejection response.
    let body = b"credential-sentinel: whsec_connector_webhook_test_secret".to_vec();
    let response = post(state(), "payments", HeaderMap::new(), body).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response_body(response).await.is_empty());
}

#[tokio::test]
async fn verified_stripe_event_is_not_acknowledged_before_durable_process_ingress_exists() {
    // This fails if a verified event is falsely acknowledged or synchronously
    // dispatched before the durable-process plan owns the ingress journal and
    // signal transaction. A non-2xx response makes Stripe retain the event.
    let body = br#"{"id":"evt_route_42","type":"checkout.session.completed","data":{"object":{"object":"checkout.session","id":"cs_route_42","client_reference_id":"00000000-0000-4000-8000-000000000042","payment_status":"paid"}}}"#.to_vec();
    let response = post(state(), "payments", signed_headers(&body), body).await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(response_body(response).await.is_empty());
}
