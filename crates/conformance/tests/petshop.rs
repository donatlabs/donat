//! Checked-in Petshop example conformance.
//!
//! The test loads the example's own metadata and migrations rather than a
//! copied fixture, making its public behavior an executable contract.

use std::collections::BTreeSet;
use std::path::Path;

use donat_conformance::{Suite, Transport, apply_sql_migration_dir};
use serde_json::Value as Json;

fn petshop_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/petshop")
}

fn petshop_suite(name: &str) -> donat_conformance::Running {
    let root = petshop_root();
    let metadata = donat_metadata::load_metadata_dir(&root.join("metadata")).unwrap();
    let running = Suite::new(name)
        .initial_metadata(metadata)
        .with_migrations()
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
        // The customer avatar is stored in an object store, and the engine
        // still signs the call reporting an upload finished.
        .env("PETSHOP_FILE_SIGNING_SECRET", "petshop-test-file-signing")
        // The avatar column names an object store. These suites never upload,
        // but the registry resolves every credential before the listener binds.
        .env("PETSHOP_S3_KEY", "petshop-test-key")
        .env("PETSHOP_S3_SECRET", "petshop-test-secret")
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
        // The tracked domain keeps its file column: an attachment is table
        // metadata, not one of the runtime sections this suite strips.
        .env("PETSHOP_FILE_SIGNING_SECRET", "petshop-test-file-signing")
        // The avatar column names an object store. These suites never upload,
        // but the registry resolves every credential before the listener binds.
        .env("PETSHOP_S3_KEY", "petshop-test-key")
        .env("PETSHOP_S3_SECRET", "petshop-test-secret")
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
fn command_view_outputs_preserve_declared_non_nullability() {
    let root = petshop_root();
    let running = Suite::new("petshop_command_view_nullability").start();
    apply_sql_migration_dir(running.db_url(), &root.join("migrations")).unwrap();
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop command-view catalog database");

    for view in [
        "cart_checkout_context",
        "cart_price_candidate",
        "cart_pricing",
        "order_current_authorization",
        "order_inventory_allocation_candidate",
        "inventory_allocation_line",
        "order_return_context",
        "return_refund_context",
        "order_vendor_split_candidate",
        "vendor_payout_candidate",
        "customer_prescription_order_line",
    ] {
        let columns = client
            .query(
                "SELECT a.attname, t.typtype::text, t.typname, t.typnotnull
                 FROM pg_attribute a
                 JOIN pg_class c ON c.oid = a.attrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_type t ON t.oid = a.atttypid
                 WHERE n.nspname = 'public'
                   AND c.relname = $1
                   AND a.attnum > 0
                   AND NOT a.attisdropped
                 ORDER BY a.attnum",
                &[&view],
            )
            .expect("inspect command-facing view columns");
        assert!(
            !columns.is_empty(),
            "command-facing view public.{view} must exist"
        );
        for column in columns {
            let column_name = column.get::<_, String>(0);
            let type_kind = column.get::<_, String>(1);
            let native_type = column.get::<_, String>(2);
            let type_not_null = column.get::<_, bool>(3);
            assert!(
                type_not_null,
                "public.{view}.{} is semantically non-null and must be represented by a NOT NULL domain",
                column_name
            );
            assert_eq!(
                type_kind, "d",
                "public.{view}.{column_name} must retain a native domain type"
            );
            assert!(
                native_type.starts_with("petshop_required_"),
                "public.{view}.{column_name} has unexpected domain {native_type}"
            );
        }
    }
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

/// Permission validators are a per-role value contract, not a table
/// constraint: the same column is written by migrations and commands without
/// them, and only the role that declared them is held to them.
#[test]
fn permission_validators() {
    let running = petshop_suite("petshop_validation");
    seed_customer_one_rows(running.db_url());
    running.check_query_f("petshop/validation.yaml", Transport::Http);
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

/// A marketplace sells other people's goods.
///
/// That makes two rules pull in opposite directions, and both have to hold at
/// once: a **buyer** sees the whole catalogue, because a catalogue split by
/// seller is not a marketplace; and a **seller** sees only its own orders,
/// payouts and disputes, because the others are somebody else's business.
///
/// Before this the second rule was absent — `vendor` read `vendor_order` with
/// `filter: {}` and could update it too, so every seller saw and could move
/// every other seller's lines.
#[test]
fn a_seller_sees_only_its_own_orders_while_a_buyer_sees_every_sellers_catalogue() {
    let running = petshop_suite("petshop_marketplace_isolation");
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop marketplace database");

    client
        .batch_execute(
            "INSERT INTO customer (customer_id, name, email) VALUES
               ('seller-a', 'Seller A', 'a@example.com'),
               ('seller-b', 'Seller B', 'b@example.com');
             INSERT INTO vendor_membership (vendor_id, user_id) VALUES
               ('11111111-1111-1111-1111-111111111111', 'seller-a'),
               ('22222222-2222-2222-2222-222222222222', 'seller-b');

             INSERT INTO orders (id, customer_id) VALUES
               ('00000000-0000-0000-0000-0000000000a1', 'customer-1');
             INSERT INTO order_line (id, order_id, variant_id, quantity,
                                     unit_price_minor, currency)
             SELECT '00000000-0000-0000-0000-0000000000b1',
                    '00000000-0000-0000-0000-0000000000a1', v.id, 1, 500, 'USD'
               FROM product_variant v ORDER BY v.id LIMIT 1;

             INSERT INTO vendor_order (id, order_id, order_line_id, vendor_id,
                                       offer_id, line_sequence, product_category,
                                       gross_minor, currency, commission_tier,
                                       commission_bps, status) VALUES
               ('00000000-0000-0000-0000-0000000000c1',
                '00000000-0000-0000-0000-0000000000a1',
                '00000000-0000-0000-0000-0000000000b1',
                '11111111-1111-1111-1111-111111111111',
                '00000000-0000-0000-0000-0000000000d1',
                1, 'food', 500, 'USD', 'standard', 1000, 'pending_acceptance'),
               ('00000000-0000-0000-0000-0000000000c2',
                '00000000-0000-0000-0000-0000000000a1',
                '00000000-0000-0000-0000-0000000000b1',
                '22222222-2222-2222-2222-222222222222',
                '00000000-0000-0000-0000-0000000000d2',
                2, 'food', 700, 'USD', 'standard', 1000, 'pending_acceptance');

             INSERT INTO vendor_payout (id, payout_cycle_id, vendor_id, payout_key,
                                        vendor_order_count, gross_minor,
                                        commission_minor, net_minor, currency, status)
             VALUES
               ('00000000-0000-0000-0000-0000000000e1',
                '00000000-0000-0000-0000-0000000000f1',
                '11111111-1111-1111-1111-111111111111', 'cycle:a:USD',
                1, 500, 50, 450, 'USD', 'pending'),
               ('00000000-0000-0000-0000-0000000000e2',
                '00000000-0000-0000-0000-0000000000f1',
                '22222222-2222-2222-2222-222222222222', 'cycle:b:USD',
                1, 700, 70, 630, 'USD', 'pending');",
        )
        .expect("seed two sellers with a line and a payout each");

    let as_seller = |user: &str| {
        vec![
            ("X-Donat-Role".to_string(), "vendor".to_string()),
            ("X-Donat-User-Id".to_string(), user.to_string()),
        ]
    };
    let gross = |body: &Json| -> Vec<i64> {
        let mut v: Vec<i64> = body["data"]["vendor_order"]
            .as_array()
            .unwrap_or_else(|| panic!("expected data.vendor_order in {body}"))
            .iter()
            .map(|row| row["gross_minor"].as_i64().expect("gross_minor"))
            .collect();
        v.sort_unstable();
        v
    };

    // Each seller sees its own line and only its own.
    for (user, expected) in [("seller-a", 500_i64), ("seller-b", 700)] {
        let (status, body) = running.post(
            "/v1/graphql",
            &serde_json::json!({ "query": "query { vendor_order { gross_minor } }" }),
            &as_seller(user),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            gross(&body),
            vec![expected],
            "{user} saw another seller's line: {body}"
        );
    }

    // ...and cannot move the other seller's line either. The update is bounded
    // by the same membership, so it matches nothing rather than accepting an
    // order that is not this seller's to accept.
    let (status, body) = running.post(
        "/v1/graphql",
        &serde_json::json!({
            "query": "mutation { update_vendor_order(\
                        where: {gross_minor: {_eq: 700}}, \
                        _set: {status: \"accepted\"}) { affected_rows } }"
        }),
        &as_seller("seller-a"),
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body["data"]["update_vendor_order"]["affected_rows"], 0,
        "a seller accepted another seller's line: {body}"
    );

    // A payout is the same rule on the money side.
    for (user, expected) in [("seller-a", 450_i64), ("seller-b", 630)] {
        let (status, body) = running.post(
            "/v1/graphql",
            &serde_json::json!({ "query": "query { vendor_payout { net_minor } }" }),
            &as_seller(user),
        );
        assert_eq!(status, 200, "{body}");
        let rows = body["data"]["vendor_payout"].as_array().expect("rows");
        assert_eq!(rows.len(), 1, "{user} saw another seller's payout: {body}");
        assert_eq!(rows[0]["net_minor"], expected, "{body}");
    }

    // The other direction, and the one that makes this a marketplace rather
    // than a row of separate shops: a shopper sees every seller's goods.
    let (status, body) = running.post(
        "/v1/graphql",
        &serde_json::json!({ "query": "query { product { slug } }" }),
        &[
            ("X-Donat-Role".to_string(), "customer".to_string()),
            ("X-Donat-User-Id".to_string(), "customer-1".to_string()),
        ],
    );
    assert_eq!(status, 200, "{body}");
    assert!(
        body["data"]["product"].as_array().expect("products").len() > 1,
        "a shopper saw a catalogue split by seller: {body}"
    );
}

/// The buying side of the same rule: an approver approves for the organization
/// they belong to, and for no other.
#[test]
fn an_approver_outside_an_organization_sees_none_of_its_quotes() {
    let running = petshop_suite("petshop_b2b_isolation");
    let mut client = postgres::Client::connect(running.db_url(), postgres::NoTls)
        .expect("connect to the Petshop B2B database");

    client
        .batch_execute(
            "INSERT INTO customer (customer_id, name, email) VALUES
               ('insider', 'Insider', 'in@example.com'),
               ('outsider', 'Outsider', 'out@example.com');
             INSERT INTO organization (id, currency, available_credit_minor)
             VALUES ('33333333-3333-3333-3333-333333333333', 'USD', 100000);
             INSERT INTO organization_membership (organization_id, user_id)
             VALUES ('33333333-3333-3333-3333-333333333333', 'insider');
             INSERT INTO cart (id, customer_id) VALUES (9001, 'insider');
             INSERT INTO quote (id, organization_id, customer_id, cart_id, status,
                                total_minor, currency, available_credit_minor)
             VALUES ('44444444-4444-4444-4444-444444444444',
                     '33333333-3333-3333-3333-333333333333',
                     'insider', 9001, 'submitted', 5000, 'USD', 100000);",
        )
        .expect("seed one organization and a quote inside it");

    let as_approver = |user: &str| {
        vec![
            ("X-Donat-Role".to_string(), "b2b_approver".to_string()),
            ("X-Donat-User-Id".to_string(), user.to_string()),
        ]
    };
    for (user, expected) in [("insider", 1_usize), ("outsider", 0)] {
        let (status, body) = running.post(
            "/v1/graphql",
            &serde_json::json!({ "query": "query { quote { total_minor } }" }),
            &as_approver(user),
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            body["data"]["quote"].as_array().expect("rows").len(),
            expected,
            "{user} saw the wrong number of quotes: {body}"
        );
    }
}
