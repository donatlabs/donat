//! The order a tenant's rows can be removed in.
//!
//! Everything else about offboarding is mechanical; the order is the part that
//! is wrong when a person writes it. A row cannot go while another references
//! it, so children precede parents, and the catalogue is what knows which is
//! which.

use std::collections::BTreeMap;

use donat_catalog_types::{Catalog, ColumnInfo, ForeignKey, RelationKind, TableInfo};
use donat_metadata::Metadata;
use donat_schema::tenancy_offboard::{Reach, plan_offboarding};
use serde_json::json;

fn col(name: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        pg_type: "text".into(),
        pg_typmod: -1,
        native_type: None,
        nullable: false,
        has_default: false,
    }
}

fn fk(name: &str, local: &str, remote_table: &str) -> ForeignKey {
    ForeignKey {
        constraint_name: name.into(),
        column_mapping: BTreeMap::from([(local.to_string(), "id".to_string())]),
        referenced_schema: "public".into(),
        referenced_table: remote_table.into(),
    }
}

fn table(name: &str, columns: &[&str], foreign_keys: Vec<ForeignKey>) -> (String, TableInfo) {
    (
        format!("public.{name}"),
        TableInfo {
            schema: "public".into(),
            name: name.into(),
            relation_kind: RelationKind::Table,
            columns: columns.iter().map(|c| col(c)).collect(),
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys,
        },
    )
}

/// store <- order <- order_line, the ordinary shape.
fn catalog(cyclic: bool) -> Catalog {
    let mut tables = BTreeMap::new();
    for (key, info) in [
        table("store", &["id", "status"], vec![]),
        table(
            "orders",
            &["id", "tenant_id"],
            if cyclic {
                vec![fk("orders_line_fkey", "id", "order_line")]
            } else {
                vec![]
            },
        ),
        table(
            "order_line",
            &["id", "tenant_id", "order_id"],
            vec![fk("order_line_order_fkey", "order_id", "orders")],
        ),
    ] {
        tables.insert(key, info);
    }
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
                { "table": { "schema": "public", "name": "orders" } },
                { "table": { "schema": "public", "name": "order_line" } }
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

fn position(plan: &donat_schema::tenancy_offboard::Offboarding, table: &str) -> usize {
    plan.steps
        .iter()
        .position(|step| step.table == table)
        .unwrap_or_else(|| panic!("{table} is in the walk: {:?}", plan.steps))
}

#[test]
fn a_child_is_removed_before_the_row_it_references() {
    let plan = plan_offboarding(&metadata(), &catalog(false));
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
    assert!(
        position(&plan, "public.order_line") < position(&plan, "public.orders"),
        "a line would be orphaned by its own order: {:?}",
        plan.steps
    );
}

#[test]
fn the_registry_row_goes_last_because_everything_points_at_it() {
    let plan = plan_offboarding(&metadata(), &catalog(false));
    assert_eq!(
        position(&plan, "public.store"),
        plan.steps.len() - 1,
        "{:?}",
        plan.steps
    );
}

#[test]
fn every_table_says_how_the_tenant_is_found_on_it() {
    let plan = plan_offboarding(&metadata(), &catalog(false));
    for step in &plan.steps {
        match &step.reach {
            Reach::Key(column) => assert!(!column.is_empty()),
            Reach::Via { remote, .. } => assert!(remote.contains('.')),
        }
    }
    // The registry is keyed by its own identifier, as the declaration says.
    let store = plan
        .steps
        .iter()
        .find(|step| step.table == "public.store")
        .expect("the registry is in the walk");
    assert_eq!(store.reach, Reach::Key("id".into()));
}

#[test]
fn a_cycle_of_references_is_named_rather_than_ordered_arbitrarily() {
    // `ON DELETE` behaviour is the deployment's to decide; guessing it here
    // would remove more than was asked for.
    let plan = plan_offboarding(&metadata(), &catalog(true));
    assert!(
        plan.refusals
            .iter()
            .any(|refusal| refusal.reason.contains("cycle of references")),
        "{:?}",
        plan.refusals
    );
}

#[test]
fn a_deployment_with_no_tenancy_has_nothing_to_remove() {
    let mut md = metadata();
    md.tenancy = None;
    let plan = plan_offboarding(&md, &catalog(false));
    assert!(plan.steps.is_empty() && plan.refusals.is_empty());
}

#[test]
fn a_shared_table_is_not_a_tenants_to_remove() {
    let mut md = metadata();
    let mut cat = catalog(false);
    let (key, info) = table("plan", &["code"], vec![]);
    cat.tables.insert(key, info);
    md.sources[0].tables.push(
        serde_json::from_value(json!({ "table": { "schema": "public", "name": "plan" } }))
            .expect("entry"),
    );
    let tenancy = md.tenancy.as_mut().expect("tenancy");
    tenancy.exempt = serde_json::from_value(json!([
        { "table": { "schema": "public", "name": "plan" }, "shared": "read_only" }
    ]))
    .expect("exempt");

    let plan = plan_offboarding(&md, &cat);
    assert!(
        !plan.steps.iter().any(|step| step.table == "public.plan"),
        "a shared table was treated as the tenant's: {:?}",
        plan.steps
    );
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
}

/// A view is tracked and carries the key, and is still not a tenant's to
/// remove.
///
/// The tenant predicate binds it, so it looks like every other scoped table
/// from the declaration's side. It holds no rows of its own: reading it out
/// duplicates what its tables already gave, and Postgres refuses to delete
/// from anything past a single-table select — which is how this was found,
/// against the real schema rather than here.
#[test]
fn a_view_is_not_part_of_the_walk() {
    let mut md = metadata();
    let mut cat = catalog(false);
    let (key, mut info) = table("order_summary", &["id", "tenant_id"], vec![]);
    info.relation_kind = RelationKind::View;
    cat.tables.insert(key, info);
    md.sources[0].tables.push(
        serde_json::from_value(json!({ "table": { "schema": "public", "name": "order_summary" } }))
            .expect("entry"),
    );

    let plan = plan_offboarding(&md, &cat);
    assert!(
        !plan
            .steps
            .iter()
            .any(|step| step.table == "public.order_summary"),
        "a view was going to be deleted from: {:?}",
        plan.steps
    );
    assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
}
