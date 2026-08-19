//! The catalog half of the tenancy gate, without a database.
//!
//! `crates/metadata` proves that the declaration is structurally sound. This
//! proves the one thing only an introspected catalog can answer: that the
//! tenant column a table is assumed to carry is really there, and that it is
//! the same type as the identifier the registry hands out.

use std::collections::{BTreeMap, HashMap};

use donat_catalog_types::{Catalog, ColumnInfo, RelationKind, TableInfo};
use donat_metadata::Metadata;
use donat_schema::validate_tenancy_catalog;
use serde_json::{Value as Json, json};

fn col(name: &str, pg_type: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        pg_typmod: -1,
        native_type: None,
        nullable: false,
        has_default: false,
    }
}

fn table(name: &str, columns: Vec<ColumnInfo>) -> (String, TableInfo) {
    (
        format!("public.{name}"),
        TableInfo {
            schema: "public".into(),
            name: name.into(),
            relation_kind: RelationKind::Table,
            columns,
            primary_key: vec![],
            unique_keys: vec![],
            foreign_keys: vec![],
        },
    )
}

/// A registry keyed by uuid, one ordinary tenanted table, and one shared
/// platform table.
fn catalog(product_columns: Vec<ColumnInfo>) -> HashMap<String, Catalog> {
    let mut tables = BTreeMap::new();
    for (key, info) in [
        table(
            "tenant",
            vec![
                col("id", "uuid"),
                col("status", "text"),
                col("slug", "text"),
            ],
        ),
        table("product", product_columns),
        table("plan", vec![col("code", "text")]),
    ] {
        tables.insert(key, info);
    }
    HashMap::from([(
        "default".to_string(),
        Catalog {
            tables,
            functions: BTreeMap::new(),
        },
    )])
}

fn metadata(tenancy: Json) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {},
            "tables": [
                { "table": { "schema": "public", "name": "tenant" } },
                { "table": { "schema": "public", "name": "product" } },
                { "table": { "schema": "public", "name": "plan" } }
            ]
        }],
        "tenancy": tenancy
    }))
    .expect("metadata deserializes")
}

fn declaration() -> Json {
    json!({
        "source": "default",
        "variable": "X-Donat-Tenant-Id",
        "key": "tenant_id",
        "registry": {
            "table": { "schema": "public", "name": "tenant" },
            "key": "id",
            "status": { "column": "status", "serving": ["active"] }
        },
        "keys": [
            { "table": { "schema": "public", "name": "tenant" }, "key": "id" }
        ],
        "exempt": [
            { "table": { "schema": "public", "name": "plan" }, "shared": "read_only" }
        ]
    })
}

fn messages(metadata: &Metadata, catalogs: &HashMap<String, Catalog>) -> Vec<String> {
    validate_tenancy_catalog(metadata, catalogs)
        .into_iter()
        .map(|error| format!("{}: {}", error.path, error.message))
        .collect()
}

#[test]
fn a_deployment_with_no_tenancy_is_never_asked_anything() {
    let metadata: Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default", "kind": "postgres", "configuration": {}, "tables": []
        }]
    }))
    .unwrap();
    assert!(messages(&metadata, &catalog(vec![])).is_empty());
}

#[test]
fn every_table_carrying_its_key_passes() {
    let metadata = metadata(declaration());
    let catalogs = catalog(vec![col("id", "uuid"), col("tenant_id", "uuid")]);
    assert_eq!(messages(&metadata, &catalogs), Vec::<String>::new());
}

/// The whole point of the gate. A table that was tracked and never given a
/// tenant column must stop the deployment, and the refusal must say what to do
/// about it — because the two reflexes that make the message go away, untrack
/// it or exempt it, are the ones that leak.
#[test]
fn a_tracked_table_with_no_tenant_column_stops_the_deployment() {
    let metadata = metadata(declaration());
    let catalogs = catalog(vec![col("id", "uuid")]);
    let messages = messages(&metadata, &catalogs);
    assert_eq!(messages.len(), 1, "{messages:?}");
    let message = &messages[0];
    assert!(
        message.contains("tenancy.tables.public.product"),
        "{message}"
    );
    assert!(
        message.contains("has no tenant key column \"tenant_id\""),
        "{message}"
    );
    assert!(message.contains("tenancy.keys"), "{message}");
    assert!(message.contains("tenancy.exempt"), "{message}");
    assert!(
        message.contains("would see every tenant's rows"),
        "{message}"
    );
}

/// Two columns of different types never compare equal, so this would not fail
/// loudly — it would serve an empty result to every caller, which is the
/// hardest kind of tenancy bug to see.
#[test]
fn a_tenant_key_of_the_wrong_type_is_refused_rather_than_silently_matching_nothing() {
    let metadata = metadata(declaration());
    let catalogs = catalog(vec![col("id", "uuid"), col("tenant_id", "text")]);
    let messages = messages(&metadata, &catalogs);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0]
            .contains("is text, but the registry `public.tenant` identifies a tenant with uuid"),
        "{}",
        messages[0]
    );
    assert!(
        messages[0].contains("would see nothing rather than fail"),
        "{}",
        messages[0]
    );
}

/// A shared table is exempt on purpose, so nothing is asked of its columns.
#[test]
fn a_shared_table_is_not_asked_for_a_tenant_column() {
    let metadata = metadata(declaration());
    let catalogs = catalog(vec![col("id", "uuid"), col("tenant_id", "uuid")]);
    // `public.plan` has only `code`, and that is fine.
    assert!(messages(&metadata, &catalogs).is_empty());
}

#[test]
fn a_registry_missing_the_column_it_names_is_refused() {
    let mut declaration = declaration();
    declaration["registry"]["status"]["column"] = json!("state");
    let metadata = metadata(declaration);
    let catalogs = catalog(vec![col("id", "uuid"), col("tenant_id", "uuid")]);
    let messages = messages(&metadata, &catalogs);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].contains("the registry `public.tenant` has no column `state`"),
        "{}",
        messages[0]
    );
}

/// A table the database does not have is already reported by the tracked-table
/// check; repeating it here would bury the tenancy problems in noise.
#[test]
fn a_table_the_database_does_not_have_is_left_to_the_check_that_owns_it() {
    let metadata = metadata(declaration());
    let mut catalogs = catalog(vec![col("id", "uuid"), col("tenant_id", "uuid")]);
    catalogs
        .get_mut("default")
        .unwrap()
        .tables
        .remove("public.product");
    assert!(messages(&metadata, &catalogs).is_empty());
}
