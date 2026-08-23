//! The Petshop example's own tests: `*_test.yaml` files beside the metadata
//! they exercise, run through `donat-testkit` — the runner behind
//! `donat test`. One `#[test]` per file keeps cargo's filtering and
//! parallelism; the last test refuses a file nobody listed, so a new
//! `_test.yaml` cannot silently go unrun.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use donat_conformance::{engine_binary, pg_admin_url};
use donat_testkit::AppTestConfig;
use donat_testkit::runner::{self, RunConfig, TEST_FILE_SUFFIX};

fn petshop_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/petshop")
        .canonicalize()
        .expect("examples/petshop exists")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn run(rel: &str) {
    let root = petshop_root();
    let app = AppTestConfig::load(&root).expect("examples/petshop/donat.test.yaml loads");
    let run = RunConfig {
        engine_binary: engine_binary(),
        engine_migrations_dir: workspace_root().join("migrations"),
        admin_database_url: pg_admin_url(),
        log_dir: workspace_root().join("target/app-test-logs"),
        filter: None,
    };
    let file = app.metadata.join(rel);
    let report = runner::run_file(&app, &run, &file).expect("test file runs");
    let mut out = Vec::new();
    report.write(&mut out, &app.metadata).unwrap();
    let text = String::from_utf8_lossy(&out);
    eprintln!("{text}");
    assert!(!report.cases.is_empty(), "{rel} holds no test cases");
    assert_eq!(report.failed(), 0, "{rel} failed:\n{text}");
}

/// Every `_test.yaml` under the example's metadata, relative to it.
fn discovered() -> BTreeSet<String> {
    let app = AppTestConfig::load(&petshop_root()).unwrap();
    runner::discover(&app.metadata)
        .unwrap()
        .into_iter()
        .map(|p| {
            p.strip_prefix(&app.metadata)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

macro_rules! yaml_files {
    ($($name:ident => $rel:literal),* $(,)?) => {
        $(
            #[test]
            fn $name() {
                run($rel);
            }
        )*

        const LISTED: &[&str] = &[$($rel),*];
    };
}

yaml_files! {
    tables => "databases/default/tables/tables_test.yaml",
    public_cart => "databases/default/tables/public_cart_test.yaml",
    public_cart_line => "databases/default/tables/public_cart_line_test.yaml",
    public_inventory_stock => "databases/default/tables/public_inventory_stock_test.yaml",
    public_order_line => "databases/default/tables/public_order_line_test.yaml",
    public_orders => "databases/default/tables/public_orders_test.yaml",
    public_product => "databases/default/tables/public_product_test.yaml",
    public_product_variant => "databases/default/tables/public_product_variant_test.yaml",
    prepare_checkout_quote => "commands/checkout/prepare-checkout-quote_test.yaml",
    authorized_order_cancellation => "flows/authorized-order-cancellation_test.yaml",
    checkout_payment => "flows/checkout-payment_test.yaml",
    grooming_booking => "flows/grooming-booking_test.yaml",
    partial_fulfilment => "flows/partial-fulfilment_test.yaml",
    payment_reconciliation => "flows/payment-reconciliation_test.yaml",
    subscription_renewal => "flows/subscription-renewal_test.yaml",
    vendor_payout => "flows/vendor-payout_test.yaml",
}

#[test]
fn every_test_file_has_a_cargo_entry() {
    let listed = LISTED
        .iter()
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>();
    let found = discovered();
    assert_eq!(
        found,
        listed,
        "every `{TEST_FILE_SUFFIX}` under examples/petshop/metadata must be listed in \
         `yaml_files!` here (found − listed = {:?}, listed − found = {:?})",
        found.difference(&listed).collect::<Vec<_>>(),
        listed.difference(&found).collect::<Vec<_>>()
    );
}
