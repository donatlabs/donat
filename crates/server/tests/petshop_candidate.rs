use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use donat_metadata::{ConnectorBaseUrl, load_metadata_dir};
use donat_schema::FinalizedCommandEffect;
use donat_server::connectors::ConnectorRegistry;
use donat_server::migrate::run_migrate;
use donat_server::state::compile_pure_engine_candidate;
use tokio_postgres::NoTls;

static NEXT_DATABASE: AtomicUsize = AtomicUsize::new(0);

fn postgres_admin_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_owned())
}

fn petshop_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop")
}

async fn create_database(label: &str) -> (String, String, String) {
    let admin_url = postgres_admin_url();
    let database_name = format!(
        "donat_{label}_{}_{}",
        std::process::id(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    );
    let (client, connection) = tokio_postgres::connect(&admin_url, NoTls)
        .await
        .expect("Postgres admin database is available");
    let connection = tokio::spawn(connection);
    client
        .execute(
            &format!("DROP DATABASE IF EXISTS {database_name} WITH (FORCE)"),
            &[],
        )
        .await
        .expect("stale isolated Petshop database drops");
    client
        .execute(&format!("CREATE DATABASE {database_name}"), &[])
        .await
        .expect("isolated Petshop database creates");
    connection.abort();

    let prefix = admin_url
        .rsplit_once('/')
        .expect("Postgres URL contains a database name")
        .0
        .to_owned();
    (
        admin_url,
        database_name.clone(),
        format!("{prefix}/{database_name}"),
    )
}

async fn drop_database(admin_url: &str, database_name: &str) {
    let (client, connection) = tokio_postgres::connect(admin_url, NoTls)
        .await
        .expect("Postgres admin database is available for cleanup");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(&format!("DROP DATABASE {database_name} WITH (FORCE);"))
        .await
        .expect("isolated Petshop database drops");
    connection.abort();
}

#[tokio::test]
async fn real_petshop_catalog_compiles_one_closed_candidate() {
    let (admin_url, database_name, database_url) = create_database("petshop_candidate").await;
    run_migrate(&database_url, &petshop_root().join("migrations"))
        .await
        .expect("all Petshop schema migrations apply");
    let (client, connection) = tokio_postgres::connect(&database_url, NoTls)
        .await
        .expect("isolated Petshop database is available");
    let connection = tokio::spawn(connection);
    let catalog = donat_catalog::introspect(&client)
        .await
        .expect("real Petshop catalog introspects");
    connection.abort();

    let metadata = load_metadata_dir(&petshop_root().join("metadata"))
        .expect("complete Petshop metadata loads");
    let mut registry_metadata = metadata.clone();
    for instance in &mut registry_metadata.connectors {
        instance.config.base_url = Some(ConnectorBaseUrl::Literal(format!(
            "https://{}.example.test",
            instance.name.replace('_', "-")
        )));
        instance.config.headers.clear();
    }
    let connectors =
        ConnectorRegistry::build(&registry_metadata).expect("Petshop connectors compile");
    let candidate = compile_pure_engine_candidate(
        &metadata,
        &HashMap::from([("default".to_owned(), catalog)]),
        &connectors,
        true,
    )
    .expect("real Petshop Commands, Processes, effects, and schema compile together");

    assert_eq!(metadata.commands.len(), 73);
    assert_eq!(candidate.process_catalog.len(), 11);
    assert_eq!(
        candidate
            .process_catalog
            .sources()
            .flat_map(|(_, source)| source.iter())
            .map(|(_, process)| process.states.len())
            .sum::<usize>(),
        168
    );

    let finalized_effects = candidate
        .finalized_command_catalog
        .sources
        .values()
        .flat_map(|source| source.commands.values())
        .flat_map(|command| &command.effects)
        .collect::<Vec<_>>();
    assert_eq!(finalized_effects.len(), 25);
    for effect in finalized_effects {
        let (source, process_name, revision) = match effect {
            FinalizedCommandEffect::Start(effect) => (
                effect.source.as_str(),
                effect.process_name.as_str(),
                effect.process_revision.as_str(),
            ),
            FinalizedCommandEffect::Signal(effect) => (
                effect.source.as_str(),
                effect.process_name.as_str(),
                effect.process_revision.as_str(),
            ),
        };
        let process = candidate
            .process_catalog
            .source(source)
            .and_then(|source| source.process(process_name))
            .expect("every finalized effect resolves source-locally");
        assert_eq!(revision, process.revision_fingerprint);
    }

    // Every declared Process must be reachable from the public API. A flow with
    // no Command that starts it is metadata nobody can execute.
    let started: BTreeSet<(&str, &str)> = candidate
        .finalized_command_catalog
        .sources
        .values()
        .flat_map(|source| source.commands.values())
        .flat_map(|command| &command.effects)
        .filter_map(|effect| match effect {
            FinalizedCommandEffect::Start(effect) => {
                Some((effect.source.as_str(), effect.process_name.as_str()))
            }
            FinalizedCommandEffect::Signal(_) => None,
        })
        .collect();
    for (source_name, source) in candidate.process_catalog.sources() {
        for (process_name, _) in source.iter() {
            assert!(
                started.contains(&(source_name, process_name)),
                "Process '{source_name}.{process_name}' has no Command that starts it"
            );
        }
    }

    for (source_name, source) in candidate.process_catalog.sources() {
        for (_, process) in source.iter() {
            for ((pinned_source, instance, operation_id), pinned) in
                &process.dependencies.connector_operations
            {
                assert_eq!(pinned_source, source_name);
                let registry_spec = connectors
                    .operation_spec_handle(source_name, instance, *operation_id)
                    .expect("pinned Process operation remains in the deployment registry");
                assert!(
                    Arc::ptr_eq(&registry_spec, &pinned.spec),
                    "Process compilation must retain the catalog-owned OperationSpec"
                );
            }
        }
    }

    let schema = candidate
        .compiled
        .as_deref()
        .expect("one immutable serving schema is produced");
    for command in &metadata.commands {
        let pre_process = candidate
            .command_catalog
            .source(&command.source)
            .and_then(|source| source.command(&command.name))
            .expect("pre-process command exists");
        let serving = schema
            .command_catalog()
            .source(&command.source)
            .and_then(|source| source.command(&command.name))
            .expect("the serving schema retains every command");
        assert_eq!(
            pre_process.descriptor().definition_fingerprint,
            serving.descriptor().definition_fingerprint
        );
    }

    drop(candidate);
    drop(connectors);
    drop_database(&admin_url, &database_name).await;
}
