mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Once};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Request, StatusCode},
};
use donat_ir::TypedValue;
use donat_metadata::Metadata;
use donat_server::{
    connector_webhook,
    connectors::ConnectorRegistry,
    processes::{
        InboundPersistence, InvalidSignatureStatus, StartConsumption, TransitionConsumption,
    },
    state::{AppState, Engine, SharedState},
};
use hmac::{Hmac, Mac};
use serde_json::{Value as Json, json};
use sha2::Sha256;
use tokio_postgres::NoTls;
use tower::ServiceExt;
use uuid::Uuid;

use support::TestDatabase;

const API_KEY_ENV: &str = "DONAT_PROCESS_INBOUND_STRIPE_API_KEY";
const WEBHOOK_SECRET_ENV: &str = "DONAT_PROCESS_INBOUND_STRIPE_WEBHOOK_SECRET";
const WEBHOOK_SECRET: &str = "whsec_process_inbound_test";
const PROCESS_NAME: &str = "checkout_webhook";
const CONNECTOR_INSTANCE: &str = "payments";
const TRIGGER_NAME: &str = "checkout.session.completed";
const ORDER_ID: &str = "00000000-0000-4000-8000-000000000042";

type HmacSha256 = Hmac<Sha256>;

fn configure_environment() {
    static CONFIGURED: Once = Once::new();
    CONFIGURED.call_once(|| {
        // SAFETY: these test-only names are initialized once before any
        // connector registry resolves them.
        unsafe {
            std::env::set_var(API_KEY_ENV, "sk_test_process_inbound");
            std::env::set_var(WEBHOOK_SECRET_ENV, WEBHOOK_SECRET);
        }
    });
}

fn inbound_metadata(database_url: &str) -> Metadata {
    configure_environment();
    serde_json::from_value(json!({
        "version": 3,
        "rules": {
            "rules": [{
                "name": "payment_is_paid",
                "parameters": { "payment_status": "string!" },
                "result": "bool!",
                "expression": "payment_status == \"paid\""
            }]
        },
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": database_url }
            }
        }],
        "connectors": [{
            "name": CONNECTOR_INSTANCE,
            "module": "stripe",
            "config": {
                "endpoint_identity": "stripe_process_inbound_test",
                "credential_identity": "stripe_process_inbound_credential",
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
        }],
        "processes": [{
            "name": PROCESS_NAME,
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "output": [{ "name": "status", "type": "string!" }],
            "start_at": "await_payment",
            "states": [
                {
                    "id": "await_payment",
                    "wait": {
                        "webhook": {
                            "connector": CONNECTOR_INSTANCE,
                            "trigger": TRIGGER_NAME,
                            "correlate": {
                                "client_reference_id": { "input": "order_id" }
                            },
                            "guard": {
                                "rule": "payment_is_paid",
                                "with": {
                                    "payment_status": { "event": "payment_status" }
                                }
                            }
                        },
                        "deadline": "1h",
                        "next": "done",
                        "on_timeout": "timed_out"
                    }
                },
                {
                    "id": "done",
                    "output": {
                        "values": {
                            "status": {
                                "state": "await_payment",
                                "field": "payment_status"
                            }
                        }
                    }
                },
                {
                    "id": "timed_out",
                    "fail": {
                        "code": "payment_timed_out",
                        "message": "payment webhook did not arrive before its deadline"
                    }
                }
            ]
        }]
    }))
    .expect("verified webhook Process metadata deserializes")
}

fn webhook_body(provider_event_id: &str, order_id: &str, payment_status: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": provider_event_id,
        "type": TRIGGER_NAME,
        "data": {
            "object": {
                "object": "checkout.session",
                "id": format!("cs_{provider_event_id}"),
                "client_reference_id": order_id,
                "payment_status": payment_status
            }
        }
    }))
    .expect("fixed webhook body serializes")
}

fn signed_headers(body: &[u8]) -> HeaderMap {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time follows the Unix epoch")
        .as_secs();
    let mut mac = HmacSha256::new_from_slice(WEBHOOK_SECRET.as_bytes())
        .expect("fixed webhook secret is a valid HMAC key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let mut headers = HeaderMap::new();
    headers.insert(
        "stripe-signature",
        HeaderValue::from_str(&format!(
            "t={timestamp},v1={:x}",
            mac.finalize().into_bytes()
        ))
        .expect("fixed signature header is valid"),
    );
    headers
}

async fn runtime(
    database: &TestDatabase,
) -> (
    donat_server::processes::ProcessRuntime,
    Arc<ConnectorRegistry>,
    Metadata,
    String,
) {
    let metadata = inbound_metadata(&database.url);
    let connectors =
        Arc::new(ConnectorRegistry::build(&metadata).expect("webhook connector compiles"));
    let (runtime, revision) = database
        .runtime_with_connectors(&metadata, PROCESS_NAME, connectors.clone())
        .await;
    (runtime, connectors, metadata, revision)
}

async fn start_instance(
    database: &TestDatabase,
    runtime: &donat_server::processes::ProcessRuntime,
    revision: &str,
    order_id: &str,
    request_id: &str,
    enter_wait: bool,
) -> Uuid {
    database
        .seed_start(
            PROCESS_NAME,
            revision,
            json!({ "order_id": order_id, "request_id": request_id }),
            request_id,
        )
        .await;
    let instance_id = match runtime.consume_one_start().await.unwrap() {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a started Process, got {other:?}"),
    };
    if enter_wait {
        assert!(matches!(
            runtime.consume_one_transition().await.unwrap(),
            TransitionConsumption::WaitEntered {
                instance_id: waiting,
                ref state,
                ..
            } if waiting == instance_id && state == "await_payment"
        ));
    }
    instance_id
}

fn verified_event(
    connectors: &ConnectorRegistry,
    provider_event_id: &str,
    order_id: &str,
    payment_status: &str,
) -> donat_connector_abi::VerifiedInboundEvent {
    let body = webhook_body(provider_event_id, order_id, payment_status);
    connectors
        .webhook_instance(CONNECTOR_INSTANCE)
        .expect("compiled webhook instance exists")
        .verify(&signed_headers(&body), &body)
        .expect("signed fixture verifies")
}

#[tokio::test]
async fn accepted_and_duplicate_webhooks_share_one_dedupe_row_and_link_one_event() {
    // This catches acknowledging before the source-local delivery, dedupe,
    // event, and relational links commit together.
    let database = TestDatabase::create("process_inbound_accepted_duplicate").await;
    let (runtime, connectors, _, revision) = runtime(&database).await;
    let instance_id = start_instance(
        &database,
        &runtime,
        &revision,
        ORDER_ID,
        "00000000-0000-4000-8000-000000000043",
        true,
    )
    .await;
    let webhook = connectors
        .webhook_instance(CONNECTOR_INSTANCE)
        .expect("compiled webhook instance exists");

    let event = verified_event(&connectors, "evt_accepted_42", ORDER_ID, "paid");
    let (left, right) = tokio::join!(
        runtime.persist_verified_inbound(CONNECTOR_INSTANCE, webhook.trigger(), event.clone(),),
        runtime.persist_verified_inbound(CONNECTOR_INSTANCE, webhook.trigger(), event),
    );
    let left = left.expect("first concurrent verified delivery commits");
    let right = right.expect("second concurrent verified delivery commits");
    let (accepted, duplicate) = match (left, right) {
        (
            accepted @ InboundPersistence::Accepted { .. },
            duplicate @ InboundPersistence::Duplicate { .. },
        )
        | (
            duplicate @ InboundPersistence::Duplicate { .. },
            accepted @ InboundPersistence::Accepted { .. },
        ) => (accepted, duplicate),
        outcomes => panic!("expected one accepted and one duplicate delivery, got {outcomes:?}"),
    };
    assert!(matches!(duplicate, InboundPersistence::Duplicate { .. }));
    let process_event_id = match accepted {
        InboundPersistence::Accepted {
            instance_id: accepted_instance,
            process_event_id,
            ..
        } => {
            assert_eq!(accepted_instance, instance_id);
            process_event_id
        }
        other => panic!("expected accepted inbound delivery, got {other:?}"),
    };
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::Advanced {
            instance_id: advanced,
            event_id,
            ref to_state,
            ..
        } if advanced == instance_id
            && event_id == process_event_id
            && to_state == "done"
    ));
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::Completed {
            instance_id: completed,
            ..
        } if completed == instance_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("inbound ledger is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_inbound_deliveries),
                (SELECT count(*) FROM donat.process_inbound_events),
                (SELECT count(*) FROM donat.process_events
                 WHERE kind = 'signal' AND instance_id = $1),
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE outcome = 'accepted'
                   AND instance_id = $1
                   AND process_event_id = $2),
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE outcome = 'duplicate'
                   AND instance_id IS NULL
                   AND process_event_id IS NULL),
                (SELECT terminal_output_json FROM donat.process_instances
                 WHERE source_name = 'default' AND id = $1)
            ",
            &[&instance_id, &process_event_id],
        )
        .await
        .expect("split inbound audit is queryable");
    assert_eq!(row.get::<_, i64>(0), 2);
    assert_eq!(row.get::<_, i64>(1), 1);
    assert_eq!(row.get::<_, i64>(2), 1);
    assert_eq!(row.get::<_, i64>(3), 1);
    assert_eq!(row.get::<_, i64>(4), 1);
    assert_eq!(row.get::<_, Json>(5), json!({ "status": "paid" }));
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn verified_webhook_outcomes_are_closed_and_never_buffered() {
    // This catches broad fan-in, future-wait buffering, and evaluating a guard
    // before exact correlation has selected one target.
    let database = TestDatabase::create("process_inbound_outcomes").await;
    let (runtime, connectors, metadata, revision) = runtime(&database).await;
    let webhook = connectors.webhook_instance(CONNECTOR_INSTANCE).unwrap();
    let state = shared_state(metadata, connectors.clone()).await;

    start_instance(
        &database,
        &runtime,
        &revision,
        ORDER_ID,
        "00000000-0000-4000-8000-000000000044",
        true,
    )
    .await;
    assert!(matches!(
        runtime
            .persist_verified_inbound(
                CONNECTOR_INSTANCE,
                webhook.trigger(),
                verified_event(&connectors, "evt_guard_false", ORDER_ID, "unpaid"),
            )
            .await
            .unwrap(),
        InboundPersistence::GuardFalse { .. }
    ));
    assert_empty_status(
        post_webhook(
            state.clone(),
            webhook_body("evt_guard_false_route", ORDER_ID, "unpaid"),
        )
        .await,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert!(matches!(
        runtime
            .persist_verified_inbound(
                CONNECTOR_INSTANCE,
                webhook.trigger(),
                verified_event(
                    &connectors,
                    "evt_unmatched",
                    "00000000-0000-4000-8000-000000000099",
                    "paid",
                ),
            )
            .await
            .unwrap(),
        InboundPersistence::Unmatched { .. }
    ));
    assert_empty_status(
        post_webhook(
            state.clone(),
            webhook_body(
                "evt_unmatched_route",
                "00000000-0000-4000-8000-000000000098",
                "paid",
            ),
        )
        .await,
        StatusCode::NO_CONTENT,
    )
    .await;

    let ambiguous_order = "00000000-0000-4000-8000-000000000062";
    for request_id in [
        "00000000-0000-4000-8000-000000000063",
        "00000000-0000-4000-8000-000000000064",
    ] {
        start_instance(
            &database,
            &runtime,
            &revision,
            ambiguous_order,
            request_id,
            true,
        )
        .await;
    }
    assert!(matches!(
        runtime
            .persist_verified_inbound(
                CONNECTOR_INSTANCE,
                webhook.trigger(),
                verified_event(&connectors, "evt_ambiguous", ambiguous_order, "paid",),
            )
            .await
            .unwrap(),
        InboundPersistence::Ambiguous { .. }
    ));
    assert_empty_status(
        post_webhook(
            state.clone(),
            webhook_body("evt_ambiguous_route", ambiguous_order, "paid"),
        )
        .await,
        StatusCode::NO_CONTENT,
    )
    .await;

    // Leave the non-receptive instance last: a later helper must not ask the
    // shared FIFO transition worker to skip this deliberately pending token.
    let early_order = "00000000-0000-4000-8000-000000000052";
    start_instance(
        &database,
        &runtime,
        &revision,
        early_order,
        "00000000-0000-4000-8000-000000000053",
        false,
    )
    .await;
    assert!(matches!(
        runtime
            .persist_verified_inbound(
                CONNECTOR_INSTANCE,
                webhook.trigger(),
                verified_event(&connectors, "evt_early", early_order, "paid"),
            )
            .await
            .unwrap(),
        InboundPersistence::UnexpectedState { .. }
    ));
    assert_empty_status(
        post_webhook(state, webhook_body("evt_early_route", early_order, "paid")).await,
        StatusCode::NO_CONTENT,
    )
    .await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("audit-only inbound outcomes are inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                count(*) FILTER (WHERE outcome = 'guard_false'),
                count(*) FILTER (WHERE outcome = 'unmatched'),
                count(*) FILTER (WHERE outcome = 'unexpected_state'),
                count(*) FILTER (WHERE outcome = 'ambiguous'),
                count(*) FILTER (
                    WHERE instance_id IS NOT NULL OR process_event_id IS NOT NULL
                ),
                (SELECT count(*) FROM donat.process_inbound_events)
            FROM donat.process_inbound_deliveries
            ",
            &[],
        )
        .await
        .expect("closed inbound outcome counts are queryable");
    assert_eq!(row.get::<_, i64>(0), 2);
    assert_eq!(row.get::<_, i64>(1), 2);
    assert_eq!(row.get::<_, i64>(2), 2);
    assert_eq!(row.get::<_, i64>(3), 2);
    assert_eq!(row.get::<_, i64>(4), 0);
    assert_eq!(row.get::<_, i64>(5), 8);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn invalid_signature_audit_has_no_provider_identity_or_process_link() {
    // This catches trusting an unverified provider event ID or storing raw
    // signature/body material in the delivery audit.
    let database = TestDatabase::create("process_inbound_invalid").await;
    let (runtime, _, _, _) = runtime(&database).await;
    let delivery_id = runtime
        .persist_invalid_inbound(
            CONNECTOR_INSTANCE,
            InvalidSignatureStatus::Missing,
            [9; 32],
            BTreeMap::from([(
                "reason".to_owned(),
                TypedValue::String("signature_missing".to_owned()),
            )]),
        )
        .await
        .expect("invalid signature audit commits");

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("invalid audit is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                provider_event_id,
                signature_status,
                outcome,
                instance_id,
                process_event_id,
                payload_digest,
                redacted_metadata,
                (SELECT count(*) FROM donat.process_inbound_events)
            FROM donat.process_inbound_deliveries
            WHERE source_name = 'default' AND id = $1
            ",
            &[&delivery_id],
        )
        .await
        .expect("invalid delivery exists");
    assert_eq!(row.get::<_, Option<String>>(0), None);
    assert_eq!(row.get::<_, String>(1), "missing");
    assert_eq!(row.get::<_, String>(2), "invalid_signature");
    assert_eq!(row.get::<_, Option<Uuid>>(3), None);
    assert_eq!(row.get::<_, Option<Uuid>>(4), None);
    assert_eq!(row.get::<_, Vec<u8>>(5), vec![9; 32]);
    assert_eq!(
        row.get::<_, Json>(6),
        json!({ "reason": "signature_missing" })
    );
    assert_eq!(row.get::<_, i64>(7), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn corrupted_webhook_wait_marker_fails_closed_without_an_acknowledgeable_audit() {
    // This catches reconstructing or guessing a correlation after the durable
    // wait marker has become incomplete. Internal journal corruption must
    // roll the ingress transaction back so the provider receives a retryable
    // failure instead of an acknowledged misroute.
    let database = TestDatabase::create("process_inbound_corrupt_marker").await;
    let (runtime, connectors, _, revision) = runtime(&database).await;
    let instance_id = start_instance(
        &database,
        &runtime,
        &revision,
        ORDER_ID,
        "00000000-0000-4000-8000-000000000083",
        true,
    )
    .await;
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("wait marker can be corrupted for fault injection");
    let connection = tokio::spawn(connection);
    assert_eq!(
        client
            .execute(
                "
                UPDATE donat.process_events
                SET payload_json = payload_json - 'correlation'
                WHERE source_name = 'default'
                  AND instance_id = $1
                  AND kind = 'timer'
                  AND status = 'pending'
                ",
                &[&instance_id],
            )
            .await
            .expect("fault injection updates the wait marker"),
        1
    );

    let webhook = connectors.webhook_instance(CONNECTOR_INSTANCE).unwrap();
    let error = runtime
        .persist_verified_inbound(
            CONNECTOR_INSTANCE,
            webhook.trigger(),
            verified_event(&connectors, "evt_corrupt_marker", ORDER_ID, "paid"),
        )
        .await
        .expect_err("an incomplete durable marker must fail closed");
    assert!(error.to_string().contains("correlation"));
    let row = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_inbound_deliveries),
                (SELECT count(*) FROM donat.process_inbound_events),
                (SELECT count(*) FROM donat.process_events
                 WHERE instance_id = $1 AND kind = 'signal')
            ",
            &[&instance_id],
        )
        .await
        .expect("rolled-back ingress tables remain inspectable");
    assert_eq!(row.get::<_, i64>(0), 0);
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 0);
    connection.abort();
    database.drop().await;
}

async fn shared_state(metadata: Metadata, connectors: Arc<ConnectorRegistry>) -> SharedState {
    let state = Arc::new(AppState {
        engine: tokio::sync::RwLock::new(Arc::new(
            Engine::bootstrap_checked(metadata).expect("initial metadata compiles"),
        )),
        connectors,
        default_url: "postgres://unused".to_owned(),
        admin_secret: None,
        unauthorized_role: None,
        stringify_numerics: false,
        infer_function_permissions: true,
        jwt: None,
        auth_hook: None,
        http: reqwest::Client::new(),
        allowlist_enabled: false,
        subscription_permits: Arc::new(tokio::sync::Semaphore::new(1_000)),
        subscription_poll_permits: Arc::new(tokio::sync::Semaphore::new(16)),
    });
    state
        .sync_sources()
        .await
        .expect("serving state publishes the reconciled Process catalog");
    state
}

async fn post_webhook(state: SharedState, body: Vec<u8>) -> axum::response::Response {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/v1/connectors/{CONNECTOR_INSTANCE}/webhooks"))
        .body(Body::from(body.clone()))
        .expect("fixed webhook request is valid");
    *request.headers_mut() = signed_headers(&body);
    connector_webhook::router()
        .with_state(state)
        .oneshot(request)
        .await
        .expect("webhook route serves the request")
}

async fn assert_empty_status(response: axum::response::Response, expected: StatusCode) {
    assert_eq!(response.status(), expected);
    assert!(
        to_bytes(response.into_body(), 1024)
            .await
            .expect("minimal webhook response body reads")
            .is_empty()
    );
}

#[tokio::test]
async fn webhook_route_acknowledges_only_a_committed_verified_delivery() {
    // This catches returning 2xx from the verifier alone or swallowing a
    // post-verification database failure.
    let database = TestDatabase::create("process_inbound_route_ack").await;
    let (runtime, connectors, metadata, revision) = runtime(&database).await;
    start_instance(
        &database,
        &runtime,
        &revision,
        ORDER_ID,
        "00000000-0000-4000-8000-000000000073",
        true,
    )
    .await;
    let state = shared_state(metadata, connectors).await;

    let accepted_body = webhook_body("evt_route_accepted", ORDER_ID, "paid");
    let accepted = post_webhook(state.clone(), accepted_body.clone()).await;
    assert_empty_status(accepted, StatusCode::NO_CONTENT).await;
    let duplicate = post_webhook(state.clone(), accepted_body).await;
    assert_empty_status(duplicate, StatusCode::NO_CONTENT).await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("route failure can be injected");
    let connection = tokio::spawn(connection);
    client
        .batch_execute("DROP TABLE donat.process_inbound_events;")
        .await
        .expect("verified ingress ledger is removed for fault injection");
    connection.abort();

    let failed = post_webhook(
        state,
        webhook_body("evt_route_database_failure", ORDER_ID, "paid"),
    )
    .await;
    assert_empty_status(failed, StatusCode::SERVICE_UNAVAILABLE).await;
    database.drop().await;
}
