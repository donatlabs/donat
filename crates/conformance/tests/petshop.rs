//! Checked-in Petshop example conformance.
//!
//! The test loads the example's own metadata and migrations rather than a
//! copied fixture, making its public behavior an executable contract.

use std::path::Path;

use donat_conformance::{Suite, Transport, apply_sql_migration_dir};

#[test]
fn catalog() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop");
    let metadata = donat_metadata::load_metadata_dir(&root.join("metadata")).unwrap();
    let running = Suite::new("petshop_catalog")
        .initial_metadata(metadata)
        .admin_secret("petshop-secret")
        .start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    running.check_query_f("petshop/catalog.yaml", Transport::Http);
}
