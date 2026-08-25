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

    assert_eq!(metadata.commands.len(), 74);
    assert_eq!(candidate.process_catalog.len(), 11);
    assert_eq!(
        candidate
            .process_catalog
            .sources()
            .flat_map(|(_, source)| source.iter())
            .map(|(_, process)| process.states.len())
            .sum::<usize>(),
        171
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

/// Every relation an active Petshop command reads or writes. The schema side
/// of this list — that each exists in the catalog — is asserted beside the
/// tables, in `metadata/databases/default/tables/tables_test.yaml`.
const COMMAND_RELATIONS: &[&str] = &[
    "cart",
    "cart_checkout_context",
    "cart_price_candidate",
    "cart_pricing",
    "checkout_quote",
    "checkout_quote_line",
    "credit_usage",
    "customer_prescription_order_line",
    "exchange",
    "exchange_item",
    "grooming_booking",
    "grooming_booking_event",
    "inventory_allocation",
    "inventory_allocation_line",
    "inventory_backorder",
    "inventory_level",
    "inventory_reservation",
    "inventory_stock",
    "notification_delivery",
    "order_adjustment",
    "order_current_authorization",
    "order_inventory_allocation_candidate",
    "order_line",
    "order_return_context",
    "order_vendor_split_candidate",
    "orders",
    "organization",
    "organization_membership",
    "payment",
    "payment_authorization",
    "payment_capture",
    "payment_capture_claim",
    "payment_chargeback",
    "payment_event",
    "payment_fraud_decision",
    "payment_fraud_review",
    "payment_reconciliation",
    "payment_reconciliation_resolution",
    "payment_void",
    "prescription_event",
    "prescription_request",
    "prescription_review",
    "purchase_approval",
    "quote",
    "quote_line",
    "refund",
    "return_event",
    "return_inspection",
    "return_item",
    "return_refund_context",
    "return_request",
    "shipment",
    "shipment_item",
    "shipment_result",
    "subscription",
    "subscription_dunning_attempt",
    "subscription_renewal",
    "vendor_dispute",
    "vendor_membership",
    "vendor_order",
    "vendor_order_acceptance",
    "vendor_payout",
    "vendor_payout_candidate",
    "vendor_payout_event",
    "vendor_payout_reconciliation",
];

#[test]
fn command_relations_are_tracked_in_petshop_metadata() {
    let metadata = load_metadata_dir(&petshop_root().join("metadata")).unwrap();
    let default = metadata
        .sources
        .iter()
        .find(|source| source.name == "default")
        .expect("Petshop default source");
    let tracked = default
        .tables
        .iter()
        .map(|entry| entry.table.name().to_string())
        .collect::<BTreeSet<_>>();
    let expected = COMMAND_RELATIONS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<BTreeSet<_>>();
    let missing = expected.difference(&tracked).cloned().collect::<Vec<_>>();

    assert!(
        missing.is_empty(),
        "active Petshop command relations must be tracked; missing: {missing:?}"
    );
}

/// The tracked domain stands on its own: with every runtime section —
/// commands, rules, connectors, processes — stripped, the tables, their
/// permissions and relationships still compile against the real catalog.
#[tokio::test]
async fn tracked_petshop_domain_compiles_without_runtime_sections() {
    let (admin_url, database_name, database_url) = create_database("petshop_domain").await;
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

    let mut metadata = load_metadata_dir(&petshop_root().join("metadata"))
        .expect("complete Petshop metadata loads");
    metadata.commands.clear();
    metadata.rules = Default::default();
    metadata.connectors.clear();
    metadata.processes.clear();
    let connectors = ConnectorRegistry::build(&metadata).expect("an empty connector set compiles");
    let candidate = compile_pure_engine_candidate(
        &metadata,
        &HashMap::from([("default".to_owned(), catalog)]),
        &connectors,
        true,
    )
    .expect("the tracked Petshop domain compiles without its runtime sections");
    assert_eq!(candidate.process_catalog.len(), 0);

    drop_database(&admin_url, &database_name).await;
}
