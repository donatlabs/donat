use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use donat_conformance::{BackendId, Suite};
use donat_metadata::Metadata;
use reqwest::blocking::Client;
use serde_json::json;

static NEXT_POSTGRES_DATABASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[derive(Clone, Default)]
struct ClickhouseState {
    requests: Arc<Mutex<Vec<String>>>,
}

struct ClickhouseStub {
    url: String,
    state: ClickhouseState,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ClickhouseStub {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn clickhouse_handler(State(state): State<ClickhouseState>, body: Bytes) -> String {
    let sql = String::from_utf8(body.to_vec()).expect("ClickHouse request is UTF-8");
    state.requests.lock().unwrap().push(sql.clone());
    if sql.contains("system.columns") {
        return concat!(
            "{\"database\":\"logs\",\"table\":\"events\",\"name\":\"id\",",
            "\"type\":\"UInt64\",\"default_kind\":\"\",\"is_in_primary_key\":1}\n",
            "{\"database\":\"logs\",\"table\":\"metrics\",\"name\":\"id\",",
            "\"type\":\"UInt64\",\"default_kind\":\"\",\"is_in_primary_key\":1}\n"
        )
        .to_string();
    }
    "{\"event\":[{\"id\":10}],\"metric\":[{\"id\":20}]}\n".to_string()
}

fn spawn_clickhouse_stub() -> ClickhouseStub {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ClickHouse stub");
    listener
        .set_nonblocking(true)
        .expect("nonblocking ClickHouse stub listener");
    let address = listener.local_addr().expect("ClickHouse stub address");
    let state = ClickhouseState::default();
    let server_state = state.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("ClickHouse stub runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener)
                .expect("Tokio ClickHouse stub listener");
            let app = Router::new()
                .route("/", post(clickhouse_handler))
                .with_state(server_state);
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .expect("ClickHouse stub server");
        });
    });
    ClickhouseStub {
        url: format!("http://{address}/"),
        state,
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
    }
}

fn table(schema: &str, name: &str) -> serde_json::Value {
    json!({
        "table": { "schema": schema, "name": name },
        "select_permissions": [{
            "role": "user",
            "permission": {
                "columns": "*",
                "filter": {},
                "allow_aggregations": true
            }
        }]
    })
}

fn metadata(clickhouse_url: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": [table("public", "item"), table("public", "note")]
        }, {
            "name": "clickhouse",
            "kind": "clickhouse",
            "configuration": {
                "connection_info": { "database_url": clickhouse_url }
            },
            "tables": [table("logs", "events"), table("logs", "metrics")]
        }]
    }))
    .expect("multi-source metadata")
}

fn post_raw(base_url: &str, role: Option<&str>) -> (u16, String) {
    let mut request = Client::new()
        .post(format!("{base_url}/v1/graphql"))
        .json(&json!({
            "query": "query Mixed { event: logs_events { id } item { id } metric: logs_metrics { id } note { id } __typename }",
            "operationName": "Mixed"
        }));
    if let Some(role) = role {
        request = request.header("x-donat-role", role);
    }
    let response = request.send().expect("GraphQL request");
    let status = response.status().as_u16();
    let body = response.text().expect("GraphQL response body");
    (status, body)
}

fn data_request_count(stub: &ClickhouseStub) -> usize {
    stub.state
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|sql| !sql.contains("system.columns"))
        .count()
}

struct PostgresDatabase {
    admin_url: String,
    name: String,
    url: String,
}

impl PostgresDatabase {
    fn create() -> Self {
        use std::sync::atomic::Ordering;

        let admin_url = std::env::var("PG_URL").unwrap_or_else(|_| {
            "postgresql://postgres:postgres@127.0.0.1:15432/postgres".to_string()
        });
        let name = format!(
            "donat_multi_secondary_{}_{}",
            std::process::id(),
            NEXT_POSTGRES_DATABASE.fetch_add(1, Ordering::Relaxed)
        );
        let mut admin = postgres::Client::connect(&admin_url, postgres::NoTls)
            .unwrap_or_else(|error| panic!("connect to Postgres at {admin_url}: {error}"));
        admin
            .batch_execute(&format!("CREATE DATABASE \"{name}\""))
            .expect("create secondary Postgres database");
        let mut url = reqwest::Url::parse(&admin_url).expect("valid PG_URL");
        url.set_path(&format!("/{name}"));
        Self {
            admin_url,
            name,
            url: url.to_string(),
        }
    }
}

impl Drop for PostgresDatabase {
    fn drop(&mut self) {
        if let Ok(mut admin) = postgres::Client::connect(&self.admin_url, postgres::NoTls) {
            let _ = admin.batch_execute(&format!(
                "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                 WHERE datname = '{0}' AND pid <> pg_backend_pid(); \
                 DROP DATABASE IF EXISTS \"{0}\"",
                self.name
            ));
        }
    }
}

fn postgres_table(name: &str, custom_name: &str) -> serde_json::Value {
    json!({
        "table": { "schema": "public", "name": name },
        "configuration": { "custom_name": custom_name },
        "select_permissions": [{
            "role": "user",
            "permission": { "columns": "*", "filter": {} }
        }],
        "insert_permissions": [{
            "role": "user",
            "permission": { "columns": "*", "check": {} }
        }]
    })
}

fn postgres_multi_source_metadata(secondary_url: &str) -> Metadata {
    let default_author = postgres_table("author", "default_author");
    let mut secondary_author = postgres_table("author", "secondary_author");
    secondary_author["array_relationships"] = json!([{
        "name": "articles",
        "using": {
            "foreign_key_constraint_on": {
                "table": { "schema": "public", "name": "article" },
                "column": "author_id"
            }
        }
    }]);
    let mut secondary_article = postgres_table("article", "secondary_article");
    secondary_article["object_relationships"] = json!([{
        "name": "author",
        "using": { "foreign_key_constraint_on": "author_id" }
    }]);

    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": {
                    "database_url": { "from_env": "DONAT_DATABASE_URL" }
                }
            },
            "tables": [default_author]
        }, {
            "name": "secondary",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": secondary_url }
            },
            "tables": [secondary_author, secondary_article]
        }]
    }))
    .expect("multi-source Postgres metadata")
}

fn graphql(base_url: &str, query: &str) -> (u16, String) {
    let response = Client::new()
        .post(format!("{base_url}/v1/graphql"))
        .header("x-donat-role", "user")
        .json(&json!({ "query": query }))
        .send()
        .expect("GraphQL request");
    let status = response.status().as_u16();
    let body = response.text().expect("GraphQL response body");
    (status, body)
}

#[test]
fn mixed_postgres_clickhouse_query_preserves_order_and_permissions() {
    let clickhouse = spawn_clickhouse_stub();
    let suite = Suite::new("multi-source-query")
        .backend(BackendId::Postgres)
        .initial_metadata(metadata(&clickhouse.url))
        .start();
    let mut postgres =
        postgres::Client::connect(suite.db_url(), postgres::NoTls).expect("Postgres connection");
    postgres
        .batch_execute(
            "CREATE TABLE item (id BIGINT PRIMARY KEY); \
             CREATE TABLE note (id BIGINT PRIMARY KEY); \
             INSERT INTO item VALUES (1); \
             INSERT INTO note VALUES (2);",
        )
        .expect("Postgres fixture");
    let base_url = suite.base_url();

    let (status, body) = post_raw(&base_url, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"errors":[{"extensions":{"path":"$","code":"access-denied"},"message":"x-donat-role header is required (this engine has no admin role)"}]}"#
    );
    assert_eq!(data_request_count(&clickhouse), 0);

    let (status, body) = post_raw(&base_url, Some("admin"));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"errors":[{"extensions":{"path":"$.selectionSet.logs_events","code":"validation-failed"},"message":"field 'logs_events' not found in type: 'query_root'"}]}"#
    );
    assert_eq!(data_request_count(&clickhouse), 0);

    let (status, body) = post_raw(&base_url, Some("user"));
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"data":{"event":[{"id":10}],"item":[{"id":1}],"metric":[{"id":20}],"note":[{"id":2}],"__typename":"query_root"}}"#
    );
    assert_eq!(data_request_count(&clickhouse), 1);
}

#[test]
fn secondary_postgres_query_relationship_and_mutation_stay_on_the_owner() {
    let secondary = PostgresDatabase::create();
    let suite = Suite::new("multi-source-postgres-routing")
        .backend(BackendId::Postgres)
        .initial_metadata(postgres_multi_source_metadata(&secondary.url))
        .start();
    let mut default =
        postgres::Client::connect(suite.db_url(), postgres::NoTls).expect("default Postgres");
    default
        .batch_execute(
            "CREATE TABLE author (id BIGINT PRIMARY KEY, name TEXT NOT NULL); \
             INSERT INTO author VALUES (1, 'default');",
        )
        .expect("seed default Postgres source");
    let mut secondary_client =
        postgres::Client::connect(&secondary.url, postgres::NoTls).expect("secondary Postgres");
    secondary_client
        .batch_execute(
            "CREATE TABLE author (id BIGINT PRIMARY KEY, name TEXT NOT NULL); \
             CREATE TABLE article ( \
                 id BIGINT PRIMARY KEY, \
                 author_id BIGINT NOT NULL REFERENCES author(id), \
                 title TEXT NOT NULL \
             ); \
             INSERT INTO author VALUES (1, 'secondary'); \
             INSERT INTO article VALUES (10, 1, 'owned by secondary');",
        )
        .expect("seed secondary Postgres source");
    let base_url = suite.base_url();

    let (status, body) = graphql(
        &base_url,
        "query { first: default_author { id name } second: secondary_article { id title author { id name } } }",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"data":{"first":[{"id":1,"name":"default"}],"second":[{"id":10,"title":"owned by secondary","author":{"id":1,"name":"secondary"}}]}}"#
    );

    let (status, body) = graphql(
        &base_url,
        "mutation { insert_secondary_author_one(object: {id: 2, name: \"inserted\"}) { id name } }",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"data":{"insert_secondary_author_one":{"id":2,"name":"inserted"}}}"#
    );

    let default_count: i64 = default
        .query_one("SELECT count(*) FROM author WHERE id = 2", &[])
        .expect("query default source")
        .get(0);
    let secondary_count: i64 = secondary_client
        .query_one("SELECT count(*) FROM author WHERE id = 2", &[])
        .expect("query secondary source")
        .get(0);
    assert_eq!(default_count, 0, "default source must remain untouched");
    assert_eq!(secondary_count, 1, "secondary source owns the mutation");
}
