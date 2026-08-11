use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
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
use tokio::sync::Mutex;
use tokio_postgres::NoTls;
use uuid::Uuid;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

const ORDER_ID: &str = "550e8400-e29b-41d4-a716-446655440201";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440202";
const INPUT_FINGERPRINT: &str = "45a23b64ba3a9b82102a9a2b3ec7d43ad7b306915a289881aa6623805d673564";
const SERIALIZATION_KEY_FINGERPRINT: &str =
    "7fec683e84559e64fd60dbc045e06ca28c2c54b93679986deacb4d04d182f7d1";

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
            .expect("stale Process activity database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("Process activity database creates");
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
        database
    }

    async fn catalog(&self) -> Catalog {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls)
            .await
            .expect("Process activity database is available for introspection");
        let connection = tokio::spawn(connection);
        let catalog = donat_catalog::introspect(&client)
            .await
            .expect("Process activity catalog introspects");
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
            .expect("Process activity database drops");
        connection.abort();
    }
}

fn activity_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            }
        }],
        "connectors": [{
            "name": "payments",
            "module": "http",
            "config": {
                "endpoint_identity": "process-test-payments-v1",
                "credential_identity": "process-test-credential",
                "base_url": "https://payments.example.test"
            },
            "operations": [{
                "name": "authorize",
                "version": "1.0.0",
                "method": "POST",
                "path": "/authorizations",
                "input_contract": { "order_id": "uuid!" },
                "body": { "order_id": { "input": "order_id" } },
                "success_statuses": [200],
                "response": {
                    "status": {
                        "json_pointer": "/status",
                        "type": "string!",
                        "max_bytes": 64
                    }
                },
                "effect": {
                    "provider_idempotent": {
                        "side_effect_steps": [{
                            "step": "request",
                            "fixed_binding": { "header": "Idempotency-Key" },
                            "scope": "payment-authorize",
                            "minimum_retention_ms": 20000,
                            "clock_safety_margin_ms": 1000,
                            "evidence": {
                                "source_record_id": "source.process-test.payments.v1",
                                "fact_ids": ["fact.fixed-idempotency-key"]
                            }
                        }]
                    }
                },
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
                    "rules": [{
                        "statuses": [429],
                        "class": "http_429",
                        "code": "rate_limited"
                    }],
                    "fallback": {
                        "class": "permanent",
                        "code": "provider_error"
                    }
                },
                "capacity": {
                    "max_in_flight": 4,
                    "rate_limit": { "permits": 10, "per": "1s", "burst": 4 },
                    "serialize_by": { "input": "order_id" }
                },
                "timeout": "2s",
                "retry": {
                    "maximum_attempts": 2,
                    "backoff": "100ms",
                    "retry_on": ["transport", "timeout"]
                },
                "idempotency": { "header": "Idempotency-Key" }
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
            "start_at": "authorize",
            "states": [
                {
                    "id": "authorize",
                    "request": {
                        "connector": "payments",
                        "operation": "authorize",
                        "input": { "order_id": { "input": "order_id" } },
                        "idempotency_key": {
                            "stable": { "run": "id", "state": "authorize" }
                        },
                        "timeout": {
                            "schedule_to_start": "1s",
                            "start_to_close": "2s"
                        },
                        "retry": {
                            "retry_on": ["transport", "timeout", "http_429", "http_5xx"],
                            "max_attempts": 2,
                            "initial_interval": "100ms",
                            "max_interval": "1s",
                            "jitter": "deterministic_full"
                        },
                        "next": "done"
                        ,
                        "on_error": {
                            "routes": [
                                {
                                    "kinds": ["authentication", "validation"],
                                    "next": "provider_rejected"
                                },
                                {
                                    "kinds": ["retry_exhausted"],
                                    "next": "retry_exhausted"
                                }
                            ],
                            "fallback": { "next": "provider_failed" }
                        }
                    }
                },
                {
                    "id": "done",
                    "output": {
                        "values": {
                            "status": { "state": "authorize", "field": "status" }
                        }
                    }
                },
                {
                    "id": "provider_rejected",
                    "fail": {
                        "code": "provider_rejected",
                        "message": "the provider rejected the activity"
                    }
                },
                {
                    "id": "retry_exhausted",
                    "fail": {
                        "code": "provider_retry_exhausted",
                        "message": "the provider retry budget was exhausted"
                    }
                },
                {
                    "id": "provider_failed",
                    "fail": {
                        "code": "provider_failed",
                        "message": "the provider activity failed"
                    }
                }
            ]
        }]
    }))
    .expect("Process activity metadata deserializes")
}

async fn runtime(
    database: &TestDatabase,
    metadata: &Metadata,
    activity_executor: Option<Arc<dyn ProcessActivityExecutor>>,
) -> (ProcessRuntime, String) {
    let catalog = database.catalog().await;
    let catalogs = HashMap::from([("default".to_owned(), catalog)]);
    let connectors =
        Arc::new(ConnectorRegistry::build(metadata).expect("Process activity connector compiles"));
    let candidate = compile_pure_engine_candidate(metadata, &catalogs, connectors.as_ref(), true)
        .expect("Process activity candidate compiles");
    let process = candidate
        .process_catalog
        .source("default")
        .and_then(|catalog| catalog.process("checkout"))
        .expect("Process activity definition compiles");
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
    .expect("Process activity definition reconciles");

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
    .expect("deployed Process activity catalog validates");
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
    .expect("Process activity runtime builds");
    (runtime, revision)
}

async fn seed_start(database_url: &str, revision: &str) -> Uuid {
    seed_start_with(database_url, revision, ORDER_ID, REQUEST_ID).await
}

async fn seed_start_with(
    database_url: &str,
    revision: &str,
    order_id: &str,
    request_key: &str,
) -> Uuid {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process activity database is available");
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
                'checkout',
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
                &json!({ "order_id": order_id, "request_id": request_key }),
                &request_key,
            ],
        )
        .await
        .expect("Process activity start request inserts")
        .get(0);
    connection.abort();
    request_id
}

#[tokio::test]
async fn request_transition_schedules_one_durable_activity_before_network_execution() {
    // This catches a request transition that performs HTTP before commit,
    // loses the pinned descriptor, or schedules twice under concurrent polls.
    let database = TestDatabase::create("process_activity_schedule").await;
    let metadata = activity_metadata();
    let (runtime, revision) = runtime(&database, &metadata, None).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let (left, right) = tokio::join!(
        runtime.consume_one_transition(),
        runtime.consume_one_transition()
    );
    let outcomes = [
        left.expect("first request transition consumer succeeds"),
        right.expect("second request transition consumer succeeds"),
    ];
    let scheduled_event_id = outcomes
        .iter()
        .find_map(|outcome| match outcome {
            TransitionConsumption::ActivityScheduled {
                instance_id: scheduled_instance,
                event_id,
                state,
                ..
            } if *scheduled_instance == instance_id && state == "authorize" => Some(*event_id),
            _ => None,
        })
        .expect("the request token schedules exactly one durable activity");
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, TransitionConsumption::NoWork))
            .count(),
        1,
        "the losing poll observes no work instead of duplicating the activity"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("scheduled Process activity is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                instance.version,
                instance.state_json,
                job.state_name,
                job.logical_activity_id,
                job.connector_instance,
                job.operation,
                job.input_json,
                job.request_fingerprint,
                encode(job.serialization_key_hash, 'hex'),
                job.status,
                job.attempts,
                (extract(epoch FROM (
                    job.schedule_to_start_deadline - job.created_at
                )) * 1000)::double precision,
                event.status,
                (SELECT count(*)
                 FROM donat.process_activity_jobs counted
                 WHERE counted.source_name = instance.source_name
                   AND counted.instance_id = instance.id),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome = 'activity_scheduled')
            FROM donat.process_instances instance
            JOIN donat.process_activity_jobs job
              ON job.source_name = instance.source_name
             AND job.instance_id = instance.id
            JOIN donat.process_events event
              ON event.source_name = job.source_name
             AND event.id = job.enqueued_from_event_id
            WHERE instance.source_name = 'default'
              AND instance.id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("one durable Process activity was scheduled");
    assert_eq!(row.get::<_, String>(0), "running");
    assert_eq!(row.get::<_, String>(1), "authorize");
    assert_eq!(row.get::<_, i64>(2), 1);
    assert_eq!(row.get::<_, Json>(3), json!({}));
    assert_eq!(row.get::<_, String>(4), "authorize");
    assert_eq!(
        row.get::<_, String>(5),
        format!("activity:v1:{revision}:{instance_id}:{scheduled_event_id}:authorize")
    );
    assert_eq!(row.get::<_, String>(6), "payments");
    assert_eq!(row.get::<_, String>(7), "authorize");
    assert_eq!(row.get::<_, Json>(8), json!({ "order_id": ORDER_ID }));
    assert_eq!(row.get::<_, String>(9), INPUT_FINGERPRINT);
    assert_eq!(
        row.get::<_, Option<String>>(10).as_deref(),
        Some(SERIALIZATION_KEY_FINGERPRINT)
    );
    assert_eq!(row.get::<_, String>(11), "scheduled");
    assert_eq!(row.get::<_, i32>(12), 0);
    assert_eq!(row.get::<_, f64>(13), 1000.0);
    assert_eq!(row.get::<_, String>(14), "consumed");
    assert_eq!(row.get::<_, i64>(15), 1);
    assert_eq!(row.get::<_, i64>(16), 1);
    connection.abort();
    database.drop().await;
}

struct RecordingActivityExecutor {
    database_url: String,
    calls: AtomicUsize,
    idempotency_key: Mutex<Option<String>>,
}

impl RecordingActivityExecutor {
    fn new(database_url: String) -> Self {
        Self {
            database_url,
            calls: AtomicUsize::new(0),
            idempotency_key: Mutex::new(None),
        }
    }
}

impl ProcessActivityExecutor for RecordingActivityExecutor {
    fn execute<'a>(
        &'a self,
        instance: &'a str,
        operation: &'a str,
        input: Json,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            assert_eq!(instance, "payments");
            assert_eq!(operation, "authorize");
            assert_eq!(input, json!({ "order_id": ORDER_ID }));
            assert!(
                deadline > tokio::time::Instant::now(),
                "the connector receives a live start_to_close deadline"
            );

            let (client, connection) = tokio_postgres::connect(&self.database_url, NoTls)
                .await
                .expect("the provider observer can open an independent connection");
            let connection = tokio::spawn(connection);
            let row = client
                .query_one(
                    "
                    SELECT
                        job.status,
                        job.attempts,
                        job.lease_generation,
                        job.lease_token IS NOT NULL,
                        step.logical_activity_id,
                        step.compiled_step_id,
                        step.idempotency_key,
                        (extract(epoch FROM (
                            step.maximum_send_deadline_at
                            - step.first_provider_attempt_at
                        )) * 1000)::double precision,
                        (extract(epoch FROM (
                            step.usable_window_expires_at
                            - step.first_provider_attempt_at
                        )) * 1000)::double precision,
                        (SELECT count(*)
                         FROM donat.process_capacity_reservations reservation
                         WHERE reservation.source_name = job.source_name
                           AND reservation.activity_job_id = job.id
                           AND reservation.released_at IS NULL)
                    FROM donat.process_activity_jobs job
                    JOIN donat.process_activity_provider_steps step
                      ON step.source_name = job.source_name
                     AND step.activity_job_id = job.id
                    WHERE job.source_name = 'default'
                      AND job.status = 'running'
                    ",
                    &[],
                )
                .await
                .expect("lease, capacity, and provider key committed before connector I/O");
            assert_eq!(row.get::<_, String>(0), "running");
            assert_eq!(row.get::<_, i32>(1), 1);
            assert_eq!(row.get::<_, i64>(2), 1);
            assert!(row.get::<_, bool>(3));
            let logical_activity_id = row.get::<_, String>(4);
            assert_eq!(row.get::<_, String>(5), "request");
            assert_eq!(row.get::<_, String>(6), idempotency_key);
            assert_eq!(row.get::<_, f64>(7), 15_100.0);
            assert_eq!(row.get::<_, f64>(8), 19_000.0);
            assert_eq!(row.get::<_, i64>(9), 1);

            let canonical = format!(
                "{{\"logical_activity_id\":\"{logical_activity_id}\",\"scope\":\"payment-authorize\",\"step\":\"request\"}}"
            );
            let mut material = b"donat.connector.idempotency.step.v1\0".to_vec();
            material.extend_from_slice(canonical.as_bytes());
            let expected =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(material));
            assert_eq!(idempotency_key, expected);
            *self.idempotency_key.lock().await = Some(idempotency_key.to_owned());
            self.calls.fetch_add(1, Ordering::Relaxed);
            connection.abort();

            Ok(ConnectorSuccess {
                output: json!({ "status": "authorized" }),
                request_fingerprint: INPUT_FINGERPRINT.to_owned(),
            })
        })
    }
}

#[tokio::test]
async fn activity_claim_commits_before_io_and_fenced_success_advances_the_process() {
    // This catches transaction-held HTTP, a rotating provider key, an
    // unfenced completion, or a success that bypasses the journal event.
    let database = TestDatabase::create("process_activity_success").await;
    let metadata = activity_metadata();
    let executor = Arc::new(RecordingActivityExecutor::new(database.url.clone()));
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("request activity schedules"),
        TransitionConsumption::ActivityScheduled {
            instance_id: scheduled,
            ..
        } if scheduled == instance_id
    ));

    let activity = runtime
        .consume_one_activity()
        .await
        .expect("scheduled activity executes");
    let activity_job_id = match activity {
        ActivityConsumption::Succeeded {
            instance_id: succeeded_instance,
            activity_job_id,
            attempt,
            lease_generation,
        } => {
            assert_eq!(succeeded_instance, instance_id);
            assert_eq!(attempt, 1);
            assert_eq!(lease_generation, 1);
            activity_job_id
        }
        other => panic!("expected a successful activity, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("activity success transition advances"),
        TransitionConsumption::Advanced {
            instance_id: advanced,
            ref from_state,
            ref to_state,
            ..
        } if advanced == instance_id && from_state == "authorize" && to_state == "done"
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("terminal output completes"),
        TransitionConsumption::Completed {
            instance_id: completed,
            ..
        } if completed == instance_id
    ));

    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);
    assert!(
        executor.idempotency_key.lock().await.is_some(),
        "the provider-idempotent operation received its durable step key"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("completed Process activity is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                instance.version,
                instance.state_json,
                instance.terminal_output_json,
                job.status,
                job.result_json,
                job.attempts,
                job.lease_generation,
                job.lease_token IS NULL,
                job.lease_expires_at IS NULL,
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.kind = 'activity_succeeded'
                   AND event.status = 'consumed'),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.status = 'pending'),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome IN (
                       'activity_scheduled',
                       'activity_completed',
                       'activity_succeeded',
                       'completed'
                   ))
            FROM donat.process_instances instance
            JOIN donat.process_activity_jobs job
              ON job.source_name = instance.source_name
             AND job.instance_id = instance.id
            WHERE instance.source_name = 'default'
              AND instance.id = $1
              AND job.id = $2
            ",
            &[&instance_id, &activity_job_id],
        )
        .await
        .expect("activity success is fully durable");
    assert_eq!(row.get::<_, String>(0), "terminal");
    assert_eq!(row.get::<_, String>(1), "done");
    assert_eq!(row.get::<_, i64>(2), 3);
    assert_eq!(
        row.get::<_, Json>(3),
        json!({
            "authorize": { "status": "authorized" },
            "done": { "status": "authorized" }
        })
    );
    assert_eq!(row.get::<_, Json>(4), json!({ "status": "authorized" }));
    assert_eq!(row.get::<_, String>(5), "succeeded");
    assert_eq!(row.get::<_, Json>(6), json!({ "status": "authorized" }));
    assert_eq!(row.get::<_, i32>(7), 1);
    assert_eq!(row.get::<_, i64>(8), 1);
    assert!(row.get::<_, bool>(9));
    assert!(row.get::<_, bool>(10));
    assert_eq!(row.get::<_, i64>(11), 1);
    assert_eq!(row.get::<_, i64>(12), 1);
    assert_eq!(row.get::<_, i64>(13), 0);
    assert_eq!(row.get::<_, i64>(14), 4);
    connection.abort();
    database.drop().await;
}

struct RetryThenSuccessExecutor {
    calls: AtomicUsize,
    idempotency_keys: Mutex<Vec<String>>,
}

impl RetryThenSuccessExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            idempotency_keys: Mutex::new(Vec::new()),
        }
    }
}

impl ProcessActivityExecutor for RetryThenSuccessExecutor {
    fn execute<'a>(
        &'a self,
        instance: &'a str,
        operation: &'a str,
        input: Json,
        idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            assert_eq!(instance, "payments");
            assert_eq!(operation, "authorize");
            assert_eq!(input, json!({ "order_id": ORDER_ID }));
            self.idempotency_keys
                .lock()
                .await
                .push(idempotency_key.to_owned());
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                Err(ConnectorFailure::new(
                    ConnectorErrorClass::Http5xx,
                    "provider_unavailable",
                    "the provider is temporarily unavailable",
                ))
            } else {
                Ok(ConnectorSuccess {
                    output: json!({ "status": "authorized" }),
                    request_fingerprint: INPUT_FINGERPRINT.to_owned(),
                })
            }
        })
    }
}

fn expected_full_jitter_ms(logical_activity_id: &str, attempt: i32, upper_ms: u64) -> u64 {
    let material = format!("donat.process.retry-jitter.v1\0{logical_activity_id}\0{attempt}");
    let digest = Sha256::digest(material.as_bytes());
    let sample = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least eight bytes"),
    );
    sample % (upper_ms + 1)
}

#[tokio::test]
async fn retryable_failure_is_durably_rescheduled_and_reuses_the_provider_key() {
    // This catches in-memory retries, rotated idempotency keys, attempt
    // off-by-one errors, and an activity failure event emitted before the
    // configured retry budget is exhausted.
    let database = TestDatabase::create("process_activity_retry").await;
    let metadata = activity_metadata();
    let executor = Arc::new(RetryThenSuccessExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("request activity schedules"),
        TransitionConsumption::ActivityScheduled { .. }
    ));

    let activity_job_id = match runtime
        .consume_one_activity()
        .await
        .expect("retryable failure commits a retry")
    {
        ActivityConsumption::RetryScheduled {
            instance_id: retried_instance,
            activity_job_id,
            failed_attempt,
            next_attempt,
        } => {
            assert_eq!(retried_instance, instance_id);
            assert_eq!(failed_attempt, 1);
            assert_eq!(next_attempt, 2);
            activity_job_id
        }
        other => panic!("expected a durable retry, got {other:?}"),
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("scheduled retry is inspectable");
    let connection = tokio::spawn(connection);
    let retry = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.lease_generation,
                job.logical_activity_id,
                (extract(epoch FROM (
                    job.available_at - job.updated_at
                )) * 1000)::double precision,
                (extract(epoch FROM (
                    job.schedule_to_start_deadline - job.available_at
                )) * 1000)::double precision,
                job.start_to_close_deadline IS NULL,
                job.lease_token IS NULL,
                job.lease_expires_at IS NULL,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = job.source_name
                   AND step.activity_job_id = job.id),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = job.source_name
                   AND event.instance_id = job.instance_id
                   AND event.kind IN ('activity_failed', 'retry_exhausted')),
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = job.source_name
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_retry_scheduled')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("retry state is durable");
    assert_eq!(retry.get::<_, String>(0), "scheduled");
    assert_eq!(retry.get::<_, i32>(1), 1);
    assert_eq!(retry.get::<_, i64>(2), 1);
    let logical_activity_id = retry.get::<_, String>(3);
    assert_eq!(
        retry.get::<_, f64>(4),
        expected_full_jitter_ms(&logical_activity_id, 1, 100) as f64
    );
    assert_eq!(retry.get::<_, f64>(5), 1000.0);
    assert!(retry.get::<_, bool>(6));
    assert!(retry.get::<_, bool>(7));
    assert!(retry.get::<_, bool>(8));
    assert_eq!(
        retry.get::<_, Json>(9),
        json!({
            "class": "http_5xx",
            "code": "provider_unavailable",
            "safe_message": "the provider is temporarily unavailable",
            "retry_after_ms": null
        })
    );
    assert_eq!(retry.get::<_, i64>(10), 1);
    assert_eq!(retry.get::<_, i64>(11), 0);
    assert_eq!(retry.get::<_, i64>(12), 1);
    assert_eq!(retry.get::<_, i64>(13), 1);

    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET available_at = statement_timestamp(),
                schedule_to_start_deadline =
                    statement_timestamp() + interval '1 second'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test advances the durable retry clock without sleeping");
    connection.abort();

    assert!(matches!(
        runtime
            .consume_one_activity()
            .await
            .expect("second configured attempt succeeds"),
        ActivityConsumption::Succeeded {
            instance_id: succeeded,
            activity_job_id: succeeded_job,
            attempt: 2,
            lease_generation: 2,
        } if succeeded == instance_id && succeeded_job == activity_job_id
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("activity success advances"),
        TransitionConsumption::Advanced { .. }
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("terminal output completes"),
        TransitionConsumption::Completed { .. }
    ));

    assert_eq!(executor.calls.load(Ordering::Relaxed), 2);
    let keys = executor.idempotency_keys.lock().await;
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0], keys[1],
        "configured retry reuses the first committed provider key"
    );
    drop(keys);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("completed retried activity is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.lease_generation,
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = job.source_name
                   AND step.activity_job_id = job.id),
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = job.source_name
                   AND event.instance_id = job.instance_id
                   AND event.kind = 'activity_succeeded'
                   AND event.status = 'consumed')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("retried activity converged");
    assert_eq!(row.get::<_, String>(0), "succeeded");
    assert_eq!(row.get::<_, i32>(1), 2);
    assert_eq!(row.get::<_, i64>(2), 2);
    assert_eq!(row.get::<_, i64>(3), 1);
    assert_eq!(row.get::<_, i64>(4), 2);
    assert_eq!(row.get::<_, i64>(5), 1);
    connection.abort();
    database.drop().await;
}

struct AuthenticationFailureExecutor {
    calls: AtomicUsize,
}

impl AuthenticationFailureExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

impl ProcessActivityExecutor for AuthenticationFailureExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Authentication,
                "provider_authentication",
                "the provider rejected its configured credential",
            ))
        })
    }
}

#[tokio::test]
async fn non_retryable_activity_failure_follows_the_declared_error_route() {
    // This catches implicit retries, hard-coded failure destinations, and a
    // failure transition that fabricates a successful activity result.
    let database = TestDatabase::create("process_activity_error_route").await;
    let metadata = activity_metadata();
    let executor = Arc::new(AuthenticationFailureExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("request activity schedules"),
        TransitionConsumption::ActivityScheduled { .. }
    ));
    let activity_job_id = match runtime
        .consume_one_activity()
        .await
        .expect("authentication failure commits")
    {
        ActivityConsumption::Failed {
            instance_id: failed_instance,
            activity_job_id,
            attempt: 1,
            lease_generation: 1,
            class: ConnectorErrorClass::Authentication,
        } => {
            assert_eq!(failed_instance, instance_id);
            activity_job_id
        }
        other => panic!("expected a terminal authentication failure, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("declared activity error route advances"),
        TransitionConsumption::Advanced {
            instance_id: routed,
            ref from_state,
            ref to_state,
            ..
        } if routed == instance_id
            && from_state == "authorize"
            && to_state == "provider_rejected"
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("routed fail state terminalizes"),
        TransitionConsumption::Failed {
            instance_id: failed,
            ref code,
            ..
        } if failed == instance_id && code == "provider_rejected"
    ));
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("routed Process activity failure is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                instance.version,
                instance.state_json,
                instance.failure_json,
                job.status,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.kind = 'activity_failed'
                   AND event.status = 'consumed'),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.status = 'pending'),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = instance.source_name
                   AND log.instance_id = instance.id
                   AND log.outcome IN (
                       'activity_scheduled',
                       'activity_failed',
                       'activity_error_routed',
                       'failed'
                   ))
            FROM donat.process_instances instance
            JOIN donat.process_activity_jobs job
              ON job.source_name = instance.source_name
             AND job.instance_id = instance.id
            WHERE instance.source_name = 'default'
              AND instance.id = $1
              AND job.id = $2
            ",
            &[&instance_id, &activity_job_id],
        )
        .await
        .expect("declared activity error route is durable");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, String>(1), "provider_rejected");
    assert_eq!(row.get::<_, i64>(2), 3);
    assert_eq!(row.get::<_, Json>(3), json!({}));
    assert_eq!(
        row.get::<_, Json>(4),
        json!({
            "kind": "process_failed",
            "code": "provider_rejected",
            "message": "the provider rejected the activity"
        })
    );
    assert_eq!(row.get::<_, String>(5), "failed");
    assert_eq!(
        row.get::<_, Json>(6),
        json!({
            "class": "authentication",
            "code": "provider_authentication",
            "safe_message": "the provider rejected its configured credential",
            "retry_after_ms": null
        })
    );
    assert_eq!(row.get::<_, i64>(7), 1);
    assert_eq!(row.get::<_, i64>(8), 0);
    assert_eq!(row.get::<_, i64>(9), 4);
    connection.abort();
    database.drop().await;
}

struct AlwaysUnavailableExecutor {
    calls: AtomicUsize,
    idempotency_keys: Mutex<Vec<String>>,
}

impl AlwaysUnavailableExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            idempotency_keys: Mutex::new(Vec::new()),
        }
    }
}

impl ProcessActivityExecutor for AlwaysUnavailableExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.idempotency_keys
                .lock()
                .await
                .push(idempotency_key.to_owned());
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Http5xx,
                "provider_unavailable",
                "the provider is temporarily unavailable",
            ))
        })
    }
}

#[tokio::test]
async fn exhausted_retry_budget_emits_retry_exhausted_and_uses_its_declared_route() {
    // This catches a hidden third attempt, routing by the last provider class
    // instead of retry_exhausted, and provider-key rotation at exhaustion.
    let database = TestDatabase::create("process_activity_retry_exhausted").await;
    let metadata = activity_metadata();
    let executor = Arc::new(AlwaysUnavailableExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("request activity schedules"),
        TransitionConsumption::ActivityScheduled { .. }
    ));
    let activity_job_id = match runtime
        .consume_one_activity()
        .await
        .expect("first failure schedules the configured retry")
    {
        ActivityConsumption::RetryScheduled {
            activity_job_id,
            failed_attempt: 1,
            next_attempt: 2,
            ..
        } => activity_job_id,
        other => panic!("expected the first configured retry, got {other:?}"),
    };
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("retry clock is mutable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET available_at = statement_timestamp(),
                schedule_to_start_deadline =
                    statement_timestamp() + interval '1 second'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test advances the second attempt without sleeping");
    connection.abort();

    assert!(matches!(
        runtime
            .consume_one_activity()
            .await
            .expect("second failure exhausts the configured budget"),
        ActivityConsumption::RetryExhausted {
            instance_id: exhausted_instance,
            activity_job_id: exhausted_job,
            attempt: 2,
            lease_generation: 2,
            last_class: ConnectorErrorClass::Http5xx,
        } if exhausted_instance == instance_id && exhausted_job == activity_job_id
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("retry_exhausted follows its declared route"),
        TransitionConsumption::Advanced {
            instance_id: routed,
            ref to_state,
            ..
        } if routed == instance_id && to_state == "retry_exhausted"
    ));
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("retry exhaustion fail state terminalizes"),
        TransitionConsumption::Failed {
            instance_id: failed,
            ref code,
            ..
        } if failed == instance_id && code == "provider_retry_exhausted"
    ));
    assert_eq!(executor.calls.load(Ordering::Relaxed), 2);
    let keys = executor.idempotency_keys.lock().await;
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    drop(keys);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("retry exhaustion is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                instance.status,
                instance.current_state,
                job.status,
                job.attempts,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.kind = 'retry_exhausted'
                   AND event.status = 'consumed'),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = instance.source_name
                   AND event.instance_id = instance.id
                   AND event.kind = 'activity_failed'),
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = job.source_name
                   AND step.activity_job_id = job.id)
            FROM donat.process_instances instance
            JOIN donat.process_activity_jobs job
              ON job.source_name = instance.source_name
             AND job.instance_id = instance.id
            WHERE instance.source_name = 'default'
              AND instance.id = $1
              AND job.id = $2
            ",
            &[&instance_id, &activity_job_id],
        )
        .await
        .expect("retry exhaustion is durable");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, String>(1), "retry_exhausted");
    assert_eq!(row.get::<_, String>(2), "failed");
    assert_eq!(row.get::<_, i32>(3), 2);
    assert_eq!(
        row.get::<_, Json>(4),
        json!({
            "class": "retry_exhausted",
            "code": "activity_retry_exhausted",
            "safe_message": "the activity retry budget was exhausted",
            "retry_after_ms": null,
            "last_failure": {
                "class": "http_5xx",
                "code": "provider_unavailable"
            }
        })
    );
    assert_eq!(row.get::<_, i64>(5), 1);
    assert_eq!(row.get::<_, i64>(6), 0);
    assert_eq!(row.get::<_, i64>(7), 1);
    connection.abort();
    database.drop().await;
}

struct BlockingSuccessExecutor {
    calls: AtomicUsize,
    first_started: tokio::sync::Notify,
    release_first: tokio::sync::Notify,
}

impl BlockingSuccessExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            first_started: tokio::sync::Notify::new(),
            release_first: tokio::sync::Notify::new(),
        }
    }
}

impl ProcessActivityExecutor for BlockingSuccessExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                self.first_started.notify_one();
                self.release_first.notified().await;
            }
            Ok(ConnectorSuccess {
                output: json!({ "status": "authorized" }),
                request_fingerprint: INPUT_FINGERPRINT.to_owned(),
            })
        })
    }
}

#[tokio::test]
async fn serialization_capacity_allows_only_one_in_flight_activity_for_the_same_key() {
    // This catches process-local semaphores, a check outside the bucket lock,
    // token consumption on deferral, and hot-loop polling before the bounded
    // schedule-to-start deadline.
    const SECOND_REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440203";

    let database = TestDatabase::create("process_activity_serialization").await;
    let metadata = activity_metadata();
    let executor = Arc::new(BlockingSuccessExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start_with(&database.url, &revision, ORDER_ID, REQUEST_ID).await;
    seed_start_with(&database.url, &revision, ORDER_ID, SECOND_REQUEST_ID).await;

    let first_instance = match runtime
        .consume_one_start()
        .await
        .expect("first Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a first Process instance, got {other:?}"),
    };
    let second_instance = match runtime
        .consume_one_start()
        .await
        .expect("second Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a second Process instance, got {other:?}"),
    };
    assert_ne!(first_instance, second_instance);
    let first_job = match runtime
        .consume_one_transition()
        .await
        .expect("first request schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected the first activity schedule, got {other:?}"),
    };
    let second_job = match runtime
        .consume_one_transition()
        .await
        .expect("second request schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected the second activity schedule, got {other:?}"),
    };
    assert_ne!(first_job, second_job);

    let runtime = Arc::new(runtime);
    let first_execution = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.consume_one_activity().await }
    });
    executor.first_started.notified().await;
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("same-key claimant is durably deferred"),
        ActivityConsumption::CapacityDeferred {
            activity_job_id: second_job
        }
    );
    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        1,
        "capacity deferral performs no provider call"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("serialized capacity is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                second.status,
                second.attempts,
                second.lease_token IS NULL,
                second.available_at = least(
                    second.schedule_to_start_deadline,
                    reservation.expires_at
                ),
                first.status,
                first.attempts,
                first.lease_token IS NOT NULL,
                reservation.released_at IS NULL,
                first.serialization_key_hash = second.serialization_key_hash,
                (SELECT count(*)
                 FROM donat.process_capacity_reservations counted
                 WHERE counted.source_name = first.source_name
                   AND counted.connector_instance = first.connector_instance
                   AND counted.operation = first.operation
                   AND counted.released_at IS NULL)
            FROM donat.process_activity_jobs first
            JOIN donat.process_activity_jobs second
              ON second.source_name = first.source_name
            JOIN donat.process_capacity_reservations reservation
              ON reservation.source_name = first.source_name
             AND reservation.activity_job_id = first.id
            WHERE first.source_name = 'default'
              AND first.id = $1
              AND second.id = $2
            ",
            &[&first_job, &second_job],
        )
        .await
        .expect("one same-key reservation is globally visible");
    assert_eq!(row.get::<_, String>(0), "scheduled");
    assert_eq!(row.get::<_, i32>(1), 0);
    assert!(row.get::<_, bool>(2));
    assert!(
        row.get::<_, bool>(3),
        "the deferred job wakes at the earliest useful bounded instant"
    );
    assert_eq!(row.get::<_, String>(4), "running");
    assert_eq!(row.get::<_, i32>(5), 1);
    assert!(row.get::<_, bool>(6));
    assert!(row.get::<_, bool>(7));
    assert!(row.get::<_, bool>(8));
    assert_eq!(row.get::<_, i64>(9), 1);
    connection.abort();

    executor.release_first.notify_one();
    assert!(matches!(
        first_execution
            .await
            .expect("first activity task joins")
            .expect("first activity succeeds"),
        ActivityConsumption::Succeeded {
            activity_job_id: succeeded_job,
            ..
        } if succeeded_job == first_job
    ));
    database.drop().await;
}

struct TakeoverExecutor {
    calls: AtomicUsize,
    idempotency_keys: Mutex<Vec<String>>,
    first_started: tokio::sync::Notify,
    release_first: tokio::sync::Notify,
}

impl TakeoverExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            idempotency_keys: Mutex::new(Vec::new()),
            first_started: tokio::sync::Notify::new(),
            release_first: tokio::sync::Notify::new(),
        }
    }
}

impl ProcessActivityExecutor for TakeoverExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.idempotency_keys
                .lock()
                .await
                .push(idempotency_key.to_owned());
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                self.first_started.notify_one();
                self.release_first.notified().await;
                Ok(ConnectorSuccess {
                    output: json!({ "status": "late-first-worker" }),
                    request_fingerprint: INPUT_FINGERPRINT.to_owned(),
                })
            } else {
                Ok(ConnectorSuccess {
                    output: json!({ "status": "authorized" }),
                    request_fingerprint: INPUT_FINGERPRINT.to_owned(),
                })
            }
        })
    }
}

#[tokio::test]
async fn expired_lease_takeover_reuses_the_attempt_and_fences_the_stale_completion() {
    // This catches attempt increments on takeover, provider-key rotation,
    // completion past the bounded takeover grace, and a stale worker
    // overwriting the winning result without an audit record.
    let database = TestDatabase::create("process_activity_takeover").await;
    let metadata = activity_metadata();
    let executor = Arc::new(TakeoverExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let first_execution = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.consume_one_activity().await }
    });
    executor.first_started.notified().await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("takeover clock is mutable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET start_to_close_deadline =
                    statement_timestamp() - interval '1 millisecond',
                lease_expires_at =
                    statement_timestamp() - interval '1 millisecond',
                available_at = statement_timestamp()
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test expires the first worker lease inside takeover grace");
    client
        .execute(
            "
            UPDATE donat.process_capacity_reservations
            SET expires_at = statement_timestamp() - interval '1 millisecond'
            WHERE source_name = 'default'
              AND activity_job_id = $1
              AND released_at IS NULL
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test expires the first worker capacity reservation");
    connection.abort();

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("expired lease is safely taken over"),
        ActivityConsumption::Succeeded {
            instance_id,
            activity_job_id,
            attempt: 1,
            lease_generation: 2,
        }
    );
    executor.release_first.notify_one();
    assert_eq!(
        first_execution
            .await
            .expect("first worker task joins")
            .expect("stale first worker completion is an ordinary outcome"),
        ActivityConsumption::StaleCompletion {
            instance_id,
            activity_job_id,
            attempt: 1,
            lease_generation: 1,
        }
    );
    let keys = executor.idempotency_keys.lock().await;
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1]);
    drop(keys);

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("winning takeover result advances"),
        TransitionConsumption::Advanced { .. }
    ));
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("takeover result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.lease_generation,
                job.result_json,
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = job.source_name
                   AND step.activity_job_id = job.id),
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = job.source_name
                   AND event.instance_id = job.instance_id
                   AND event.kind = 'activity_succeeded'),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = job.source_name
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_stale_completion')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("takeover converged to one durable winner");
    assert_eq!(row.get::<_, String>(0), "succeeded");
    assert_eq!(row.get::<_, i32>(1), 1);
    assert_eq!(row.get::<_, i64>(2), 2);
    assert_eq!(row.get::<_, Json>(3), json!({ "status": "authorized" }));
    assert_eq!(row.get::<_, i64>(4), 1);
    assert_eq!(row.get::<_, i64>(5), 2);
    assert_eq!(row.get::<_, i64>(6), 1);
    assert_eq!(row.get::<_, i64>(7), 1);
    connection.abort();
    database.drop().await;
}

struct NeverCalledExecutor {
    calls: AtomicUsize,
}

impl NeverCalledExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

impl ProcessActivityExecutor for NeverCalledExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ConnectorSuccess {
                output: json!({ "status": "must-not-run" }),
                request_fingerprint: INPUT_FINGERPRINT.to_owned(),
            })
        })
    }
}

#[tokio::test]
async fn schedule_to_start_timeout_is_audited_and_never_claims_capacity_or_calls_provider() {
    // This catches checking the deadline after capacity/lease assignment,
    // provider I/O after queue expiry, and an unaudited timeout transition.
    let database = TestDatabase::create("process_activity_schedule_timeout").await;
    let metadata = activity_metadata();
    let executor = Arc::new(NeverCalledExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("schedule deadline is mutable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET available_at = statement_timestamp(),
                schedule_to_start_deadline =
                    statement_timestamp() - interval '1 millisecond'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test expires the durable schedule deadline");
    connection.abort();

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("expired schedule is terminalized"),
        ActivityConsumption::ScheduleToStartTimedOut {
            instance_id,
            activity_job_id,
        }
    );
    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        0,
        "an expired scheduled activity performs no provider I/O"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("schedule timeout is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.lease_generation,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_capacity_buckets bucket
                 WHERE bucket.source_name = job.source_name),
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id),
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = job.source_name
                   AND step.activity_job_id = job.id),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = job.source_name
                   AND log.activity_job_id = job.id
                   AND log.activity_attempt = 0
                   AND log.activity_lease_generation = 0
                   AND log.outcome =
                       'activity_schedule_to_start_timed_out')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("schedule timeout journal converged");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, i32>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 0);
    assert_eq!(
        row.get::<_, Json>(3),
        json!({
            "class": "timeout",
            "code": "schedule_to_start_timeout",
            "safe_message":
                "activity did not start before its schedule_to_start deadline",
            "retry_after_ms": null,
        })
    );
    assert_eq!(row.get::<_, i64>(4), 0);
    assert_eq!(row.get::<_, i64>(5), 0);
    assert_eq!(row.get::<_, i64>(6), 0);
    assert_eq!(row.get::<_, i64>(7), 1);
    connection.abort();

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("timeout follows the declared fallback"),
        TransitionConsumption::Advanced {
            instance_id: routed,
            ref to_state,
            ..
        } if routed == instance_id && to_state == "provider_failed"
    ));
    database.drop().await;
}

struct ExcessiveRetryAfterExecutor {
    calls: AtomicUsize,
}

impl ExcessiveRetryAfterExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

impl ProcessActivityExecutor for ExcessiveRetryAfterExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Http429,
                "rate_limited",
                "the provider requested an excessive delay",
            )
            .with_retry_after(Some(std::time::Duration::from_millis(101))))
        })
    }
}

#[tokio::test]
async fn retry_after_beyond_the_declared_backoff_bound_takes_the_typed_timeout_path() {
    // This catches trusting a provider delay beyond the compiled horizon,
    // retaining the provider's http_429 class, or making another send.
    let database = TestDatabase::create("process_activity_retry_after_bound").await;
    let metadata = activity_metadata();
    let executor = Arc::new(ExcessiveRetryAfterExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("out-of-bound Retry-After is a durable timeout"),
        ActivityConsumption::Failed {
            instance_id,
            activity_job_id,
            attempt: 1,
            lease_generation: 1,
            class: ConnectorErrorClass::Timeout,
        }
    );
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("bounded Retry-After failure is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = job.source_name
                   AND event.instance_id = job.instance_id
                   AND event.kind = 'activity_failed')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("bounded Retry-After journal converged");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, i32>(1), 1);
    assert_eq!(
        row.get::<_, Json>(2),
        json!({
            "class": "timeout",
            "code": "retry_after_exceeds_retry_bound",
            "safe_message":
                "provider Retry-After exceeds the declared retry delay bound",
            "retry_after_ms": 101,
        })
    );
    assert_eq!(row.get::<_, i64>(3), 1);
    assert_eq!(row.get::<_, i64>(4), 1);
    connection.abort();

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("bounded timeout follows the declared fallback"),
        TransitionConsumption::Advanced {
            instance_id: routed,
            ref to_state,
            ..
        } if routed == instance_id && to_state == "provider_failed"
    ));
    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("terminal bounded timeout has no retry"),
        ActivityConsumption::NoWork
    );
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);
    database.drop().await;
}

struct SeedProviderDeadlineExecutor {
    calls: AtomicUsize,
}

impl SeedProviderDeadlineExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

impl ProcessActivityExecutor for SeedProviderDeadlineExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                Err(ConnectorFailure::new(
                    ConnectorErrorClass::Http5xx,
                    "provider_unavailable",
                    "the provider is temporarily unavailable",
                ))
            } else {
                Ok(ConnectorSuccess {
                    output: json!({ "status": "must-not-run" }),
                    request_fingerprint: INPUT_FINGERPRINT.to_owned(),
                })
            }
        })
    }
}

async fn assert_provider_deadline_refusal(
    label: &str,
    maximum_send_offset_ms: i64,
    usable_window_offset_ms: i64,
    expected_class: ConnectorErrorClass,
    expected_code: &str,
    expected_message: &str,
) {
    let database = TestDatabase::create(label).await;
    let metadata = activity_metadata();
    let executor = Arc::new(SeedProviderDeadlineExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };
    assert!(matches!(
        runtime
            .consume_one_activity()
            .await
            .expect("first failure seeds one provider step"),
        ActivityConsumption::RetryScheduled {
            activity_job_id: retried,
            failed_attempt: 1,
            next_attempt: 2,
            ..
        } if retried == activity_job_id
    ));
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("provider deadlines are mutable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET available_at = statement_timestamp(),
                schedule_to_start_deadline =
                    statement_timestamp() + interval '1 second'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test advances the durable retry");
    client
        .execute(
            "
            UPDATE donat.process_activity_provider_steps
            SET maximum_send_deadline_at =
                    statement_timestamp()
                      + ($2::bigint * interval '1 millisecond'),
                usable_window_expires_at =
                    statement_timestamp()
                      + ($3::bigint * interval '1 millisecond')
            WHERE source_name = 'default' AND activity_job_id = $1
            ",
            &[
                &activity_job_id,
                &maximum_send_offset_ms,
                &usable_window_offset_ms,
            ],
        )
        .await
        .expect("test positions the persisted provider deadlines");
    connection.abort();

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("expired provider authorization is terminalized"),
        ActivityConsumption::Failed {
            instance_id,
            activity_job_id,
            attempt: 2,
            lease_generation: 2,
            class: expected_class,
        }
    );
    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        1,
        "provider authorization refusal happens before network I/O"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("provider refusal is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.lease_generation,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = job.source_name
                   AND step.activity_job_id = job.id),
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = job.source_name
                   AND event.instance_id = job.instance_id
                   AND event.kind = 'activity_failed')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("provider refusal journal converged");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(row.get::<_, i32>(1), 2);
    assert_eq!(row.get::<_, i64>(2), 2);
    assert_eq!(
        row.get::<_, Json>(3),
        json!({
            "class": match expected_class {
                ConnectorErrorClass::Timeout => "timeout",
                ConnectorErrorClass::Permanent => "permanent",
                other => panic!("unexpected provider refusal class {other:?}"),
            },
            "code": expected_code,
            "safe_message": expected_message,
            "retry_after_ms": null,
        })
    );
    assert_eq!(row.get::<_, i64>(4), 1);
    assert_eq!(row.get::<_, i64>(5), 2);
    assert_eq!(row.get::<_, i64>(6), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn provider_deadline_refusals_happen_before_io_with_usable_window_precedence() {
    // This catches an anyhow escape that leaves the job running, a send after
    // either persisted bound, and checking maximum-send before usable-window.
    assert_provider_deadline_refusal(
        "process_activity_send_horizon",
        -1,
        1_000,
        ConnectorErrorClass::Timeout,
        "provider_send_horizon_exhausted",
        "the compiled provider send horizon was exhausted",
    )
    .await;
    assert_provider_deadline_refusal(
        "process_activity_usable_window",
        -1,
        -1,
        ConnectorErrorClass::Permanent,
        "connector_idempotency_window_exhausted",
        "the provider idempotency window was exhausted",
    )
    .await;
}

#[tokio::test]
async fn late_takeover_refuses_io_and_durably_schedules_the_next_configured_attempt() {
    // This catches sending after the terminal takeover grace, misclassifying
    // it as schedule-to-start, leaking capacity, and losing the configured
    // timeout retry while the stale worker is still in flight.
    let database = TestDatabase::create("process_activity_late_takeover").await;
    let metadata = activity_metadata();
    let executor = Arc::new(BlockingSuccessExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let first_execution = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.consume_one_activity().await }
    });
    executor.first_started.notified().await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("late-takeover clock is mutable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET start_to_close_deadline =
                    statement_timestamp() - interval '5001 milliseconds',
                lease_expires_at =
                    statement_timestamp() - interval '5001 milliseconds',
                available_at = statement_timestamp()
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test moves the first attempt beyond terminal takeover grace");
    client
        .execute(
            "
            UPDATE donat.process_capacity_reservations
            SET expires_at = statement_timestamp() - interval '5001 milliseconds'
            WHERE source_name = 'default'
              AND activity_job_id = $1
              AND released_at IS NULL
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test expires the abandoned capacity reservation");
    connection.abort();

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("late takeover becomes the configured timeout retry"),
        ActivityConsumption::RetryScheduled {
            instance_id,
            activity_job_id,
            failed_attempt: 1,
            next_attempt: 2,
        }
    );
    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        1,
        "late takeover performs no second provider send"
    );

    executor.release_first.notify_one();
    assert_eq!(
        first_execution
            .await
            .expect("first worker task joins")
            .expect("late first completion is fenced"),
        ActivityConsumption::StaleCompletion {
            instance_id,
            activity_job_id,
            attempt: 1,
            lease_generation: 1,
        }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("late takeover retry is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.status,
                job.attempts,
                job.lease_generation,
                job.start_to_close_deadline IS NULL,
                job.lease_token IS NULL,
                job.lease_expires_at IS NULL,
                job.last_error_json,
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id
                   AND reservation.released_at IS NOT NULL),
                (SELECT count(*)
                 FROM donat.process_events event
                 WHERE event.source_name = job.source_name
                   AND event.instance_id = job.instance_id
                   AND event.kind IN ('activity_failed', 'retry_exhausted')),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = job.source_name
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_retry_scheduled'),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = job.source_name
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_stale_completion')
            FROM donat.process_activity_jobs job
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("late takeover retry journal converged");
    assert_eq!(row.get::<_, String>(0), "scheduled");
    assert_eq!(row.get::<_, i32>(1), 1);
    assert_eq!(row.get::<_, i64>(2), 1);
    assert!(row.get::<_, bool>(3));
    assert!(row.get::<_, bool>(4));
    assert!(row.get::<_, bool>(5));
    assert_eq!(
        row.get::<_, Json>(6),
        json!({
            "class": "timeout",
            "code": "start_to_close_timeout",
            "safe_message":
                "the activity exceeded start_to_close and its takeover grace",
            "retry_after_ms": null,
        })
    );
    assert_eq!(row.get::<_, i64>(7), 1);
    assert_eq!(row.get::<_, i64>(8), 0);
    assert_eq!(row.get::<_, i64>(9), 1);
    assert_eq!(row.get::<_, i64>(10), 1);
    connection.abort();
    database.drop().await;
}

#[tokio::test]
async fn fractional_rate_token_defers_only_until_the_exact_next_token_boundary() {
    // This catches rounding every partial-token wait up to a whole refill
    // interval, consuming a token on deferral, or hot-looping before due.
    let database = TestDatabase::create("process_activity_fractional_rate").await;
    let metadata = activity_metadata();
    let executor = Arc::new(NeverCalledExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    assert!(matches!(
        runtime
            .consume_one_start()
            .await
            .expect("Process start consumes"),
        StartConsumption::Started { .. }
    ));
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("rate bucket is seedable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            INSERT INTO donat.process_capacity_buckets (
                source_name,
                connector_instance,
                operation,
                available_tokens,
                last_refill_at,
                policy_fingerprint
            )
            VALUES (
                'default',
                'payments',
                'authorize',
                0.75,
                statement_timestamp() + interval '1 hour',
                'fe86f83aedf74ca82352c1a265e4991b4e8bad60f17940e39bbe7ef3d4d52357'
            )
            ",
            &[],
        )
        .await
        .expect("test seeds three quarters of one rate token");
    connection.abort();

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("partial token produces a bounded durable deferral"),
        ActivityConsumption::CapacityDeferred { activity_job_id }
    );
    assert_eq!(executor.calls.load(Ordering::Relaxed), 0);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("rate deferral is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                (extract(epoch FROM (
                    job.available_at - job.updated_at
                )) * 1000)::double precision,
                job.status,
                job.attempts,
                job.lease_token IS NULL,
                bucket.available_tokens::double precision,
                (SELECT count(*)
                 FROM donat.process_capacity_reservations reservation
                 WHERE reservation.source_name = job.source_name
                   AND reservation.activity_job_id = job.id)
            FROM donat.process_activity_jobs job
            JOIN donat.process_capacity_buckets bucket
              ON bucket.source_name = job.source_name
             AND bucket.connector_instance = job.connector_instance
             AND bucket.operation = job.operation
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("partial-token journal converged");
    assert_eq!(row.get::<_, f64>(0), 25.0);
    assert_eq!(row.get::<_, String>(1), "scheduled");
    assert_eq!(row.get::<_, i32>(2), 0);
    assert!(row.get::<_, bool>(3));
    assert_eq!(row.get::<_, f64>(4), 0.75);
    assert_eq!(row.get::<_, i64>(5), 0);
    connection.abort();

    assert_eq!(executor.calls.load(Ordering::Relaxed), 0);
    database.drop().await;
}

#[tokio::test]
async fn takeover_capacity_deferral_uses_takeover_grace_not_the_stale_schedule_deadline() {
    // This catches a running takeover being deferred to its already-expired
    // schedule_to_start deadline and immediately hot-looping under contention.
    const SECOND_REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440204";

    let database = TestDatabase::create("process_activity_takeover_capacity").await;
    let metadata = activity_metadata();
    let executor = Arc::new(BlockingSuccessExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start_with(&database.url, &revision, ORDER_ID, REQUEST_ID).await;
    seed_start_with(&database.url, &revision, ORDER_ID, SECOND_REQUEST_ID).await;
    assert!(matches!(
        runtime
            .consume_one_start()
            .await
            .expect("first Process start consumes"),
        StartConsumption::Started { .. }
    ));
    assert!(matches!(
        runtime
            .consume_one_start()
            .await
            .expect("second Process start consumes"),
        StartConsumption::Started { .. }
    ));
    let first_job = match runtime
        .consume_one_transition()
        .await
        .expect("first request schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected the first activity schedule, got {other:?}"),
    };
    let second_job = match runtime
        .consume_one_transition()
        .await
        .expect("second request schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected the second activity schedule, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let first_execution = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.consume_one_activity().await }
    });
    executor.first_started.notified().await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("takeover contention is seedable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET start_to_close_deadline =
                    statement_timestamp() - interval '1 millisecond',
                lease_expires_at =
                    statement_timestamp() - interval '1 millisecond',
                schedule_to_start_deadline =
                    statement_timestamp() - interval '1 millisecond',
                available_at = statement_timestamp()
            WHERE source_name = 'default' AND id = $1
            ",
            &[&first_job],
        )
        .await
        .expect("test makes the first job eligible for takeover");
    client
        .execute(
            "
            UPDATE donat.process_capacity_reservations
            SET expires_at = statement_timestamp() - interval '1 millisecond'
            WHERE source_name = 'default'
              AND activity_job_id = $1
              AND released_at IS NULL
            ",
            &[&first_job],
        )
        .await
        .expect("test expires the abandoned first reservation");
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET available_at = statement_timestamp() + interval '1 hour'
            WHERE source_name = 'default' AND id = $1
            ",
            &[&second_job],
        )
        .await
        .expect("the synthetic contending job is not itself claimable");
    client
        .execute(
            "
            INSERT INTO donat.process_capacity_reservations (
                source_name,
                activity_job_id,
                connector_instance,
                operation,
                serialization_key_hash,
                lease_token,
                reserved_at,
                expires_at
            )
            SELECT
                source_name,
                id,
                connector_instance,
                operation,
                serialization_key_hash,
                gen_random_uuid(),
                statement_timestamp(),
                statement_timestamp() + interval '1 second'
            FROM donat.process_activity_jobs
            WHERE source_name = 'default' AND id = $1
            ",
            &[&second_job],
        )
        .await
        .expect("test seeds one valid same-key capacity contender");
    connection.abort();

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("contended takeover is durably deferred"),
        ActivityConsumption::CapacityDeferred {
            activity_job_id: first_job,
        }
    );
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("takeover deferral is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                first.available_at > statement_timestamp(),
                first.available_at = least(
                    first.start_to_close_deadline
                      + interval '5000 milliseconds',
                    contender.expires_at
                ),
                first.schedule_to_start_deadline < statement_timestamp(),
                first.status,
                first.attempts,
                first.lease_generation
            FROM donat.process_activity_jobs first
            JOIN donat.process_capacity_reservations contender
              ON contender.source_name = first.source_name
             AND contender.activity_job_id = $2
             AND contender.released_at IS NULL
            WHERE first.source_name = 'default' AND first.id = $1
            ",
            &[&first_job, &second_job],
        )
        .await
        .expect("takeover deferral uses the bounded useful wake-up");
    assert!(row.get::<_, bool>(0));
    assert!(row.get::<_, bool>(1));
    assert!(row.get::<_, bool>(2));
    assert_eq!(row.get::<_, String>(3), "running");
    assert_eq!(row.get::<_, i32>(4), 1);
    assert_eq!(row.get::<_, i64>(5), 1);
    connection.abort();

    first_execution.abort();
    let _ = first_execution.await;
    database.drop().await;
}

struct MalformedSuccessExecutor {
    calls: AtomicUsize,
}

impl MalformedSuccessExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
}

impl ProcessActivityExecutor for MalformedSuccessExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ConnectorSuccess {
                output: json!({}),
                request_fingerprint: INPUT_FINGERPRINT.to_owned(),
            })
        })
    }
}

#[tokio::test]
async fn malformed_connector_success_becomes_a_durable_invariant_failure() {
    // This catches returning anyhow after provider I/O and leaving a claimed
    // job permanently running when its declared output contract is violated.
    let database = TestDatabase::create("process_activity_malformed_success").await;
    let metadata = activity_metadata();
    let executor = Arc::new(MalformedSuccessExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    assert_eq!(
        runtime
            .consume_one_activity()
            .await
            .expect("malformed success is journaled, not escaped"),
        ActivityConsumption::Failed {
            instance_id,
            activity_job_id,
            attempt: 1,
            lease_generation: 1,
            class: ConnectorErrorClass::Invariant,
        }
    );
    assert_eq!(executor.calls.load(Ordering::Relaxed), 1);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("malformed connector result is inspectable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                status,
                last_error_json,
                lease_token IS NULL,
                lease_expires_at IS NULL
            FROM donat.process_activity_jobs
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("malformed result journal converged");
    assert_eq!(row.get::<_, String>(0), "failed");
    assert_eq!(
        row.get::<_, Json>(1),
        json!({
            "class": "invariant",
            "code": "connector_output_contract_violation",
            "safe_message":
                "connector output violated its declared operation contract",
            "retry_after_ms": null,
        })
    );
    assert!(row.get::<_, bool>(2));
    assert!(row.get::<_, bool>(3));
    connection.abort();

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("invariant failure follows the declared fallback"),
        TransitionConsumption::Advanced {
            instance_id: routed,
            ref to_state,
            ..
        } if routed == instance_id && to_state == "provider_failed"
    ));
    database.drop().await;
}

/// The environment variable the at-most-once fixture resolves its credential
/// from. Its value is a sentinel: the activity executor here is a stub.
const AT_MOST_ONCE_TOKEN: &str = "DONAT_TEST_PROCESS_ACTIVITY_SLACK_TOKEN";

/// A deployment whose one write publishes no idempotency mechanism, and the
/// Process that accepted it (ADR 063).
fn at_most_once_metadata() -> Metadata {
    // Safety: the connector registry reads this variable on the same thread
    // that sets it, before any listener or worker exists.
    unsafe { std::env::set_var(AT_MOST_ONCE_TOKEN, "xoxb-process-activity-sentinel") };
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": "postgres://unused" }
            }
        }],
        "connectors": [{
            "name": "chat",
            "module": "slack",
            "config": {
                "endpoint_identity": "process-activity-slack-v1",
                "credential_identity": "process-activity-slack-credential",
                "secret_key": { "value_from_env": AT_MOST_ONCE_TOKEN }
            },
            "operations": [{
                "name": "message.post",
                "capacity": {
                    "max_in_flight": 4,
                    "rate_limit": { "permits": 10, "per": "1s", "burst": 4 }
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
                { "name": "channel", "type": "string!" },
                { "name": "text", "type": "string!" },
                { "name": "request_id", "type": "uuid!" }
            ],
            "output": [{ "name": "status", "type": "string!" }],
            "idempotency": {
                "key": { "input": "request_id" },
                "scope": [{ "input": "channel" }]
            },
            "start_at": "post",
            "states": [
                {
                    "id": "post",
                    "request": {
                        "connector": "chat",
                        "operation": "message.post",
                        "input": {
                            "channel": { "input": "channel" },
                            "text": { "input": "text" }
                        },
                        "at_most_once": true,
                        "on_ambiguous": "unknown",
                        "timeout": {
                            "schedule_to_start": "1s",
                            "start_to_close": "6s"
                        },
                        "retry": {
                            "retry_on": [],
                            "max_attempts": 1,
                            "initial_interval": "100ms",
                            "max_interval": "1s",
                            "jitter": "deterministic_full"
                        },
                        "next": "done",
                        "on_error": {
                            "routes": [{
                                "kinds": ["validation"],
                                "next": "refused"
                            }],
                            "fallback": { "next": "refused" }
                        }
                    }
                },
                {
                    "id": "done",
                    "output": { "values": { "status": { "literal": "posted" } } }
                },
                {
                    "id": "unknown",
                    "output": { "values": { "status": { "literal": "unknown" } } }
                },
                {
                    "id": "refused",
                    "fail": {
                        "code": "provider_refused",
                        "message": "the provider refused the message"
                    }
                }
            ]
        }]
    }))
    .expect("at-most-once Process activity metadata deserializes")
}

async fn seed_at_most_once_start(database_url: &str, revision: &str) -> Uuid {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process activity database is available");
    let connection = tokio::spawn(connection);
    let request_id = client
        .query_one(
            "
            INSERT INTO donat.process_start_requests (
                source_name, process_name, revision, input_json,
                command_invocation_id, effect_position, idempotency_key, status
            )
            VALUES ('default', 'checkout', $1, $2, gen_random_uuid(), 0, $3, 'pending')
            RETURNING id
            ",
            &[
                &revision,
                &json!({ "channel": "C0000001", "text": "one send", "request_id": REQUEST_ID }),
                &REQUEST_ID,
            ],
        )
        .await
        .expect("Process activity start request inserts")
        .get(0);
    connection.abort();
    request_id
}

/// An executor that never answers, so the worker holding the one send
/// authorization is exactly a worker that vanished mid-request.
struct StalledExecutor {
    calls: AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl StalledExecutor {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        }
    }
}

impl ProcessActivityExecutor for StalledExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.started.notify_one();
            self.release.notified().await;
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Transport,
                "transport",
                "the first worker never learned the outcome",
            ))
        })
    }
}

/// ADR 063 at runtime. The send authorization is committed before the request
/// leaves and is claimed exactly once, so the worker that takes the lease over
/// cannot send again — and does not pretend the send failed. It reports an
/// outcome nobody knows, and the Process takes the route it declared for one.
#[tokio::test]
async fn an_at_most_once_send_is_claimed_once_and_a_takeover_routes_the_unknown_outcome() {
    let database = TestDatabase::create("process_activity_at_most_once").await;
    let metadata = at_most_once_metadata();
    let executor = Arc::new(StalledExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_at_most_once_start(&database.url, &revision).await;
    let instance_id = match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { instance_id, .. } => instance_id,
        other => panic!("expected a new Process instance, got {other:?}"),
    };
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let first_worker = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.consume_one_activity().await }
    });
    executor.started.notified().await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("the at-most-once clock is mutable by the test");
    let connection = tokio::spawn(connection);
    // The send authorization is durable before the request leaves, which is the
    // whole of the promise: whatever happens to this worker, the row is there.
    assert_eq!(
        client
            .query_one(
                "
                SELECT count(*)
                FROM donat.process_activity_provider_steps
                WHERE source_name = 'default' AND activity_job_id = $1
                ",
                &[&activity_job_id],
            )
            .await
            .expect("the authorization is readable")
            .get::<_, i64>(0),
        1,
        "the one send is claimed in the database before any byte leaves"
    );
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET lease_expires_at = statement_timestamp() - interval '1 millisecond',
                available_at = statement_timestamp()
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test expires the first worker lease inside the takeover grace");
    client
        .execute(
            "
            UPDATE donat.process_capacity_reservations
            SET expires_at = statement_timestamp() - interval '1 millisecond'
            WHERE source_name = 'default'
              AND activity_job_id = $1
              AND released_at IS NULL
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test expires the first worker capacity reservation");

    let taken_over = runtime
        .consume_one_activity()
        .await
        .expect("the lease is taken over");
    let ActivityConsumption::Failed {
        instance_id: failed_instance,
        activity_job_id: failed_job,
        ..
    } = taken_over
    else {
        panic!("expected a terminal outcome, got {taken_over:?}")
    };
    assert_eq!(failed_instance, instance_id);
    assert_eq!(failed_job, activity_job_id);
    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        1,
        "the second worker never reached the provider"
    );

    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the unknown outcome routes"),
        TransitionConsumption::Advanced { .. }
    ));
    let row = client
        .query_one(
            "
            SELECT
                instance.current_state,
                job.last_error_json->>'code',
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = 'default'
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_ambiguous_routed'
                   AND log.to_state = 'unknown'),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = 'default'
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_error_routed'),
                (SELECT count(*)
                 FROM donat.process_activity_provider_steps step
                 WHERE step.source_name = 'default'
                   AND step.activity_job_id = job.id)
            FROM donat.process_activity_jobs job
            JOIN donat.process_instances instance
              ON instance.source_name = job.source_name
             AND instance.id = job.instance_id
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("the ambiguous outcome is inspectable");
    assert_eq!(
        row.get::<_, String>(0),
        "unknown",
        "the instance took its declared ambiguous route"
    );
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        Some("provider_send_ambiguous")
    );
    assert_eq!(
        row.get::<_, i64>(2),
        1,
        "the journal says it was not an error"
    );
    assert_eq!(
        row.get::<_, i64>(3),
        0,
        "no `on_error` route or fallback ever claims an unknown outcome"
    );
    assert_eq!(
        row.get::<_, i64>(4),
        1,
        "the one authorization is never rotated or duplicated"
    );
    connection.abort();

    executor.release.notify_one();
    first_worker.abort();
    database.drop().await;
}

/// The other way a worker can be lost: nobody takes the lease over inside the
/// grace at all. The activity is late rather than failed, and the engine still
/// has no idea whether the send happened — so the outcome is the same unknown,
/// not the `timeout` an activity with no authorized send would report.
#[tokio::test]
async fn a_late_at_most_once_activity_with_an_authorized_send_is_unknown_not_timed_out() {
    let database = TestDatabase::create("process_activity_at_most_once_late").await;
    let metadata = at_most_once_metadata();
    let executor = Arc::new(StalledExecutor::new());
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_at_most_once_start(&database.url, &revision).await;
    match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { .. } => {}
        other => panic!("expected a new Process instance, got {other:?}"),
    }
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };

    let runtime = Arc::new(runtime);
    let first_worker = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.consume_one_activity().await }
    });
    executor.started.notified().await;

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("the at-most-once clock is mutable by the test");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET lease_expires_at = statement_timestamp() - interval '1 millisecond',
                start_to_close_deadline =
                    statement_timestamp() - interval '6 seconds',
                available_at = statement_timestamp()
            WHERE source_name = 'default' AND id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("test pushes the activity past start_to_close and its takeover grace");

    let resolved = runtime
        .consume_one_activity()
        .await
        .expect("the late activity resolves");
    assert!(
        matches!(resolved, ActivityConsumption::Failed { .. }),
        "expected a terminal outcome, got {resolved:?}"
    );
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the unknown outcome routes"),
        TransitionConsumption::Advanced { .. }
    ));
    let row = client
        .query_one(
            "
            SELECT
                instance.current_state,
                job.last_error_json->>'code'
            FROM donat.process_activity_jobs job
            JOIN donat.process_instances instance
              ON instance.source_name = job.source_name
             AND instance.id = job.instance_id
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("the late outcome is inspectable");
    assert_eq!(row.get::<_, String>(0), "unknown");
    assert_eq!(
        row.get::<_, Option<String>>(1).as_deref(),
        Some("provider_send_ambiguous"),
        "a lost worker that had authorized its send is never reported as a timeout"
    );
    connection.abort();

    executor.release.notify_one();
    first_worker.abort();
    database.drop().await;
}

/// An executor that answers one fixed failure, so a test can say exactly what
/// the provider seam reported and nothing else.
struct FixedFailureExecutor {
    calls: AtomicUsize,
    class: ConnectorErrorClass,
    code: &'static str,
}

impl FixedFailureExecutor {
    fn new(class: ConnectorErrorClass, code: &'static str) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            class,
            code,
        }
    }
}

impl ProcessActivityExecutor for FixedFailureExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(ConnectorFailure::new(
                self.class,
                self.code,
                "the fixed provider outcome for this test",
            ))
        })
    }
}

/// Run the at-most-once fixture to its terminal outcome under one worker that
/// survives its own request, and report the recorded error, the state the
/// instance took, and how the journal named the routing.
async fn at_most_once_single_worker_outcome(
    label: &str,
    class: ConnectorErrorClass,
    code: &'static str,
) -> (Json, String, i64, i64) {
    let database = TestDatabase::create(label).await;
    let metadata = at_most_once_metadata();
    let executor = Arc::new(FixedFailureExecutor::new(class, code));
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_at_most_once_start(&database.url, &revision).await;
    match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { .. } => {}
        other => panic!("expected a new Process instance, got {other:?}"),
    }
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };
    let consumed = runtime
        .consume_one_activity()
        .await
        .expect("the one worker completes the activity");
    assert!(
        matches!(consumed, ActivityConsumption::Failed { .. }),
        "an at-most-once activity retries nothing, so it is terminal: {consumed:?}"
    );
    assert_eq!(
        executor.calls.load(Ordering::Relaxed),
        1,
        "the one send is made exactly once"
    );
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the terminal outcome routes"),
        TransitionConsumption::Advanced { .. }
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("the at-most-once outcome is readable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT
                job.last_error_json,
                instance.current_state,
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = 'default'
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_ambiguous_routed'),
                (SELECT count(*)
                 FROM donat.process_transition_logs log
                 WHERE log.source_name = 'default'
                   AND log.activity_job_id = job.id
                   AND log.outcome = 'activity_error_routed')
            FROM donat.process_activity_jobs job
            JOIN donat.process_instances instance
              ON instance.source_name = job.source_name
             AND instance.id = job.instance_id
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("the outcome is inspectable");
    let outcome = (
        row.get::<_, Option<Json>>(0)
            .expect("a terminal activity has a safe error"),
        row.get::<_, String>(1),
        row.get::<_, i64>(2),
        row.get::<_, i64>(3),
    );
    connection.abort();
    database.drop().await;
    outcome
}

/// ADR 063, the case a surviving worker produces. The send authorization is
/// committed before the request leaves, so a transport-shaped failure that comes
/// back to the *original* worker is exactly as unknown as one a takeover finds:
/// the engine cannot tell a request that never left from one whose answer was
/// lost. Reporting `timeout` here would tell the Process the mail was not sent,
/// and the `on_ambiguous` route the compiler forces an operator to declare would
/// be unreachable in the only case that usually happens.
#[tokio::test]
async fn a_lost_answer_is_unknown_even_when_the_at_most_once_worker_survives() {
    for (label, class, code) in [
        (
            "process_activity_amo_timeout",
            ConnectorErrorClass::Timeout,
            "connector_timeout",
        ),
        (
            "process_activity_amo_transport",
            ConnectorErrorClass::Transport,
            "connector_transport",
        ),
        (
            "process_activity_amo_5xx",
            ConnectorErrorClass::Http5xx,
            "provider_unavailable",
        ),
        (
            "process_activity_amo_429",
            ConnectorErrorClass::Http429,
            "provider_throttled",
        ),
    ] {
        let (error, state, ambiguous_routed, error_routed) =
            at_most_once_single_worker_outcome(label, class, code).await;
        assert_eq!(
            error.get("code").and_then(Json::as_str),
            Some("provider_send_ambiguous"),
            "{code} left the outcome unknown, so it is not reported as a failure"
        );
        // ADR 031: the cause a durable outcome was reclassified from is still
        // the thing an operator has to read.
        assert_eq!(
            error
                .get("caused_by")
                .and_then(|cause| cause.get("code"))
                .and_then(Json::as_str),
            Some(code),
            "the unknown outcome still names what stopped the send"
        );
        assert_eq!(state, "unknown", "{code} took the declared ambiguous route");
        assert_eq!(ambiguous_routed, 1);
        assert_eq!(
            error_routed, 0,
            "no `on_error` route or fallback ever claims an unknown outcome"
        );
    }
}

/// The other half of the same rule, and the reason it is not "every failure is
/// unknown": a provider that answered and refused told the engine what happened,
/// and a Process that declared a route for that class must still get it.
#[tokio::test]
async fn an_at_most_once_failure_the_provider_answered_keeps_its_class() {
    for (label, class, code) in [
        (
            "process_activity_amo_validation",
            ConnectorErrorClass::Validation,
            "provider_rejected",
        ),
        (
            "process_activity_amo_auth",
            ConnectorErrorClass::Authentication,
            "provider_unauthorized",
        ),
        (
            "process_activity_amo_permanent",
            ConnectorErrorClass::Permanent,
            "provider_refused",
        ),
    ] {
        let (error, state, ambiguous_routed, error_routed) =
            at_most_once_single_worker_outcome(label, class, code).await;
        assert_eq!(
            error.get("code").and_then(Json::as_str),
            Some(code),
            "{code} is the provider's own answer and keeps it"
        );
        assert_eq!(state, "refused", "{code} took its declared error route");
        assert_eq!(ambiguous_routed, 0);
        assert_eq!(error_routed, 1);
    }
}

/// An executor whose provider answered, and whose answer the engine refuses.
struct AnsweredButUnusableExecutor {
    calls: AtomicUsize,
}

impl ProcessActivityExecutor for AnsweredButUnusableExecutor {
    fn execute<'a>(
        &'a self,
        _instance: &'a str,
        _operation: &'a str,
        _input: Json,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ConnectorSuccess {
                output: json!({}),
                request_fingerprint: "not-the-fingerprint-this-activity-sent".to_owned(),
            })
        })
    }
}

/// The third position an at-most-once send can end in: the provider answered,
/// and the engine refused the answer. The mail went out, so the one thing the
/// Process must not be told is that it did not — an `invariant` on `on_error` is
/// where a compensating re-send lives.
#[tokio::test]
async fn an_at_most_once_send_the_engine_could_not_read_is_not_reported_as_unsent() {
    let database = TestDatabase::create("process_activity_amo_answered").await;
    let metadata = at_most_once_metadata();
    let executor = Arc::new(AnsweredButUnusableExecutor {
        calls: AtomicUsize::new(0),
    });
    let (runtime, revision) = runtime(&database, &metadata, Some(executor.clone())).await;
    seed_at_most_once_start(&database.url, &revision).await;
    match runtime
        .consume_one_start()
        .await
        .expect("Process start consumes")
    {
        StartConsumption::Started { .. } => {}
        other => panic!("expected a new Process instance, got {other:?}"),
    }
    let activity_job_id = match runtime
        .consume_one_transition()
        .await
        .expect("request activity schedules")
    {
        TransitionConsumption::ActivityScheduled {
            activity_job_id, ..
        } => activity_job_id,
        other => panic!("expected an activity schedule, got {other:?}"),
    };
    let consumed = runtime
        .consume_one_activity()
        .await
        .expect("the unusable answer is journaled");
    assert!(
        matches!(consumed, ActivityConsumption::Failed { .. }),
        "expected a terminal outcome, got {consumed:?}"
    );
    assert!(matches!(
        runtime
            .consume_one_transition()
            .await
            .expect("the outcome routes"),
        TransitionConsumption::Advanced { .. }
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("the outcome is readable");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(
            "
            SELECT job.last_error_json, instance.current_state
            FROM donat.process_activity_jobs job
            JOIN donat.process_instances instance
              ON instance.source_name = job.source_name
             AND instance.id = job.instance_id
            WHERE job.source_name = 'default' AND job.id = $1
            ",
            &[&activity_job_id],
        )
        .await
        .expect("the outcome is inspectable");
    let error: Json = row
        .get::<_, Option<Json>>(0)
        .expect("a terminal activity has a safe error");
    assert_eq!(
        error.get("code").and_then(Json::as_str),
        Some("provider_send_ambiguous")
    );
    assert_eq!(
        error
            .get("caused_by")
            .and_then(|cause| cause.get("code"))
            .and_then(Json::as_str),
        Some("connector_invariant"),
        "the connector defect that produced the unknown outcome is still named"
    );
    assert_eq!(row.get::<_, String>(1), "unknown");
    connection.abort();
    database.drop().await;
}
