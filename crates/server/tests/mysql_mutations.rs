//! End-to-end validation of MySQL MUTATIONS (insert / update / delete) through
//! the REAL server path (`AppState` + `gql::execute_full`) against the live
//! MySQL 8 container at `mysql://root:root@127.0.0.1:13306/donat`.
//!
//! MySQL has no `RETURNING` and read-only CTEs, so neither the Postgres
//! CTE-wrapped assembly nor the SQLite top-level-`RETURNING` shape works. The
//! MySQL path runs the DML and recovers the `returning` set with a COMPANION
//! SELECT in the same transaction (see
//! `knowledgebase/multi-backend/decisions/004-mysql-mutations-companion-select.md`):
//!   - insert: INSERT, then SELECT the new rows by last_insert_id() range (when
//!     the PK is auto-increment and the insert omitted it) or by supplied PK;
//!   - update: UPDATE ... WHERE <pred>, then re-SELECT WHERE <pred>;
//!   - delete: SELECT WHERE <pred> first, then DELETE ... WHERE <pred>.
//! Any violated permission CHECK rolls the whole transaction back and surfaces
//! the same `permission-error` body as Postgres/SQLite.
//!
//! The test is skipped (passes trivially) when no MySQL server is reachable so
//! the crate's suite stays green in environments without the container.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{HeaderMap, StatusCode};
use donat_metadata::Metadata;
use donat_schema::Session;
use donat_server::gql;
use donat_server::state::{AppState, Engine};
use mysql::prelude::Queryable;
use serde_json::{json, Value as Json};

const MYSQL_URL: &str = "mysql://root:root@127.0.0.1:13306/donat";

/// Open a MySQL connection with a short retry loop (the container may still be
/// starting). Returns `None` if MySQL never becomes reachable, so the test can
/// skip rather than fail in an environment without the container.
fn connect_with_retry() -> Option<mysql::Conn> {
    for _ in 0..30 {
        match mysql::Conn::new(MYSQL_URL) {
            Ok(conn) => return Some(conn),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }
    None
}

/// Drop and recreate `note` so re-runs start from a clean slate. `id` is an
/// AUTO_INCREMENT primary key so insert-returning exercises last_insert_id()
/// recovery (the insert omits `id`).
fn seed_db(conn: &mut mysql::Conn) {
    conn.query_drop("DROP TABLE IF EXISTS note")
        .expect("drop note");
    conn.query_drop(
        "CREATE TABLE note (\
            id    INT AUTO_INCREMENT PRIMARY KEY, \
            body  VARCHAR(255), \
            owner VARCHAR(255)\
        )",
    )
    .expect("create note");
}

/// One `mysql` source tracking `note` with insert/update/delete permissions for
/// role `user`. The insert check `owner _eq X-Donat-User-Id` lets us test both a
/// passing insert and a violating one (and hence rollback).
fn metadata(db_url: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "mysql",
            "configuration": { "connection_info": { "database_url": db_url } },
            "tables": [
                {
                    "table": { "schema": "donat", "name": "note" },
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

fn app_state(db_url: &str) -> Arc<AppState> {
    Arc::new(AppState {
        pools: tokio::sync::RwLock::new(HashMap::new()),
        sqlite_paths: tokio::sync::RwLock::new(HashMap::new()),
        mysql_urls: tokio::sync::RwLock::new(HashMap::new()),
        engine: tokio::sync::RwLock::new(Engine {
            metadata: metadata(db_url),
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
async fn mysql_mutations_through_runtime() {
    let Some(mut conn) = connect_with_retry() else {
        eprintln!("MySQL not reachable at {MYSQL_URL}; skipping");
        return;
    };
    seed_db(&mut conn);
    drop(conn);

    let state = app_state(MYSQL_URL);
    state
        .sync_sources()
        .await
        .expect("sync_sources introspects the mysql source");

    // 1. Valid multi-row insert (owner matches the session var). The PK is
    //    omitted, so AUTO_INCREMENT assigns 1 and 2; last_insert_id() recovery
    //    must return both rows in order.
    let (status, body) = run(
        &state,
        r#"mutation {
            insert_note(objects: [
                { body: "first", owner: "alice" },
                { body: "second", owner: "alice" }
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
            insert_note(objects: [{ body: "evil", owner: "mallory" }]) {
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

    // Prove the rollback: no row with owner 'mallory' exists.
    let (_status, body) = run(
        &state,
        r#"query { note(where: { owner: { _eq: "mallory" } }) { id } }"#,
    )
    .await;
    assert_eq!(
        body,
        json!({ "data": { "note": [] } }),
        "violating insert must not have persisted: {body}"
    );

    // 3. Update by predicate; re-select returns the edited row.
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

    // 4. Delete by predicate; the companion SELECT (run BEFORE the DELETE)
    //    captures the returning row.
    let (status, body) = run(
        &state,
        r#"mutation {
            delete_note(where: { id: { _eq: 2 } }) {
                affected_rows returning { id body }
            }
        }"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "data": { "delete_note": {
            "affected_rows": 1,
            "returning": [ { "id": 2, "body": "second" } ]
        }}}),
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
}
