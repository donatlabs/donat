//! Checked-in Petshop example conformance.
//!
//! The test loads the example's own metadata and migrations rather than a
//! copied fixture, making its public behavior an executable contract.

use std::collections::BTreeSet;
use std::path::Path;

use donat_conformance::{apply_sql_migration_dir, Suite, Transport};

fn petshop_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop")
}

fn petshop_suite(name: &str) -> donat_conformance::Running {
    let root = petshop_root();
    let metadata = donat_metadata::load_metadata_dir(&root.join("metadata")).unwrap();
    let running = Suite::new(name)
        .initial_metadata(metadata)
        .admin_secret("petshop-secret")
        .env("PETSHOP_PAYMENT_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_PAYMENT_API_TOKEN", "petshop-test-payment")
        .env("DONAT_MOCK_CARRIER_BASE_URL", "http://127.0.0.1:9")
        .env("DONAT_MOCK_CARRIER_TOKEN", "petshop-test-carrier")
        .env("PETSHOP_TAX_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_TAX_API_TOKEN", "petshop-test-tax")
        .env("PETSHOP_NOTIFICATION_BASE_URL", "http://127.0.0.1:9")
        .env(
            "PETSHOP_NOTIFICATION_API_TOKEN",
            "petshop-test-notification",
        )
        .env("PETSHOP_PAYOUT_BASE_URL", "http://127.0.0.1:9")
        .env("PETSHOP_PAYOUT_API_TOKEN", "petshop-test-payout")
        .start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    running
}

fn petshop_domain_suite(name: &str) -> donat_conformance::Running {
    let root = petshop_root();
    let mut metadata = donat_metadata::load_metadata_dir(&root.join("metadata")).unwrap();
    metadata.commands.clear();
    metadata.rules = Default::default();
    metadata.connectors.clear();
    metadata.processes.clear();
    let running = Suite::new(name)
        .initial_metadata(metadata)
        .admin_secret("petshop-secret")
        .start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    running
}

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
fn command_relations_exist_in_the_petshop_catalog() {
    let root = petshop_root();
    let running = Suite::new("petshop_command_relations").start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop command-relation database");

    let actual = client
        .query(
            "SELECT c.relname
             FROM pg_class c
             JOIN pg_namespace n ON n.oid = c.relnamespace
             WHERE n.nspname = 'public'
               AND c.relkind IN ('r', 'p', 'v', 'm')
               AND c.relname = ANY($1)
             ORDER BY c.relname",
            &[&COMMAND_RELATIONS],
        )
        .expect("read Petshop command relations")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let expected = COMMAND_RELATIONS
        .iter()
        .map(|name| (*name).to_string())
        .collect::<BTreeSet<_>>();

    assert_eq!(
        actual, expected,
        "every active Petshop command relation must exist before the engine compiles metadata"
    );
}

#[test]
fn command_relations_are_tracked_in_petshop_metadata() {
    let metadata = donat_metadata::load_metadata_dir(&petshop_root().join("metadata")).unwrap();
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

#[test]
fn tracked_petshop_domain_compiles_without_runtime_sections() {
    let running = petshop_domain_suite("petshop_tracked_domain");
    running.check_query_f("petshop/catalog.yaml", Transport::Http);
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

#[test]
fn checkout_quotes_reject_discounts_above_the_subtotal() {
    let root = petshop_root();
    let running = Suite::new("petshop_checkout_quote_constraints").start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop checkout-quote constraint database");
    client
        .batch_execute("INSERT INTO cart (customer_id) VALUES ('customer-1')")
        .expect("seed a customer cart");

    assert_check_violation(
        client.execute(
            "INSERT INTO checkout_quote (
               cart_id, customer_id, price_list_code, discount_bps,
               shipping_service_code, subtotal_minor, discount_minor,
               shipping_minor, taxable_minor, currency,
               destination_country_code, status
             ) VALUES (
               1, 'customer-1', 'retail', 5000, 'ground',
               1000, 1001, 0, 0, 'USD', 'US', 'awaiting_tax'
             )",
            &[],
        ),
        "checkout quotes reject discounts above their subtotal",
    );
}

#[test]
fn order_lines_accept_checkout_and_subscription_snapshots() {
    let root = petshop_root();
    let running = Suite::new("petshop_order_line_snapshots").start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop order-line snapshot database");
    client
        .batch_execute(
            "INSERT INTO orders (id, customer_id) VALUES
               ('00000000-0000-0000-0000-000000000001', 'customer-1'),
               ('00000000-0000-0000-0000-000000000002', 'customer-1');",
        )
        .expect("seed checkout and subscription orders");

    client
        .batch_execute(
            "INSERT INTO order_line (
               order_id, variant_id, quantity, unit_price_minor,
               line_subtotal_minor, discount_minor, taxable_minor,
               tax_code, tax_bps, currency
             ) VALUES (
               '00000000-0000-0000-0000-000000000001', 1, 2, 1999,
               3998, 0, 3998, 'standard', 0, 'USD'
             );
             INSERT INTO order_line (
               order_id, variant_id, quantity, unit_price_minor,
               line_total_minor, currency
             ) VALUES (
               '00000000-0000-0000-0000-000000000002', 2, 1, 2499,
               2499, 'USD'
             );",
        )
        .expect("both active Command line-snapshot shapes must satisfy the schema");
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
