use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_catalog::Catalog;
use donat_metadata::Metadata;
use donat_server::connectors::{
    ConnectorErrorClass, ConnectorFailure, ConnectorRegistry, ConnectorSuccess,
};
use donat_server::migrate::run_migrate;
use donat_server::processes::{
    ActivityConsumption, ProcessActivityExecutor, ProcessPlanningSnapshot, ProcessRuntime,
    StartConsumption, TransitionConsumption, build_process_runtime,
    build_process_runtime_with_activity_executor, reconcile, validate_serving_catalogs,
};
use donat_server::state::{SourceRuntime, compile_pure_engine_candidate};
use futures_util::future::BoxFuture;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio_postgres::NoTls;
use uuid::Uuid;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440301";

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

struct TestDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl TestDatabase {
    async fn create(label: &str) -> Self {
        let admin_url = postgres_admin_url();
        let name = format!(
            "donat_{label}_{}_{}",
            std::process::id(),
            NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
        );
        let (client, connection) = tokio_postgres::connect(&admin_url, NoTls)
            .await
            .expect("Postgres admin database is available");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE);"))
            .await
            .expect("stale Process fan-out database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("Process fan-out database creates");
        connection.abort();
        let prefix = admin_url
            .rsplit_once('/')
            .expect("Postgres URL has a database segment")
            .0
            .to_owned();
        let database = Self {
            admin_url,
            name: name.clone(),
            url: format!("{prefix}/{name}"),
        };
        run_migrate(&database.url, &migrations_dir())
            .await
            .expect("bundled migrations apply");
        let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
            .await
            .expect("Process fan-out database is available");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(
                "
                CREATE TABLE public.process_fanout_ledger (
                    item_id text PRIMARY KEY,
                    request_id uuid NOT NULL UNIQUE,
                    status text NOT NULL
                );
                ",
            )
            .await
            .expect("Process fan-out domain table creates");
        connection.abort();
        database
    }

    async fn catalog(&self) -> Catalog {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls)
            .await
            .expect("Process fan-out database is available for introspection");
        let connection = tokio::spawn(connection);
        let catalog = donat_catalog::introspect(&client)
            .await
            .expect("Process fan-out catalog introspects");
        connection.abort();
        catalog
    }

    async fn drop(self) {
        let (client, connection) = tokio_postgres::connect(&self.admin_url, NoTls)
            .await
            .expect("Postgres admin database is available for cleanup");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE);", self.name))
            .await
            .expect("Process fan-out database drops");
        connection.abort();
    }
}

fn base_metadata() -> Json {
    json!({
        "version": 3,
        "rules": {
            "types": [
                {
                    "name": "FanoutItem",
                    "object": {
                        "id": "string!",
                        "value": "int!"
                    }
                }
            ]
        },
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            },
            "tables": [{
                "table": { "schema": "public", "name": "process_fanout_ledger" },
                "command_select_permissions": [{
                    "role": "worker",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "command_insert_permissions": [{
                    "role": "worker",
                    "permission": { "columns": "*", "check": {} }
                }]
            }]
        }],
        "commands": [{
            "name": "record_fanout_item",
            "source": "default",
            "permissions": [{ "role": "worker" }],
            "arguments": [
                { "name": "item_id", "type": "string!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "steps": [{
                "name": "record",
                "insert": {
                    "table": { "schema": "public", "name": "process_fanout_ledger" },
                    "object": {
                        "item_id": { "arg": "item_id" },
                        "request_id": { "arg": "request_id" },
                        "status": { "literal": "recorded" }
                    },
                    "returning": ["item_id", "request_id", "status"]
                }
            }],
            "result": {
                "item_id": { "step": "record", "column": "item_id" },
                "request_id": { "step": "record", "column": "request_id" },
                "status": { "step": "record", "column": "status" }
            },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command",
                "retention": "1d"
            }
        }],
        "connectors": [{
            "name": "dispatcher",
            "module": "http",
            "config": {
                "endpoint_identity": "process-fanout-dispatcher-v1",
                "credential_identity": "process-fanout-credential",
                "base_url": "https://dispatcher.example.test"
            },
            "operations": [{
                "name": "send",
                "version": "1.0.0",
                "method": "POST",
                "path": "/send",
                "input_contract": { "item_id": "string!" },
                "body": { "item_id": { "input": "item_id" } },
                "success_statuses": [200],
                "response": {
                    "provider_id": {
                        "json_pointer": "/provider_id",
                        "type": "string!",
                        "max_bytes": 128
                    },
                    "status": {
                        "json_pointer": "/status",
                        "type": "string!",
                        "max_bytes": 64
                    }
                },
                "effect": "read_only",
                "bounds": {
                    "deadline_ms": 2000,
                    "maximum_calls": 1,
                    "maximum_pages": 1,
                    "maximum_items": 1,
                    "maximum_aggregate_request_bytes": 1024,
                    "maximum_aggregate_response_bytes": 1024,
                    "maximum_output_canonical_bytes": 1024,
                    "maximum_redirects": 0,
                    "maximum_json_depth": 4,
                    "maximum_json_nodes": 16
                },
                "error_map": {
                    "rules": [],
                    "fallback": {
                        "class": "permanent",
                        "code": "provider_error"
                    }
                },
                "capacity": {
                    "max_in_flight": 8,
                    "rate_limit": { "permits": 20, "per": "1s", "burst": 8 },
                    "serialize_by": { "input": "item_id" }
                }
            }]
        }]
    })
}

fn request_fanout_metadata() -> Metadata {
    let mut document = base_metadata();
    document["processes"] = json!([{
        "name": "fanout_test",
        "kind": "process",
        "version": 1,
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "input": [
            { "name": "rows", "type": "[FanoutItem!]!" },
            { "name": "request_id", "type": "uuid!" }
        ],
        "output": [{ "name": "status", "type": "string!" }],
        "idempotency": {
            "key": { "input": "request_id" },
            "scope": []
        },
        "start_at": "dispatch",
        "states": [
            {
                "id": "dispatch",
                "for_each": {
                    "input": { "input": "rows" },
                    "item_key": "id",
                    "max_items": 4,
                    "max_concurrency": 2,
                    "completion": "collect",
                    "preserve_input": true,
                    "request": {
                        "connector": "dispatcher",
                        "operation": "send",
                        "input": {
                            "item_id": { "item": "id" }
                        },
                        "timeout": {
                            "schedule_to_start": "2s",
                            "start_to_close": "2s"
                        },
                        "retry": {
                            "retry_on": ["transport"],
                            "max_attempts": 1,
                            "initial_interval": "10ms",
                            "max_interval": "10ms",
                            "jitter": "deterministic_full"
                        },
                        "on_error": {
                            "routes": [{
                                "kinds": ["permanent"],
                                "next": "done"
                            }],
                            "fallback": { "next": "done" }
                        }
                    },
                    "next": "done"
                }
            },
            {
                "id": "done",
                "output": {
                    "values": {
                        "status": { "literal": "done" }
                    }
                }
            }
        ]
    }]);
    serde_json::from_value(document).expect("request fan-out metadata deserializes")
}

fn command_fanout_metadata() -> Metadata {
    let mut document = base_metadata();
    document["processes"] = json!([{
        "name": "fanout_test",
        "kind": "process",
        "version": 1,
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "input": [
            { "name": "rows", "type": "[FanoutItem!]!" },
            { "name": "request_id", "type": "uuid!" }
        ],
        "output": [{ "name": "status", "type": "string!" }],
        "idempotency": {
            "key": { "input": "request_id" },
            "scope": []
        },
        "start_at": "record",
        "states": [
            {
                "id": "record",
                "for_each": {
                    "input": { "input": "rows" },
                    "item_key": "id",
                    "max_items": 4,
                    "max_concurrency": 2,
                    "completion": "collect",
                    "command": {
                        "name": "record_fanout_item",
                        "run_as": "worker",
                        "arguments": {
                            "item_id": { "item": "id" },
                            "request_id": { "activity_key": "record", "as": "uuid" }
                        }
                    },
                    "next": "done"
                }
            },
            {
                "id": "done",
                "output": {
                    "values": {
                        "status": { "literal": "done" }
                    }
                }
            }
        ]
    }]);
    serde_json::from_value(document).expect("command fan-out metadata deserializes")
}

async fn runtime(
    database: &TestDatabase,
    metadata: &Metadata,
    activity_executor: Option<Arc<dyn ProcessActivityExecutor>>,
) -> (ProcessRuntime, String) {
    let catalog = database.catalog().await;
    let catalogs = HashMap::from([("default".to_owned(), catalog)]);
    let connectors =
        Arc::new(ConnectorRegistry::build(metadata).expect("Process fan-out connectors compile"));
    let candidate = compile_pure_engine_candidate(metadata, &catalogs, connectors.as_ref(), true)
        .expect("Process fan-out candidate compiles");
    let process = candidate
        .process_catalog
        .source("default")
        .and_then(|catalog| catalog.process("fanout_test"))
        .expect("Process fan-out definition compiles");
    let revision = process.revision_fingerprint.clone();
    reconcile(
        "default",
        &database.url,
        catalogs.get("default").expect("default catalog exists"),
        candidate
            .process_catalog
            .source("default")
            .expect("default Process catalog exists"),
    )
    .await
    .expect("Process fan-out definition reconciles");

    let source_runtime =
        SourceRuntime::postgres(&database.url).expect("Process source runtime constructs");
    let deployed = validate_serving_catalogs(
        &HashMap::from([("default".to_owned(), source_runtime.clone())]),
        metadata,
        candidate.rule_catalog(),
        candidate.process_catalog.as_ref(),
        candidate.command_catalog.as_ref(),
        connectors.as_ref(),
    )
    .await
    .expect("deployed Process fan-out catalog validates");
    let planning_snapshot = Arc::new(ProcessPlanningSnapshot::new(
        Arc::new(metadata.clone()),
        Arc::new(catalogs),
        candidate
            .compiled
            .clone()
            .expect("candidate has a compiled serving schema"),
        candidate.rule_catalog_handle(),
    ));
    let deployed = Arc::new(
        deployed
            .source("default")
            .expect("deployed default Process catalog exists")
            .clone(),
    );
    let runtime = match activity_executor {
        Some(activity_executor) => build_process_runtime_with_activity_executor(
            "default",
            &source_runtime,
            deployed,
            planning_snapshot,
            candidate.command_catalog,
            candidate.finalized_command_catalog,
            connectors,
            activity_executor,
        ),
        None => build_process_runtime(
            "default",
            &source_runtime,
            deployed,
            planning_snapshot,
            candidate.command_catalog,
            candidate.finalized_command_catalog,
            connectors,
        ),
    }
    .expect("Process fan-out runtime builds");
    (runtime, revision)
}

async fn seed_start(database_url: &str, revision: &str, rows: Json) -> Uuid {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process fan-out database is available");
    let connection = tokio::spawn(connection);
    let request_id = client
        .query_one(
            "
            INSERT INTO donat.process_start_requests (
                source_name,
                process_name,
                revision,
                input_json,
                command_invocation_id,
                effect_position,
                idempotency_key,
                status
            )
            VALUES (
                'default',
                'fanout_test',
                $1,
                $2,
                gen_random_uuid(),
                0,
                $3,
                'pending'
            )
            RETURNING id
            ",
            &[
                &revision,
                &json!({ "rows": rows, "request_id": REQUEST_ID }),
                &REQUEST_ID,
            ],
        )
        .await
        .expect("Process fan-out start request inserts")
        .get(0);
    connection.abort();
    request_id
}

async fn start_instance(runtime: &ProcessRuntime) -> Uuid {
    match runtime
        .consume_one_start()
        .await
        .expect("Process fan-out start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    }
}

struct FanoutExecutor;

fn canonical_json_sha256(input: &Json) -> String {
    fn canonical(value: &Json) -> Json {
        match value {
            Json::Object(object) => Json::Object(
                object
                    .iter()
                    .map(|(name, value)| (name.clone(), canonical(value)))
                    .collect(),
            ),
            Json::Array(values) => Json::Array(values.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }

    let bytes = serde_json::to_vec(&canonical(input)).expect("test input serializes");
    format!("{:x}", Sha256::digest(bytes))
}

impl ProcessActivityExecutor for FanoutExecutor {
    fn execute<'a>(
        &'a self,
        instance: &'a str,
        operation: &'a str,
        input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            assert_eq!(instance, "dispatcher");
            assert_eq!(operation, "send");
            let item_id = input
                .get("item_id")
                .and_then(Json::as_str)
                .expect("fan-out input has a string item ID");
            if item_id == "b" {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Permanent,
                    "provider_rejected",
                    "provider rejected the item",
                ));
            }
            Ok(ConnectorSuccess {
                output: json!({
                    "provider_id": format!("provider-{item_id}"),
                    "status": "sent"
                }),
                request_fingerprint: canonical_json_sha256(&input),
            })
        })
    }
}

#[tokio::test]
async fn request_fanout_is_durable_bounded_and_collects_partial_results_in_input_order() {
    let database = TestDatabase::create("process_fanout_request").await;
    let metadata = request_fanout_metadata();
    let (runtime, revision) = runtime(&database, &metadata, Some(Arc::new(FanoutExecutor))).await;
    seed_start(
        &database.url,
        &revision,
        json!([
            { "id": "a", "value": 1 },
            { "id": "b", "value": 2 },
            { "id": "c", "value": 3 }
        ]),
    )
    .await;
    let instance_id = start_instance(&runtime).await;

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("fan-out expands"),
        TransitionConsumption::FanOutExpanded {
            instance_id: expanded,
            item_count: 3,
            scheduled_count: 2,
            ..
        } if expanded == instance_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("expanded Process fan-out is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                count(*) AS item_count,
                count(*) FILTER (WHERE status = 'scheduled') AS scheduled_count,
                (SELECT count(*)
                 FROM donat.process_activity_jobs job
                 WHERE job.source_name = 'default'
                   AND job.instance_id = $1) AS activity_count
            FROM donat.process_fanout_items
            WHERE source_name = 'default'
              AND instance_id = $1
              AND state_name = 'dispatch'
            ",
            &[&instance_id],
        )
        .await
        .expect("fan-out expansion persists one bounded item set");
    assert_eq!(row.get::<_, i64>("item_count"), 3);
    assert_eq!(row.get::<_, i64>("scheduled_count"), 2);
    assert_eq!(
        row.get::<_, i64>("activity_count"),
        3,
        "all bounded descriptors are durable, but only two items are active"
    );

    for completed in 0..3 {
        assert!(matches!(
            runtime
                .consume_one_activity()
                .await
                .expect("one fan-out activity executes"),
            ActivityConsumption::Succeeded { .. } | ActivityConsumption::Failed { .. }
        ));
        let transition = runtime
            .consume_one_transition()
            .await
            .expect("one fan-out completion commits");
        if completed < 2 {
            assert!(matches!(
                transition,
                TransitionConsumption::FanOutItemCompleted {
                    instance_id: completed_instance,
                    ..
                } if completed_instance == instance_id
            ));
        } else {
            assert!(matches!(
                transition,
                TransitionConsumption::Advanced {
                    instance_id: completed_instance,
                    ref from_state,
                    ref to_state,
                    ..
                } if completed_instance == instance_id
                    && from_state == "dispatch"
                    && to_state == "done"
            ));
        }
    }

    let row = client
        .query_one(
            "
            SELECT
                state_json -> 'dispatch' AS fanout_output,
                current_state,
                version,
                (SELECT max(active)
                 FROM (
                    SELECT count(*) AS active
                    FROM donat.process_fanout_items item
                    WHERE item.source_name = instance.source_name
                      AND item.instance_id = instance.id
                      AND item.status = 'scheduled'
                 ) samples) AS scheduled_count
            FROM donat.process_instances instance
            WHERE source_name = 'default'
              AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("completed fan-out output is inspectable");
    let output: Json = row.get("fanout_output");
    assert_eq!(
        output["successful_items"],
        json!([
            {
                "id": "a",
                "value": 1,
                "provider_id": "provider-a",
                "status": "sent"
            },
            {
                "id": "c",
                "value": 3,
                "provider_id": "provider-c",
                "status": "sent"
            }
        ])
    );
    assert_eq!(
        output["ordered_results"],
        json!([
            { "provider_id": "provider-a", "status": "sent" },
            { "provider_id": "provider-c", "status": "sent" }
        ])
    );
    assert_eq!(output["failed_items"].as_array().map(Vec::len), Some(1));
    assert_eq!(output["failed_items"][0]["id"], "b");
    assert_eq!(output["failed_items"][0]["value"], 2);
    assert_eq!(output["failed_items"][0]["item_key"], "b");
    assert_eq!(output["failed_items"][0]["stage"], "request");
    assert_eq!(output["failed_items"][0]["code"], "provider_rejected");
    assert_eq!(
        output["failed_items"][0]["safe_message"],
        "provider rejected the item"
    );
    assert_eq!(output["failed_items"][0]["requires_reconciliation"], true);
    assert!(
        output["failed_items"][0]["activity_key"]
            .as_str()
            .is_some_and(|value| value.starts_with("fanout:v1:") && value.contains(":dispatch:"))
    );
    assert_eq!(row.get::<_, String>("current_state"), "done");
    assert_eq!(row.get::<_, i64>("version"), 2);

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("terminal output consumes"),
        TransitionConsumption::Completed {
            instance_id: completed,
            ..
        } if completed == instance_id
    ));
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn command_fanout_uses_distinct_item_keys_and_survives_competing_workers() {
    let database = TestDatabase::create("process_fanout_command").await;
    let metadata = command_fanout_metadata();
    let (runtime, revision) = runtime(&database, &metadata, None).await;
    seed_start(
        &database.url,
        &revision,
        json!([
            { "id": "a", "value": 1 },
            { "id": "b", "value": 2 },
            { "id": "c", "value": 3 }
        ]),
    )
    .await;
    let instance_id = start_instance(&runtime).await;
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("command fan-out expands"),
        TransitionConsumption::FanOutExpanded {
            instance_id: expanded,
            item_count: 3,
            scheduled_count: 2,
            ..
        } if expanded == instance_id
    ));

    let runtime = Arc::new(runtime);
    for _ in 0..3 {
        let (left, right) = tokio::join!(
            runtime.consume_one_transition(),
            runtime.consume_one_transition()
        );
        left.expect("first command fan-out worker succeeds");
        right.expect("second command fan-out worker succeeds");
    }

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("command fan-out results are inspectable");
    let connection = tokio::spawn(connection);
    let rows = client
        .query(
            "
            SELECT item_id, request_id, status
            FROM public.process_fanout_ledger
            ORDER BY item_id
            ",
            &[],
        )
        .await
        .expect("command fan-out domain rows read");
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows.iter()
            .map(|row| row.get::<_, String>("item_id"))
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    let request_ids = rows
        .iter()
        .map(|row| row.get::<_, Uuid>("request_id"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        request_ids.len(),
        3,
        "activity_key inside for_each includes the stable item identity"
    );
    assert!(
        rows.iter()
            .all(|row| row.get::<_, String>("status") == "recorded")
    );
    let instance = client
        .query_one(
            "
            SELECT current_state, state_json -> 'record'
            FROM donat.process_instances
            WHERE source_name = 'default'
              AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("command fan-out instance reads");
    assert_eq!(instance.get::<_, String>(0), "done");
    let output: Json = instance.get(1);
    assert_eq!(
        output["ordered_results"]
            .as_array()
            .expect("ordered results are a list")
            .iter()
            .map(|item| item["item_id"].as_str().expect("item ID"))
            .collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(output["failed_items"], json!([]));
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn duplicate_fanout_item_keys_fail_closed_without_scheduling_work() {
    let database = TestDatabase::create("process_fanout_duplicate_key").await;
    let metadata = request_fanout_metadata();
    let (runtime, revision) = runtime(&database, &metadata, Some(Arc::new(FanoutExecutor))).await;
    seed_start(
        &database.url,
        &revision,
        json!([
            { "id": "duplicate", "value": 1 },
            { "id": "duplicate", "value": 2 }
        ]),
    )
    .await;
    let instance_id = start_instance(&runtime).await;
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("duplicate fan-out keys produce a durable failure"),
        TransitionConsumption::Failed {
            instance_id: failed,
            ref code,
            ..
        } if failed == instance_id && code == "fanout_item_key_duplicate"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("failed Process fan-out is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                status,
                failure_json ->> 'code',
                (SELECT count(*)
                 FROM donat.process_activity_jobs job
                 WHERE job.source_name = instance.source_name
                   AND job.instance_id = instance.id),
                (SELECT count(*)
                 FROM donat.process_fanout_items item
                 WHERE item.source_name = instance.source_name
                   AND item.instance_id = instance.id)
            FROM donat.process_instances instance
            WHERE source_name = 'default'
              AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("failed fan-out instance reads");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, String>(1), "fanout_item_key_duplicate");
    assert_eq!(row.get::<_, i64>(2), 0);
    assert_eq!(row.get::<_, i64>(3), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn fanout_input_above_declared_maximum_fails_before_expansion() {
    let database = TestDatabase::create("process_fanout_max_items").await;
    let metadata = request_fanout_metadata();
    let (runtime, revision) = runtime(&database, &metadata, Some(Arc::new(FanoutExecutor))).await;
    seed_start(
        &database.url,
        &revision,
        json!([
            { "id": "a", "value": 1 },
            { "id": "b", "value": 2 },
            { "id": "c", "value": 3 },
            { "id": "d", "value": 4 },
            { "id": "e", "value": 5 }
        ]),
    )
    .await;
    let instance_id = start_instance(&runtime).await;
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("oversized fan-out input produces a durable failure"),
        TransitionConsumption::Failed {
            instance_id: failed,
            ref code,
            ..
        } if failed == instance_id && code == "fanout_max_items_exceeded"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("oversized Process fan-out is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                status,
                failure_json ->> 'code',
                (SELECT count(*)
                 FROM donat.process_activity_jobs job
                 WHERE job.source_name = instance.source_name
                   AND job.instance_id = instance.id),
                (SELECT count(*)
                 FROM donat.process_fanout_items item
                 WHERE item.source_name = instance.source_name
                   AND item.instance_id = instance.id)
            FROM donat.process_instances instance
            WHERE source_name = 'default'
              AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("oversized fan-out instance reads");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, String>(1), "fanout_max_items_exceeded");
    assert_eq!(row.get::<_, i64>(2), 0);
    assert_eq!(row.get::<_, i64>(3), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn empty_fanout_collects_empty_output_without_creating_item_work() {
    let database = TestDatabase::create("process_fanout_empty").await;
    let metadata = request_fanout_metadata();
    let (runtime, revision) = runtime(&database, &metadata, Some(Arc::new(FanoutExecutor))).await;
    seed_start(&database.url, &revision, json!([])).await;
    let instance_id = start_instance(&runtime).await;
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("empty fan-out advances immediately"),
        TransitionConsumption::Advanced {
            instance_id: advanced,
            ref from_state,
            ref to_state,
            ..
        } if advanced == instance_id && from_state == "dispatch" && to_state == "done"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("empty Process fan-out is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                state_json -> 'dispatch',
                (SELECT count(*)
                 FROM donat.process_activity_jobs job
                 WHERE job.source_name = instance.source_name
                   AND job.instance_id = instance.id),
                (SELECT count(*)
                 FROM donat.process_fanout_items item
                 WHERE item.source_name = instance.source_name
                   AND item.instance_id = instance.id)
            FROM donat.process_instances instance
            WHERE source_name = 'default'
              AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("empty fan-out instance reads");
    assert_eq!(
        row.get::<_, Json>(0),
        json!({
            "successful_items": [],
            "failed_items": [],
            "ordered_results": []
        })
    );
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 0);
    connection.abort();
    database.drop().await;
}

/// The environment variable the at-most-once fan-out fixture resolves its
/// credential from. Its value is a sentinel: the executor here is a stub.
const AT_MOST_ONCE_TOKEN: &str = "DONAT_TEST_PROCESS_FANOUT_SLACK_TOKEN";

/// A fan-out whose per-item write publishes no idempotency mechanism, so each
/// item is one at-most-once send with its own ambiguous destination (ADR 063).
fn at_most_once_fanout_metadata() -> Metadata {
    // Safety: the connector registry reads this variable on the same thread
    // that sets it, before any listener or worker exists.
    unsafe { std::env::set_var(AT_MOST_ONCE_TOKEN, "xoxb-process-fanout-sentinel") };
    let mut document = base_metadata();
    document["connectors"] = json!([{
        "name": "chat",
        "module": "slack",
        "config": {
            "endpoint_identity": "process-fanout-slack-v1",
            "credential_identity": "process-fanout-slack-credential",
            "secret_key": { "value_from_env": AT_MOST_ONCE_TOKEN }
        },
        "operations": [{
            "name": "message.post",
            "capacity": {
                "max_in_flight": 4,
                "rate_limit": { "permits": 10, "per": "1s", "burst": 4 }
            }
        }]
    }]);
    document["processes"] = json!([{
        "name": "fanout_test",
        "kind": "process",
        "version": 1,
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "input": [
            { "name": "rows", "type": "[FanoutItem!]!" },
            { "name": "request_id", "type": "uuid!" }
        ],
        "output": [{ "name": "status", "type": "string!" }],
        "idempotency": {
            "key": { "input": "request_id" },
            "scope": []
        },
        "start_at": "dispatch",
        "states": [
            {
                "id": "dispatch",
                "for_each": {
                    "input": { "input": "rows" },
                    "item_key": "id",
                    "max_items": 4,
                    "max_concurrency": 2,
                    "completion": "collect",
                    "preserve_input": true,
                    "request": {
                        "connector": "chat",
                        "operation": "message.post",
                        "input": {
                            "channel": { "item": "id" },
                            "text": { "literal": "one send" }
                        },
                        "at_most_once": true,
                        "on_ambiguous": "unknown",
                        "timeout": {
                            "schedule_to_start": "2s",
                            "start_to_close": "6s"
                        },
                        "retry": {
                            "retry_on": [],
                            "max_attempts": 1,
                            "initial_interval": "10ms",
                            "max_interval": "10ms",
                            "jitter": "deterministic_full"
                        },
                        "on_error": {
                            "routes": [{ "kinds": ["permanent"], "next": "refused" }],
                            "fallback": { "next": "refused" }
                        }
                    },
                    "next": "done"
                }
            },
            {
                "id": "done",
                "output": { "values": { "status": { "literal": "done" } } }
            },
            {
                "id": "refused",
                "output": { "values": { "status": { "literal": "refused" } } }
            },
            {
                "id": "unknown",
                "output": { "values": { "status": { "literal": "unknown" } } }
            }
        ]
    }]);
    serde_json::from_value(document).expect("at-most-once fan-out metadata deserializes")
}

/// Item `a` is refused by the provider; item `b`'s answer never comes back.
struct AtMostOnceFanoutExecutor;

impl ProcessActivityExecutor for AtMostOnceFanoutExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            let channel = input
                .get("channel")
                .and_then(Json::as_str)
                .expect("the fan-out item binds a channel");
            if channel == "a" {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Permanent,
                    "provider_rejected",
                    "the provider refused this message",
                ));
            }
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Timeout,
                "connector_timeout",
                "the answer to this send never came back",
            ))
        })
    }
}

/// ADR 063 across a fan-out. One item's send is one logical activity, so one
/// item can end unknown while another was refused outright — and the aggregate
/// has one destination. An unknown outcome must not be absorbed by a sibling's
/// error route: `on_error` routes failures, and no failure route may claim the
/// absence of knowledge, whichever ordinal it happened to arrive on.
#[tokio::test]
async fn an_ambiguous_fanout_item_is_not_absorbed_by_an_earlier_items_error_route() {
    let database = TestDatabase::create("process_fanout_at_most_once").await;
    let metadata = at_most_once_fanout_metadata();
    let (runtime, revision) = runtime(
        &database,
        &metadata,
        Some(Arc::new(AtMostOnceFanoutExecutor)),
    )
    .await;
    seed_start(
        &database.url,
        &revision,
        json!([
            { "id": "a", "value": 1 },
            { "id": "b", "value": 2 }
        ]),
    )
    .await;
    let instance_id = start_instance(&runtime).await;
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("fan-out expands"),
        TransitionConsumption::FanOutExpanded { item_count: 2, .. }
    ));
    for _ in 0..2 {
        assert!(matches!(
            runtime
                .consume_one_activity()
                .await
                .expect("one fan-out activity executes"),
            ActivityConsumption::Failed { .. }
        ));
        runtime
            .consume_one_transition()
            .await
            .expect("one fan-out completion commits");
    }

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("the collected fan-out is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT current_state, state_json -> 'dispatch' AS fanout_output
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("the collected fan-out output reads");
    let output: Json = row.get("fanout_output");
    assert_eq!(
        output["failed_items"]
            .as_array()
            .map(|items| items.len())
            .unwrap_or_default(),
        2,
        "both items are collected whichever route the aggregate takes"
    );
    assert_eq!(output["failed_items"][0]["code"], "provider_rejected");
    assert_eq!(
        output["failed_items"][1]["code"], "provider_send_ambiguous",
        "the second item's send was authorized and its outcome is unknown"
    );
    assert_eq!(
        row.get::<_, String>("current_state"),
        "unknown",
        "an unknown outcome takes the ambiguous route even when a sibling failed first"
    );
    connection.abort();
    database.drop().await;
}
