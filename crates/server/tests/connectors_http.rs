use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::StatusCode;
use donat_server::connectors::{
    ConnectorErrorClass, ConnectorRegistry, ExecutionContext,
    http::{
        HostResolver, HttpConnector, HttpConnectorConfig, HttpMethod, HttpOperation, HttpTransport,
        JsonTemplate, RawHttpResponse, ResolveError, TransportError,
    },
};
use serde_json::json;
use tokio::sync::Mutex;

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

#[test]
fn registry_rejects_private_network_policy_in_deployment_metadata() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [],
        "connectors": [{
            "name": "internal_only",
            "module": "http",
            "config": {
                "endpoint_identity": "internal_only_test",
                "credential_identity": "internal_only_credential",
                "base_url": "https://provider.example.test",
                "network_policy": "private_allowed"
            }
        }]
    }))
    .expect("connector metadata deserializes before runtime validation");

    let error = match ConnectorRegistry::build(&metadata) {
        Ok(_) => panic!("private network access is not a deployable HTTP connector capability"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("network_policy must be public_only"),
        "startup rejects the bypass rather than publishing an SSRF-capable connector"
    );
}

#[tokio::test]
async fn declarative_registry_rejects_undeclared_job_transport_input_before_network() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [],
        "connectors": [{
            "name": "logistics",
            "module": "http",
            "config": {
                "endpoint_identity": "logistics_test",
                "credential_identity": "logistics_test_credential",
                "base_url": "https://provider.example.test"
            },
            "operations": [{
                "name": "create_shipment",
                "version": "v1",
                "method": "POST",
                "path": "/v1/shipments/{input.order_id}",
                "body": { "order_id": { "input": "order_id" } },
                "success_statuses": [200],
                "idempotency": { "header": "Idempotency-Key" },
                "capacity": {
                    "max_in_flight": 8,
                    "rate_limit": { "permits": 20, "per": "1s", "burst": 8 },
                    "serialize_by": { "input": "order_id" }
                }
            }]
        }]
    }))
    .expect("declarative operation metadata deserializes");
    let registry = ConnectorRegistry::build(&metadata).expect("operation compiles at startup");

    let failure = registry
        .execute(
            "logistics",
            "create_shipment",
            json!({
                "order_id": "order-42",
                "url": "https://attacker.invalid/override",
                "method": "DELETE",
                "headers": { "Authorization": "attacker" }
            }),
            "logical-activity-42",
            context().deadline,
        )
        .await
        .expect_err("a job may fill only the operation's named input bindings");

    assert_eq!(failure.class, ConnectorErrorClass::Invariant);
    assert_eq!(failure.code, "connector_invariant");
    assert!(
        !failure.safe_message.contains("attacker.invalid"),
        "rejection must not surface the caller-provided raw URL"
    );
}

#[test]
fn declarative_registry_rejects_a_serialization_key_outside_declared_input_bindings() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [],
        "connectors": [{
            "name": "logistics",
            "module": "http",
            "config": {
                "endpoint_identity": "logistics_test",
                "credential_identity": "logistics_test_credential",
                "base_url": "https://provider.example.test"
            },
            "operations": [{
                "name": "create_shipment",
                "version": "v1",
                "method": "POST",
                "path": "/v1/shipments/{input.order_id}",
                "success_statuses": [200],
                "idempotency": { "header": "Idempotency-Key" },
                "capacity": {
                    "max_in_flight": 8,
                    "rate_limit": { "permits": 20, "per": "1s", "burst": 8 },
                    "serialize_by": { "input": "not_a_declared_binding" }
                }
            }]
        }]
    }))
    .expect("operation metadata deserializes before module validation");

    let error = match ConnectorRegistry::build(&metadata) {
        Ok(_) => panic!("serialize_by must name one declared scalar operation input"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("serialize_by"),
        "startup identifies the invalid deploy-time serialization declaration: {error}"
    );
}

fn public_connector(
    resolver: Arc<dyn HostResolver>,
    transport: Arc<dyn HttpTransport>,
) -> HttpConnector {
    let config = HttpConnectorConfig::new("https://provider.example.test", vec![])
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

#[tokio::test]
async fn public_only_requires_a_known_vetted_remote_peer_before_accepting_a_response() {
    let transport = Arc::new(RecordingTransport::response(RawHttpResponse::json(
        StatusCode::OK,
        json!({"ok": true}),
    )));
    let failure = public_connector(
        Arc::new(SequenceResolver::new([
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
        ])),
        transport.clone(),
    )
    .execute(&operation("/peer"), json!({}), context())
    .await
    .expect_err("public-only responses need an observed peer from the vetted destination set");

    assert_eq!(failure.class, ConnectorErrorClass::Invariant);
    assert_eq!(transport.calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn public_only_rejects_an_unvetted_or_private_observed_peer() {
    for peer in [
        SocketAddr::from(([1, 1, 1, 1], 443)),
        SocketAddr::from(([127, 0, 0, 1], 443)),
    ] {
        let transport = Arc::new(RecordingTransport::response(
            RawHttpResponse::json(StatusCode::OK, json!({"ok": true})).with_peer(peer),
        ));
        let failure = public_connector(
            Arc::new(SequenceResolver::new([
                Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
                Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
            ])),
            transport,
        )
        .execute(&operation("/peer"), json!({}), context())
        .await
        .expect_err("the observed peer must be public and in the vetted set");
        assert_eq!(failure.class, ConnectorErrorClass::Invariant, "peer {peer}");
    }
}

#[tokio::test]
async fn public_only_accepts_a_vetted_public_observed_peer() {
    let result = public_connector(
        Arc::new(SequenceResolver::new([
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
        ])),
        Arc::new(RecordingTransport::response(
            RawHttpResponse::json(StatusCode::OK, json!({"ok": true}))
                .with_peer(SocketAddr::from(([8, 8, 8, 8], 443))),
        )),
    )
    .execute(&operation("/peer"), json!({}), context())
    .await
    .expect("a public peer from the vetted set is accepted");

    assert_eq!(result.output, json!({"ok": true}));
}

#[tokio::test]
async fn public_only_resolution_precedes_request_template_preparation() {
    let transport = Arc::new(RecordingTransport::response(RawHttpResponse::json(
        StatusCode::OK,
        json!({}),
    )));
    let operation = HttpOperation::builder("unsafe_first", HttpMethod::Post, "/declared")
        .body(JsonTemplate::input("missing_body_input"))
        .success_statuses([StatusCode::OK])
        .build()
        .expect("static operation is valid");
    let failure = public_connector(
        Arc::new(SequenceResolver::new([Ok(vec![
            Ipv4Addr::LOCALHOST.into(),
        ])])),
        transport.clone(),
    )
    .execute(&operation, json!({}), context())
    .await
    .expect_err("unsafe DNS must stop execution before input/template processing");

    assert_eq!(failure.class, ConnectorErrorClass::Invariant);
    assert_eq!(
        failure.safe_message,
        "connector network policy rejected a non-public destination"
    );
    assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
}
