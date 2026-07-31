mod support;

use std::sync::Arc;

use donat_metadata::Metadata;
use donat_server::processes::{SignalConsumption, StartConsumption, TransitionConsumption};
use serde_json::{Value as Json, json};
use tokio_postgres::NoTls;
use uuid::Uuid;

use support::TestDatabase;

const ENTITY_ID: &str = "550e8400-e29b-41d4-a716-446655440301";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440302";
const PROCESS_NAME: &str = "approval_wait";
const SIGNAL_NAME: &str = "approval_decision";

fn signal_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            }
        }],
        "processes": [{
            "name": PROCESS_NAME,
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [
                { "name": "entity_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "output": [{ "name": "status", "type": "string!" }],
            "signals": [{
                "name": SIGNAL_NAME,
                "role": "approver",
                "correlation": { "entity_id": "uuid!" },
                "payload": { "status": "string!" }
            }],
            "start_at": "await_approval",
            "states": [
                {
                    "id": "await_approval",
                    "wait": {
                        "signal": SIGNAL_NAME,
                        "role": "approver",
                        "verification": "required",
                        "persist_before_match": true,
                        "correlate": {
                            "entity_id": { "input": "entity_id" }
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
                                "state": "await_approval",
                                "field": "status"
                            }
                        }
                    }
                },
                {
                    "id": "timed_out",
                    "fail": {
                        "code": "approval_timed_out",
                        "message": "approval did not arrive before its deadline"
                    }
                }
            ]
        }]
    }))
    .expect("signal wait metadata deserializes")
}

fn absolute_deadline_metadata() -> Metadata {
    let mut document = serde_json::to_value(signal_metadata()).expect("signal metadata serializes");
    document["processes"][0]["input"]
        .as_array_mut()
        .expect("Process input is an array")
        .push(json!({ "name": "deadline", "type": "timestamptz!" }));
    document["processes"][0]["states"][0]["wait"]["deadline"] = json!({ "input": "deadline" });
    serde_json::from_value(document).expect("absolute signal deadline metadata deserializes")
}

async fn start_instance(
    database: &TestDatabase,
    runtime: &donat_server::processes::ProcessRuntime,
    revision: &str,
) -> Uuid {
    start_instance_with(database, runtime, revision, REQUEST_ID).await
}

async fn start_instance_with(
    database: &TestDatabase,
    runtime: &donat_server::processes::ProcessRuntime,
    revision: &str,
    request_id: &str,
) -> Uuid {
    database
        .seed_start(
            PROCESS_NAME,
            revision,
            json!({ "entity_id": ENTITY_ID, "request_id": request_id }),
            request_id,
        )
        .await;
    match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    }
}

async fn seed_signal(database_url: &str, revision: &str, idempotency_key: &str) -> Uuid {
    seed_signal_for(
        database_url,
        revision,
        idempotency_key,
        ENTITY_ID,
        "approved",
    )
    .await
}

async fn seed_signal_for(
    database_url: &str,
    revision: &str,
    idempotency_key: &str,
    entity_id: &str,
    status: &str,
) -> Uuid {
    seed_signal_json(
        database_url,
        revision,
        idempotency_key,
        json!({ "entity_id": entity_id }),
        json!({ "status": status }),
    )
    .await
}

async fn seed_signal_json(
    database_url: &str,
    revision: &str,
    idempotency_key: &str,
    correlation: Json,
    payload: Json,
) -> Uuid {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process signal database is available");
    let connection = tokio::spawn(connection);
    let request_id = client
        .query_one(
            "
            INSERT INTO donat.process_signal_requests (
                source_name,
                process_name,
                process_revision,
                signal_name,
                correlation_json,
                payload_json,
                command_invocation_id,
                effect_position,
                idempotency_key,
                status
            )
            VALUES (
                'default',
                $1,
                $2,
                $3,
                $4,
                $5,
                gen_random_uuid(),
                0,
                $6,
                'pending'
            )
            RETURNING id
            ",
            &[
                &PROCESS_NAME,
                &revision,
                &SIGNAL_NAME,
                &correlation,
                &payload,
                &idempotency_key,
            ],
        )
        .await
        .expect("Process signal request inserts")
        .get(0);
    connection.abort();
    request_id
}

#[tokio::test]
async fn absolute_timestamptz_deadline_is_persisted_exactly() {
    // This catches replacing a business deadline with worker-local elapsed
    // time or reparsing it through the server's local timezone.
    let database = TestDatabase::create("process_signal_absolute_deadline").await;
    let metadata = absolute_deadline_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("absolute deadline fixture is available");
    let connection = tokio::spawn(connection);
    let deadline: Json = client
        .query_one(
            "SELECT to_jsonb(statement_timestamp() + interval '10 minutes')",
            &[],
        )
        .await
        .expect("database creates an absolute deadline")
        .get(0);
    connection.abort();
    database
        .seed_start(
            PROCESS_NAME,
            &revision,
            json!({
                "entity_id": ENTITY_ID,
                "request_id": REQUEST_ID,
                "deadline": deadline.clone()
            }),
            REQUEST_ID,
        )
        .await;
    let instance_id = match runtime.consume_one_start().await.unwrap() {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let timer_event_id = match runtime.consume_one_transition().await.unwrap() {
        TransitionConsumption::WaitEntered { timer_event_id, .. } => timer_event_id,
        other => panic!("expected an absolute durable wait, got {other:?}"),
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("absolute deadline result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                available_at = (($2::jsonb #>> '{}')::timestamptz),
                payload_json -> 'correlation',
                status
            FROM donat.process_events
            WHERE source_name = 'default'
              AND instance_id = $1
              AND id = $3
            ",
            &[&instance_id, &deadline, &timer_event_id],
        )
        .await
        .expect("absolute deadline is stored without drift");
    assert!(row.get::<_, bool>(0));
    assert_eq!(row.get::<_, Json>(1), json!({ "entity_id": ENTITY_ID }));
    assert_eq!(row.get::<_, String>(2), "pending");
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn unrestricted_signal_can_be_narrowed_to_a_process_caller_role() {
    // This catches runtime rejecting the compiler-supported form used by the
    // Petshop grooming flow: declaration-wide role omitted, wait role fixed.
    let mut document = serde_json::to_value(signal_metadata()).expect("signal metadata serializes");
    document["processes"][0]["signals"][0]
        .as_object_mut()
        .expect("signal declaration is an object")
        .remove("role");
    document["processes"][0]["states"][0]["wait"]["role"] = json!("customer");
    let metadata: Metadata =
        serde_json::from_value(document).expect("unrestricted signal metadata deserializes");
    let database = TestDatabase::create("process_signal_unrestricted_role").await;
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the narrowed unrestricted signal wait becomes receptive"),
        TransitionConsumption::WaitEntered {
            instance_id: waiting,
            ref state,
            ..
        } if waiting == instance_id && state == "await_approval"
    ));

    database.drop().await;
}

#[tokio::test]
async fn identical_signal_contract_can_target_a_live_retired_revision() {
    // This catches substituting the active Process revision or requiring
    // revision identity when reconciliation proved the signal ABI identical.
    let database = TestDatabase::create("process_signal_compatible_revision").await;
    let first_metadata = signal_metadata();
    let (first_runtime, first_revision) = database.runtime(&first_metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &first_runtime, &first_revision).await;
    assert!(matches!(
        first_runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    drop(first_runtime);

    let mut second_metadata = signal_metadata();
    second_metadata.processes[0].version = 2;
    let (second_runtime, second_revision) = database.runtime(&second_metadata, PROCESS_NAME).await;
    assert_ne!(second_revision, first_revision);
    let request_id = seed_signal(&database.url, &second_revision, "compatible-revision").await;
    assert!(matches!(
        second_runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Accepted {
            request_id: accepted_request,
            instance_id: accepted_instance,
            ..
        } if accepted_request == request_id && accepted_instance == instance_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("compatible revision outcome is inspectable");
    let connection = tokio::spawn(connection);
    let event_revision: String = client
        .query_one(
            "
            SELECT revision
            FROM donat.process_events
            WHERE source_name = 'default'
              AND instance_id = $1
              AND kind = 'signal'
            ",
            &[&instance_id],
        )
        .await
        .expect("compatible signal event targets the instance revision")
        .get(0);
    assert_eq!(event_revision, first_revision);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn changed_signal_contract_cannot_target_a_live_retired_revision() {
    // This catches inferring rolling compatibility from a shared signal name
    // while silently widening its payload ABI.
    let database = TestDatabase::create("process_signal_incompatible_revision").await;
    let first_metadata = signal_metadata();
    let (first_runtime, first_revision) = database.runtime(&first_metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &first_runtime, &first_revision).await;
    assert!(matches!(
        first_runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    drop(first_runtime);

    let mut second_metadata = signal_metadata();
    second_metadata.processes[0].version = 2;
    second_metadata.processes[0].signals[0]
        .payload
        .insert("reason".to_owned(), "string!".to_owned());
    let (second_runtime, second_revision) = database.runtime(&second_metadata, PROCESS_NAME).await;
    let request_id = seed_signal_json(
        &database.url,
        &second_revision,
        "incompatible-revision",
        json!({ "entity_id": ENTITY_ID }),
        json!({ "status": "approved", "reason": "manual review completed" }),
    )
    .await;
    assert_eq!(
        second_runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Unmatched { request_id }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("incompatible revision outcome is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $2),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND kind = 'signal')
            ",
            &[&instance_id, &request_id],
        )
        .await
        .expect("incompatible signal remains audit-only");
    assert_eq!(row.get::<_, String>(0), "unmatched");
    assert_eq!(row.get::<_, i64>(1), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn signal_committed_before_the_deadline_wins_after_a_delayed_poll() {
    // This catches ordering an accepted signal by worker wake-up time instead
    // of the durable outbox time at which it became eligible.
    let database = TestDatabase::create("process_signal_before_deadline").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    let request_id = seed_signal(&database.url, &revision, "before-deadline").await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("signal deadline ordering is controllable");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_events timer
            SET available_at = request.created_at + interval '1 microsecond'
            FROM donat.process_signal_requests request
            WHERE timer.source_name = 'default'
              AND timer.instance_id = $1
              AND timer.kind = 'timer'
              AND timer.status = 'pending'
              AND request.source_name = timer.source_name
              AND request.id = $2
            ",
            &[&instance_id, &request_id],
        )
        .await
        .expect("timer becomes due immediately after the signal outbox time");
    connection.abort();

    assert!(
        matches!(
            runtime
                .consume_one_due_timer()
                .await
                .expect("timer worker observes the earlier durable signal"),
            TransitionConsumption::NoWork
        ),
        "a due timeout must not overtake a matching signal request committed before its deadline"
    );
    let signal_event_id = match runtime.consume_one_signal().await.unwrap() {
        SignalConsumption::Accepted { event_id, .. } => event_id,
        other => panic!("expected an accepted signal, got {other:?}"),
    };
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::Advanced {
            event_id,
            ref to_state,
            ..
        } if event_id == signal_event_id && to_state == "done"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("signal deadline winner is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                current_state,
                (SELECT status
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND kind = 'timer')
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("pre-deadline signal closed the timer");
    assert_eq!(row.get::<_, String>(0), "done");
    assert_eq!(row.get::<_, String>(1), "failed");
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn signal_created_after_the_deadline_is_rejected_before_timeout_polling() {
    // This catches treating a delayed timer worker as an extension of the
    // declared business deadline.
    let database = TestDatabase::create("process_signal_after_deadline").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("signal deadline is controllable");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_events
            SET available_at = statement_timestamp() - interval '1 second'
            WHERE source_name = 'default'
              AND instance_id = $1
              AND kind = 'timer'
              AND status = 'pending'
            ",
            &[&instance_id],
        )
        .await
        .expect("wait deadline expires by database time");
    connection.abort();

    let request_id = seed_signal(&database.url, &revision, "after-deadline").await;
    assert_eq!(
        runtime
            .consume_one_signal()
            .await
            .expect("post-deadline signal is audited"),
        SignalConsumption::UnexpectedState { request_id }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("post-deadline signal result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $2),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND kind = 'signal')
            ",
            &[&instance_id, &request_id],
        )
        .await
        .expect("post-deadline signal remains audit-only");
    assert_eq!(row.get::<_, String>(0), "unexpected_state");
    assert_eq!(row.get::<_, i64>(1), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn signal_created_before_the_wait_is_receptive_is_not_buffered() {
    // This catches accepting an early outbox row merely because its worker
    // happens to poll after the instance later enters the matching wait.
    let database = TestDatabase::create("process_signal_early").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    let request_id = seed_signal(&database.url, &revision, "early-signal").await;

    assert_eq!(
        runtime
            .consume_one_signal()
            .await
            .expect("early signal is audited"),
        SignalConsumption::UnexpectedState { request_id }
    );
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the Process can enter its wait after the early signal"),
        TransitionConsumption::WaitEntered {
            instance_id: waiting,
            ref state,
            ..
        } if waiting == instance_id && state == "await_approval"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("early signal outcome is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $1),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $2
                   AND kind = 'signal'),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $2
                   AND kind = 'timer'
                   AND status = 'pending')
            ",
            &[&request_id, &instance_id],
        )
        .await
        .expect("early signal remains audit-only");
    assert_eq!(row.get::<_, String>(0), "unexpected_state");
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn a_future_matching_wait_does_not_make_one_receptive_wait_ambiguous() {
    // This catches counting a wait that became receptive only after the
    // durable signal arrival as a competing target.
    let database = TestDatabase::create("process_signal_future_wait").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let first_instance = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    let request_id = seed_signal(&database.url, &revision, "one-receptive-wait").await;

    let second_request_id = "550e8400-e29b-41d4-a716-446655440399";
    let second_instance =
        start_instance_with(&database, &runtime, &revision, second_request_id).await;
    assert_ne!(second_instance, first_instance);
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered {
            instance_id,
            ..
        } if instance_id == second_instance
    ));

    assert!(matches!(
        runtime
            .consume_one_signal()
            .await
            .expect("the one receptive wait receives the signal"),
        SignalConsumption::Accepted {
            request_id: accepted_request,
            instance_id,
            ..
        } if accepted_request == request_id && instance_id == first_instance
    ));

    database.drop().await;
}

#[tokio::test]
async fn receptive_wait_accepts_one_typed_signal_and_cancels_its_timer() {
    // This catches non-atomic request/event creation, payloads that bypass
    // their pinned contract, and a timeout left live after signal success.
    let database = TestDatabase::create("process_signal_accepted").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    let request_id = seed_signal(&database.url, &revision, "accepted-signal").await;
    let accepted = runtime
        .consume_one_signal()
        .await
        .expect("matching signal request consumes");
    let event_id = match accepted {
        SignalConsumption::Accepted {
            request_id: accepted_request,
            instance_id: accepted_instance,
            event_id,
        } => {
            assert_eq!(accepted_request, request_id);
            assert_eq!(accepted_instance, instance_id);
            event_id
        }
        other => panic!("expected an accepted signal, got {other:?}"),
    };

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("accepted signal advances its wait"),
        TransitionConsumption::Advanced {
            event_id: advanced_event,
            ref from_state,
            ref to_state,
            ..
        } if advanced_event == event_id
            && from_state == "await_approval"
            && to_state == "done"
    ));
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::Completed { instance_id: completed, .. }
            if completed == instance_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("accepted signal outcome is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                status,
                version,
                state_json,
                terminal_output_json,
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $2),
                (SELECT status
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND kind = 'timer'),
                (SELECT count(*)
                 FROM donat.process_transition_logs
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND outcome = 'signal_received')
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id, &request_id],
        )
        .await
        .expect("accepted signal is fully durable");
    assert_eq!(row.get::<_, String>(0), "terminal");
    assert_eq!(row.get::<_, i64>(1), 3);
    assert_eq!(
        row.get::<_, Json>(2),
        json!({
            "await_approval": {
                "entity_id": ENTITY_ID,
                "status": "approved"
            },
            "done": { "status": "approved" }
        })
    );
    assert_eq!(row.get::<_, Json>(3), json!({ "status": "approved" }));
    assert_eq!(row.get::<_, String>(4), "consumed");
    assert_eq!(row.get::<_, String>(5), "failed");
    assert_eq!(row.get::<_, i64>(6), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn signal_and_due_timer_compete_for_exactly_one_wait_version() {
    // This catches independently advancing signal/timer workers or leaving
    // the losing durable event pending after the wait has already closed.
    let database = TestDatabase::create("process_signal_timer_race").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    seed_signal(&database.url, &revision, "racing-signal").await;
    let signal_event_id = match runtime.consume_one_signal().await.unwrap() {
        SignalConsumption::Accepted { event_id, .. } => event_id,
        other => panic!("expected an accepted racing signal, got {other:?}"),
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("signal/timer race is controllable");
    let connection = tokio::spawn(connection);
    let timer_event_id: Uuid = client
        .query_one(
            "
            UPDATE donat.process_events
            SET available_at = statement_timestamp() - interval '1 hour'
            WHERE source_name = 'default'
              AND instance_id = $1
              AND kind = 'timer'
              AND status = 'pending'
            RETURNING id
            ",
            &[&instance_id],
        )
        .await
        .expect("timer becomes due by database time")
        .get(0);
    client
        .execute(
            "
            UPDATE donat.process_events
            SET available_at = statement_timestamp() - interval '2 hours'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&signal_event_id],
        )
        .await
        .expect("signal is the generic consumer's first snapshot");
    connection.abort();

    let runtime = Arc::new(runtime);
    let (signal_side, timer_side) = tokio::join!(
        runtime.consume_one_transition(),
        runtime.consume_one_due_timer()
    );
    let outcomes = [
        signal_side.expect("signal-side transition consumer succeeds"),
        timer_side.expect("timer-side transition consumer succeeds"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, TransitionConsumption::Advanced { .. }))
            .count(),
        1,
        "only one event may close a wait version"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, TransitionConsumption::NoWork))
            .count(),
        1,
        "the losing event observes a stale instance version"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("signal/timer race result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                current_state,
                version,
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND id IN ($2, $3)
                   AND status = 'consumed'),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND id IN ($2, $3)
                   AND status = 'failed'),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND id IN ($2, $3)
                   AND status = 'pending')
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id, &signal_event_id, &timer_event_id],
        )
        .await
        .expect("race has one durable winner and one closed loser");
    assert!(
        matches!(row.get::<_, String>(0).as_str(), "done" | "timed_out"),
        "one declared route wins"
    );
    assert_eq!(row.get::<_, i64>(1), 2);
    assert_eq!(row.get::<_, i64>(2), 1);
    assert_eq!(row.get::<_, i64>(3), 1);
    assert_eq!(row.get::<_, i64>(4), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn late_signal_is_audited_without_reopening_a_closed_wait() {
    // This catches treating a known, already-closed correlation as unmatched
    // or buffering it for a later state.
    let database = TestDatabase::create("process_signal_late").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    seed_signal(&database.url, &revision, "first-decision").await;
    assert!(matches!(
        runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Accepted { .. }
    ));
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::Advanced { ref to_state, .. } if to_state == "done"
    ));

    let late_request_id = seed_signal(&database.url, &revision, "late-decision").await;
    assert_eq!(
        runtime
            .consume_one_signal()
            .await
            .expect("late signal is audited"),
        SignalConsumption::UnexpectedState {
            request_id: late_request_id
        }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("late signal result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                current_state,
                version,
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $2),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND kind = 'signal')
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id, &late_request_id],
        )
        .await
        .expect("late signal did not reopen the wait");
    assert_eq!(row.get::<_, String>(0), "done");
    assert_eq!(row.get::<_, i64>(1), 2);
    assert_eq!(row.get::<_, String>(2), "unexpected_state");
    assert_eq!(row.get::<_, i64>(3), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn signal_semantic_key_deduplicates_distinct_command_generations() {
    // This catches relying only on command invocation/effect identity after
    // an expired command idempotency generation emits the same business signal.
    let database = TestDatabase::create("process_signal_duplicate").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    let first_request = seed_signal(&database.url, &revision, "same-decision").await;
    let duplicate_request = seed_signal(&database.url, &revision, "same-decision").await;

    assert!(matches!(
        runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Accepted {
            request_id,
            instance_id: accepted,
            ..
        } if request_id == first_request && accepted == instance_id
    ));
    assert_eq!(
        runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Duplicate {
            request_id: duplicate_request
        }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("duplicate signal result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $1),
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $2),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id = $3
                   AND kind = 'signal')
            ",
            &[&first_request, &duplicate_request, &instance_id],
        )
        .await
        .expect("semantic duplicate creates no second event");
    assert_eq!(row.get::<_, String>(0), "consumed");
    assert_eq!(row.get::<_, String>(1), "duplicate");
    assert_eq!(row.get::<_, i64>(2), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn matching_more_than_one_receptive_instance_is_ambiguous() {
    // This catches choosing an arbitrary instance when a supposedly unique
    // business correlation is duplicated.
    let database = TestDatabase::create("process_signal_ambiguous").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let first_instance = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    let second_request_id = "550e8400-e29b-41d4-a716-446655440303";
    let second_instance =
        start_instance_with(&database, &runtime, &revision, second_request_id).await;
    assert_ne!(first_instance, second_instance);
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { instance_id, .. }
            if instance_id == second_instance
    ));

    let request_id = seed_signal(&database.url, &revision, "ambiguous-decision").await;
    assert_eq!(
        runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Ambiguous { request_id }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("ambiguous signal result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT status
                 FROM donat.process_signal_requests
                 WHERE source_name = 'default' AND id = $1),
                (SELECT count(*)
                 FROM donat.process_events
                 WHERE source_name = 'default'
                   AND instance_id IN ($2, $3)
                   AND kind = 'signal')
            ",
            &[&request_id, &first_instance, &second_instance],
        )
        .await
        .expect("ambiguous signal remains audit-only");
    assert_eq!(row.get::<_, String>(0), "ambiguous");
    assert_eq!(row.get::<_, i64>(1), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn unknown_correlation_is_unmatched_and_creates_no_event() {
    // This catches broad process-name matching that ignores the compiled
    // correlation contract.
    let database = TestDatabase::create("process_signal_unmatched").await;
    let metadata = signal_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    let instance_id = start_instance(&database, &runtime, &revision).await;
    assert!(matches!(
        runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::WaitEntered { .. }
    ));
    let request_id = seed_signal_for(
        &database.url,
        &revision,
        "unknown-correlation",
        "550e8400-e29b-41d4-a716-446655440399",
        "approved",
    )
    .await;
    assert_eq!(
        runtime.consume_one_signal().await.unwrap(),
        SignalConsumption::Unmatched { request_id }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("unmatched signal result is inspectable");
    let connection = tokio::spawn(connection);
    let count: i64 = client
        .query_one(
            "
            SELECT count(*)
            FROM donat.process_events
            WHERE source_name = 'default'
              AND instance_id = $1
              AND kind = 'signal'
            ",
            &[&instance_id],
        )
        .await
        .expect("unmatched signal creates no event")
        .get(0);
    assert_eq!(count, 0);
    connection.abort();
    database.drop().await;
}
