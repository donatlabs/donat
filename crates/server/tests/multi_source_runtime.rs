use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use axum::http::HeaderMap;
use donat_metadata::Metadata;
use donat_schema::{Session, SourceQueryPlan};
use donat_server::gql::{self, SourceQueryExecutor, execute_source_query_plans};
use donat_server::state::{AppState, Engine, QueryError};
use mysql::prelude::Queryable;
use rusqlite::Connection;
use serde_json::json;
use tokio::sync::Notify;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct SqliteFixtures {
    default_path: std::path::PathBuf,
    secondary_path: std::path::PathBuf,
}

impl SqliteFixtures {
    fn new() -> Self {
        let suffix = format!(
            "{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        );
        let default_path = std::env::temp_dir().join(format!("donat-multi-default-{suffix}.db"));
        let secondary_path =
            std::env::temp_dir().join(format!("donat-multi-secondary-{suffix}.db"));
        for (path, name) in [(&default_path, "default"), (&secondary_path, "secondary")] {
            let connection = Connection::open(path).expect("create SQLite fixture");
            connection
                .execute_batch(&format!(
                    "CREATE TABLE author (id INTEGER PRIMARY KEY, name TEXT NOT NULL); \
                     INSERT INTO author VALUES (1, '{name}');"
                ))
                .expect("seed SQLite fixture");
        }
        Self {
            default_path,
            secondary_path,
        }
    }
}

impl Drop for SqliteFixtures {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.default_path);
        let _ = std::fs::remove_file(&self.secondary_path);
    }
}

fn source(name: &str, path: &std::path::Path, custom_name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "kind": "sqlite",
        "configuration": {
            "connection_info": {
                "database_url": path.to_str().expect("UTF-8 SQLite path")
            }
        },
        "tables": [{
            "table": { "schema": "main", "name": "author" },
            "configuration": { "custom_name": custom_name },
            "select_permissions": [{
                "role": "user",
                "permission": { "columns": "*", "filter": {} }
            }],
            "insert_permissions": [{
                "role": "user",
                "permission": { "columns": "*", "check": {} }
            }],
            "update_permissions": [{
                "role": "user",
                "permission": { "columns": "*", "filter": {}, "check": {} }
            }],
            "delete_permissions": [{
                "role": "user",
                "permission": { "filter": {} }
            }]
        }]
    })
}

fn metadata(fixtures: &SqliteFixtures) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [
            source("default", &fixtures.default_path, "default_author"),
            source("secondary", &fixtures.secondary_path, "secondary_author")
        ]
    }))
    .expect("multi-source SQLite metadata")
}

fn session() -> Session {
    Session {
        role: "user".to_string(),
        vars: HashMap::new(),
        backend_request: false,
    }
}

fn state(fixtures: &SqliteFixtures) -> Arc<AppState> {
    Arc::new(AppState {
        engine: tokio::sync::RwLock::new(Arc::new(Engine::bootstrap(metadata(fixtures)))),
        default_url: "postgres://unused".to_string(),
        admin_secret: None,
        unauthorized_role: None,
        stringify_numerics: false,
        infer_function_permissions: true,
        jwt: None,
        auth_hook: None,
        http: reqwest::Client::new(),
        allowlist_enabled: false,
        subscription_permits: Arc::new(tokio::sync::Semaphore::new(1_000)),
    })
}

#[derive(Default)]
struct RecordingExecutor {
    calls: Mutex<Vec<(String, Vec<String>)>>,
}

impl SourceQueryExecutor for RecordingExecutor {
    fn execute_source_query<'a>(
        &'a self,
        source: &'a str,
        roots: &'a [donat_ir::RootField],
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, QueryError>> + Send + 'a>> {
        Box::pin(async move {
            let aliases = roots
                .iter()
                .map(|root| match root {
                    donat_ir::RootField::Select { alias, .. }
                    | donat_ir::RootField::Connection { alias, .. }
                    | donat_ir::RootField::Typename { alias, .. } => alias.clone(),
                })
                .collect::<Vec<_>>();
            self.calls
                .lock()
                .expect("recording executor lock")
                .push((source.to_string(), aliases.clone()));
            Ok(json!(
                aliases
                    .into_iter()
                    .map(|alias| (alias, json!([])))
                    .collect::<serde_json::Map<_, _>>()
            ))
        })
    }
}

#[tokio::test]
async fn grouped_source_plans_execute_once_per_source_with_all_roots() {
    let executor = RecordingExecutor::default();
    let plans = vec![
        SourceQueryPlan {
            source: "default".to_string(),
            roots: vec![
                donat_ir::RootField::Typename {
                    alias: "first".to_string(),
                    value: "query_root".to_string(),
                },
                donat_ir::RootField::Typename {
                    alias: "third".to_string(),
                    value: "query_root".to_string(),
                },
            ],
        },
        SourceQueryPlan {
            source: "clickhouse".to_string(),
            roots: vec![
                donat_ir::RootField::Typename {
                    alias: "second".to_string(),
                    value: "query_root".to_string(),
                },
                donat_ir::RootField::Typename {
                    alias: "fourth".to_string(),
                    value: "query_root".to_string(),
                },
            ],
        },
    ];

    let results = execute_source_query_plans(&executor, &plans)
        .await
        .expect("source execution succeeds");
    assert_eq!(results.len(), 2);
    assert_eq!(
        *executor.calls.lock().expect("recording executor lock"),
        vec![
            (
                "default".to_string(),
                vec!["first".to_string(), "third".to_string()]
            ),
            (
                "clickhouse".to_string(),
                vec!["second".to_string(), "fourth".to_string()]
            )
        ]
    );
}

#[tokio::test]
async fn source_less_query_plan_executes_no_backends() {
    let executor = RecordingExecutor::default();

    let results = execute_source_query_plans(&executor, &[])
        .await
        .expect("empty source execution succeeds");

    assert!(results.is_empty());
    assert!(
        executor
            .calls
            .lock()
            .expect("recording executor lock")
            .is_empty()
    );
}

struct GatedExecutor {
    started: AtomicUsize,
    all_started: Notify,
    release: Notify,
}

impl GatedExecutor {
    fn new() -> Self {
        Self {
            started: AtomicUsize::new(0),
            all_started: Notify::new(),
            release: Notify::new(),
        }
    }
}

impl SourceQueryExecutor for GatedExecutor {
    fn execute_source_query<'a>(
        &'a self,
        source: &'a str,
        _roots: &'a [donat_ir::RootField],
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, QueryError>> + Send + 'a>> {
        Box::pin(async move {
            if self.started.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
                self.all_started.notify_one();
            }
            self.release.notified().await;
            Ok(json!({ "source": source }))
        })
    }
}

#[tokio::test]
async fn independent_source_plans_start_concurrently_and_preserve_result_order() {
    let executor = GatedExecutor::new();
    let plans = vec![
        SourceQueryPlan {
            source: "slow-first".to_string(),
            roots: vec![],
        },
        SourceQueryPlan {
            source: "fast-second".to_string(),
            roots: vec![],
        },
    ];

    let execution = execute_source_query_plans(&executor, &plans);
    let release = async {
        tokio::time::timeout(Duration::from_millis(500), executor.all_started.notified())
            .await
            .expect("all independent source plans should start before either completes");
        executor.release.notify_waiters();
    };
    let (results, ()) = tokio::join!(execution, release);

    assert_eq!(
        results.expect("source execution succeeds"),
        vec![
            json!({ "source": "slow-first" }),
            json!({ "source": "fast-second" })
        ]
    );
}

struct ErrorExecutor;

impl SourceQueryExecutor for ErrorExecutor {
    fn execute_source_query<'a>(
        &'a self,
        source: &'a str,
        _roots: &'a [donat_ir::RootField],
    ) -> Pin<Box<dyn Future<Output = Result<serde_json::Value, QueryError>> + Send + 'a>> {
        Box::pin(async move { Err(QueryError::Sqlite(source.to_string())) })
    }
}

#[tokio::test]
async fn concurrent_source_errors_remain_deterministic_in_plan_order() {
    let plans = vec![
        SourceQueryPlan {
            source: "first".to_string(),
            roots: vec![],
        },
        SourceQueryPlan {
            source: "second".to_string(),
            roots: vec![],
        },
    ];

    let error = execute_source_query_plans(&ErrorExecutor, &plans)
        .await
        .expect_err("source execution should fail");
    match error {
        QueryError::Sqlite(source) => assert_eq!(source, "first"),
        _ => panic!("expected the first source error"),
    }
}

struct MysqlDatabases {
    admin_url: String,
    names: Vec<String>,
}

impl Drop for MysqlDatabases {
    fn drop(&mut self) {
        if let Ok(mut connection) = mysql::Conn::new(self.admin_url.as_str()) {
            for name in &self.names {
                let _ = connection.query_drop(format!("DROP DATABASE IF EXISTS `{name}`"));
            }
        }
    }
}

fn mysql_admin_url() -> Option<String> {
    let url = std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:root@127.0.0.1:13306/donat".to_string());
    match mysql::Conn::new(url.as_str()) {
        Ok(_) => Some(url),
        Err(error) if std::env::var_os("DONAT_EXTERNAL_DB_TESTS").is_some() => {
            panic!("MySQL is required at {url}: {error}")
        }
        Err(_) => None,
    }
}

fn mysql_database_url(admin_url: &str, database: &str) -> String {
    let mut url = reqwest::Url::parse(admin_url).expect("valid MYSQL_URL");
    url.set_path(&format!("/{database}"));
    url.to_string()
}

fn mysql_source(name: &str, url: &str, schema: &str, custom_name: &str) -> serde_json::Value {
    let mut source = source(name, std::path::Path::new(url), custom_name);
    source["kind"] = json!("mysql");
    source["configuration"]["connection_info"]["database_url"] = json!(url);
    source["tables"][0]["table"]["schema"] = json!(schema);
    source
}

#[tokio::test]
async fn mixed_query_routes_each_root_to_its_source_and_preserves_order() {
    let fixtures = SqliteFixtures::new();
    let state = state(&fixtures);
    state.sync_sources().await.expect("source introspection");

    let (status, body) = gql::execute_full(
        &state,
        &session(),
        &json!({
            "query": "query { second: secondary_author { id name } __typename first: default_author { id name } }"
        }),
        false,
        &HeaderMap::new(),
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(
        body.to_string(),
        r#"{"data":{"second":[{"id":1,"name":"secondary"}],"__typename":"query_root","first":[{"id":1,"name":"default"}]}}"#
    );
}

#[tokio::test]
async fn secondary_mutation_never_falls_back_to_default_source() {
    let fixtures = SqliteFixtures::new();
    let state = state(&fixtures);
    state.sync_sources().await.expect("source introspection");

    let (status, body) = gql::execute_full(
        &state,
        &session(),
        &json!({
            "query": "mutation { insert_secondary_author_one(object: {id: 2, name: \"inserted\"}) { id name } }"
        }),
        false,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "data": {
            "insert_secondary_author_one": { "id": 2, "name": "inserted" }
        }})
    );

    let default_count: i64 = Connection::open(&fixtures.default_path)
        .unwrap()
        .query_row("SELECT count(*) FROM author WHERE id = 2", [], |row| {
            row.get(0)
        })
        .unwrap();
    let secondary_count: i64 = Connection::open(&fixtures.secondary_path)
        .unwrap()
        .query_row("SELECT count(*) FROM author WHERE id = 2", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(default_count, 0, "default source must remain untouched");
    assert_eq!(secondary_count, 1, "secondary source owns the mutation");
}

#[tokio::test]
async fn secondary_mysql_mutation_never_falls_back_to_default_source() {
    let Some(admin_url) = mysql_admin_url() else {
        eprintln!("skipping multi-source MySQL runtime: MYSQL_URL is not available");
        return;
    };
    let suffix = format!(
        "{}_{}",
        std::process::id(),
        NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
    );
    let default_db = format!("donat_multi_default_{suffix}");
    let secondary_db = format!("donat_multi_secondary_{suffix}");
    let databases = MysqlDatabases {
        admin_url: admin_url.clone(),
        names: vec![default_db.clone(), secondary_db.clone()],
    };
    let mut admin = mysql::Conn::new(admin_url.as_str()).expect("MySQL admin connection");
    for (database, seed) in [(&default_db, "default"), (&secondary_db, "secondary")] {
        admin
            .query_drop(format!("CREATE DATABASE `{database}`"))
            .expect("create MySQL fixture database");
        admin
            .query_drop(format!(
                "CREATE TABLE `{database}`.`author` (id BIGINT PRIMARY KEY, name VARCHAR(255));"
            ))
            .expect("create MySQL fixture table");
        admin
            .query_drop(format!(
                "INSERT INTO `{database}`.`author` VALUES (1, '{seed}')"
            ))
            .expect("seed MySQL fixture table");
    }
    let default_url = mysql_database_url(&admin_url, &default_db);
    let secondary_url = mysql_database_url(&admin_url, &secondary_db);
    let metadata: Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [
            mysql_source("default", &default_url, &default_db, "default_author"),
            mysql_source(
                "secondary",
                &secondary_url,
                &secondary_db,
                "secondary_author"
            )
        ]
    }))
    .expect("multi-source MySQL metadata");
    let state = Arc::new(AppState {
        engine: tokio::sync::RwLock::new(Arc::new(Engine::bootstrap(metadata))),
        default_url: "postgres://unused".to_string(),
        admin_secret: None,
        unauthorized_role: None,
        stringify_numerics: false,
        infer_function_permissions: true,
        jwt: None,
        auth_hook: None,
        http: reqwest::Client::new(),
        allowlist_enabled: false,
        subscription_permits: Arc::new(tokio::sync::Semaphore::new(1_000)),
    });
    state
        .sync_sources()
        .await
        .expect("MySQL source introspection");

    let (status, body) = gql::execute_full(
        &state,
        &session(),
        &json!({
            "query": "mutation { insert_secondary_author_one(object: {id: 2, name: \"inserted\"}) { id name } }"
        }),
        false,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "data": {
            "insert_secondary_author_one": { "id": 2, "name": "inserted" }
        }})
    );

    let default_count: Option<u64> = admin
        .query_first(format!(
            "SELECT count(*) FROM `{default_db}`.`author` WHERE id = 2"
        ))
        .expect("query default MySQL database");
    let secondary_count: Option<u64> = admin
        .query_first(format!(
            "SELECT count(*) FROM `{secondary_db}`.`author` WHERE id = 2"
        ))
        .expect("query secondary MySQL database");
    assert_eq!(default_count, Some(0));
    assert_eq!(secondary_count, Some(1));
    drop(databases);
}
