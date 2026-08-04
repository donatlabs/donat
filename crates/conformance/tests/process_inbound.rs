//! Native black-box contracts for verified inbound Process delivery.
//!
//! These cases drive the real engine binary over HTTP end to end: a caller
//! invokes a domain Command through `/v1/graphql`, the Command's committed
//! outbox starts a Process, the Process parks on a declared provider wait, and
//! a signed provider request to the public connector webhook route advances it.
//! Nothing between the caller and the durable outcome is stubbed.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use donat_conformance::Suite;
use donat_metadata::Metadata;
use hmac::{Hmac, Mac};
use postgres::NoTls;
use serde_json::{Value as Json, json};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

const ORDER_ID: &str = "550e8400-e29b-41d4-a716-446655440310";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440311";
const CONNECTOR_INSTANCE: &str = "payments";
const PROCESS_NAME: &str = "payment_confirmation";
const TRIGGER_NAME: &str = "checkout.session.completed";
const WEBHOOK_PATH: &str = "/v1/connectors/payments/webhooks";
const API_KEY_ENV: &str = "DONAT_PROCESS_INBOUND_CONFORMANCE_API_KEY";
const WEBHOOK_SECRET_ENV: &str = "DONAT_PROCESS_INBOUND_CONFORMANCE_WEBHOOK_SECRET";
const WEBHOOK_SECRET: &str = "whsec-process-inbound-conformance-secret";
const PROVIDER_EVENT_ID: &str = "evt_process_inbound_conformance";

fn inbound_metadata() -> Metadata {
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
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": []
        }],
        "commands": [{
            "name": "enqueue_confirmation",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "steps": [],
            "result": { "order_id": { "arg": "order_id" } },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command",
                "retention": "1d"
            },
            "effects": [{
                "start_process": {
                    "process": PROCESS_NAME,
                    "input": {
                        "order_id": { "arg": "order_id" },
                        "request_id": { "arg": "request_id" }
                    },
                    "idempotency_key": { "argument": "request_id" }
                }
            }]
        }],
        "connectors": [{
            "name": CONNECTOR_INSTANCE,
            "module": "stripe",
            "config": {
                "endpoint_identity": "stripe_process_inbound_conformance",
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
            "idempotency": {
                "key": { "input": "request_id" },
                "scope": [{ "input": "order_id" }]
            },
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
                        "next": "confirmed",
                        "on_timeout": "timed_out"
                    }
                },
                {
                    "id": "confirmed",
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
                        "message": "the payment webhook did not arrive before its deadline"
                    }
                }
            ]
        }]
    }))
    .expect("verified webhook Process conformance metadata deserializes")
}

fn start_suite(name: &str) -> donat_conformance::Running {
    Suite::new(name)
        .initial_metadata(inbound_metadata())
        .with_migrations()
        .env(API_KEY_ENV, "sk_test_process_inbound_conformance")
        .env(WEBHOOK_SECRET_ENV, WEBHOOK_SECRET)
        .start()
}

fn enqueue_confirmation(suite: &donat_conformance::Running) -> (u16, Json) {
    suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ enqueue_confirmation(order_id: \"{ORDER_ID}\", request_id: \"{REQUEST_ID}\") {{ order_id }} }}"
            )
        }),
        &[("X-Donat-Role".to_owned(), "customer".to_owned())],
    )
}

fn webhook_body(payment_status: &str) -> Vec<u8> {
    json!({
        "id": PROVIDER_EVENT_ID,
        "type": TRIGGER_NAME,
        "data": {
            "object": {
                "object": "checkout.session",
                "id": "cs_process_inbound_conformance",
                "client_reference_id": ORDER_ID,
                "payment_status": payment_status
            }
        }
    })
    .to_string()
    .into_bytes()
}

fn signed_headers(body: &[u8]) -> Vec<(String, String)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs();
    let mut mac = HmacSha256::new_from_slice(WEBHOOK_SECRET.as_bytes())
        .expect("the fixed webhook secret is a valid HMAC key");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    let signature = mac.finalize().into_bytes();
    vec![
        ("Content-Type".to_owned(), "application/json".to_owned()),
        (
            "Stripe-Signature".to_owned(),
            format!("t={timestamp},v1={signature:x}"),
        ),
    ]
}

/// Wait until the instance parks on its declared provider wait, so the delivery
/// under test meets a genuinely receptive instance.
fn await_receptive_wait(client: &mut postgres::Client) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = client
            .query_opt(
                "
                SELECT instance.current_state, count(event.id)
                FROM donat.process_instances instance
                LEFT JOIN donat.process_events event
                  ON event.source_name = instance.source_name
                 AND event.instance_id = instance.id
                 AND event.kind = 'timer'
                 AND event.status = 'pending'
                WHERE instance.source_name = 'default'
                  AND instance.process_name = $1
                GROUP BY instance.current_state
                ",
                &[&PROCESS_NAME],
            )
            .expect("poll for the receptive Process wait");
        if let Some(row) = row
            && row.get::<_, String>(0) == "await_payment"
            && row.get::<_, i64>(1) == 1
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the Process never parked on its declared provider wait"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn await_terminal(client: &mut postgres::Client) -> (String, Option<Json>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let row = client
            .query_one(
                "
                SELECT status, terminal_output_json::text
                FROM donat.process_instances
                WHERE source_name = 'default' AND process_name = $1
                ",
                &[&PROCESS_NAME],
            )
            .expect("poll the durable Process instance");
        let status: String = row.get(0);
        if status != "running" {
            let output = row
                .get::<_, Option<String>>(1)
                .map(|text| serde_json::from_str(&text).expect("terminal output is valid JSON"));
            return (status, output);
        }
        assert!(
            Instant::now() < deadline,
            "the Process did not reach a terminal state before the timeout"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn signed_provider_delivery_advances_a_waiting_process_exactly_once() {
    let suite = start_suite("process_inbound_delivery");
    let (status, body) = enqueue_confirmation(&suite);
    assert_eq!(status, 200, "command mutation status: {body}");

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    await_receptive_wait(&mut client);

    let payload = webhook_body("paid");
    let (delivery_status, delivery_body) =
        suite.post_bytes(WEBHOOK_PATH, &payload, &signed_headers(&payload));
    assert_eq!(
        delivery_status, 204,
        "a committed verified delivery is acknowledged with an empty 204"
    );
    assert!(
        delivery_body.is_empty(),
        "the provider acknowledgement carries no body"
    );

    let (instance_status, output) = await_terminal(&mut client);
    assert_eq!(instance_status, "terminal");
    assert_eq!(
        output,
        Some(json!({ "status": "paid" })),
        "the terminal output carries the verified provider event's own data"
    );

    // The provider retries; the replay must be acknowledged and audited without
    // creating a second Process event or a second dedupe identity.
    let (replay_status, replay_body) =
        suite.post_bytes(WEBHOOK_PATH, &payload, &signed_headers(&payload));
    assert_eq!(
        replay_status, 204,
        "a replayed verified event is acknowledged"
    );
    assert!(replay_body.is_empty());

    let ledger = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE source_name = 'default' AND provider_event_id = $1),
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE source_name = 'default' AND provider_event_id = $1
                   AND outcome = 'accepted'),
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE source_name = 'default' AND provider_event_id = $1
                   AND outcome = 'duplicate'),
                (SELECT count(*) FROM donat.process_inbound_events
                 WHERE source_name = 'default' AND provider_event_id = $1),
                (SELECT count(*) FROM donat.process_events event
                 JOIN donat.process_instances instance
                   ON instance.source_name = event.source_name
                  AND instance.id = event.instance_id
                 WHERE event.source_name = 'default'
                   AND instance.process_name = $2
                   AND event.kind = 'signal')
            ",
            &[&PROVIDER_EVENT_ID, &PROCESS_NAME],
        )
        .expect("read the split inbound audit and dedupe ledger");
    assert_eq!(ledger.get::<_, i64>(0), 2, "every attempt is audited");
    assert_eq!(ledger.get::<_, i64>(1), 1, "one delivery is accepted");
    assert_eq!(
        ledger.get::<_, i64>(2),
        1,
        "the replay is audited as duplicate"
    );
    assert_eq!(ledger.get::<_, i64>(3), 1, "one provider dedupe identity");
    assert_eq!(
        ledger.get::<_, i64>(4),
        1,
        "the replay creates no Process event"
    );

    let accepted = client
        .query_one(
            "
            SELECT delivery.instance_id = instance.id, delivery.process_event_id IS NOT NULL
            FROM donat.process_inbound_deliveries delivery
            JOIN donat.process_instances instance
              ON instance.source_name = delivery.source_name
            WHERE delivery.source_name = 'default'
              AND delivery.outcome = 'accepted'
            ",
            &[],
        )
        .expect("the accepted delivery links its instance and event");
    assert!(
        accepted.get::<_, bool>(0),
        "the accepted delivery names its instance"
    );
    assert!(
        accepted.get::<_, bool>(1),
        "the accepted delivery names its event"
    );
}

#[test]
fn unsigned_and_unknown_provider_requests_keep_the_raw_rejection_matrix() {
    let suite = start_suite("process_inbound_rejections");
    let payload = webhook_body("paid");

    let (missing_signature, body) = suite.post_bytes(
        WEBHOOK_PATH,
        &payload,
        &[("Content-Type".to_owned(), "application/json".to_owned())],
    );
    assert_eq!(missing_signature, 400, "an unsigned delivery is rejected");
    assert!(body.is_empty(), "a rejection discloses nothing");

    let (unknown_instance, body) = suite.post_bytes(
        "/v1/connectors/not-declared/webhooks",
        &payload,
        &signed_headers(&payload),
    );
    assert_eq!(
        unknown_instance, 404,
        "an undeclared connector instance is indistinguishable from an absent route"
    );
    assert!(body.is_empty());

    // A rejected delivery is audited without a provider identity and never
    // reaches the dedupe ledger or Process state.
    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect to the source database");
    let audit = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_inbound_deliveries
                 WHERE source_name = 'default' AND outcome = 'invalid_signature'
                   AND provider_event_id IS NULL),
                (SELECT count(*) FROM donat.process_inbound_events
                 WHERE source_name = 'default'),
                (SELECT count(*) FROM donat.process_instances
                 WHERE source_name = 'default')
            ",
            &[],
        )
        .expect("read the invalid-signature audit");
    assert_eq!(
        audit.get::<_, i64>(0),
        1,
        "the unsigned delivery is audited without a provider identity"
    );
    assert_eq!(audit.get::<_, i64>(1), 0, "no dedupe identity is written");
    assert_eq!(audit.get::<_, i64>(2), 0, "no Process state is created");
}
