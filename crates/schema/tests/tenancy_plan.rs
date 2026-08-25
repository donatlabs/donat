//! What the declaration implies and the database does not have.
//!
//! The case that earns the module is `a_unique_key_over_a_tenant_scoped_identity`:
//! Petshop kept a unique index on `cart(customer_id)` after Pethub made
//! `customer.customer_id` unique only within a store, so one shopper could not
//! hold an open cart in two stores. A person reading 498 lines missed it. This
//! reads the catalogue.

use std::collections::{BTreeMap, BTreeSet};

use donat_catalog_types::{Catalog, ColumnInfo, ForeignKey, RelationKind, TableInfo};
use donat_metadata::Metadata;
use donat_schema::tenancy_plan::{TenancyChange, UniqueIndex, plan_tenancy, render_sql};
use serde_json::json;

fn col(name: &str, sql: &str, has_default: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        pg_type: sql.into(),
        pg_typmod: -1,
        native_type: None,
        nullable: false,
        has_default,
    }
}

fn table(
    name: &str,
    kind: RelationKind,
    columns: Vec<ColumnInfo>,
    foreign_keys: Vec<ForeignKey>,
) -> (String, TableInfo) {
    (
        format!("public.{name}"),
        TableInfo {
            schema: "public".into(),
            name: name.into(),
            relation_kind: kind,
            columns,
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys,
        },
    )
}

fn fk(name: &str, local: &str, remote: &str, table: &str) -> ForeignKey {
    ForeignKey {
        constraint_name: name.into(),
        column_mapping: BTreeMap::from([(local.to_string(), remote.to_string())]),
        referenced_schema: "public".into(),
        referenced_table: table.into(),
    }
}

/// A store registry, a customer keyed by a chosen id, and a cart pointing at
/// one — the shape the real migration has.
fn catalog(cart_has_tenant: bool, cart_fk_composite: bool) -> Catalog {
    let mut tables = BTreeMap::new();
    for (key, info) in [
        table(
            "store",
            RelationKind::Table,
            vec![col("id", "text", false), col("status", "text", false)],
            vec![],
        ),
        table(
            "customer",
            RelationKind::Table,
            vec![
                col("id", "int8", true),
                col("tenant_id", "text", false),
                col("customer_id", "text", false),
            ],
            vec![],
        ),
    ] {
        tables.insert(key, info);
    }

    let mut cart_columns = vec![col("id", "int8", true), col("customer_id", "text", false)];
    if cart_has_tenant {
        cart_columns.push(col("tenant_id", "text", false));
    }
    let cart_fk = if cart_fk_composite {
        ForeignKey {
            constraint_name: "cart_customer_id_fkey".into(),
            column_mapping: BTreeMap::from([
                ("tenant_id".to_string(), "tenant_id".to_string()),
                ("customer_id".to_string(), "customer_id".to_string()),
            ]),
            referenced_schema: "public".into(),
            referenced_table: "customer".into(),
        }
    } else {
        fk(
            "cart_customer_id_fkey",
            "customer_id",
            "customer_id",
            "customer",
        )
    };
    let (key, info) = table("cart", RelationKind::Table, cart_columns, vec![cart_fk]);
    tables.insert(key, info);

    Catalog {
        tables,
        functions: BTreeMap::new(),
    }
}

fn metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default", "kind": "postgres", "configuration": {},
            "tables": [
                { "table": { "schema": "public", "name": "store" } },
                { "table": { "schema": "public", "name": "customer" } },
                { "table": { "schema": "public", "name": "cart" } }
            ]
        }],
        "tenancy": {
            "source": "default",
            "variable": "X-Donat-Tenant-Id",
            "key": "tenant_id",
            "registry": {
                "table": { "schema": "public", "name": "store" }, "key": "id",
                "status": { "column": "status", "serving": ["active"] }
            },
            "keys": [{ "table": { "schema": "public", "name": "store" }, "key": "id" }]
        }
    }))
    .expect("metadata")
}

/// `customer` keyed by a chosen id, unique only within a store — the fact that
/// makes everything downstream tenant-scoped.
fn customer_identity() -> UniqueIndex {
    unique(
        "customer_tenant_customer_id_key",
        "customer",
        &["tenant_id", "customer_id"],
        None,
    )
}

fn unique(name: &str, table: &str, columns: &[&str], predicate: Option<&str>) -> UniqueIndex {
    UniqueIndex {
        schema: "public".into(),
        table: table.into(),
        name: name.into(),
        columns: columns.iter().map(|c| c.to_string()).collect(),
        predicate: predicate.map(str::to_string),
        constraint: false,
    }
}

#[test]
fn a_table_missing_the_key_gets_the_column_and_an_index() {
    let plan = plan_tenancy(
        &metadata(),
        &catalog(false, false),
        &[],
        &BTreeSet::new(),
        None,
    );
    assert!(plan.changes.iter().any(|change| matches!(
        change,
        TenancyChange::AddColumn { table, column, .. } if table == "public.cart" && column == "tenant_id"
    )), "{:?}", plan.changes);
    assert!(
        plan.changes.iter().any(|change| matches!(
            change,
            TenancyChange::AddIndex { table, .. } if table == "public.cart"
        )),
        "{:?}",
        plan.changes
    );
}

#[test]
fn a_table_that_already_carries_it_is_left_alone() {
    let plan = plan_tenancy(
        &metadata(),
        &catalog(true, true),
        &[],
        &BTreeSet::new(),
        None,
    );
    assert!(
        !plan
            .changes
            .iter()
            .any(|change| matches!(change, TenancyChange::AddColumn { .. })),
        "{:?}",
        plan.changes
    );
}

#[test]
fn a_reference_into_a_tenant_scoped_identity_becomes_composite() {
    let uniques = [customer_identity()];
    let plan = plan_tenancy(
        &metadata(),
        &catalog(true, false),
        &uniques,
        &BTreeSet::new(),
        None,
    );
    assert!(
        plan.changes.iter().any(|change| matches!(
            change,
            TenancyChange::CompositeForeignKey { table, constraint, .. }
                if table == "public.cart" && constraint == "cart_customer_id_fkey"
        )),
        "{:?}",
        plan.changes
    );
}

#[test]
fn a_unique_key_over_a_tenant_scoped_identity_is_rescoped() {
    // The cart. `customer_id` is unique only within a store, so this index is
    // a cross-store constraint until it carries the tenant.
    let uniques = [
        customer_identity(),
        unique(
            "cart_one_open_per_customer",
            "cart",
            &["customer_id"],
            Some("status = 'cart_open'"),
        ),
    ];
    let plan = plan_tenancy(
        &metadata(),
        &catalog(true, true),
        &uniques,
        &BTreeSet::new(),
        None,
    );
    let scoped = plan
        .changes
        .iter()
        .find_map(|change| match change {
            TenancyChange::ScopeUnique { index, because, .. } => Some((index, because)),
            _ => None,
        })
        .expect("the index is rescoped");
    assert_eq!(scoped.0.name, "cart_one_open_per_customer");
    assert_eq!(scoped.1, "customer_id");

    // The partial predicate survives, or the rewrite changes what is unique.
    let sql = render_sql(&plan);
    assert!(sql.contains("WHERE status = 'cart_open'"), "{sql}");
    assert!(sql.contains("(tenant_id, customer_id)"), "{sql}");
}

#[test]
fn a_unique_key_the_database_issues_is_left_as_it_is() {
    // Keyed on a column with a default: unique across stores by construction.
    let uniques = [unique("cart_pkey", "cart", &["id"], None)];
    let plan = plan_tenancy(
        &metadata(),
        &catalog(true, true),
        &uniques,
        &BTreeSet::new(),
        None,
    );
    assert!(plan.unresolved.is_empty(), "{:?}", plan.unresolved);
    assert!(
        !plan
            .changes
            .iter()
            .any(|change| matches!(change, TenancyChange::ScopeUnique { .. })),
        "{:?}",
        plan.changes
    );
}

#[test]
fn a_unique_key_over_a_chosen_value_is_named_and_left() {
    // `customer_id` on `customer` itself: chosen by whoever signs in. Whether
    // it is unique per store or across all of them is a question about the
    // business, so the generator asks rather than decides.
    let uniques = [unique(
        "customer_customer_id_key",
        "customer",
        &["customer_id"],
        None,
    )];
    let plan = plan_tenancy(
        &metadata(),
        &catalog(true, true),
        &uniques,
        &BTreeSet::new(),
        None,
    );
    assert_eq!(plan.unresolved.len(), 1, "{:?}", plan.unresolved);
    assert!(
        plan.unresolved[0]
            .object
            .contains("customer_customer_id_key"),
        "{:?}",
        plan.unresolved
    );
    assert!(
        plan.unresolved[0].reason.contains("chose"),
        "{:?}",
        plan.unresolved
    );
}

#[test]
fn a_view_that_does_not_expose_the_key_is_named_rather_than_rewritten() {
    let mut cat = catalog(true, true);
    let (key, info) = table(
        "cart_summary",
        RelationKind::View,
        vec![col("id", "int8", false)],
        vec![],
    );
    cat.tables.insert(key, info);
    let mut md = metadata();
    md.sources[0].tables.push(
        serde_json::from_value(json!({ "table": { "schema": "public", "name": "cart_summary" } }))
            .expect("entry"),
    );

    let plan = plan_tenancy(&md, &cat, &[], &BTreeSet::new(), None);
    let named = plan
        .unresolved
        .iter()
        .find(|u| u.object == "public.cart_summary")
        .expect("the view is named");
    assert!(named.reason.contains("GROUP BY"), "{}", named.reason);
    // And nothing was invented for it.
    assert!(
        !plan.changes.iter().any(|change| matches!(
            change,
            TenancyChange::AddColumn { table, .. } if table == "public.cart_summary"
        )),
        "{:?}",
        plan.changes
    );
}

#[test]
fn a_deployment_with_no_tenancy_gets_an_empty_plan() {
    let mut md = metadata();
    md.tenancy = None;
    assert!(plan_tenancy(&md, &catalog(false, false), &[], &BTreeSet::new(), None).is_empty());
}

#[test]
fn the_registry_is_keyed_by_its_own_identifier_and_needs_no_column() {
    let plan = plan_tenancy(
        &metadata(),
        &catalog(true, true),
        &[],
        &BTreeSet::new(),
        None,
    );
    assert!(
        !plan.changes.iter().any(|change| matches!(
            change,
            TenancyChange::AddColumn { table, .. } if table == "public.store"
        )),
        "{:?}",
        plan.changes
    );
}
