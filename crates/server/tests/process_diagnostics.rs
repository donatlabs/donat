//! The two read-only diagnostics on a real instance.
//!
//! `inspect` has to read the journal a running engine actually wrote, and
//! `verify-history` has to notice the one thing an operator would call it
//! about: a journal that no longer agrees with itself. Both are asserted here
//! against an instance produced by the ordinary start path, not by hand-built
//! rows.

mod support;

use donat_server::processes::StartConsumption;
use donat_server::processes::diagnostics;
use serde_json::json;
use support::TestDatabase;
use tokio_postgres::NoTls;
use uuid::Uuid;

fn process_metadata() -> donat_metadata::Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": []
        }],
        "processes": [{
            "name": "checkout",
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [{ "name": "order_id", "type": "uuid!" }],
            "output": [{ "name": "status", "type": "string!" }],
            "start_at": "done",
            "states": [{
                "id": "done",
                "output": { "values": { "status": { "literal": "ready" } } }
            }]
        }]
    }))
    .expect("minimal Process metadata deserializes")
}

async fn started_instance(database: &TestDatabase) -> Uuid {
    let metadata = process_metadata();
    let (runtime, revision) = database.runtime(&metadata, "checkout").await;
    let request_id = database
        .seed_start(
            "checkout",
            &revision,
            json!({ "order_id": "550e8400-e29b-41d4-a716-446655440110" }),
            "diagnostics-1",
        )
        .await;
    match runtime
        .consume_one_start()
        .await
        .expect("the Process start consumes")
    {
        StartConsumption::Started {
            request_id: consumed,
            instance_id,
        } => {
            assert_eq!(consumed, request_id);
            instance_id
        }
        other => panic!("expected a new Process instance, got {other:?}"),
    }
}

#[tokio::test]
async fn inspect_reads_the_journal_of_a_real_instance() {
    let database = TestDatabase::create("process_inspect").await;
    let instance = started_instance(&database).await;

    let history = diagnostics::inspect(&database.url, "default", instance)
        .await
        .expect("the instance is readable");

    assert_eq!(history["instance"], json!(instance));
    assert_eq!(history["source"], "default");
    assert_eq!(history["process"], "checkout");
    assert_eq!(
        history["input"]["order_id"],
        "550e8400-e29b-41d4-a716-446655440110"
    );
    // The three journals are present, so an operator can see what the instance
    // is waiting for rather than only what it says it is.
    assert!(history["events"].is_array());
    assert!(history["activities"].is_array());
    assert!(history["transitions"].is_array());

    // A journal the engine itself wrote must be consistent.
    assert_eq!(diagnostics::verify(&history), Vec::new());

    database.drop().await;
}

#[tokio::test]
async fn inspect_reports_an_instance_that_is_not_there() {
    let database = TestDatabase::create("process_inspect_missing").await;
    let error = diagnostics::inspect(&database.url, "default", Uuid::nil())
        .await
        .expect_err("an unknown instance is an error, not an empty document");
    assert!(
        error.to_string().contains("no Process instance"),
        "unexpected error: {error}"
    );
    database.drop().await;
}

/// The case the command exists for: a journal changed out of band — a restored
/// backup, a manual `UPDATE`, a partial write — so the instance's state no
/// longer matches the transitions that supposedly produced it.
#[tokio::test]
async fn verify_history_notices_a_state_nothing_transitioned_to() {
    let database = TestDatabase::create("process_verify_history").await;
    let instance = started_instance(&database).await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("the test database is available");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "UPDATE donat.process_instances SET current_state = 'somewhere_else' \
             WHERE source_name = 'default' AND id = $1",
            &[&instance],
        )
        .await
        .expect("the journal is edited out of band");
    connection.abort();

    let history = diagnostics::inspect(&database.url, "default", instance)
        .await
        .expect("the instance is readable");
    let findings = diagnostics::verify(&history);
    assert!(
        findings
            .iter()
            .any(|finding| finding.code == "state-mismatch"),
        "expected a state mismatch, got {findings:?}"
    );

    database.drop().await;
}
