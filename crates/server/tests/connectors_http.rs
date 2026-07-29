use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{OriginalUri, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use donat_server::connectors::{
    ConnectorErrorClass, ConnectorRegistry, ExecutionContext,
    http::{
        ConfiguredHeader, HostResolver, HttpConnector, HttpConnectorConfig, HttpMethod,
        HttpOperation, HttpTransport, JsonTemplate, NetworkPolicy, RawHttpResponse,
        ReqwestTransport, ResolveError, SystemResolver, TransportError,
    },
};
use serde_json::{Value as JsonValue, json};
use tokio::sync::Mutex;

const ONE_MIB: usize = 1024 * 1024;

#[test]
fn registry_has_only_the_compiled_http_and_stripe_modules_and_is_immutable_after_build() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [],
        "connectors": [{
            "name": "logistics",
            "module": "http",
            "config": {
                "endpoint_identity": "logistics_test",
                "credential_identity": "logistics_test_credential",
                "base_url": "https://logistics.example.test"
            }
        }]
    }))
    .expect("compiled connector metadata deserializes");

    let registry = ConnectorRegistry::build(&metadata).expect("built-in http instance validates");

    assert_eq!(
        ConnectorRegistry::built_in_module_names(),
        ["http", "stripe"],
        "registry exposes the compiled-in table, not a dynamic module loader"
    );
    assert!(registry.http_instance("logistics").is_some());
    assert!(registry.http_instance("unknown").is_none());
}

struct LocalServer {
    base_url: String,
    task: tokio::task::JoinHandle<()>,
}

impl LocalServer {
    async fn start(router: Router) -> Self {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("local connector test listener binds");
        let address = listener
            .local_addr()
            .expect("local connector test listener has an address");
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("local connector test server serves");
        });
        Self {
            base_url: format!("http://{address}"),
            task,
        }
    }
}

impl Drop for LocalServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn local_connector(base_url: &str) -> HttpConnector {
    let config = HttpConnectorConfig::new(
        base_url,
        NetworkPolicy::PrivateAllowed,
        vec![
            ConfiguredHeader::new("Authorization", "test-credential")
                .expect("fixed credential header is valid"),
        ],
    )
    .expect("local static base URL is valid");
    HttpConnector::with_components(
        config,
        Arc::new(SystemResolver),
        Arc::new(ReqwestTransport::new()),
    )
}

fn public_connector(
    resolver: Arc<dyn HostResolver>,
    transport: Arc<dyn HttpTransport>,
) -> HttpConnector {
    let config = HttpConnectorConfig::new(
        "https://provider.example.test",
        NetworkPolicy::PublicOnly,
        vec![],
    )
    .expect("static provider base URL is valid");
    HttpConnector::with_components(config, resolver, transport)
}

fn context() -> ExecutionContext {
    ExecutionContext::with_deadline(tokio::time::Instant::now() + Duration::from_secs(1))
}

fn operation(path: &str) -> HttpOperation {
    HttpOperation::builder("test_operation", HttpMethod::Post, path)
        .success_statuses([StatusCode::OK])
        .build()
        .expect("static operation declaration is valid")
}

#[tokio::test]
async fn declared_static_base_path_query_headers_and_json_body_are_encoded_without_host_input() {
    async fn echo_item(
        OriginalUri(uri): OriginalUri,
        headers: HeaderMap,
        Json(body): Json<JsonValue>,
    ) -> Json<JsonValue> {
        Json(json!({
            "uri": uri.to_string(),
            "authorization": headers.get("authorization").and_then(|value| value.to_str().ok()),
            "operation_header": headers.get("x-operation").and_then(|value| value.to_str().ok()),
            "body": body,
        }))
    }

    let server = LocalServer::start(Router::new().route("/v1/items/{id}", post(echo_item))).await;
    let connector = local_connector(&server.base_url);
    let operation = HttpOperation::builder(
        "create_item",
        HttpMethod::Post,
        "/v1/items/{input.order_id}",
    )
    .query_input("search", "search")
    .static_header("X-Operation", "fixed")
    .body(JsonTemplate::object([(
        "order_id",
        JsonTemplate::input("order_id"),
    )]))
    .success_statuses([StatusCode::OK])
    .response_pointer("uri", "/uri", true)
    .response_pointer("authorization", "/authorization", true)
    .response_pointer("operation_header", "/operation_header", true)
    .response_pointer("body_order_id", "/body/order_id", true)
    .build()
    .expect("declared operation is valid");

    let result = connector
        .execute(
            &operation,
            json!({
                "order_id": "a/b ? #",
                "search": "one/two & three",
                "unbound_url": "https://attacker.invalid/should-not-be-used",
            }),
            context(),
        )
        .await
        .expect("static endpoint request succeeds");

    assert_eq!(
        result.output,
        json!({
            "uri": "/v1/items/a%2Fb%20%3F%20%23?search=one%2Ftwo%20%26%20three",
            "authorization": "test-credential",
            "operation_header": "fixed",
            "body_order_id": "a/b ? #",
        }),
        "declared path and query slots are percent-encoded while the input cannot choose a host"
    );
}

#[tokio::test]
async fn extracts_declared_json_pointers_from_a_successful_provider_response() {
    async fn response() -> Json<JsonValue> {
        Json(json!({"id": "ship_123", "tracking": {"url": "https://tracking.example.test/123"}}))
    }

    let server = LocalServer::start(Router::new().route("/response", post(response))).await;
    let connector = local_connector(&server.base_url);
    let operation = HttpOperation::builder("extract", HttpMethod::Post, "/response")
        .success_statuses([StatusCode::OK])
        .response_pointer("shipment_id", "/id", true)
        .response_pointer("tracking_url", "/tracking/url", false)
        .build()
        .expect("response declaration is valid");

    let result = connector
        .execute(&operation, json!({}), context())
        .await
        .expect("JSON response satisfies the declared extraction");

    assert_eq!(
        result.output,
        json!({
            "shipment_id": "ship_123",
            "tracking_url": "https://tracking.example.test/123",
        })
    );
}

#[tokio::test]
async fn redirects_are_not_followed_or_treated_as_success() {
    async fn redirect() -> impl IntoResponse {
        (StatusCode::FOUND, [("location", "/redirect-target")])
    }

    async fn redirect_target(State(hits): State<Arc<AtomicUsize>>) -> impl IntoResponse {
        hits.fetch_add(1, Ordering::Relaxed);
        Json(json!({"unexpected": true}))
    }

    let target_hits = Arc::new(AtomicUsize::new(0));
    let server = LocalServer::start(
        Router::new()
            .route("/redirect", post(redirect))
            .route("/redirect-target", get(redirect_target))
            .with_state(target_hits.clone()),
    )
    .await;
    let failure = local_connector(&server.base_url)
        .execute(&operation("/redirect"), json!({}), context())
        .await
        .expect_err("redirect status is not followed");

    assert_eq!(failure.class, ConnectorErrorClass::Permanent);
    assert_eq!(target_hits.load(Ordering::Relaxed), 0);
    assert!(!failure.safe_message.contains("redirect-target"));
}

#[tokio::test]
async fn request_and_response_bodies_are_bounded_to_one_mib() {
    async fn oversized_response() -> Response {
        (StatusCode::OK, "x".repeat(ONE_MIB + 1)).into_response()
    }

    let request_failure = local_connector("http://127.0.0.1:9")
        .execute(
            &HttpOperation::builder("oversized_request", HttpMethod::Post, "/unused")
                .body(JsonTemplate::literal(
                    json!({"payload": "x".repeat(ONE_MIB)}),
                ))
                .success_statuses([StatusCode::OK])
                .build()
                .expect("declared request is valid"),
            json!({}),
            context(),
        )
        .await
        .expect_err("the module refuses an oversized request before connecting");
    assert_eq!(request_failure.class, ConnectorErrorClass::Invariant);
    assert!(
        !request_failure.safe_message.contains(&"x".repeat(64)),
        "the oversized request body is not included in the safe error message"
    );

    let server = LocalServer::start(Router::new().route("/large", post(oversized_response))).await;
    let response_failure = local_connector(&server.base_url)
        .execute(&operation("/large"), json!({}), context())
        .await
        .expect_err("the module refuses an oversized provider response");
    assert_eq!(response_failure.class, ConnectorErrorClass::Validation);
    assert!(!response_failure.safe_message.contains(&"x".repeat(32)));
}

#[tokio::test]
async fn finite_activity_deadline_classifies_a_slow_provider_as_timeout() {
    async fn slow() -> Json<JsonValue> {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Json(json!({"ok": true}))
    }

    let server = LocalServer::start(Router::new().route("/slow", post(slow))).await;
    let failure = local_connector(&server.base_url)
        .execute(
            &operation("/slow"),
            json!({}),
            ExecutionContext::with_deadline(
                tokio::time::Instant::now() + Duration::from_millis(10),
            ),
        )
        .await
        .expect_err("the finite activity deadline is enforced");

    assert_eq!(failure.class, ConnectorErrorClass::Timeout);
    assert!(!failure.safe_message.contains("/slow"));
}

#[tokio::test]
async fn status_and_malformed_response_failures_use_the_closed_error_classes() {
    async fn status(Path(status): Path<u16>) -> Response {
        (
            StatusCode::from_u16(status).expect("test status is valid"),
            "provider response body must not leak",
        )
            .into_response()
    }

    async fn malformed() -> impl IntoResponse {
        (StatusCode::OK, "not JSON")
    }

    let server = LocalServer::start(
        Router::new()
            .route("/status/{status}", post(status))
            .route("/malformed", post(malformed)),
    )
    .await;
    let connector = local_connector(&server.base_url);

    for (path, operation, expected) in [
        (
            "/status/429",
            operation("/status/429"),
            ConnectorErrorClass::Http429,
        ),
        (
            "/status/503",
            HttpOperation::builder("declared_5xx", HttpMethod::Post, "/status/503")
                .success_statuses([StatusCode::OK])
                .declared_5xx([StatusCode::SERVICE_UNAVAILABLE])
                .build()
                .expect("5xx declaration is valid"),
            ConnectorErrorClass::Http5xx,
        ),
        (
            "/status/401",
            operation("/status/401"),
            ConnectorErrorClass::Authentication,
        ),
        (
            "/status/403",
            operation("/status/403"),
            ConnectorErrorClass::Authentication,
        ),
        (
            "/status/400",
            operation("/status/400"),
            ConnectorErrorClass::Validation,
        ),
        (
            "/status/404",
            operation("/status/404"),
            ConnectorErrorClass::Permanent,
        ),
    ] {
        let failure = connector
            .execute(&operation, json!({}), context())
            .await
            .expect_err("non-success provider status fails the operation");
        assert_eq!(failure.class, expected, "status path {path}");
        assert!(
            !failure
                .safe_message
                .contains("provider response body must not leak"),
            "provider body is not an activity error message"
        );
    }

    let malformed_failure = connector
        .execute(&operation("/malformed"), json!({}), context())
        .await
        .expect_err("successful response must still satisfy the JSON contract");
    assert_eq!(malformed_failure.class, ConnectorErrorClass::Validation);
    assert!(!malformed_failure.safe_message.contains("not JSON"));
}

type ResolverResults = VecDeque<Result<Vec<IpAddr>, ResolveError>>;

#[derive(Clone)]
struct SequenceResolver {
    results: Arc<Mutex<ResolverResults>>,
}

impl SequenceResolver {
    fn new(results: impl IntoIterator<Item = Result<Vec<IpAddr>, ResolveError>>) -> Self {
        Self {
            results: Arc::new(Mutex::new(results.into_iter().collect())),
        }
    }
}

impl HostResolver for SequenceResolver {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
        _port: u16,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<IpAddr>, ResolveError>> {
        Box::pin(async move {
            self.results
                .lock()
                .await
                .pop_front()
                .expect("test supplied a resolution result for every lookup")
        })
    }
}

struct RecordingTransport {
    result: Mutex<Result<RawHttpResponse, TransportError>>,
    calls: AtomicUsize,
}

impl RecordingTransport {
    fn response(response: RawHttpResponse) -> Self {
        Self {
            result: Mutex::new(Ok(response)),
            calls: AtomicUsize::new(0),
        }
    }

    fn failure(error: TransportError) -> Self {
        Self {
            result: Mutex::new(Err(error)),
            calls: AtomicUsize::new(0),
        }
    }
}

impl HttpTransport for RecordingTransport {
    fn execute<'a>(
        &'a self,
        _request: donat_server::connectors::http::PreparedHttpRequest,
        _destination: &'a [IpAddr],
        _deadline: tokio::time::Instant,
    ) -> futures_util::future::BoxFuture<'a, Result<RawHttpResponse, TransportError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.result.lock().await.clone()
        })
    }
}

#[tokio::test]
async fn dns_and_connect_failures_are_transport_errors() {
    let dns_resolver = Arc::new(SequenceResolver::new([Err(ResolveError::new(
        "DNS lookup failed",
    ))]));
    let unused_transport = Arc::new(RecordingTransport::response(RawHttpResponse::json(
        StatusCode::OK,
        json!({}),
    )));
    let dns_failure = public_connector(dns_resolver, unused_transport)
        .execute(&operation("/dns"), json!({}), context())
        .await
        .expect_err("DNS failure prevents a connection");
    assert_eq!(dns_failure.class, ConnectorErrorClass::Transport);

    let resolver = Arc::new(SequenceResolver::new([
        Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
        Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
    ]));
    let connect_failure = public_connector(
        resolver,
        Arc::new(RecordingTransport::failure(TransportError::new(
            "connection reset",
        ))),
    )
    .execute(&operation("/connect"), json!({}), context())
    .await
    .expect_err("connect failure is typed");
    assert_eq!(connect_failure.class, ConnectorErrorClass::Transport);
    assert!(!connect_failure.safe_message.contains("connection reset"));
}

#[tokio::test]
async fn public_only_rejects_every_non_global_initial_address_without_a_loopback_test_escape_hatch()
{
    for address in [
        Ipv4Addr::LOCALHOST.into(),
        Ipv4Addr::new(10, 0, 0, 1).into(),
        Ipv4Addr::new(169, 254, 1, 1).into(),
        Ipv4Addr::new(224, 0, 0, 1).into(),
        Ipv4Addr::UNSPECIFIED.into(),
        Ipv4Addr::new(192, 0, 2, 1).into(),
        Ipv6Addr::UNSPECIFIED.into(),
        Ipv6Addr::LOCALHOST.into(),
        Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0x2001, 0x0db8, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0x2001, 0x0002, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0x0100, 0, 0, 0, 0, 0, 0, 1).into(),
        Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0xc000, 0x0201).into(),
        Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001).into(),
    ] {
        let transport = Arc::new(RecordingTransport::response(RawHttpResponse::json(
            StatusCode::OK,
            json!({}),
        )));
        let failure = public_connector(
            Arc::new(SequenceResolver::new([
                Ok(vec![address]),
                Ok(vec![address]),
            ])),
            transport.clone(),
        )
        .execute(&operation("/blocked"), json!({}), context())
        .await
        .expect_err("public_only refuses a non-global address");
        assert_eq!(
            failure.class,
            ConnectorErrorClass::Invariant,
            "address {address}"
        );
        assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
    }
}

#[tokio::test]
async fn public_only_re_resolves_immediately_before_connect_and_rejects_dns_rebinding() {
    let transport = Arc::new(RecordingTransport::response(RawHttpResponse::json(
        StatusCode::OK,
        json!({}),
    )));
    let failure = public_connector(
        Arc::new(SequenceResolver::new([
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
            Ok(vec![Ipv4Addr::LOCALHOST.into()]),
        ])),
        transport.clone(),
    )
    .execute(&operation("/rebinding"), json!({}), context())
    .await
    .expect_err("the connect-time resolution rejects a rebinding to loopback");

    assert_eq!(failure.class, ConnectorErrorClass::Invariant);
    assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_declared_template_contract_violation_is_an_invariant_failure() {
    let connector = public_connector(
        Arc::new(SequenceResolver::new([
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
        ])),
        Arc::new(RecordingTransport::response(RawHttpResponse::json(
            StatusCode::OK,
            json!({}),
        ))),
    );
    let operation = HttpOperation::builder("missing_input", HttpMethod::Post, "/{input.id}")
        .success_statuses([StatusCode::OK])
        .build()
        .expect("the declaration is static; an absent runtime value is an execution invariant");

    let failure = connector
        .execute(&operation, json!({}), context())
        .await
        .expect_err("missing declared input violates the module contract");
    assert_eq!(failure.class, ConnectorErrorClass::Invariant);
    assert!(!failure.safe_message.contains("input.id"));
}
