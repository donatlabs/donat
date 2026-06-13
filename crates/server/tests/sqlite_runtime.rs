//! Runtime validation that the REAL server path (`AppState` +
//! `gql::execute_full`) can introspect and serve QUERIES against a
//! file-based SQLite data source — not just the standalone pipeline that
//! `sqlite_e2e` exercises.
//!
//! This boots an `AppState` from metadata declaring a single `kind: sqlite`
//! source pointed at a temp-file SQLite database, runs `sync_sources()` (the
//! boot introspection), then issues a GraphQL query through
//! `gql::execute_full` and asserts the seeded rows come back.
//!
//! `:memory:` would NOT work here: the server opens its own connection per
//! query, and an in-memory SQLite database is private to the connection that
//! created it. Hence a temp file on disk.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::HeaderMap;
use donat_metadata::Metadata;
use donat_schema::Session;
use donat_server::gql;
use donat_server::state::{AppState, Engine};
use rusqlite::Connection;
use serde_json::json;

/// Write the schema + seed rows to a temp-file SQLite database, then close
/// the setup connection so the runtime opens its own.
fn seed_db(path: &std::path::Path) {
    let conn = Connection::open(path).expect("open temp sqlite file");
    conn.execute_batch(
        r#"
        CREATE TABLE author (
            id   INTEGER PRIMARY KEY,
            name TEXT
        );
        INSERT INTO author (id, name) VALUES
            (1, 'Alice'),
            (2, 'Bob'),
            (3, 'Carol');
        "#,
    )
    .expect("seed schema + data");
    // Drop closes the connection.
}

/// Metadata with one `sqlite` source whose `database_url` is the temp file,
/// tracking `author` (schema `main`) with a `user` select permission.
fn metadata(db_path: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "sqlite",
            "configuration": { "connection_info": { "database_url": db_path } },
            "tables": [
                {
                    "table": { "schema": "main", "name": "author" },
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

#[tokio::test]
async fn sqlite_source_served_through_runtime() {
    // A unique temp file (cleaned up at the end).
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("donat-sqlite-runtime-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    seed_db(&db_path);
    let db_path_str = db_path.to_str().expect("utf8 path").to_string();

    let state = app_state(&db_path_str);

    // Boot introspection must handle the SQLite source.
    state
        .sync_sources()
        .await
        .expect("sync_sources introspects the sqlite source");

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

    let _ = std::fs::remove_file(&db_path);
}
