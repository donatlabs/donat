//! The generator against the schema it was written for.
//!
//! The unit tests in `donat-schema` drive fixtures, and fixtures are what got
//! three of its rules wrong: each was right about the one example it was
//! written from and wrong about the sixty tables Petshop actually has. This
//! runs it against those sixty, which is the only way the earlier mistakes
//! would have been caught before a person applied the output.
//!
//! Needs a Postgres. `PG_URL` names it; the conformance default is assumed
//! otherwise. Skipped rather than failed when there is none, because a unit
//! suite that cannot run is not a red build.

use std::path::{Path, PathBuf};

use donat_schema::tenancy_plan::{TenancyChange, plan_tenancy};

fn base_url() -> String {
    std::env::var("PG_URL")
        .unwrap_or_else(|_| "postgresql://postgres:postgres@127.0.0.1:15442/postgres".to_string())
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn url_for(database: &str) -> String {
    let base = base_url();
    match base.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{database}"),
        None => base,
    }
}

async fn connect(url: &str) -> Option<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .ok()?;
    tokio::spawn(connection);
    Some(client)
}

/// A database carrying the store, the platform tables and the tenant column —
/// the schema Pethub actually deploys.
async fn migrated(database: &str) -> Option<tokio_postgres::Client> {
    let admin = connect(&base_url()).await?;
    let _ = admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {database}"))
        .await;
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .await
        .ok()?;
    drop(admin);

    let url = url_for(database);
    let root = repo();
    for dir in [
        root.join("migrations"),
        root.join("examples/petshop/migrations"),
        root.join("examples/pethub/migrations"),
    ] {
        donat_server::migrate::run_migrate(&url, &dir)
            .await
            .expect("migrations apply");
    }
    connect(&url).await
}

async fn derive(client: &tokio_postgres::Client) -> donat_schema::tenancy_plan::TenancyPlan {
    let metadata = donat_metadata::load_metadata_dir(&repo().join("examples/pethub/metadata"))
        .expect("pethub metadata loads");
    let catalog = donat_catalog::introspect(client)
        .await
        .expect("introspection");
    let uniques = donat_server::tenancy_cli::unique_indexes(client)
        .await
        .expect("unique indexes");
    let populated = donat_server::tenancy_cli::populated_tables(client)
        .await
        .expect("populated tables");
    plan_tenancy(&metadata, &catalog, &uniques, &populated, None)
}

/// The schema Pethub ships is what its declaration implies, and the generator
/// says so by proposing nothing.
///
/// This is the assertion that would have failed while any of the three rules
/// were still too wide: composite references proposed 76 statements against
/// this very schema.
#[tokio::test]
async fn the_shipped_schema_needs_nothing() {
    let Some(client) = migrated("tenancy_generator_clean").await else {
        eprintln!("no Postgres at PG_URL; skipping");
        return;
    };
    let plan = derive(&client).await;
    assert!(
        plan.changes.is_empty(),
        "the generator would rewrite a schema that is already right: {:#?}",
        plan.changes
    );
    // What it leaves is the class it is not allowed to decide, and it does
    // leave some — a generator that resolved everything here would be guessing.
    assert!(!plan.unresolved.is_empty());
}

/// And it finds the one the hand-written migration missed.
#[tokio::test]
async fn an_unscoped_unique_index_over_a_tenant_identity_is_found() {
    let Some(client) = migrated("tenancy_generator_broken").await else {
        eprintln!("no Postgres at PG_URL; skipping");
        return;
    };
    // Put the constraint back the way Petshop wrote it: unique on a column
    // Pethub made unique only within a store.
    client
        .batch_execute(
            "DROP INDEX cart_one_open_per_customer;
             CREATE UNIQUE INDEX cart_one_open_per_customer
               ON public.cart (customer_id) WHERE status = 'cart_open';",
        )
        .await
        .expect("unscoping the index");

    let plan = derive(&client).await;
    let found = plan
        .changes
        .iter()
        .find_map(|change| match change {
            TenancyChange::ScopeUnique { index, because, .. }
                if index.name == "cart_one_open_per_customer" =>
            {
                Some(because.clone())
            }
            _ => None,
        })
        .expect("the unscoped index is found");
    assert_eq!(found, "customer_id");

    // The predicate has to survive, or the rewrite changes what is unique.
    let sql = donat_schema::tenancy_plan::render_sql(&plan);
    assert!(sql.contains("cart_open"), "{sql}");
    assert!(sql.contains("(tenant_id, customer_id)"), "{sql}");
}
