use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_catalog::Catalog;
use donat_metadata::Metadata;
use donat_schema::{CompiledCommandCatalog, FinalizedCommandCatalog};
use donat_server::connectors::ConnectorRegistry;
use donat_server::migrate::run_migrate;
use donat_server::processes::{
    DeployedSourceProcessCatalog, ProcessRuntime, StartConsumption, build_process_runtime,
    reconcile, validate_serving_catalogs,
};
use donat_server::state::{SourceRuntime, compile_pure_engine_candidate};
use serde_json::{Value as Json, json};
use tokio_postgres::NoTls;
use uuid::Uuid;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

const ORDER_ID: &str = "550e8400-e29b-41d4-a716-446655440110";
const REQUEST_ID: &str = "550e8400-e29b-41d4-a716-446655440112";

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
            .expect("stale Process start database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("Process start database creates");
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

    async fn drop(self) {
        let (client, connection) = tokio_postgres::connect(&self.admin_url, NoTls)
            .await
            .expect("Postgres admin database is available for cleanup");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE);", self.name))
            .await
            .expect("Process start database drops");
        connection.abort();
    }
}

fn process_metadata(source_name: &str, terminal_value: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": source_name,
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": "postgres://unused"
                }
            },
            "tables": []
        }],
        "processes": [{
            "name": "checkout",
            "kind": "process",
            "version": 1,
            "source": source_name,
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
                    "values": {
                        "status": { "literal": terminal_value }
                    }
                }
            }]
        }]
    }))
    .expect("minimal Process metadata deserializes")
}

fn numeric_process_metadata(source_name: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": source_name,
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": "postgres://unused"
                }
            },
            "tables": []
        }],
        "processes": [{
            "name": "checkout",
            "kind": "process",
            "version": 1,
            "source": source_name,
            "permissions": [{ "role": "customer" }],
            "input": [
                { "name": "amount", "type": "decimal!" },
                { "name": "zero", "type": "decimal!" },
                { "name": "quantities", "type": "[int64!]!" },
                { "name": "details", "type": "json!" }
            ],
            "output": [{ "name": "status", "type": "string!" }],
            "start_at": "done",
            "states": [{
                "id": "done",
                "output": {
                    "values": {
                        "status": { "literal": "ready" }
                    }
                }
            }]
        }]
    }))
    .expect("numeric Process metadata deserializes")
}

fn catalogs(source_name: &str) -> HashMap<String, Catalog> {
    HashMap::from([(source_name.to_owned(), Catalog::default())])
}

async fn reconcile_definition(
    database_url: &str,
    metadata: &Metadata,
    source_name: &str,
) -> String {
    let connectors = ConnectorRegistry::empty();
    let source_catalogs = catalogs(source_name);
    let candidate = compile_pure_engine_candidate(metadata, &source_catalogs, &connectors, true)
        .expect("Process candidate compiles");
    let process = candidate
        .process_catalog
        .source(source_name)
        .and_then(|source| source.process("checkout"))
        .expect("compiled checkout Process");
    let revision = process.revision_fingerprint.clone();
    reconcile(
        source_name,
        database_url,
        source_catalogs
            .get(source_name)
            .expect("source catalog exists"),
        candidate
            .process_catalog
            .source(source_name)
            .expect("compiled source Process catalog exists"),
    )
    .await
    .expect("Process definition reconciles");
    revision
}

async fn runtime_from_snapshot(
    database_url: &str,
    metadata: &Metadata,
    source_name: &str,
) -> ProcessRuntime {
    let connectors = Arc::new(ConnectorRegistry::empty());
    let source_catalogs = catalogs(source_name);
    let candidate =
        compile_pure_engine_candidate(metadata, &source_catalogs, connectors.as_ref(), true)
            .expect("Process candidate compiles");
    let source_runtime =
        SourceRuntime::postgres(database_url).expect("Postgres Process source constructs");
    let deployed = validate_serving_catalogs(
        &HashMap::from([(source_name.to_owned(), source_runtime.clone())]),
        metadata,
        candidate.rule_catalog(),
        candidate.process_catalog.as_ref(),
        candidate.command_catalog.as_ref(),
        connectors.as_ref(),
    )
    .await
    .expect("deployed Process snapshot validates");
    build_process_runtime(
        source_name,
        &source_runtime,
        Arc::new(
            deployed
                .source(source_name)
                .expect("deployed source Process catalog exists")
                .clone(),
        ),
        candidate.command_catalog,
        candidate.finalized_command_catalog,
        connectors,
    )
    .expect("Process runtime builds")
}

#[allow(clippy::too_many_arguments)]
async fn insert_start_request(
    database_url: &str,
    source_name: &str,
    revision: &str,
    request_id: Uuid,
    command_invocation_id: Uuid,
    effect_position: i32,
    idempotency_key: &str,
    input: &Json,
) {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process database is available");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            INSERT INTO donat.process_start_requests (
                source_name,
                id,
                process_name,
                revision,
                input_json,
                command_invocation_id,
                effect_position,
                idempotency_key,
                status
            )
            VALUES ($1, $2, 'checkout', $3, $4, $5, $6, $7, 'pending')
            ",
            &[
                &source_name,
                &request_id,
                &revision,
                &input,
                &command_invocation_id,
                &effect_position,
                &idempotency_key,
            ],
        )
        .await
        .expect("pending Process start request inserts");
    connection.abort();
}

#[allow(clippy::too_many_arguments)]
async fn insert_start_request_json_text(
    database_url: &str,
    source_name: &str,
    revision: &str,
    request_id: Uuid,
    command_invocation_id: Uuid,
    effect_position: i32,
    idempotency_key: &str,
    input_json: &str,
) {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("Process database is available");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            INSERT INTO donat.process_start_requests (
                source_name,
                id,
                process_name,
                revision,
                input_json,
                command_invocation_id,
                effect_position,
                idempotency_key,
                status
            )
            VALUES ($1, $2, 'checkout', $3, $4::text::jsonb, $5, $6, $7, 'pending')
            ",
            &[
                &source_name,
                &request_id,
                &revision,
                &input_json,
                &command_invocation_id,
                &effect_position,
                &idempotency_key,
            ],
        )
        .await
        .expect("exact numeric Process start request inserts");
    connection.abort();
}

fn valid_input() -> Json {
    json!({
        "order_id": ORDER_ID,
        "request_id": REQUEST_ID
    })
}

#[test]
fn process_runtime_rejects_non_postgres_source() {
    let error = build_process_runtime(
        "analytics",
        &SourceRuntime::Clickhouse {
            url: "http://clickhouse.invalid".to_owned(),
        },
        Arc::new(DeployedSourceProcessCatalog::default()),
        Arc::new(CompiledCommandCatalog::default()),
        Arc::new(FinalizedCommandCatalog::default()),
        Arc::new(ConnectorRegistry::empty()),
    )
    .err()
    .expect("non-Postgres Process runtime must be rejected");
    assert!(
        error
            .to_string()
            .contains("Process source `analytics` must use Postgres"),
        "unexpected constructor error: {error:#}"
    );
}

#[test]
fn process_runtime_rejects_cross_source_deployed_catalog() {
    let metadata = process_metadata("secondary", "ready");
    let connectors = Arc::new(ConnectorRegistry::empty());
    let source_catalogs = catalogs("secondary");
    let candidate =
        compile_pure_engine_candidate(&metadata, &source_catalogs, connectors.as_ref(), true)
            .expect("secondary Process candidate compiles");
    let definition = candidate
        .process_catalog
        .source("secondary")
        .and_then(|source| source.process("checkout"))
        .expect("secondary checkout Process compiles")
        .clone();
    let deployed = DeployedSourceProcessCatalog {
        active: BTreeMap::from([("checkout".to_owned(), Arc::new(definition))]),
        live_retired: BTreeMap::new(),
    };
    let source_runtime =
        SourceRuntime::postgres("postgresql://postgres:postgres@127.0.0.1:1/not_connected")
            .expect("lazy Postgres pool constructs without connecting");

    let error = build_process_runtime(
        "default",
        &source_runtime,
        Arc::new(deployed),
        candidate.command_catalog,
        candidate.finalized_command_catalog,
        connectors,
    )
    .err()
    .expect("cross-source deployed Process catalog must be rejected");
    assert!(
        error
            .to_string()
            .contains("deployed Process `secondary.checkout`"),
        "unexpected cross-source constructor error: {error:#}"
    );
}

#[tokio::test]
async fn process_start_request_pins_revision() {
    let database = TestDatabase::create("process_start_revision").await;
    let first_metadata = process_metadata("default", "revision-a");
    let first_revision = reconcile_definition(&database.url, &first_metadata, "default").await;
    let request_id = Uuid::from_u128(0x1001);
    insert_start_request(
        &database.url,
        "default",
        &first_revision,
        request_id,
        Uuid::from_u128(0x2001),
        0,
        REQUEST_ID,
        &valid_input(),
    )
    .await;

    let replacement_metadata = process_metadata("default", "revision-b");
    let replacement_revision =
        reconcile_definition(&database.url, &replacement_metadata, "default").await;
    assert_ne!(first_revision, replacement_revision);
    let runtime = runtime_from_snapshot(&database.url, &replacement_metadata, "default").await;

    let outcome = runtime
        .consume_one_start()
        .await
        .expect("pinned start request consumes");
    let StartConsumption::Started {
        request_id: consumed_request,
        instance_id,
    } = outcome
    else {
        panic!("expected a started instance, got {outcome:?}");
    };
    assert_eq!(consumed_request, request_id);

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for assertions");
    let connection = tokio::spawn(connection);
    let instance = client
        .query_one(
            "
            SELECT revision, current_state, input_json
            FROM donat.process_instances
            WHERE source_name = 'default' AND id = $1
            ",
            &[&instance_id],
        )
        .await
        .expect("started instance is durable");
    assert_eq!(instance.get::<_, String>(0), first_revision);
    assert_eq!(instance.get::<_, String>(1), "done");
    assert_eq!(instance.get::<_, Json>(2), valid_input());
    let history = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = $1
                   AND revision = $2 AND kind = 'start' AND status = 'pending'),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = $1
                   AND definition_revision = $2 AND outcome = 'started'),
                (SELECT status FROM donat.process_start_requests
                 WHERE source_name = 'default' AND id = $3)
            ",
            &[&instance_id, &first_revision, &request_id],
        )
        .await
        .expect("start history is durable");
    assert_eq!(history.get::<_, i64>(0), 1);
    assert_eq!(history.get::<_, i64>(1), 1);
    assert_eq!(history.get::<_, String>(2), "consumed");
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_start_semantic_dedupe_is_separate() {
    let database = TestDatabase::create("process_start_dedupe").await;
    let metadata = process_metadata("default", "ready");
    let revision = reconcile_definition(&database.url, &metadata, "default").await;
    let first_request_id = Uuid::from_u128(0x1101);
    let duplicate_request_id = Uuid::from_u128(0x1102);
    insert_start_request(
        &database.url,
        "default",
        &revision,
        first_request_id,
        Uuid::from_u128(0x2101),
        0,
        "same-semantic-key",
        &valid_input(),
    )
    .await;
    insert_start_request(
        &database.url,
        "default",
        &revision,
        duplicate_request_id,
        Uuid::from_u128(0x2102),
        0,
        "same-semantic-key",
        &valid_input(),
    )
    .await;
    let runtime = runtime_from_snapshot(&database.url, &metadata, "default").await;

    let first = runtime
        .consume_one_start()
        .await
        .expect("first semantic start consumes");
    let StartConsumption::Started { instance_id, .. } = first else {
        panic!("expected first request to start, got {first:?}");
    };
    let second = runtime
        .consume_one_start()
        .await
        .expect("duplicate semantic start consumes");
    assert_eq!(
        second,
        StartConsumption::Duplicate {
            request_id: duplicate_request_id,
            instance_id,
        }
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for assertions");
    let connection = tokio::spawn(connection);
    let counts = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_instances
                 WHERE source_name = 'default' AND process_name = 'checkout'
                   AND start_idempotency_key = 'same-semantic-key'),
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = $1
                   AND kind = 'start'),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = $1
                   AND outcome = 'started'),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = $1
                   AND outcome = 'duplicate_start')
            ",
            &[&instance_id],
        )
        .await
        .expect("semantic dedupe state is inspectable");
    assert_eq!(counts.get::<_, i64>(0), 1);
    assert_eq!(counts.get::<_, i64>(1), 1);
    assert_eq!(counts.get::<_, i64>(2), 1);
    assert_eq!(counts.get::<_, i64>(3), 1);
    let statuses = client
        .query(
            "
            SELECT id, status, instance_id
            FROM donat.process_start_requests
            WHERE source_name = 'default' AND id = ANY($1)
            ORDER BY id
            ",
            &[&vec![first_request_id, duplicate_request_id]],
        )
        .await
        .expect("both start request outcomes are durable");
    assert_eq!(statuses.len(), 2);
    for row in statuses {
        let row_id: Uuid = row.get(0);
        let expected_status = if row_id == first_request_id {
            "consumed"
        } else {
            "duplicate"
        };
        assert_eq!(row.get::<_, String>(1), expected_status);
        assert_eq!(row.get::<_, Uuid>(2), instance_id);
    }
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_start_crash_before_commit_retries() {
    let database = TestDatabase::create("process_start_before_commit").await;
    let metadata = process_metadata("default", "ready");
    let revision = reconcile_definition(&database.url, &metadata, "default").await;
    let request_id = Uuid::from_u128(0x1201);
    insert_start_request(
        &database.url,
        "default",
        &revision,
        request_id,
        Uuid::from_u128(0x2201),
        0,
        REQUEST_ID,
        &valid_input(),
    )
    .await;
    let runtime = Arc::new(runtime_from_snapshot(&database.url, &metadata, "default").await);

    let (mut blocker, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for lock injection");
    let connection = tokio::spawn(connection);
    let transaction = blocker
        .transaction()
        .await
        .expect("blocking transaction starts");
    transaction
        .batch_execute("LOCK TABLE donat.process_transition_logs IN ACCESS EXCLUSIVE MODE")
        .await
        .expect("transition history is blocked before worker claim");

    let worker_runtime = Arc::clone(&runtime);
    let worker = tokio::spawn(async move { worker_runtime.consume_one_start().await });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let waiting: bool = transaction
            .query_one(
                "
                SELECT EXISTS (
                    SELECT 1
                    FROM pg_locks
                    WHERE relation = 'donat.process_transition_logs'::regclass
                      AND mode = 'RowExclusiveLock'
                      AND NOT granted
                )
                ",
                &[],
            )
            .await
            .expect("blocked worker lock is observable")
            .get(0);
        if waiting {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Process worker did not reach the pre-commit history write"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    worker.abort();
    assert!(
        worker
            .await
            .expect_err("aborted pre-commit worker cannot return an outcome")
            .is_cancelled(),
        "worker task must stop at the injected pre-commit crash"
    );
    transaction
        .rollback()
        .await
        .expect("history blocker releases after the simulated crash");

    let retry_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let retried = loop {
        match runtime
            .consume_one_start()
            .await
            .expect("rolled-back pending request remains valid")
        {
            StartConsumption::NoWork => {
                assert!(
                    tokio::time::Instant::now() < retry_deadline,
                    "aborted start transaction did not release its pending request"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            outcome => break outcome,
        }
    };
    assert!(matches!(
        retried,
        StartConsumption::Started {
            request_id: consumed,
            ..
        } if consumed == request_id
    ));
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_start_crash_after_commit_does_not_duplicate() {
    let database = TestDatabase::create("process_start_after_commit").await;
    let metadata = process_metadata("default", "ready");
    let revision = reconcile_definition(&database.url, &metadata, "default").await;
    let request_id = Uuid::from_u128(0x1301);
    insert_start_request(
        &database.url,
        "default",
        &revision,
        request_id,
        Uuid::from_u128(0x2301),
        0,
        REQUEST_ID,
        &valid_input(),
    )
    .await;
    let runtime = runtime_from_snapshot(&database.url, &metadata, "default").await;

    let started = runtime
        .consume_one_start()
        .await
        .expect("start transaction commits");
    let StartConsumption::Started { instance_id, .. } = started else {
        panic!("expected committed start, got {started:?}");
    };
    assert_eq!(
        runtime
            .consume_one_start()
            .await
            .expect("post-commit retry finds no pending request"),
        StartConsumption::NoWork
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for assertions");
    let connection = tokio::spawn(connection);
    let counts = client
        .query_one(
            "
            SELECT
                (SELECT count(*) FROM donat.process_instances
                 WHERE source_name = 'default' AND id = $1),
                (SELECT count(*) FROM donat.process_events
                 WHERE source_name = 'default' AND instance_id = $1),
                (SELECT count(*) FROM donat.process_transition_logs
                 WHERE source_name = 'default' AND instance_id = $1)
            ",
            &[&instance_id],
        )
        .await
        .expect("post-commit Process state is inspectable");
    assert_eq!(counts.get::<_, i64>(0), 1);
    assert_eq!(counts.get::<_, i64>(1), 1);
    assert_eq!(counts.get::<_, i64>(2), 1);
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_start_refuses_missing_deployed_revision() {
    let database = TestDatabase::create("process_start_missing_revision").await;
    let metadata = process_metadata("default", "ready");
    reconcile_definition(&database.url, &metadata, "default").await;
    let runtime = runtime_from_snapshot(&database.url, &metadata, "default").await;
    let missing_revision = "not-in-the-published-engine-snapshot";

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available");
    let connection = tokio::spawn(connection);
    client
        .execute(
            "
            INSERT INTO donat.process_definition_versions (
                source_name,
                process_name,
                revision,
                canonical_definition,
                dependency_descriptors,
                runtime_abi,
                status
            )
            VALUES ('default', 'checkout', $1, '{}'::jsonb, '[]'::jsonb, 1, 'retired')
            ",
            &[&missing_revision],
        )
        .await
        .expect("database-only retired revision inserts");
    connection.abort();
    let request_id = Uuid::from_u128(0x1401);
    insert_start_request(
        &database.url,
        "default",
        missing_revision,
        request_id,
        Uuid::from_u128(0x2401),
        0,
        REQUEST_ID,
        &valid_input(),
    )
    .await;

    let error = runtime
        .consume_one_start()
        .await
        .expect_err("worker must not fall back to current metadata");
    let rendered = error.to_string();
    assert!(rendered.contains("default.checkout"), "{error:#}");
    assert!(rendered.contains(missing_revision), "{error:#}");

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for assertions");
    let connection = tokio::spawn(connection);
    let state = client
        .query_one(
            "
            SELECT status,
                   (SELECT count(*) FROM donat.process_instances
                    WHERE source_name = 'default')
            FROM donat.process_start_requests
            WHERE source_name = 'default' AND id = $1
            ",
            &[&request_id],
        )
        .await
        .expect("refused request remains inspectable");
    assert_eq!(state.get::<_, String>(0), "pending");
    assert_eq!(state.get::<_, i64>(1), 0);
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_start_rejects_invalid_typed_input_without_consuming() {
    let database = TestDatabase::create("process_start_invalid_input").await;
    let metadata = process_metadata("default", "ready");
    let revision = reconcile_definition(&database.url, &metadata, "default").await;
    let runtime = runtime_from_snapshot(&database.url, &metadata, "default").await;
    let request_id = Uuid::from_u128(0x1501);
    insert_start_request(
        &database.url,
        "default",
        &revision,
        request_id,
        Uuid::from_u128(0x2501),
        0,
        REQUEST_ID,
        &json!({
            "order_id": "not-a-canonical-uuid",
            "request_id": REQUEST_ID
        }),
    )
    .await;

    let error = runtime
        .consume_one_start()
        .await
        .expect_err("invalid typed Process input must be rejected");
    assert!(
        error.to_string().contains("input"),
        "unexpected input validation error: {error:#}"
    );

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for assertions");
    let connection = tokio::spawn(connection);
    let state = client
        .query_one(
            "
            SELECT status,
                   (SELECT count(*) FROM donat.process_instances
                    WHERE source_name = 'default')
            FROM donat.process_start_requests
            WHERE source_name = 'default' AND id = $1
            ",
            &[&request_id],
        )
        .await
        .expect("invalid request remains inspectable");
    assert_eq!(state.get::<_, String>(0), "pending");
    assert_eq!(state.get::<_, i64>(1), 0);
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_start_preserves_exact_numeric_input() {
    let database = TestDatabase::create("process_start_exact_numbers").await;
    let metadata = numeric_process_metadata("default");
    let revision = reconcile_definition(&database.url, &metadata, "default").await;
    let runtime = runtime_from_snapshot(&database.url, &metadata, "default").await;
    let request_id = Uuid::from_u128(0x1551);
    insert_start_request_json_text(
        &database.url,
        "default",
        &revision,
        request_id,
        Uuid::from_u128(0x2551),
        0,
        "exact-numeric-input",
        r#"{
            "amount": 123456789012345678901234567890.125,
            "zero": 0.0,
            "quantities": [-2, 3],
            "details": {"nested": [true, null]}
        }"#,
    )
    .await;

    let started = runtime
        .consume_one_start()
        .await
        .expect("valid exact numeric Process input consumes");
    let StartConsumption::Started { instance_id, .. } = started else {
        panic!("expected exact numeric input to start, got {started:?}");
    };

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for exact-number assertions");
    let connection = tokio::spawn(connection);
    let values = client
        .query_one(
            "
            SELECT
                request.input_json::text,
                instance.input_json::text,
                event.payload_json::text
            FROM donat.process_start_requests request
            JOIN donat.process_instances instance
              ON instance.source_name = request.source_name
             AND instance.source_request_id = request.id
            JOIN donat.process_events event
              ON event.source_name = instance.source_name
             AND event.instance_id = instance.id
             AND event.kind = 'start'
            WHERE request.source_name = 'default'
              AND request.id = $1
              AND instance.id = $2
            ",
            &[&request_id, &instance_id],
        )
        .await
        .expect("exact numeric input is durable in every start record");
    let outbox_json: String = values.get(0);
    assert_eq!(values.get::<_, String>(1), outbox_json);
    assert_eq!(values.get::<_, String>(2), outbox_json);
    assert!(
        outbox_json.contains("123456789012345678901234567890.125"),
        "large decimal changed at rest: {outbox_json}"
    );
    assert!(
        outbox_json.contains("\"zero\": 0.0"),
        "decimal scale changed at rest: {outbox_json}"
    );
    connection.abort();

    database.drop().await;
}

#[tokio::test]
async fn process_workers_are_source_local() {
    let database = TestDatabase::create("process_start_sources").await;
    let default_metadata = process_metadata("default", "default-ready");
    let secondary_metadata = process_metadata("secondary", "secondary-ready");
    let default_revision = reconcile_definition(&database.url, &default_metadata, "default").await;
    let secondary_revision =
        reconcile_definition(&database.url, &secondary_metadata, "secondary").await;
    let shared_request_id = Uuid::from_u128(0x1601);
    let shared_invocation_id = Uuid::from_u128(0x2601);
    insert_start_request(
        &database.url,
        "default",
        &default_revision,
        shared_request_id,
        shared_invocation_id,
        0,
        "shared-key",
        &valid_input(),
    )
    .await;
    insert_start_request(
        &database.url,
        "secondary",
        &secondary_revision,
        shared_request_id,
        shared_invocation_id,
        0,
        "shared-key",
        &valid_input(),
    )
    .await;
    let default_runtime = runtime_from_snapshot(&database.url, &default_metadata, "default").await;
    let secondary_runtime =
        runtime_from_snapshot(&database.url, &secondary_metadata, "secondary").await;

    let default_started = default_runtime
        .consume_one_start()
        .await
        .expect("default source request consumes");
    assert!(matches!(
        default_started,
        StartConsumption::Started {
            request_id,
            ..
        } if request_id == shared_request_id
    ));
    assert_eq!(
        default_runtime
            .consume_one_start()
            .await
            .expect("default source cannot claim secondary work"),
        StartConsumption::NoWork
    );
    let secondary_started = secondary_runtime
        .consume_one_start()
        .await
        .expect("secondary source request consumes");
    assert!(matches!(
        secondary_started,
        StartConsumption::Started {
            request_id,
            ..
        } if request_id == shared_request_id
    ));

    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("Process database is available for assertions");
    let connection = tokio::spawn(connection);
    let rows = client
        .query(
            "
            SELECT source_name, count(*)
            FROM donat.process_instances
            WHERE source_name IN ('default', 'secondary')
              AND process_name = 'checkout'
              AND start_idempotency_key = 'shared-key'
            GROUP BY source_name
            ORDER BY source_name
            ",
            &[],
        )
        .await
        .expect("source-local instances are inspectable");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<_, String>(0), "default");
    assert_eq!(rows[0].get::<_, i64>(1), 1);
    assert_eq!(rows[1].get::<_, String>(0), "secondary");
    assert_eq!(rows[1].get::<_, i64>(1), 1);
    connection.abort();

    database.drop().await;
}
