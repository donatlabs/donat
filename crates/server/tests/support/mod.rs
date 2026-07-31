use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_catalog::Catalog;
use donat_metadata::Metadata;
use donat_server::connectors::ConnectorRegistry;
use donat_server::migrate::run_migrate;
use donat_server::processes::{
    ProcessPlanningSnapshot, ProcessRuntime, build_process_runtime, reconcile,
    validate_serving_catalogs,
};
use donat_server::state::{SourceRuntime, compile_pure_engine_candidate};
use serde_json::Value as Json;
use tokio_postgres::NoTls;
use uuid::Uuid;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations")
}

pub struct TestDatabase {
    admin_url: String,
    name: String,
    pub url: String,
}

impl TestDatabase {
    pub async fn create(label: &str) -> Self {
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
            .expect("stale Process wait database drops");
        client
            .batch_execute(&format!("CREATE DATABASE {name};"))
            .await
            .expect("Process wait database creates");
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
            .expect("Process wait database is available for introspection");
        let connection = tokio::spawn(connection);
        let catalog = donat_catalog::introspect(&client)
            .await
            .expect("Process wait catalog introspects");
        connection.abort();
        catalog
    }

    pub async fn runtime(
        &self,
        metadata: &Metadata,
        process_name: &str,
    ) -> (ProcessRuntime, String) {
        self.runtime_with_connectors(metadata, process_name, Arc::new(ConnectorRegistry::empty()))
            .await
    }

    pub async fn runtime_with_connectors(
        &self,
        metadata: &Metadata,
        process_name: &str,
        connectors: Arc<ConnectorRegistry>,
    ) -> (ProcessRuntime, String) {
        let catalog = self.catalog().await;
        let catalogs = HashMap::from([("default".to_owned(), catalog)]);
        let candidate =
            compile_pure_engine_candidate(metadata, &catalogs, connectors.as_ref(), true)
                .expect("Process wait candidate compiles");
        let process = candidate
            .process_catalog
            .source("default")
            .and_then(|catalog| catalog.process(process_name))
            .expect("Process wait definition compiles");
        let revision = process.revision_fingerprint.clone();
        reconcile(
            "default",
            &self.url,
            catalogs.get("default").expect("default catalog exists"),
            candidate
                .process_catalog
                .source("default")
                .expect("default Process catalog exists"),
        )
        .await
        .expect("Process wait definition reconciles");

        let source_runtime =
            SourceRuntime::postgres(&self.url).expect("Process source runtime constructs");
        let deployed = validate_serving_catalogs(
            &HashMap::from([("default".to_owned(), source_runtime.clone())]),
            metadata,
            candidate.rule_catalog(),
            candidate.process_catalog.as_ref(),
            candidate.command_catalog.as_ref(),
            connectors.as_ref(),
        )
        .await
        .expect("deployed Process wait catalog validates");
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
        .expect("Process wait runtime builds");
        (runtime, revision)
    }

    pub async fn seed_start(
        &self,
        process_name: &str,
        revision: &str,
        input: Json,
        idempotency_key: &str,
    ) -> Uuid {
        let (client, connection) = tokio_postgres::connect(&self.url, NoTls)
            .await
            .expect("Process wait database is available");
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
                    $1,
                    $2,
                    $3,
                    gen_random_uuid(),
                    0,
                    $4,
                    'pending'
                )
                RETURNING id
                ",
                &[&process_name, &revision, &input, &idempotency_key],
            )
            .await
            .expect("Process wait start request inserts")
            .get(0);
        connection.abort();
        request_id
    }

    pub async fn drop(self) {
        let (client, connection) = tokio_postgres::connect(&self.admin_url, NoTls)
            .await
            .expect("Postgres admin database is available for cleanup");
        let connection = tokio::spawn(connection);
        client
            .batch_execute(&format!("DROP DATABASE {} WITH (FORCE);", self.name))
            .await
            .expect("Process wait database drops");
        connection.abort();
    }
}
