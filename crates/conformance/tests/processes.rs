//! Native black-box contracts for command-to-Process durable hand-off.

use donat_conformance::Suite;
use donat_metadata::Metadata;
use postgres::NoTls;
use serde_json::{Value as Json, json};

const ORDER_ID: &str = "550e8400-e29b-41d4-a716-446655440110";
const CHANGED_ORDER_ID: &str = "550e8400-e29b-41d4-a716-446655440111";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440112";

fn process_effect_metadata(retired: bool) -> Metadata {
    process_effect_metadata_with_terminal(retired, "queued")
}

fn process_effect_metadata_with_terminal(retired: bool, terminal_value: &str) -> Metadata {
    let mut document = json!({
        "version": 3,
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
            "name": "enqueue_checkout",
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "arguments": [
                { "name": "order_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" },
                { "name": "semantic_key", "type": "uuid!" }
            ],
            "steps": [],
            "result": {
                "order_id": { "arg": "order_id" }
            },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command",
                "retention": "1d"
            },
            "effects": [{
                "start_process": {
                    "process": "checkout",
                    "input": {
                        "order_id": { "arg": "order_id" },
                        "request_id": { "arg": "semantic_key" }
                    },
                    "idempotency_key": { "argument": "semantic_key" }
                }
            }]
        }],
        "processes": [{
            "name": "checkout",
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
            "start_at": "done",
            "states": [{
                "id": "done",
                "output": {
                    "values": { "status": { "literal": terminal_value } }
                }
            }]
        }]
    });
    if retired {
        document["processes"][0]["lifecycle"] = json!("retired");
    }
    serde_json::from_value(document).expect("command and Process metadata deserialize")
}

fn enqueue_checkout(suite: &donat_conformance::Running, order_id: &str) -> (u16, Json) {
    suite.post(
        "/v1/graphql",
        &json!({
            "query": format!(
                "mutation {{ enqueue_checkout(order_id: \"{order_id}\", request_id: \"{REQUEST_ID}\", semantic_key: \"{REQUEST_ID}\") {{ order_id }} }}"
            )
        }),
        &[("X-Donat-Role".to_owned(), "customer".to_owned())],
    )
}

#[test]
fn command_start_effect_is_atomic_revision_pinned_and_replay_safe() {
    let suite = Suite::new("process_start_effect")
        .initial_metadata(process_effect_metadata(false))
        .with_migrations()
        .env("DONAT_PROCESS_WORKERS_DISABLED", "true")
        .start();

    let (status, first) = enqueue_checkout(&suite, ORDER_ID);
    assert_eq!(status, 200, "first command response: {first}");
    assert_eq!(
        first,
        json!({
            "data": {
                "enqueue_checkout": { "order_id": ORDER_ID }
            }
        }),
        "internal invocation fields must not leak into the public GraphQL result"
    );

    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect to inspect process hand-off");
    let first_handoff = client
        .query_one(
            "SELECT invocation.invocation_id::text, request.command_invocation_id::text, \
                    request.revision, request.input_json::text, request.effect_position, \
                    request.idempotency_key, request.status, definition.status \
             FROM donat.command_invocations AS invocation \
             JOIN donat.process_start_requests AS request \
               ON request.command_invocation_id = invocation.invocation_id \
             JOIN donat.process_definition_versions AS definition \
               ON definition.source_name = request.source_name \
              AND definition.process_name = request.process_name \
              AND definition.revision = request.revision \
             WHERE invocation.command_name = 'enqueue_checkout' \
               AND invocation.key = $1",
            &[&REQUEST_ID],
        )
        .expect("first command generation has one pinned start request");
    let first_generation: String = first_handoff.get(0);
    assert_eq!(first_handoff.get::<_, String>(1), first_generation);
    let input: Json = serde_json::from_str(&first_handoff.get::<_, String>(3))
        .expect("start input is canonical JSON");
    assert_eq!(
        input,
        json!({ "order_id": ORDER_ID, "request_id": REQUEST_ID })
    );
    assert_eq!(first_handoff.get::<_, i32>(4), 0);
    assert_eq!(first_handoff.get::<_, String>(5), REQUEST_ID);
    assert_eq!(first_handoff.get::<_, String>(6), "pending");
    assert_eq!(first_handoff.get::<_, String>(7), "active");
    assert!(
        !first_handoff.get::<_, String>(2).is_empty(),
        "the outbox must pin a nonempty compiled revision"
    );

    let (status, replay) = enqueue_checkout(&suite, ORDER_ID);
    assert_eq!(status, 200, "replay response: {replay}");
    assert_eq!(replay, first);
    let replayed = client
        .query_one(
            "SELECT count(*), min(command_invocation_id::text), \
                    max(command_invocation_id::text) \
             FROM donat.process_start_requests \
             WHERE source_name = 'default' AND process_name = 'checkout'",
            &[],
        )
        .expect("replayed hand-off remains inspectable");
    assert_eq!(replayed.get::<_, i64>(0), 1);
    assert_eq!(
        replayed.get::<_, Option<String>>(1).as_deref(),
        Some(first_generation.as_str())
    );
    assert_eq!(
        replayed.get::<_, Option<String>>(2).as_deref(),
        Some(first_generation.as_str())
    );

    let (status, conflict) = enqueue_checkout(&suite, CHANGED_ORDER_ID);
    assert_eq!(status, 200, "changed-input response: {conflict}");
    assert_eq!(
        conflict,
        json!({
            "errors": [{
                "extensions": {
                    "path": "$.selectionSet.enqueue_checkout",
                    "code": "validation-failed"
                },
                "message": "idempotency key was reused with different input"
            }]
        })
    );
    let start_count: i64 = client
        .query_one(
            "SELECT count(*) FROM donat.process_start_requests \
             WHERE source_name = 'default' AND process_name = 'checkout'",
            &[],
        )
        .expect("count start requests after rejected key reuse")
        .get(0);
    assert_eq!(start_count, 1, "a rejected reuse cannot emit another start");
}

#[test]
fn retired_process_rejects_before_command_or_outbox_state() {
    let suite = Suite::new("retired_process_start_effect")
        .initial_metadata(process_effect_metadata(true))
        .with_migrations()
        .env("DONAT_PROCESS_WORKERS_DISABLED", "true")
        .start();

    let (status, response) = enqueue_checkout(&suite, ORDER_ID);
    assert_eq!(status, 200, "retired Process response: {response}");
    assert_eq!(
        response,
        json!({
            "errors": [{
                "extensions": {
                    "path": "$.selectionSet.enqueue_checkout",
                    "code": "validation-failed"
                },
                "message": "process 'default.checkout' does not accept new starts"
            }]
        })
    );

    let mut client =
        postgres::Client::connect(suite.db_url(), NoTls).expect("connect after retired rejection");
    let counts = client
        .query_one(
            "SELECT \
                (SELECT count(*) FROM donat.command_invocation_claims \
                 WHERE command_name = 'enqueue_checkout' AND key = $1), \
                (SELECT count(*) FROM donat.command_invocations \
                 WHERE command_name = 'enqueue_checkout' AND key = $1), \
                (SELECT count(*) FROM donat.process_start_requests \
                 WHERE source_name = 'default' AND process_name = 'checkout')",
            &[&REQUEST_ID],
        )
        .expect("rejected command state remains inspectable");
    assert_eq!(counts.get::<_, i64>(0), 0, "claim must not survive");
    assert_eq!(counts.get::<_, i64>(1), 0, "invocation must not survive");
    assert_eq!(counts.get::<_, i64>(2), 0, "outbox must not survive");
}

#[test]
fn process_start_worker_is_durable() {
    let mut suite = Suite::new("process_start_worker")
        .initial_metadata(process_effect_metadata_with_terminal(false, "revision-a"))
        .with_migrations()
        .env("DONAT_PROCESS_WORKERS_DISABLED", "true")
        .start();

    suite.check_query_f(
        "processes/start_worker/first_generation.yaml",
        donat_conformance::Transport::Http,
    );
    suite.check_query_f(
        "processes/start_worker/second_generation.yaml",
        donat_conformance::Transport::Http,
    );

    let mut client = postgres::Client::connect(suite.db_url(), NoTls)
        .expect("connect before rolling Process deployment");
    let before_restart = client
        .query_one(
            "
            SELECT count(*), min(revision), max(revision)
            FROM donat.process_start_requests
            WHERE source_name = 'default'
              AND process_name = 'checkout'
              AND status = 'pending'
            ",
            &[],
        )
        .expect("both revision-A start requests are durable before restart");
    assert_eq!(before_restart.get::<_, i64>(0), 2);
    let revision_a: String = before_restart
        .get::<_, Option<String>>(1)
        .expect("revision A is present");
    assert_eq!(
        before_restart.get::<_, Option<String>>(2).as_deref(),
        Some(revision_a.as_str())
    );

    suite.set_engine_env_for_restart("DONAT_PROCESS_WORKERS_DISABLED", "false");
    suite.restart_with_metadata(process_effect_metadata_with_terminal(false, "revision-b"));
    let _ = suite.base_url();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = client
            .query_one(
                "
                SELECT
                    (SELECT count(*) FROM donat.process_start_requests
                     WHERE source_name = 'default' AND process_name = 'checkout'
                       AND status = 'pending'),
                    (SELECT count(*) FROM donat.process_instances
                     WHERE source_name = 'default' AND process_name = 'checkout'
                       AND start_idempotency_key = $1),
                    (SELECT count(*) FROM donat.process_transition_logs log
                     JOIN donat.process_instances instance
                       ON instance.source_name = log.source_name
                      AND instance.id = log.instance_id
                     WHERE log.source_name = 'default'
                       AND instance.process_name = 'checkout'
                       AND log.outcome = 'duplicate_start')
                ",
                &[&REQUEST_ID],
            )
            .expect("poll durable Process worker outcome");
        if state.get::<_, i64>(0) == 0 && state.get::<_, i64>(1) == 1 && state.get::<_, i64>(2) == 1
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Process worker did not consume both pinned requests before timeout"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    let outcome = client
        .query_one(
            "
            SELECT
                instance.revision,
                definition_a.status,
                definition_b.revision,
                (SELECT count(*) FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.kind = 'start'),
                (SELECT count(*) FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome = 'started'),
                (SELECT count(*) FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome = 'duplicate_start'),
                (SELECT count(*) FROM donat.process_start_requests request
                 WHERE request.source_name = instance.source_name
                   AND request.process_name = instance.process_name
                   AND request.status = 'consumed'),
                (SELECT count(*) FROM donat.process_start_requests request
                 WHERE request.source_name = instance.source_name
                   AND request.process_name = instance.process_name
                   AND request.status = 'duplicate')
            FROM donat.process_instances instance
            JOIN donat.process_definition_versions definition_a
              ON definition_a.source_name = instance.source_name
             AND definition_a.process_name = instance.process_name
             AND definition_a.revision = instance.revision
            JOIN donat.process_definition_versions definition_b
              ON definition_b.source_name = instance.source_name
             AND definition_b.process_name = instance.process_name
             AND definition_b.status = 'active'
            WHERE instance.source_name = 'default'
              AND instance.process_name = 'checkout'
              AND instance.start_idempotency_key = $1
            ",
            &[&REQUEST_ID],
        )
        .expect("rolling Process start outcome is durable");
    assert_eq!(outcome.get::<_, String>(0), revision_a);
    assert_eq!(outcome.get::<_, String>(1), "retired");
    assert_ne!(
        outcome.get::<_, String>(2),
        revision_a,
        "the active B revision must differ from the instance's pinned A revision"
    );
    assert_eq!(outcome.get::<_, i64>(3), 1);
    assert_eq!(outcome.get::<_, i64>(4), 1);
    assert_eq!(outcome.get::<_, i64>(5), 1);
    assert_eq!(outcome.get::<_, i64>(6), 1);
    assert_eq!(outcome.get::<_, i64>(7), 1);
}
