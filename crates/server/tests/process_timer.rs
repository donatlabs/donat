mod support;

use donat_metadata::Metadata;
use donat_server::processes::{StartConsumption, TransitionConsumption};
use serde_json::{Value as Json, json};
use tokio_postgres::NoTls;

use support::TestDatabase;

const PROCESS_NAME: &str = "dunning_delay";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440401";

fn timer_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "rules": {
            "decision_tables": [{
                "name": "dunning_schedule",
                "inputs": { "attempt": "int!" },
                "output": { "delay_seconds": "int!", "terminal": "bool!" },
                "hit_policy": "first",
                "rows": [
                    {
                        "id": "first_retry",
                        "when": { "attempt": "attempt == 1" },
                        "output": { "delay_seconds": 60, "terminal": false }
                    },
                    {
                        "id": "default",
                        "when": { "attempt": "true" },
                        "output": { "delay_seconds": 0, "terminal": true }
                    }
                ],
                "test_cases": [{
                    "name": "first retry waits one minute",
                    "input": { "attempt": 1 },
                    "expect": {
                        "output": { "delay_seconds": 60, "terminal": false },
                        "matched_row_id": "first_retry"
                    }
                }]
            }]
        },
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
            "input": [{ "name": "request_id", "type": "uuid!" }],
            "output": [{ "name": "delay_seconds", "type": "int!" }],
            "start_at": "wait_before_retry",
            "states": [
                {
                    "id": "wait_before_retry",
                    "wait": {
                        "timer": {
                            "decision_table": "dunning_schedule",
                            "with": { "attempt": { "literal": 1 } },
                            "output": "delay_seconds"
                        },
                        "next": "done"
                    }
                },
                {
                    "id": "done",
                    "output": {
                        "values": {
                            "delay_seconds": {
                                "state": "wait_before_retry",
                                "field": "delay_seconds"
                            }
                        }
                    }
                }
            ]
        }]
    }))
    .expect("timer wait metadata deserializes")
}

#[tokio::test]
async fn database_clock_timer_survives_a_fresh_runtime_and_fires_once() {
    // This catches an in-memory timer wheel, recomputing a decision after
    // restart, or a due event advancing the same wait more than once.
    let database = TestDatabase::create("process_timer_restart").await;
    let metadata = timer_metadata();
    let (runtime, revision) = database.runtime(&metadata, PROCESS_NAME).await;
    database
        .seed_start(
            PROCESS_NAME,
            &revision,
            json!({ "request_id": REQUEST_ID }),
            REQUEST_ID,
        )
        .await;
    let instance_id = match runtime.consume_one_start().await.unwrap() {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let timer_event_id = match runtime.consume_one_transition().await.unwrap() {
        TransitionConsumption::WaitEntered {
            instance_id: waiting,
            timer_event_id,
            ref state,
            ..
        } => {
            assert_eq!(waiting, instance_id);
            assert_eq!(state, "wait_before_retry");
            timer_event_id
        }
        other => panic!("expected a durable timer wait, got {other:?}"),
    };
    drop(runtime);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("durable timer is inspectable");
    let connection = tokio::spawn(connection);
    let scheduled = client
        .query_one(
            "
            SELECT
                kind,
                status,
                payload_json,
                available_at > created_at + interval '59 seconds'
                    AND available_at < created_at + interval '61 seconds'
            FROM donat.process_events
            WHERE source_name = 'default' AND id = $1
            ",
            &[&timer_event_id],
        )
        .await
        .expect("decision-table timer is journaled");
    assert_eq!(scheduled.get::<_, String>(0), "timer");
    assert_eq!(scheduled.get::<_, String>(1), "pending");
    assert_eq!(
        scheduled.get::<_, Json>(2)["output"],
        json!({ "delay_seconds": 60, "terminal": false })
    );
    assert!(scheduled.get::<_, bool>(3));
    client
        .execute(
            "
            UPDATE donat.process_events
            SET available_at = statement_timestamp() - interval '1 second'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&timer_event_id],
        )
        .await
        .expect("database clock makes the persisted timer due");
    connection.abort();

    let (fresh_runtime, fresh_revision) = database.runtime(&metadata, PROCESS_NAME).await;
    assert_eq!(fresh_revision, revision);
    assert!(matches!(
        fresh_runtime
            .consume_one_due_timer()
            .await
            .expect("fresh runtime consumes the database timer"),
        TransitionConsumption::Advanced {
            instance_id: advanced,
            event_id,
            ref from_state,
            ref to_state,
        } if advanced == instance_id
            && event_id == timer_event_id
            && from_state == "wait_before_retry"
            && to_state == "done"
    ));
    assert!(matches!(
        fresh_runtime
            .consume_one_due_timer()
            .await
            .expect("consumed timer cannot fire twice"),
        TransitionConsumption::NoWork
    ));
    assert!(matches!(
        fresh_runtime.consume_one_transition().await.unwrap(),
        TransitionConsumption::Completed { instance_id: completed, .. }
            if completed == instance_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("timer completion is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                status,
                version,
                state_json,
                terminal_output_json,
                (SELECT count(*)
                 FROM donat.process_transition_logs
                 WHERE source_name = 'default'
                   AND instance_id = $1
                   AND outcome = 'timer_fired')
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("timer result is durable");
    assert_eq!(row.get::<_, String>(0), "terminal");
    assert_eq!(row.get::<_, i64>(1), 3);
    assert_eq!(
        row.get::<_, Json>(2),
        json!({
            "wait_before_retry": {
                "delay_seconds": 60,
                "terminal": false
            },
            "done": { "delay_seconds": 60 }
        })
    );
    assert_eq!(row.get::<_, Json>(3), json!({ "delay_seconds": 60 }));
    assert_eq!(row.get::<_, i64>(4), 1);
    connection.abort();
    database.drop().await;
}
