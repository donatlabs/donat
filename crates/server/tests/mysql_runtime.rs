//! Runtime validation that the REAL server path (`AppState` +
//! `gql::execute_full`) can introspect and serve QUERIES against a MySQL
//! data source — mirroring `sqlite_runtime` but against a live MySQL 8
//! container at `mysql://root:root@127.0.0.1:13306/donat`.
//!
//! This boots an `AppState` from metadata declaring a single `kind: mysql`
//! source pointed at the container, runs `sync_sources()` (boot
//! introspection via `mysql_introspect`), then issues a GraphQL query
//! through `gql::execute_full` and asserts the seeded rows come back.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::HeaderMap;
use donat_metadata::Metadata;
use donat_schema::Session;
use donat_server::gql;
use donat_server::state::{AppState, Engine};
use mysql::prelude::Queryable;
use serde_json::json;

const MYSQL_URL: &str = "mysql://root:root@127.0.0.1:13306/donat";

/// Open a MySQL connection with a short retry loop (the container may still
/// be starting). Returns `None` if MySQL never becomes reachable, so the
/// test can skip rather than fail in an environment without the container.
fn connect_with_retry() -> Option<mysql::Conn> {
    for _ in 0..30 {
        match mysql::Conn::new(MYSQL_URL) {
            Ok(conn) => return Some(conn),
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(500)),
        }
    }
    None
}

/// Create + seed the `author` table on a clean slate.
fn seed_db(conn: &mut mysql::Conn) {
    // Drop dependents first; a leftover `article` may FK-reference `author`.
    conn.query_drop("DROP TABLE IF EXISTS article")
        .expect("drop article");
    conn.query_drop("DROP TABLE IF EXISTS author")
        .expect("drop author");
    conn.query_drop("CREATE TABLE author (id INT PRIMARY KEY, name VARCHAR(255))")
        .expect("create author");
    conn.query_drop("INSERT INTO author (id, name) VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol')")
        .expect("seed author");
}

/// Metadata with one `mysql` source whose `database_url` is the container,
/// tracking `author` (schema = db name `donat`) with a `user` select perm.
fn metadata(db_url: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "mysql",
            "configuration": { "connection_info": { "database_url": db_url } },
            "tables": [
                {
                    "table": { "schema": "donat", "name": "author" },
                    "configuration": { "custom_name": "author" },
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": "*", "filter": {}
                        }}
                    ]
                }
            ]
        }]
    }))
    .expect("metadata deserializes")
}

fn session_for(role: &str) -> Session {
    Session {
        role: role.to_string(),
        vars: HashMap::new(),
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

#[tokio::test]
async fn mysql_source_served_through_runtime() {
    let Some(mut conn) = connect_with_retry() else {
        eprintln!("MySQL not reachable at {MYSQL_URL}; skipping");
        return;
    };
    seed_db(&mut conn);
    drop(conn);

    let state = app_state(MYSQL_URL);

    // Boot introspection must handle the MySQL source.
    state
        .sync_sources()
        .await
        .expect("sync_sources introspects the mysql source");

    let (status, body) = gql::execute_full(
        &state,
        &session_for("user"),
        &json!({ "query": "{ author { id name } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "body: {body}");

    assert_eq!(
        body,
        json!({ "data": { "author": [
            { "id": 1, "name": "Alice" },
            { "id": 2, "name": "Bob" },
            { "id": 3, "name": "Carol" },
        ]}}),
        "unexpected response body: {body}"
    );
}
