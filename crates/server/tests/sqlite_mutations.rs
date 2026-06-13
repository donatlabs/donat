//! End-to-end validation of SQLite MUTATIONS through the REAL server path
//! (`AppState` + `gql::execute_full`) against a temp-file SQLite database.
//!
//! SQLite forbids a DML statement inside a CTE/subquery, so the Postgres
//! mutation shape (`WITH ins AS (INSERT ... RETURNING *) SELECT <json+check>`)
//! is impossible. The SQLite path emits one top-level DML per mutation root
//! with a `RETURNING json_object(<bare cols>), <violated-flag>` clause, runs it
//! inside a transaction, folds the RETURNING rows into the response in Rust,
//! and rolls back when the permission check is violated. See
//! `knowledgebase/multi-backend/decisions/003-sqlite-mutation-rust-assembly.md`.
//!
//! A temp file (not `:memory:`) is required: the runtime opens its own
//! connection per request, and an in-memory database is private to its
//! creating connection.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use donat_metadata::Metadata;
use donat_schema::Session;
use donat_server::gql;
use donat_server::state::{AppState, Engine};
use rusqlite::Connection;
use serde_json::{json, Value as Json};

/// Create the `note` schema on a temp-file database, then close the setup
/// connection so the runtime opens its own.
fn seed_db(path: &std::path::Path) {
    let conn = Connection::open(path).expect("open temp sqlite file");
    conn.execute_batch(
        r#"
        CREATE TABLE note (
            id    INTEGER PRIMARY KEY,
            body  TEXT,
            owner TEXT
        );
        "#,
    )
    .expect("seed schema");
    // Drop closes the connection.
}

/// One `sqlite` source tracking `note` with insert/update/delete permissions
/// for role `user`. The insert check `owner _eq X-Donat-User-Id` lets us test
/// both a passing insert and a violating one (and hence rollback).
fn metadata(db_path: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "sqlite",
            "configuration": { "connection_info": { "database_url": db_path } },
            "tables": [
                {
                    "table": { "schema": "main", "name": "note" },
                    "configuration": { "custom_name": "note" },
                    "select_permissions": [
                        { "role": "user", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "insert_permissions": [
                        { "role": "user", "permission": {
                            "columns": ["id", "body", "owner"],
                            "check": { "owner": { "_eq": "X-Donat-User-Id" } }
                        }}
                    ],
                    "update_permissions": [
                        { "role": "user", "permission": {
                            "columns": ["body", "owner"], "filter": {}
                        }}
                    ],
                    "delete_permissions": [
                        { "role": "user", "permission": { "filter": {} } }
                    ]
                }
            ]
        }]
    }))
    .expect("metadata deserializes")
}

/// A `user` session whose `X-Donat-User-Id` is `alice`.
fn session() -> Session {
    let mut vars = HashMap::new();
    vars.insert("x-donat-user-id".to_string(), "alice".to_string());
    Session {
        role: "user".to_string(),
        vars,
        backend_request: false,
    }
}

fn app_state(db_path: &str) -> Arc<AppState> {
    Arc::new(AppState {
        pools: tokio::sync::RwLock::new(HashMap::new()),
        sqlite_paths: tokio::sync::RwLock::new(HashMap::new()),
        mysql_urls: tokio::sync::RwLock::new(HashMap::new()),
        engine: tokio::sync::RwLock::new(Engine {
            metadata: metadata(db_path),
            catalogs: HashMap::new(),
        }),
        default_url: "postgres://unused".to_string(),
        admin_secret: None,
        unauthorized_role: None,
        stringify_numerics: false,
        infer_function_permissions: true,
        jwt: None,
        auth_hook: None,
        http: reqwest::Client::new(),
        allowlist_enabled: false,
    })
}

async fn run(state: &Arc<AppState>, query: &str) -> (StatusCode, Json) {
    gql::execute_full(
        state,
        &session(),
        &json!({ "query": query }),
        false,
        &HeaderMap::new(),
    )
    .await
}

#[tokio::test]
async fn sqlite_mutations_through_runtime() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("donat-sqlite-mut-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    seed_db(&db_path);
    let db_path_str = db_path.to_str().expect("utf8 path").to_string();

    let state = app_state(&db_path_str);
    state
        .sync_sources()
        .await
        .expect("sync_sources introspects the sqlite source");

    // 1. Valid insert (owner matches the session var) -> inserted, returning.
    let (status, body) = run(
        &state,
        r#"mutation {
            insert_note(objects: [
                { id: 1, body: "first", owner: "alice" },
                { id: 2, body: "second", owner: "alice" }
            ]) { affected_rows returning { id body } }
        }"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "data": { "insert_note": {
            "affected_rows": 2,
            "returning": [
                { "id": 1, "body": "first" },
                { "id": 2, "body": "second" }
            ]
        }}}),
        "unexpected insert body: {body}"
    );

    // 2. Violating insert (owner != session var) -> permission error, and the
    //    row must NOT persist (transaction rolled back).
    let (status, body) = run(
        &state,
        r#"mutation {
            insert_note(objects: [{ id: 99, body: "evil", owner: "mallory" }]) {
                affected_rows returning { id }
            }
        }"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "errors": [{
            "extensions": {
                "path": "$.selectionSet.insert_note.args.objects",
                "code": "permission-error"
            },
            "message": "check constraint of an insert/update permission has failed"
        }]}),
        "unexpected violation body: {body}"
    );

    // Prove the rollback: row 99 is absent.
    let (_status, body) = run(
        &state,
        "query { note(where: { id: { _eq: 99 } }) { id } }",
    )
    .await;
    assert_eq!(
        body,
        json!({ "data": { "note": [] } }),
        "violating insert must not have persisted: {body}"
    );

    // 3. Update.
    let (status, body) = run(
        &state,
        r#"mutation {
            update_note(where: { id: { _eq: 1 } }, _set: { body: "edited" }) {
                affected_rows returning { id body }
            }
        }"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "data": { "update_note": {
            "affected_rows": 1,
            "returning": [ { "id": 1, "body": "edited" } ]
        }}}),
        "unexpected update body: {body}"
    );

    // 4. Delete.
    let (status, body) = run(
        &state,
        r#"mutation {
            delete_note(where: { id: { _eq: 2 } }) { affected_rows }
        }"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "data": { "delete_note": { "affected_rows": 1 } } }),
        "unexpected delete body: {body}"
    );

    // Final state: only the (edited) row 1 remains.
    let (_status, body) = run(
        &state,
        "query { note(order_by: { id: asc }) { id body owner } }",
    )
    .await;
    assert_eq!(
        body,
        json!({ "data": { "note": [
            { "id": 1, "body": "edited", "owner": "alice" }
        ]}}),
        "unexpected final state: {body}"
    );

    let _ = std::fs::remove_file(&db_path);
}
