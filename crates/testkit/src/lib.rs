//! Test stand and declarative test runner for donat applications.
//!
//! An application's tests are `*_test.yaml` files that sit next to the
//! metadata file they exercise (`public_orders.yaml` → `public_orders_test.yaml`,
//! `flows/checkout-payment.yaml` → `flows/checkout-payment_test.yaml`), the way
//! Go keeps `_test.go` beside the source. The metadata loader only follows
//! known file names and `!include`, so a test file is never booted as metadata.
//!
//! The same runner serves two entry points: `donat test` in the shipped binary
//! and the conformance crate's cargo tests, so CI runs an example's tests
//! without a container stand.
//!
//! The leaf pieces (`auth_hook`, `provider_stub`, fixture loading, response
//! matching, migrations) are shared with `donat-conformance`.

pub mod auth_hook;
pub mod config;
pub mod fixture;
pub mod matching;
pub mod migrations;
pub mod model;
pub mod provider_stub;
pub mod runner;
pub mod stand;

pub use config::AppTestConfig;
pub use fixture::load_fixture;
pub use matching::{
    SelMap, json_matches, response_matches, sel_tree_from_query, strip_mcp_content,
};
pub use migrations::apply_sql_migration_dir;
pub use runner::{Report, RunConfig};
pub use stand::{Stand, StandConfig};

#[cfg(test)]
mod unit_tests;
