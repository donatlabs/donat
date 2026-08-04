use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_catalog::Catalog;
use donat_metadata::{Metadata, ProcessLifecycle};
use donat_schema::{CompiledCommandCatalog, compile_command_source_catalog};
use donat_server::connectors::ConnectorRegistry;
use donat_server::migrate::run_migrate;
use donat_server::processes::{
    CompiledProcessCatalog, CompiledSourceProcessCatalog, compile_process_source_catalog,
    reconcile, validate_serving_catalogs,
};
use donat_server::state::SourceRuntime;
use serde_json::json;
use tokio_postgres::NoTls;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

async fn isolated_database(label: &str) -> (String, String, String) {
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
        .expect("stale reconciliation database drops");
    client
        .batch_execute(&format!("CREATE DATABASE {name};"))
        .await
        .expect("reconciliation database creates");
    connection.abort();
    let prefix = admin_url
        .rsplit_once('/')
        .expect("Postgres URL has a database segment")
        .0
        .to_owned();
    (admin_url, name.clone(), format!("{prefix}/{name}"))
}

async fn drop_database(admin_url: &str, name: &str) {
    let (client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .expect("Postgres admin database is available for cleanup");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE);"))
        .await
        .expect("reconciliation database drops");
    connection.abort();
}

fn metadata(source: &str, terminal_value: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": source,
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
            "source": source,
            "permissions": [{ "role": "customer" }],
            "output": [{ "name": "status", "type": "string!" }],
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

fn compile_source(metadata: &Metadata, source_name: &str) -> CompiledSourceProcessCatalog {
    let catalog = Catalog::default();
    let rules = donat_rules::compile_catalog(&[], &[]).expect("empty Rule catalog compiles");
    let commands = compile_command_source_catalog(metadata, source_name, &catalog, &rules, true)
        .expect("empty source-local Command catalog compiles");
    compile_process_source_catalog(
        metadata,
        source_name,
        &commands,
        &rules,
        &ConnectorRegistry::empty(),
    )
    .expect("source-local Process catalog compiles")
}

async fn seed_running_instance(
    database_url: &str,
    source_name: &str,
    process_name: &str,
    revision: &str,
) {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .expect("reconciliation database is available");
    let connection = tokio::spawn(connection);
    let request_id: uuid::Uuid = client
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
            VALUES ($1, $2, $3, '{}'::jsonb, gen_random_uuid(), 0, 'seed', 'consumed')
            RETURNING id
            ",
            &[&source_name, &process_name, &revision],
        )
        .await
        .expect("seed start request inserts")
        .get(0);
    client
        .execute(
            "
            INSERT INTO donat.process_instances (
                source_name,
                process_name,
                revision,
                source_request_id,
                start_idempotency_key,
                status,
                current_state,
                input_json,
                state_json
            )
            VALUES ($1, $2, $3, $4, 'seed', 'running', 'done', '{}'::jsonb, '{}'::jsonb)
            ",
            &[&source_name, &process_name, &revision, &request_id],
        )
        .await
        .expect("seed running Process instance inserts");
    connection.abort();
}

#[tokio::test]
async fn process_reconcile_is_idempotent_and_preserves_live_retired_revision() {
    let (admin_url, database_name, database_url) = isolated_database("process_reconcile").await;
    run_migrate(&database_url, &migrations_dir())
        .await
        .expect("bundled migrations apply");
    let catalog = Catalog::default();

    let first_metadata = metadata("default", "first");
    let first = compile_source(&first_metadata, "default");
    reconcile("default", &database_url, &catalog, &first)
        .await
        .expect("first Process revision deploys");
    reconcile("default", &database_url, &catalog, &first)
        .await
        .expect("identical Process reconciliation is idempotent");
    let first_revision = first
        .process("checkout")
        .expect("first Process exists")
        .revision_fingerprint
        .clone();
    seed_running_instance(&database_url, "default", "checkout", &first_revision).await;

    let replacement_metadata = metadata("default", "replacement");
    let replacement = compile_source(&replacement_metadata, "default");
    let replacement_revision = replacement
        .process("checkout")
        .expect("replacement Process exists")
        .revision_fingerprint
        .clone();
    assert_ne!(first_revision, replacement_revision);
    reconcile("default", &database_url, &catalog, &replacement)
        .await
        .expect("replacement activates while the old revision remains live-retired");

    let (client, connection) = tokio_postgres::connect(&database_url, NoTls)
        .await
        .expect("reconciled database is available");
    let connection = tokio::spawn(connection);
    let rows = client
        .query(
            "
            SELECT revision, status
            FROM donat.process_definition_versions
            WHERE source_name = 'default' AND process_name = 'checkout'
            ORDER BY revision
            ",
            &[],
        )
        .await
        .expect("deployed revisions query succeeds")
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(rows.contains(&(first_revision.clone(), "retired".to_owned())));
    assert!(rows.contains(&(replacement_revision, "active".to_owned())));
    connection.abort();

    let rules = donat_rules::compile_catalog(&[], &[]).expect("empty Rule catalog compiles");
    let commands =
        compile_command_source_catalog(&replacement_metadata, "default", &catalog, &rules, true)
            .expect("replacement Command catalog compiles");
    let deployed = validate_serving_catalogs(
        &HashMap::from([(
            "default".to_owned(),
            SourceRuntime::postgres(&database_url)
                .expect("replacement Postgres runtime constructs"),
        )]),
        &replacement_metadata,
        &rules,
        &CompiledProcessCatalog::single_source("default", replacement.clone()),
        &CompiledCommandCatalog::single_source("default", commands),
        &ConnectorRegistry::empty(),
    )
    .await
    .expect("fresh serving loader validates active and live-retired revisions");
    let deployed = deployed
        .source("default")
        .expect("default deployed source exists");
    assert_eq!(
        deployed
            .active
            .get("checkout")
            .expect("replacement is active")
            .revision_fingerprint,
        replacement
            .process("checkout")
            .expect("replacement Process exists")
            .revision_fingerprint
    );
    assert!(
        deployed
            .live_retired
            .contains_key(&("checkout".to_owned(), first_revision.clone())),
        "the fresh loader must recompile the live retired revision"
    );

    let mut omitted_metadata = replacement_metadata;
    omitted_metadata.processes.clear();
    let omitted = compile_source(&omitted_metadata, "default");
    let error = reconcile("default", &database_url, &catalog, &omitted)
        .await
        .expect_err("omitting a process name with live retired work must fail");
    assert!(
        error.to_string().contains("non-terminal work exists"),
        "unexpected omission error: {error:#}"
    );

    drop_database(&admin_url, &database_name).await;
}

#[tokio::test]
async fn process_sources_sharing_one_database_reconcile_independently() {
    let (admin_url, database_name, database_url) = isolated_database("process_sources").await;
    run_migrate(&database_url, &migrations_dir())
        .await
        .expect("bundled migrations apply");
    let catalog = Catalog::default();

    let default_metadata = metadata("default", "default");
    let secondary_metadata = metadata("secondary", "secondary");
    let default = compile_source(&default_metadata, "default");
    let secondary = compile_source(&secondary_metadata, "secondary");
    reconcile("default", &database_url, &catalog, &default)
        .await
        .expect("default source reconciles");
    reconcile("secondary", &database_url, &catalog, &secondary)
        .await
        .expect("secondary source reconciles");

    let mut retired_metadata = default_metadata;
    retired_metadata.processes[0].lifecycle = ProcessLifecycle::Retired;
    let retired = compile_source(&retired_metadata, "default");
    reconcile("default", &database_url, &catalog, &retired)
        .await
        .expect("default source retires independently");

    let (client, connection) = tokio_postgres::connect(&database_url, NoTls)
        .await
        .expect("shared database is available");
    let connection = tokio::spawn(connection);
    let secondary_status: String = client
        .query_one(
            "
            SELECT status
            FROM donat.process_definition_versions
            WHERE source_name = 'secondary'
              AND process_name = 'checkout'
            ",
            &[],
        )
        .await
        .expect("secondary active revision remains")
        .get(0);
    assert_eq!(secondary_status, "active");
    connection.abort();

    drop_database(&admin_url, &database_name).await;
}
