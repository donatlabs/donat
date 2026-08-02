use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_catalog::Catalog;
use donat_metadata::Metadata;
use donat_server::connectors::ConnectorRegistry;
use donat_server::migrate::run_migrate;
use donat_server::processes::{
    ProcessPlanningSnapshot, ProcessRuntime, StartConsumption, TransitionConsumption,
    build_process_runtime, reconcile, validate_serving_catalogs,
};
use donat_server::state::{SourceRuntime, compile_pure_engine_candidate};
use serde_json::{Value as Json, json};
use tokio_postgres::NoTls;
use uuid::Uuid;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

const ENTITY_ID: &str = "550e8400-e29b-41d4-a716-446655440201";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440202";

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
            .expect("stale Process transition database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("Process transition database creates");
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
            .expect("Process transition database is available");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(
                "
                CREATE TABLE public.process_test_ledger (
                    id uuid PRIMARY KEY,
                    entity_id uuid NOT NULL UNIQUE,
                    customer_id uuid,
                    status text NOT NULL
                );
                ",
            )
            .await
            .expect("Process transition domain table creates");
        connection.abort();
        database
    }

    async fn catalog(&self) -> Catalog {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls)
            .await
            .expect("Process transition database is available for introspection");
        let connection = tokio::spawn(connection);
        let catalog = donat_catalog::introspect(&client)
            .await
            .expect("Process transition catalog introspects");
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
            .expect("Process transition database drops");
        connection.abort();
    }
}

fn transition_metadata(reject_after_insert: bool) -> Metadata {
    let steps = if reject_after_insert {
        json!([
            {
                "name": "domain_write",
                "insert": {
                    "table": { "schema": "public", "name": "process_test_ledger" },
                    "object": {
                        "id": { "arg": "request_id" },
                        "entity_id": { "arg": "entity_id" },
                        "status": { "literal": "written" }
                    },
                    "returning": ["id", "entity_id", "status"]
                }
            },
            {
                "name": "later_rejection",
                "assert": {
                    "rule": "always_false",
                    "with": {},
                    "message": "the later assertion rejected"
                }
            }
        ])
    } else {
        json!([
            {
                "name": "domain_write",
                "insert": {
                    "table": { "schema": "public", "name": "process_test_ledger" },
                    "object": {
                        "id": { "arg": "request_id" },
                        "entity_id": { "arg": "entity_id" },
                        "status": { "literal": "written" }
                    },
                    "returning": ["id", "entity_id", "status"]
                }
            }
        ])
    };
    serde_json::from_value(json!({
        "version": 3,
        "rules": {
            "rules": [{
                "name": "always_false",
                "result": "bool!",
                "expression": "false"
            }]
        },
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            },
            "tables": [{
                "table": { "schema": "public", "name": "process_test_ledger" },
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
            "name": "record_process_step",
            "source": "default",
            "permissions": [{ "role": "worker" }],
            "arguments": [
                { "name": "entity_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "steps": steps,
            "result": {
                "record_id": { "step": "domain_write", "column": "id" },
                "entity_id": { "step": "domain_write", "column": "entity_id" },
                "status": { "step": "domain_write", "column": "status" }
            },
            "idempotency": {
                "key": { "argument": "request_id" },
                "scope": "command",
                "retention": "1d"
            }
        }],
        "processes": [{
            "name": "transition_test",
            "kind": "process",
            "version": 1,
            "source": "default",
            "permissions": [{ "role": "customer" }],
            "input": [
                { "name": "entity_id", "type": "uuid!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "output": [{ "name": "status", "type": "string!" }],
            "start_at": "record",
            "states": [
                {
                    "id": "record",
                    "command": {
                        "name": "record_process_step",
                        "run_as": "worker",
                        "arguments": {
                            "entity_id": { "input": "entity_id" },
                            "request_id": { "input": "request_id" }
                        },
                        "next": "done"
                    }
                },
                {
                    "id": "done",
                    "output": {
                        "values": {
                            "status": { "state": "record", "field": "status" }
                        }
                    }
                }
            ]
        }]
    }))
    .expect("Process transition metadata deserializes")
}

fn routed_transition_metadata() -> Metadata {
    let mut document =
        serde_json::to_value(transition_metadata(false)).expect("transition metadata serializes");
    document["rules"]["rules"]
        .as_array_mut()
        .expect("rules is an array")
        .push(json!({
            "name": "status_is_written",
            "parameters": { "status": "string!" },
            "result": "bool!",
            "expression": "status == 'written'"
        }));
    document["rules"]["decision_tables"] = json!([{
        "name": "written_status_route",
        "inputs": { "status": "string!" },
        "output": { "route": "string!" },
        "hit_policy": "first",
        "rows": [
            {
                "id": "written",
                "when": { "status": "status == 'written'" },
                "output": { "route": "complete" }
            },
            {
                "id": "fallback",
                "when": { "status": "true" },
                "output": { "route": "fail" }
            }
        ],
        "test_cases": [{
            "name": "written status completes",
            "input": { "status": "written" },
            "expect": {
                "output": { "route": "complete" },
                "matched_row_id": "written"
            }
        }]
    }]);
    document["processes"][0]["states"] = json!([
        {
            "id": "record",
            "command": {
                "name": "record_process_step",
                "run_as": "worker",
                "arguments": {
                    "entity_id": { "input": "entity_id" },
                    "request_id": { "input": "request_id" }
                },
                "next": "route_rule"
            }
        },
        {
            "id": "route_rule",
            "when": {
                "cases": [
                    {
                        "rule": "always_false",
                        "with": {},
                        "next": "unexpected"
                    },
                    {
                        "rule": "status_is_written",
                        "with": {
                            "status": { "state": "record", "field": "status" }
                        },
                        "next": "route_decision"
                    }
                ],
                "default": "unexpected"
            }
        },
        {
            "id": "route_decision",
            "when": {
                "decision_table": "written_status_route",
                "input": {
                    "status": { "state": "record", "field": "status" }
                },
                "cases": [{
                    "matches": { "route": "complete" },
                    "next": "done"
                }],
                "default": "unexpected"
            }
        },
        {
            "id": "done",
            "output": {
                "values": {
                    "status": { "state": "record", "field": "status" }
                }
            }
        },
        {
            "id": "unexpected",
            "fail": {
                "code": "unexpected_route",
                "message": "the deterministic route was not recognized"
            }
        }
    ]);
    serde_json::from_value(document).expect("routed Process transition metadata deserializes")
}

fn failed_transition_metadata() -> Metadata {
    let mut document =
        serde_json::to_value(transition_metadata(false)).expect("transition metadata serializes");
    document["processes"][0]["start_at"] = json!("stop");
    document["processes"][0]["states"] = json!([{
        "id": "stop",
        "fail": {
            "code": "manual_stop",
            "message": "the process was stopped deliberately"
        }
    }]);
    serde_json::from_value(document).expect("failed Process transition metadata deserializes")
}

fn caller_transition_metadata() -> Metadata {
    let mut document =
        serde_json::to_value(transition_metadata(false)).expect("transition metadata serializes");
    document["sources"][0]["tables"][0]["command_select_permissions"] = json!([{
        "role": "customer",
        "permission": {
            "columns": "*",
            "filter": {
                "customer_id": { "_eq": "X-Donat-User-Id" }
            }
        }
    }]);
    document["sources"][0]["tables"][0]["command_insert_permissions"] = json!([{
        "role": "customer",
        "permission": {
            "columns": "*",
            "check": {
                "customer_id": { "_eq": "X-Donat-User-Id" }
            }
        }
    }]);
    document["commands"][0]["permissions"] = json!([{ "role": "customer" }]);
    document["commands"][0]["steps"][0]["insert"]["object"]["customer_id"] =
        json!({ "session_variable": "x-donat-user-id" });
    document["processes"][0]["states"][0]["command"]["run_as"] = json!("caller");
    serde_json::from_value(document).expect("caller Process transition metadata deserializes")
}

async fn runtime(database: &TestDatabase, metadata: &Metadata) -> (ProcessRuntime, String) {
    let catalog = database.catalog().await;
    let catalogs = HashMap::from([("default".to_owned(), catalog)]);
    let connectors = Arc::new(ConnectorRegistry::empty());
    let candidate = compile_pure_engine_candidate(metadata, &catalogs, connectors.as_ref(), true)
        .expect("Process transition candidate compiles");
    let process = candidate
        .process_catalog
        .source("default")
        .and_then(|catalog| catalog.process("transition_test"))
        .expect("Process transition definition compiles");
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
    .expect("Process transition definition reconciles");

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
    .expect("deployed Process transition catalog validates");
    let planning_snapshot = Arc::new(ProcessPlanningSnapshot::new(
        Arc::new(metadata.clone()),
        Arc::new(catalogs),
        candidate
            .compiled
            .clone()
            .expect("candidate has a compiled serving schema"),
        candidate.rule_catalog_handle(),
    ));
    let runtime = build_process_runtime(
        "default",
        &source_runtime,
        Arc::new(
            deployed
                .source("default")
                .expect("deployed default Process catalog exists")
                .clone(),
        ),
        planning_snapshot,
        candidate.command_catalog,
        candidate.finalized_command_catalog,
        connectors,
    )
    .expect("Process transition runtime builds");
    (runtime, revision)
}

async fn seed_start(database_url: &str, revision: &str) -> Uuid {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process transition database is available");
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
                'transition_test',
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
                &json!({ "entity_id": ENTITY_ID, "request_id": REQUEST_ID }),
                &REQUEST_ID,
            ],
        )
        .await
        .expect("Process transition start request inserts")
        .get(0);
    connection.abort();
    request_id
}

async fn seed_caller_start(database_url: &str, revision: &str, caller_session: Json) -> Uuid {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process transition database is available");
    let connection = tokio::spawn(connection);
    let request_id = client
        .query_one(
            "
            INSERT INTO donat.process_start_requests (
                source_name,
                process_name,
                revision,
                input_json,
                caller_role,
                caller_session_json,
                command_invocation_id,
                effect_position,
                idempotency_key,
                status
            )
            VALUES (
                'default',
                'transition_test',
                $1,
                $2,
                'customer',
                $3,
                gen_random_uuid(),
                0,
                $4,
                'pending'
            )
            RETURNING id
            ",
            &[
                &revision,
                &json!({ "entity_id": ENTITY_ID, "request_id": REQUEST_ID }),
                &caller_session,
                &REQUEST_ID,
            ],
        )
        .await
        .expect("caller Process transition start request inserts")
        .get(0);
    connection.abort();
    request_id
}

#[tokio::test]
async fn process_command_transition_executes_once_and_advances_atomically() {
    let database = TestDatabase::create("process_transition_applied").await;
    let metadata = transition_metadata(false);
    let (runtime, revision) = runtime(&database, &metadata).await;
    let request_id = seed_start(&database.url, &revision).await;
    let started = runtime
        .consume_one_start()
        .await
        .expect("Process start consumes");
    let instance_id = match started {
        StartConsumption::Started {
            request_id: consumed,
            instance_id,
        } => {
            assert_eq!(consumed, request_id);
            instance_id
        }
        other => panic!("expected a new Process instance, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let (left, right) = tokio::join!(
        runtime.consume_one_transition(),
        runtime.consume_one_transition()
    );
    let outcomes = [
        left.expect("first transition consumer succeeds"),
        right.expect("second transition consumer succeeds"),
    ];
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| {
                matches!(
                    outcome,
                    TransitionConsumption::Advanced {
                        from_state,
                        to_state,
                        ..
                    } if from_state == "record" && to_state == "done"
                )
            })
            .count(),
        1,
        "the command event advances exactly once"
    );
    let completed_during_race = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, TransitionConsumption::Completed { .. }))
        .count();
    let stale_during_race = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, TransitionConsumption::NoWork))
        .count();
    assert_eq!(
        completed_during_race + stale_during_race,
        1,
        "the second legal linearization is stale or consumes the newly committed output token"
    );
    if completed_during_race == 0 {
        let completed = runtime
            .consume_one_transition()
            .await
            .expect("terminal output transition succeeds");
        assert!(
            matches!(
                completed,
                TransitionConsumption::Completed {
                    instance_id: completed_instance,
                    ..
                } if completed_instance == instance_id
            ),
            "the output state must terminalize the same instance, got {completed:?}"
        );
    }

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process transition database is inspectable");
    let connection = tokio::spawn(connection);
    let state = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                instance.version,
                instance.state_json,
                instance.terminal_output_json,
                (SELECT count(*) FROM public.process_test_ledger),
                (SELECT count(*) FROM donat.command_invocations
                 WHERE command_name = 'record_process_step'),
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND kind = 'start' AND status = 'consumed'),
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND kind = 'continue' AND status = 'consumed'),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND outcome = 'command_applied'),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND outcome = 'completed')
            FROM donat.process_instances instance
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("applied Process transition is durable");
    assert_eq!(state.get::<_, String>(0), "terminal");
    assert_eq!(state.get::<_, String>(1), "done");
    assert_eq!(state.get::<_, i64>(2), 2);
    assert_eq!(
        state.get::<_, Json>(3),
        json!({
            "record": {
                "record_id": REQUEST_ID,
                "entity_id": ENTITY_ID,
                "status": "written"
            },
            "done": { "status": "written" }
        })
    );
    assert_eq!(state.get::<_, Json>(4), json!({ "status": "written" }));
    for index in 5..11 {
        assert_eq!(state.get::<_, i64>(index), 1, "count column {index}");
    }
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn activity_event_cannot_execute_an_unrelated_deterministic_state() {
    // This catches widening the transition event query without retaining the
    // closed event-kind gate for Command/When/Output/Fail states.
    let database = TestDatabase::create("process_transition_event_kind_gate").await;
    let metadata = transition_metadata(false);
    let (runtime, revision) = runtime(&database, &metadata).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process event-kind gate is seedable");
    let connection = tokio::spawn(connection);
    let start_event_id: Uuid = client
        .query_one(
            "
            SELECT id
            FROM donat.process_events
            WHERE source_name = 'default'
              AND instance_id = $1
              AND kind = 'start'
              AND status = 'pending'
            ",
            &[&instance_id],
        )
        .await
        .expect("pending start token exists")
        .get(0);
    let unrelated_event_id: Uuid = client
        .query_one(
            "
            INSERT INTO donat.process_events (
                source_name,
                instance_id,
                process_name,
                revision,
                kind,
                payload_json,
                idempotency_key,
                available_at,
                status
            )
            VALUES (
                'default',
                $1,
                'transition_test',
                $2,
                'activity_succeeded',
                '{}'::jsonb,
                'unrelated-activity-success',
                statement_timestamp() - interval '1 hour',
                'pending'
            )
            RETURNING id
            ",
            &[&instance_id, &revision],
        )
        .await
        .expect("an older unrelated activity event inserts")
        .get(0);
    connection.abort();

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the legal start token executes the Command"),
        TransitionConsumption::Advanced {
            event_id,
            from_state,
            to_state,
            ..
        } if event_id == start_event_id
            && from_state == "record"
            && to_state == "done"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("event-kind outcome is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM public.process_test_ledger),
                (SELECT status
                 FROM donat.process_events
                 WHERE source_name = 'default' AND id = $1),
                (SELECT status
                 FROM donat.process_events
                 WHERE source_name = 'default' AND id = $2)
            ",
            &[&unrelated_event_id, &start_event_id],
        )
        .await
        .expect("only the legal transition token was consumed");
    assert_eq!(row.get::<_, i64>(0), 1);
    assert_eq!(row.get::<_, String>(1), "pending");
    assert_eq!(row.get::<_, String>(2), "consumed");
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn process_command_later_assert_rolls_back_savepoint_and_commits_rejection() {
    let database = TestDatabase::create("process_transition_rejected").await;
    let metadata = transition_metadata(true);
    let (runtime, revision) = runtime(&database, &metadata).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let rejection = runtime
        .consume_one_transition()
        .await
        .expect("business rejection is a committed Process outcome");
    let TransitionConsumption::CommandRejected { error, .. } = rejection else {
        panic!("expected a command rejection, got {rejection:?}");
    };
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "the later assertion rejected");
    assert!(
        error
            .path
            .starts_with("$.processes.transition_test.states.record")
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("rejected Process transition is inspectable");
    let connection = tokio::spawn(connection);
    let state = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                instance.version,
                instance.state_json,
                instance.failure_json,
                (SELECT count(*) FROM public.process_test_ledger),
                (SELECT count(*) FROM donat.command_invocations
                 WHERE command_name = 'record_process_step'),
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND kind = 'command_rejected' AND status = 'consumed'),
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND kind = 'continue'),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = instance.id
                   AND outcome = 'command_rejected')
            FROM donat.process_instances instance
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("rejected Process transition is durable");
    assert_eq!(state.get::<_, String>(0), "failed");
    assert_eq!(state.get::<_, String>(1), "record");
    assert_eq!(state.get::<_, i64>(2), 1);
    assert_eq!(state.get::<_, Json>(3), json!({}));
    let failure: Json = state.get(4);
    assert_eq!(failure["kind"], "command_rejected");
    assert_eq!(failure["code"], "validation-failed");
    for index in 5..7 {
        assert_eq!(
            state.get::<_, i64>(index),
            0,
            "savepoint-owned row count {index} must roll back"
        );
    }
    assert_eq!(state.get::<_, i64>(7), 1);
    assert_eq!(state.get::<_, i64>(8), 0);
    assert_eq!(state.get::<_, i64>(9), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn process_when_uses_ordered_rules_and_decision_tables_before_completion() {
    let database = TestDatabase::create("process_transition_when").await;
    let metadata = routed_transition_metadata();
    let (runtime, revision) = runtime(&database, &metadata).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };

    let transitions = [
        runtime.consume_one_transition().await,
        runtime.consume_one_transition().await,
        runtime.consume_one_transition().await,
        runtime.consume_one_transition().await,
    ]
    .map(|outcome| outcome.expect("deterministic Process transition succeeds"));
    assert!(matches!(
        &transitions[0],
        TransitionConsumption::Advanced {
            from_state,
            to_state,
            ..
        } if from_state == "record" && to_state == "route_rule"
    ));
    assert!(matches!(
        &transitions[1],
        TransitionConsumption::Advanced {
            from_state,
            to_state,
            ..
        } if from_state == "route_rule" && to_state == "route_decision"
    ));
    assert!(matches!(
        &transitions[2],
        TransitionConsumption::Advanced {
            from_state,
            to_state,
            ..
        } if from_state == "route_decision" && to_state == "done"
    ));
    assert!(matches!(
        &transitions[3],
        TransitionConsumption::Completed {
            instance_id: completed,
            ..
        } if *completed == instance_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("routed Process transition is inspectable");
    let connection = tokio::spawn(connection);
    let state = client
        .query_one(
            "
            SELECT
                status,
                version,
                state_json,
                terminal_output_json,
                (SELECT count(*) FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome = 'when_routed'),
                (SELECT count(*) FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.status = 'pending')
            FROM donat.process_instances instance
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("routed Process transition is durable");
    assert_eq!(state.get::<_, String>(0), "terminal");
    assert_eq!(state.get::<_, i64>(1), 4);
    assert_eq!(
        state.get::<_, Json>(2),
        json!({
            "record": {
                "record_id": REQUEST_ID,
                "entity_id": ENTITY_ID,
                "status": "written"
            },
            "route_rule": {},
            "route_decision": { "route": "complete" },
            "done": { "status": "written" }
        })
    );
    assert_eq!(state.get::<_, Json>(3), json!({ "status": "written" }));
    assert_eq!(state.get::<_, i64>(4), 2);
    assert_eq!(state.get::<_, i64>(5), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn explicit_fail_state_commits_a_safe_terminal_failure() {
    let database = TestDatabase::create("process_transition_fail").await;
    let metadata = failed_transition_metadata();
    let (runtime, revision) = runtime(&database, &metadata).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };

    let failed = runtime
        .consume_one_transition()
        .await
        .expect("explicit Process fail transition commits");
    assert!(matches!(
        failed,
        TransitionConsumption::Failed {
            instance_id: failed_instance,
            ref code,
            ..
        } if failed_instance == instance_id && code == "manual_stop"
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("failed Process transition is inspectable");
    let connection = tokio::spawn(connection);
    let state = client
        .query_one(
            "
            SELECT
                status,
                version,
                failure_json,
                (SELECT count(*) FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome = 'failed'),
                (SELECT count(*) FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.status = 'pending')
            FROM donat.process_instances instance
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("explicit Process failure is durable");
    assert_eq!(state.get::<_, String>(0), "failed");
    assert_eq!(state.get::<_, i64>(1), 1);
    assert_eq!(
        state.get::<_, Json>(2),
        json!({
            "kind": "process_failed",
            "code": "manual_stop",
            "message": "the process was stopped deliberately"
        })
    );
    assert_eq!(state.get::<_, i64>(3), 1);
    assert_eq!(state.get::<_, i64>(4), 0);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn caller_command_executes_with_only_the_persisted_closed_session() {
    let database = TestDatabase::create("process_transition_caller").await;
    let metadata = caller_transition_metadata();
    let (runtime, revision) = runtime(&database, &metadata).await;
    seed_caller_start(
        &database.url,
        &revision,
        json!({ "x-donat-user-id": ENTITY_ID }),
    )
    .await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("caller Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new caller Process instance, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("caller Process command executes"),
        TransitionConsumption::Advanced { .. }
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("caller Process output completes"),
        TransitionConsumption::Completed { .. }
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("caller Process transition is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.caller_role,
                instance.caller_session_json,
                ledger.customer_id
            FROM donat.process_instances instance
            JOIN public.process_test_ledger ledger
              ON ledger.entity_id = $2
            WHERE instance.source_name = 'default' AND instance.id = $1
            ",
            &[&instance_id, &Uuid::parse_str(ENTITY_ID).unwrap()],
        )
        .await
        .expect("caller context and domain write are durable");
    assert_eq!(row.get::<_, String>(0), "terminal");
    assert_eq!(row.get::<_, String>(1), "customer");
    assert_eq!(
        row.get::<_, Json>(2),
        json!({ "x-donat-user-id": ENTITY_ID })
    );
    assert_eq!(row.get::<_, Uuid>(3), Uuid::parse_str(ENTITY_ID).unwrap());
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn caller_command_rejects_a_persisted_session_outside_its_compiled_contract() {
    let database = TestDatabase::create("process_transition_caller_closed").await;
    let metadata = caller_transition_metadata();
    let (runtime, revision) = runtime(&database, &metadata).await;
    seed_caller_start(
        &database.url,
        &revision,
        json!({
            "x-donat-user-id": ENTITY_ID,
            "x-ambient-secret": "must-never-enter-a-process"
        }),
    )
    .await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("corrupt caller Process start remains structurally consumable")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new caller Process instance, got {other:?}"),
    };
    let error = runtime
        .consume_one_transition()
        .await
        .expect_err("an ambient persisted variable must fail closed");
    assert!(
        error
            .to_string()
            .contains("does not match its compiled closed contract"),
        "unexpected closed-session error: {error:#}"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("closed caller failure is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.version,
                (SELECT count(*) FROM public.process_test_ledger),
                (SELECT count(*) FROM donat.command_invocations
                 WHERE command_name = 'record_process_step'),
                (SELECT count(*) FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.status = 'pending')
            FROM donat.process_instances instance
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("closed caller failure leaves no partial transition");
    assert_eq!(row.get::<_, String>(0), "running");
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 0);
    assert_eq!(row.get::<_, i64>(3), 0);
    assert_eq!(row.get::<_, i64>(4), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn a_constraint_violation_fails_the_instance_without_writing_anything() {
    let database = TestDatabase::create("process_transition_database_error").await;
    let metadata = transition_metadata(false);
    let (runtime, revision) = runtime(&database, &metadata).await;
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process transition database is available");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            INSERT INTO public.process_test_ledger (id, entity_id, status)
            VALUES (gen_random_uuid(), $1, 'preexisting')
            ",
            &[&Uuid::parse_str(ENTITY_ID).unwrap()],
        )
        .await
        .expect("conflicting domain row inserts");
    connection.abort();
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };

    // A unique violation is not a business rejection the Process can route,
    // and it is not transient either: the same write refuses again however
    // often it is retried. Retrying it forever also held the head of the
    // shared transition queue, stopping every other instance in the
    // deployment, so it fails this instance instead. The constraint that
    // refused it goes to the log; the journal keeps only the safe code.
    let consumption = runtime
        .consume_one_transition()
        .await
        .expect("a unique violation is reported, not propagated");
    assert!(
        matches!(
            consumption,
            TransitionConsumption::CommandFailed {
                code: "command_constraint_violation",
                ..
            }
        ),
        "unexpected consumption: {consumption:?}"
    );
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("aborted Process transition is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                instance.version,
                instance.state_json,
                (SELECT count(*) FROM donat.command_invocations
                 WHERE command_name = 'record_process_step'),
                (SELECT count(*) FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome <> 'started'),
                (SELECT count(*) FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.status = 'pending')
            FROM donat.process_instances instance
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("database error rolled back every Process-owned write");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, String>(1), "record");
    assert_eq!(row.get::<_, i64>(2), 1, "failing the instance advances it once");
    assert_eq!(row.get::<_, Json>(3), json!({}));
    assert_eq!(
        row.get::<_, i64>(4),
        0,
        "the refused command wrote no invocation"
    );
    assert_eq!(
        row.get::<_, i64>(5),
        1,
        "the failure is one auditable transition-log entry"
    );
    assert_eq!(
        row.get::<_, i64>(6),
        0,
        "the failed instance leaves no pending event behind"
    );
    connection.abort();
    database.drop().await;
}
