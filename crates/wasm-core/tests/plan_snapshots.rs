//! insta snapshot tests for the PlanV1 contract produced by compile().
//!
//! Fixture metadata/catalog is copied from crates/schema/tests/planner.rs
//! (test-private there; duplicated here to keep the wasm-core crate
//! self-contained and avoid a dev-dependency cycle).

use std::collections::{BTreeMap, HashMap};

use donat_catalog_types::{Catalog, ColumnInfo, ForeignKey, TableInfo};
use donat_metadata::Metadata;
use donat_wasm_core::compile::{compile, CompileInput, CoreState};
use donat_wasm_core::plan::PlanV1;

// -----------------------------------------------------------------------
// Fixture helpers (mirroring crates/schema/tests/planner.rs)
// -----------------------------------------------------------------------

fn metadata() -> Metadata {
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": [
                {
                    "table": { "schema": "public", "name": "author" },
                    "array_relationships": [{
                        "name": "articles",
                        "using": { "foreign_key_constraint_on": {
                            "table": { "schema": "public", "name": "article" },
                            "column": "author_id"
                        }}
                    }],
                    "insert_permissions": [
                        { "role": "user", "permission": { "check": {}, "columns": ["name"] } }
                    ],
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": ["id", "name"],
                            "filter": { "id": { "_eq": "X-Donat-User-Id" } }
                        }},
                        { "role": "nopk", "permission": { "columns": ["name"], "filter": {} } },
                        { "role": "s1", "permission": {
                            "columns": ["id"], "filter": { "id": { "_eq": 1 } }, "limit": 10
                        }},
                        { "role": "s2", "permission": {
                            "columns": ["id", "name"], "filter": { "id": { "_eq": 2 } }, "limit": 20
                        }},
                        { "role": "s3", "permission": { "columns": ["id"], "filter": {} } }
                    ],
                    "update_permissions": [
                        { "role": "user", "permission": { "columns": ["name"], "filter": {} } },
                        { "role": "preset_user", "permission": {
                            "columns": ["name"], "filter": {}, "set": { "name": "preset" }
                        }}
                    ]
                },
                {
                    "table": { "schema": "public", "name": "article" },
                    "object_relationships": [{
                        "name": "author",
                        "using": { "foreign_key_constraint_on": "author_id" }
                    }],
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": "*", "filter": {}, "limit": 100, "allow_aggregations": true
                        }},
                        { "role": "counter", "permission": {
                            "columns": [], "filter": {}, "allow_aggregations": true
                        }},
                        { "role": "tagged", "permission": {
                            "columns": ["id", "title"],
                            "filter": { "id": { "_in": "X-Donat-Allowed-Ids" } }
                        }}
                    ],
                    "delete_permissions": [
                        { "role": "p1", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "p2", "permission": { "filter": { "published": { "_eq": false } } } },
                        { "role": "q1", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "q2", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "kidfix", "permission": { "filter": {} } }
                    ]
                }
            ]
        }],
        "inherited_roles": [
            { "role_name": "kid", "role_set": ["p1", "p2"] },
            { "role_name": "kidfix", "role_set": ["p1", "p2"] },
            { "role_name": "twins", "role_set": ["q1", "q2"] },
            { "role_name": "inh", "role_set": ["s1", "s2"] },
            { "role_name": "inh2", "role_set": ["s1", "s3"] }
        ]
    }))
    .expect("metadata deserializes")
}

fn col(name: &str, pg_type: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        nullable: false,
        has_default: false,
    }
}

fn catalog() -> Catalog {
    let mut tables = BTreeMap::new();
    tables.insert(
        "public.author".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "author".into(),
            columns: vec![col("id", "int4"), col("name", "text"), col("secret", "text")],
            primary_key: vec!["id".into()],
            foreign_keys: vec![],
        },
    );
    tables.insert(
        "public.article".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "article".into(),
            columns: vec![
                col("id", "int4"),
                col("title", "text"),
                col("author_id", "int4"),
                col("published", "bool"),
            ],
            primary_key: vec!["id".into()],
            foreign_keys: vec![ForeignKey {
                constraint_name: "article_author_id_fkey".into(),
                column_mapping: BTreeMap::from([("author_id".into(), "id".into())]),
                referenced_schema: "public".into(),
                referenced_table: "author".into(),
            }],
        },
    );
    Catalog { tables, functions: BTreeMap::new() }
}

fn fixture_state() -> CoreState {
    CoreState { metadata: metadata(), catalog: catalog() }
}

fn session_vars(role: &str) -> HashMap<String, String> {
    [("x-donat-role".to_string(), role.to_string())].into_iter().collect()
}

fn user_session_vars() -> HashMap<String, String> {
    let mut m = session_vars("user");
    m.insert("x-donat-user-id".to_string(), "7".to_string());
    m
}

// -----------------------------------------------------------------------
// Task 2.5: query path snapshot
// -----------------------------------------------------------------------

/// The "article" table has unrestricted `select` for the "user" role
/// (filter:{}, columns:*, limit:100).  The session supplies x-donat-user-id
/// so the "author" permission filter can be resolved, but we select from
/// article which carries no session-var filter — the SQL must be a straight
/// SELECT with LIMIT 100 over "public"."article".
#[test]
fn query_plan_v1() {
    let state = fixture_state();
    let input = CompileInput {
        query: "query { article { id title } }".to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: user_session_vars(),
        stringify_numerics: false,
    };
    let plan = compile(&state, &input);
    insta::assert_json_snapshot!(plan);
}

/// A request with no x-donat-role must be denied with the exact no-admin
/// message produced by session_from() (copied from server/gql.rs).
#[test]
fn missing_role_is_denied() {
    let state = fixture_state();
    let input = CompileInput {
        query: "{ __typename }".to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: Default::default(), // no x-donat-role
        stringify_numerics: false,
    };
    match compile(&state, &input) {
        PlanV1::Error(e) => {
            assert_eq!(
                e.message,
                "x-donat-role header is required (this engine has no admin role)"
            );
            assert_eq!(e.code, "access-denied");
        }
        _ => panic!("expected PlanV1::Error for missing role"),
    }
}
