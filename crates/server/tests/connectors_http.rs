use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
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

fn postgres_sources() -> serde_json::Value {
    json!([{
        "name": "default",
        "kind": "postgres",
        "configuration": {}
    }])
}

#[test]
fn registry_has_only_the_compiled_http_and_stripe_modules_and_is_immutable_after_build() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": postgres_sources(),
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
fn registry_rejects_a_declared_network_policy_in_deployment_metadata() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": postgres_sources(),
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
        Ok(_) => panic!("the engine implements no connector network policy to declare"),
        Err(error) => error,
    };

    assert!(
        error
            .to_string()
            .contains("http connector does not accept network_policy"),
        "startup rejects an unimplemented setting rather than silently ignoring it"
    );
}

#[tokio::test]
async fn declarative_registry_rejects_undeclared_job_transport_input_before_network() {
    let metadata: donat_metadata::Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": postgres_sources(),
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
        "sources": postgres_sources(),
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

#[test]
fn declarative_registry_rejects_case_insensitive_header_collisions_before_any_header_can_overwrite()
{
    let connector = |config_headers: serde_json::Value,
                     operation_headers: serde_json::Value,
                     idempotency_header: &str| {
        serde_json::from_value::<donat_metadata::Metadata>(json!({
            "version": 3,
            "sources": postgres_sources(),
            "connectors": [{
                "name": "logistics",
                "module": "http",
                "config": {
                    "endpoint_identity": "logistics_test",
                    "credential_identity": "logistics_test_credential",
                    "base_url": "https://provider.example.test",
                    "headers": config_headers
                },
                "operations": [{
                    "name": "create_shipment",
                    "version": "v1",
                    "method": "POST",
                    "path": "/v1/shipments/{input.order_id}",
                    "headers": operation_headers,
                    "success_statuses": [200],
                    "idempotency": { "header": idempotency_header },
                    "capacity": {
                        "max_in_flight": 8,
                        "rate_limit": { "permits": 20, "per": "1s", "burst": 8 }
                    }
                }]
            }]
        }))
        .expect("header names remain syntactically valid metadata")
    };

    let cases = [
        connector(
            json!([
                { "name": "Authorization", "value_from_env": "TOKEN_A" },
                { "name": "authorization", "value_from_env": "TOKEN_B" }
            ]),
            json!([]),
            "Idempotency-Key",
        ),
        connector(
            json!([{ "name": "Authorization", "value_from_env": "TOKEN_A" }]),
            json!([{ "name": "authorization", "value": "fixed" }]),
            "Idempotency-Key",
        ),
        connector(
            json!([{ "name": "Authorization", "value_from_env": "TOKEN_A" }]),
            json!([]),
            "AUTHORIZATION",
        ),
        connector(
            json!([]),
            json!([
                { "name": "X-Request-Mode", "value": "first" },
                { "name": "x-request-mode", "value": "second" }
            ]),
            "Idempotency-Key",
        ),
        connector(
            json!([]),
            json!([{ "name": "Idempotency-Key", "value": "fixed" }]),
            "idempotency-key",
        ),
    ];

    for metadata in cases {
        let error = match ConnectorRegistry::build(&metadata) {
            Ok(_) => panic!("each colliding header declaration must reject startup"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("header names must not collide"),
            "the registry must reject the configuration instead of silently overwriting a header: {error}"
        );
    }
}

#[test]
fn declarative_registry_exposes_a_non_secret_operation_configuration_fingerprint() {
    const SECRET_ENV: &str = "DONAT_CONNECTOR_FINGERPRINT_TEST_TOKEN";

    let metadata = || {
        serde_json::from_value::<donat_metadata::Metadata>(json!({
            "version": 3,
            "sources": postgres_sources(),
            "connectors": [{
                "name": "logistics",
                "module": "http",
                "config": {
                    "endpoint_identity": "logistics_test",
                    "credential_identity": "logistics_test_credential",
                    "base_url": "https://provider.example.test",
                    "headers": [{ "name": "Authorization", "value_from_env": SECRET_ENV }]
                },
                "operations": [{
                    "name": "create_shipment",
                    "version": "v1",
                    "method": "POST",
                    "path": "/v1/shipments/{input.order_id}",
                    "body": {
                        "order_id": { "input": "order_id" },
                        "customer_id": { "input": "customer_id" }
                    },
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
        .expect("fingerprint fixture metadata deserializes")
    };
    let fingerprint = |metadata: &donat_metadata::Metadata| {
        ConnectorRegistry::build(metadata)
            .expect("fingerprint fixture compiles")
            .configuration_fingerprint("logistics", "create_shipment")
            .expect("compiled operation exposes its immutable configuration fingerprint")
            .to_owned()
    };

    // SAFETY: this test uses a connector-specific variable and does not read
    // it outside registry construction; the resolved value must not affect
    // the resulting fingerprint.
    unsafe { std::env::set_var(SECRET_ENV, "credential-value-one") };
    let baseline_metadata = metadata();
    let baseline = fingerprint(&baseline_metadata);
    assert_eq!(baseline.len(), 64, "the exposed value is a SHA-256 digest");
    assert!(!baseline.contains("credential-value-one"));

    let mut operation_version = metadata();
    let donat_metadata::ConnectorOperationProfile::Http(operation) =
        &mut operation_version.connectors[0].operations[0].profile
    else {
        panic!("HTTP operation")
    };
    operation.version = "v2".to_owned();
    assert_ne!(baseline, fingerprint(&operation_version));

    let mut endpoint_identity = metadata();
    endpoint_identity.connectors[0].config.endpoint_identity = "logistics_test_next".to_owned();
    assert_ne!(baseline, fingerprint(&endpoint_identity));

    let mut credential_identity = metadata();
    credential_identity.connectors[0].config.credential_identity =
        "logistics_test_credential_next".to_owned();
    assert_ne!(baseline, fingerprint(&credential_identity));

    let mut capacity = metadata();
    capacity.connectors[0].operations[0]
        .capacity
        .as_mut()
        .expect("operation capacity")
        .max_in_flight = 9;
    assert_ne!(baseline, fingerprint(&capacity));

    let mut serialization = metadata();
    serialization.connectors[0].operations[0]
        .capacity
        .as_mut()
        .expect("operation capacity")
        .serialize_by
        .as_mut()
        .expect("serialization policy")
        .input = "customer_id".to_owned();
    assert_ne!(baseline, fingerprint(&serialization));

    let mut base_url = metadata();
    base_url.connectors[0].config.base_url = Some(donat_metadata::ConnectorBaseUrl::Literal(
        "https://provider-next.example.test".to_owned(),
    ));
    assert_ne!(baseline, fingerprint(&base_url));

    // SAFETY: see the first assignment above. Changing a resolved credential
    // value must not change the non-secret configuration fingerprint.
    unsafe { std::env::set_var(SECRET_ENV, "credential-value-two") };
    assert_eq!(baseline, fingerprint(&baseline_metadata));
    // SAFETY: cleanup restores process state for subsequent tests.
    unsafe { std::env::remove_var(SECRET_ENV) };
}

#[test]
fn declarative_registry_fingerprint_distinguishes_base_url_environment_source_names_without_exposure()
 {
    const BASE_URL_A: &str = "DONAT_CONNECTOR_FINGERPRINT_BASE_URL_A";
    const BASE_URL_B: &str = "DONAT_CONNECTOR_FINGERPRINT_BASE_URL_B";
    const CREDENTIAL_ENV: &str = "DONAT_CONNECTOR_FINGERPRINT_SOURCE_TOKEN";
    const RESOLVED_BASE_URL: &str = "https://provider.example.test";
    const CREDENTIAL_VALUE: &str = "credential-value-must-not-be-exposed";

    let metadata = |base_url_environment: &str| {
        serde_json::from_value::<donat_metadata::Metadata>(json!({
            "version": 3,
            "sources": postgres_sources(),
            "connectors": [{
                "name": "logistics",
                "module": "http",
                "config": {
                    "endpoint_identity": "logistics_test",
                    "credential_identity": "logistics_test_credential",
                    "base_url": { "value_from_env": base_url_environment },
                    "headers": [{ "name": "Authorization", "value_from_env": CREDENTIAL_ENV }]
                },
                "operations": [{
                    "name": "create_shipment",
                    "version": "v1",
                    "method": "POST",
                    "path": "/v1/shipments/{input.order_id}",
                    "success_statuses": [200],
                    "idempotency": { "header": "Idempotency-Key" },
                    "capacity": {
                        "max_in_flight": 1,
                        "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                    }
                }]
            }]
        }))
        .expect("base URL source fingerprint fixture metadata deserializes")
    };
    let fingerprint = |metadata: &donat_metadata::Metadata| {
        ConnectorRegistry::build(metadata)
            .expect("base URL source fingerprint fixture compiles")
            .configuration_fingerprint("logistics", "create_shipment")
            .expect("compiled operation exposes its immutable configuration fingerprint")
            .to_owned()
    };

    // SAFETY: these connector-specific environment variables exist only for
    // this test. The two base URL sources deliberately resolve to the same
    // endpoint, while the public fingerprint must not expose their values.
    unsafe {
        std::env::set_var(BASE_URL_A, RESOLVED_BASE_URL);
        std::env::set_var(BASE_URL_B, RESOLVED_BASE_URL);
        std::env::set_var(CREDENTIAL_ENV, CREDENTIAL_VALUE);
    }
    let source_a = fingerprint(&metadata(BASE_URL_A));
    let source_b = fingerprint(&metadata(BASE_URL_B));

    assert_ne!(
        source_a, source_b,
        "different deployment base URL environment names must produce different revisions"
    );
    for value in [BASE_URL_A, BASE_URL_B, RESOLVED_BASE_URL, CREDENTIAL_VALUE] {
        assert!(
            !source_a.contains(value) && !source_b.contains(value),
            "public fingerprints expose only a digest, never source values: {value}"
        );
    }

    // SAFETY: cleanup restores process state for subsequent tests.
    unsafe {
        std::env::remove_var(BASE_URL_A);
        std::env::remove_var(BASE_URL_B);
        std::env::remove_var(CREDENTIAL_ENV);
    }
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
async fn the_connected_peer_is_pinned_to_the_connect_time_resolution() {
    // The name resolves to one address while the request is prepared and to a
    // different one immediately before connecting. Whatever the deployment's
    // egress rules allow, one request must stay on one resolved host, so a peer
    // outside the connect-time set is an invariant failure.
    let transport = Arc::new(RecordingTransport::response(
        RawHttpResponse::json(StatusCode::OK, json!({}))
            .with_peer(SocketAddr::from(([8, 8, 8, 8], 443))),
    ));
    let failure = public_connector(
        Arc::new(SequenceResolver::new([
            Ok(vec![Ipv4Addr::new(8, 8, 8, 8).into()]),
            Ok(vec![Ipv4Addr::new(1, 1, 1, 1).into()]),
        ])),
        transport.clone(),
    )
    .execute(&operation("/rebinding"), json!({}), context())
    .await
    .expect_err("a peer outside the connect-time resolution is rejected");

    assert_eq!(failure.class, ConnectorErrorClass::Invariant);
    assert_eq!(
        failure.safe_message,
        "connector transport connected to an unresolved peer"
    );
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
async fn a_response_without_an_observed_peer_is_rejected() {
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
async fn an_unvetted_observed_peer_is_rejected() {
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
        .expect_err("the observed peer must be in the vetted set");
        assert_eq!(failure.class, ConnectorErrorClass::Invariant, "peer {peer}");
    }
}

#[tokio::test]
async fn a_vetted_observed_peer_is_accepted() {
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
async fn host_resolution_precedes_request_template_preparation() {
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
        Arc::new(SequenceResolver::new([Err(ResolveError::new(
            "unresolved",
        ))])),
        transport.clone(),
    )
    .execute(&operation, json!({}), context())
    .await
    .expect_err("a failed resolution stops execution before input/template processing");

    // The operation body names an input the caller never sent: reporting the
    // transport failure instead proves resolution ran first.
    assert_eq!(failure.class, ConnectorErrorClass::Transport);
    assert_eq!(transport.calls.load(Ordering::Relaxed), 0);
}
