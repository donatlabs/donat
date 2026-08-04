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
        pg_typmod: -1,
        native_type: None,
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
            relation_kind: donat_catalog_types::RelationKind::Table,
            unique_keys: vec![],
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
            relation_kind: donat_catalog_types::RelationKind::Table,
            unique_keys: vec![],
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

fn state_of(metadata: donat_metadata::Metadata) -> CoreState {
    CoreState::compile_snapshot(
        metadata,
        HashMap::from([("default".to_string(), catalog())]),
    )
    .expect("fixture metadata compiles")
}

fn fixture_state() -> CoreState {
    state_of(metadata())
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
        dialect: None,
    };
    let plan = compile(&state, &input);
    insta::assert_json_snapshot!(plan);
}

// -----------------------------------------------------------------------
// Task 2.6: mutation path snapshot
// -----------------------------------------------------------------------

/// The "user" role has insert permission on "author" (columns: ["name"],
/// check: {}).  This insert must produce a `transaction:true` PlanV1::Mutation
/// with one Statement whose SQL is the engine's `mutation_to_sql_opts` output.
#[test]
fn mutation_plan_v1() {
    let state = fixture_state();
    let input = CompileInput {
        query: r#"mutation { insert_author(objects: [{ name: "Alice" }]) { affected_rows } }"#
            .to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: session_vars("user"),
        stringify_numerics: false,
        dialect: None,
    };
    let plan = compile(&state, &input);
    insta::assert_json_snapshot!(plan);
}

// -----------------------------------------------------------------------
// Task 2.7: permission-error path + no-admin denial
// -----------------------------------------------------------------------

/// The "stranger" role has no permissions on any table, so querying "article"
/// returns PlanV1::Error with code "validation-failed" (field not found in
/// query_root) — identical to what the server's Planner returns.
#[test]
fn permission_error_plan_v1() {
    let state = fixture_state();
    let input = CompileInput {
        query: "{ article { id } }".to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: session_vars("stranger"),
        stringify_numerics: false,
        dialect: None,
    };
    let plan = compile(&state, &input);
    insta::assert_json_snapshot!(plan);
}

// -----------------------------------------------------------------------
// Task 3.0: mutation emits event hooks from table event_triggers
// -----------------------------------------------------------------------

/// Metadata variant that adds an event trigger on `author` (insert only).
/// Used only in `mutation_emits_event_hook` so that `mutation_plan_v1`
/// (which uses plain `metadata()`) remains hook-free and the two tests stay
/// orthogonal.
fn metadata_with_author_trigger() -> Metadata {
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
                        }}
                    ],
                    "update_permissions": [
                        { "role": "user", "permission": { "columns": ["name"], "filter": {} } }
                    ],
                    "event_triggers": [
                        {
                            "name": "on_author_change",
                            "definition": {
                                "insert": { "columns": "*" }
                            },
                            "webhook": "http://unused"
                        }
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
                        }}
                    ],
                    "delete_permissions": [
                        { "role": "p1", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "p2", "permission": { "filter": { "published": { "_eq": false } } } }
                    ]
                }
            ]
        }],
        "inherited_roles": []
    }))
    .expect("metadata_with_author_trigger deserializes")
}

/// An insert into `author` as role `user` must emit one post-commit Hook for
/// the `on_author_change` event trigger (INSERT op only).
#[test]
fn mutation_emits_event_hook() {
    let state = state_of(metadata_with_author_trigger());
    let input = CompileInput {
        query: r#"mutation { insert_author(objects: [{ name: "Bob" }]) { affected_rows } }"#
            .to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: session_vars("user"),
        stringify_numerics: false,
        dialect: None,
    };
    let plan = compile(&state, &input);
    insta::assert_json_snapshot!(plan);
}

// -----------------------------------------------------------------------
// Task 1 (dialect): SQLite snapshot — same query as query_plan_v1 but with
// dialect: Some("sqlite").  The SQL must use SQLite json1 functions
// (json_object / json_group_array / json_array) instead of Postgres
// json_build_object / json_agg.
// -----------------------------------------------------------------------

/// Same fixture/query/role as `query_plan_v1`, but compiled for SQLite.
/// The `sql` field must differ from the Postgres snapshot in a dialect-specific
/// way: `json_object(…)` replaces `json_build_object(…)`, `json_group_array`
/// replaces `json_agg`, and `json_array()` replaces `'[]'::json`.
#[test]
fn query_plan_v1_sqlite() {
    let state = fixture_state();
    let input = CompileInput {
        query: "query { article { id title } }".to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: user_session_vars(),
        stringify_numerics: false,
        dialect: Some("sqlite".into()),
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
        dialect: None,
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

// -----------------------------------------------------------------------
// Declarative commands
// -----------------------------------------------------------------------

/// The fixture metadata plus one declarative command: rename an author the
/// caller owns. It is deliberately the smallest command that still exercises
/// the parts that matter — a role gate, a step that writes, and a result
/// projection — because the point here is that a YAML-declared command
/// reaches the plan at all, not that a complex one does.
fn metadata_with_command() -> Metadata {
    let mut value = serde_json::to_value(metadata()).expect("fixture metadata serializes");
    value["commands"] = serde_json::json!([{
        "name": "rename_author",
        "source": "default",
        "permissions": [{ "role": "user" }],
        "arguments": [
            { "name": "author_id", "type": "int!" },
            { "name": "new_name", "type": "string!" }
        ],
        "steps": [{
            "name": "renamed",
            "update": {
                "table": "public.author",
                "where": { "id": { "arg": "author_id" } },
                "set": { "name": { "arg": "new_name" } },
                "returning": ["id", "name"],
                "require_affected": true
            }
        }],
        "result": {
            "author_id": { "step": "renamed", "column": "id" },
            "name": { "step": "renamed", "column": "name" }
        }
    }]);
    serde_json::from_value(value).expect("metadata with a command deserializes")
}

/// The regression this whole path exists for: a command declared in YAML must
/// appear as a mutation root the embedded core can plan. Before the core
/// compiled a command catalog it used `Planner::new`, which hardcodes
/// `commands: None`, so this query failed as an unknown field.
#[test]
fn declarative_command_is_planned() {
    let state = state_of(metadata_with_command());
    let input = CompileInput {
        query: r#"mutation { rename_author(author_id: 1, new_name: "Ada") { author_id name } }"#
            .to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: session_vars("user"),
        stringify_numerics: false,
        dialect: None,
    };
    match compile(&state, &input) {
        PlanV1::Mutation(body) => {
            assert!(body.transaction, "a command must run in a transaction");
            assert_eq!(body.statements.len(), 1, "one command, one statement");
            assert_eq!(body.statements[0].alias, "rename_author");
            let sql = &body.statements[0].sql;
            assert!(
                sql.contains("UPDATE") && sql.contains("author"),
                "the command must render its write: {sql}"
            );
        }
        other => panic!("expected a mutation plan, got {other:?}"),
    }
}

/// A role the command does not name must be refused by the planner, not by
/// the host after the fact.
#[test]
fn declarative_command_denies_unlisted_role() {
    let state = state_of(metadata_with_command());
    let input = CompileInput {
        query: r#"mutation { rename_author(author_id: 1, new_name: "Ada") { author_id } }"#
            .to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: session_vars("nopk"),
        stringify_numerics: false,
        dialect: None,
    };
    match compile(&state, &input) {
        PlanV1::Error(_) => {}
        other => panic!("expected the command to be denied, got {other:?}"),
    }
}

/// A command writes through its steps, not through a root field, so the tables
/// its hooks must cover are the ones its steps touch. Before `written_tables`
/// read the resolved steps, a command emitted no hooks at all and an embedded
/// host's event handlers silently never ran for command-written rows.
#[test]
fn command_emits_hooks_for_the_tables_its_steps_write() {
    let mut value = serde_json::to_value(metadata_with_command()).expect("serializes");
    // Put an INSERT trigger on `author`, the table the command's step updates,
    // and an UPDATE trigger too so the op filter has something to reject.
    value["sources"][0]["tables"][0]["event_triggers"] = serde_json::json!([
        {
            "name": "on_author_updated",
            "definition": { "enable_manual": false, "update": { "columns": "*" } },
            "webhook": "http://in-process/events"
        },
        {
            "name": "on_author_inserted",
            "definition": { "enable_manual": false, "insert": { "columns": "*" } },
            "webhook": "http://in-process/events"
        }
    ]);
    let metadata: Metadata = serde_json::from_value(value).expect("deserializes");
    let state = state_of(metadata);

    let input = CompileInput {
        query: r#"mutation { rename_author(author_id: 1, new_name: "Ada") { author_id } }"#
            .to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: session_vars("user"),
        stringify_numerics: false,
        dialect: None,
    };
    match compile(&state, &input) {
        PlanV1::Mutation(body) => {
            let triggers: Vec<&str> =
                body.hooks.iter().map(|h| h.trigger.as_str()).collect();
            assert_eq!(
                triggers,
                vec!["on_author_updated"],
                "the command's only step updates author, so only the update \
                 trigger may fire: {:?}",
                body.hooks
            );
            assert_eq!(body.hooks[0].table, "author");
            assert_eq!(body.hooks[0].op, "UPDATE");
            assert_eq!(body.hooks[0].phase, "post_commit");
        }
        other => panic!("expected a mutation plan, got {other:?}"),
    }
}
