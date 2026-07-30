//! Checked-in Petshop example conformance.
//!
//! The test loads the example's own metadata and migrations rather than a
//! copied fixture, making its public behavior an executable contract.

use std::path::Path;

use donat_conformance::{Suite, Transport, apply_sql_migration_dir};

fn petshop_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop")
}

fn petshop_suite(name: &str) -> donat_conformance::Running {
    let root = petshop_root();
    let metadata = donat_metadata::load_metadata_dir(&root.join("metadata")).unwrap();
    let running = Suite::new(name)
        .initial_metadata(metadata)
        .admin_secret("petshop-secret")
        .start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    running
}

#[test]
fn catalog() {
    let running = petshop_suite("petshop_catalog");
    running.check_query_f("petshop/catalog.yaml", Transport::Http);
}

#[test]
fn cart() {
    let running = petshop_suite("petshop_cart");
    running.check_query_f("petshop/cart.yaml", Transport::Http);
}

#[test]
fn permissions() {
    let running = petshop_suite("petshop_permissions");
    seed_customer_one_rows(running.db_url());
    running.check_query_f("petshop/permissions.yaml", Transport::Http);
}

#[test]
fn store_constraints() {
    let root = petshop_root();
    let running = Suite::new("petshop_constraints").start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop suite database");

    client
        .batch_execute("INSERT INTO cart (customer_id) VALUES ('customer-1')")
        .expect("seed an open cart for the quantity constraint");

    assert_check_violation(
        client.execute(
            "INSERT INTO cart_line (cart_id, variant_id, quantity) VALUES (1, 1, -1)",
            &[],
        ),
        "cart lines reject non-positive quantities",
    );
    assert_check_violation(
        client.execute(
            "UPDATE inventory_stock SET reserved = on_hand + 1 WHERE variant_id = 1",
            &[],
        ),
        "inventory rejects reservations above on-hand stock",
    );
    assert_unique_violation(
        client.execute(
            "INSERT INTO product_variant (product_id, sku, title, price_minor, currency) \
             VALUES (1, 'DOG-CHICKEN-1KG', 'Duplicate SKU', 1, 'USD')",
            &[],
        ),
        "product variants reject duplicate SKUs",
    );
    assert_check_violation(
        client.execute(
            "INSERT INTO product_variant (product_id, sku, title, price_minor, currency) \
             VALUES (1, 'INVALID-CURRENCY', 'Invalid currency', 1, 'US')",
            &[],
        ),
        "product variants reject non-three-letter currencies",
    );
}

fn assert_check_violation(result: Result<u64, postgres::Error>, behavior: &str) {
    let error = result.expect_err(behavior);
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::CHECK_VIOLATION),
        "{behavior}: expected a CHECK violation, got {error}"
    );
}

fn assert_unique_violation(result: Result<u64, postgres::Error>, behavior: &str) {
    let error = result.expect_err(behavior);
    assert_eq!(
        error.code(),
        Some(&postgres::error::SqlState::UNIQUE_VIOLATION),
        "{behavior}: expected a unique violation, got {error}"
    );
}

fn seed_customer_one_rows(database_url: &str) {
    let mut client = postgres::Client::connect(database_url, postgres::NoTls)
        .expect("connect to the Petshop permissions database");
    client
        .batch_execute(
            "INSERT INTO cart (customer_id) VALUES ('customer-1');
             INSERT INTO orders (id, customer_id) VALUES
               ('00000000-0000-0000-0000-000000000001', 'customer-1');",
        )
        .expect("seed customer-1 rows that customer-2 must not read");
}
