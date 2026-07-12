use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use donat_metadata::Metadata;
use donat_schema::Session;
use donat_server::gql;
use donat_server::state::{AppState, Engine};
use serde_json::json;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct StubState {
    requests: Arc<Mutex<Vec<String>>>,
}

async fn clickhouse_stub(State(state): State<StubState>, body: Bytes) -> impl IntoResponse {
    let sql = String::from_utf8(body.to_vec()).expect("request body is SQL");
    state.requests.lock().await.push(sql.clone());
    if sql.contains("system.columns") {
        return concat!(
            "{\"table\":\"author\",\"name\":\"id\",\"type\":\"UInt64\",",
            "\"default_kind\":\"\",\"is_in_primary_key\":1}\n",
            "{\"table\":\"author\",\"name\":\"name\",\"type\":\"String\",",
            "\"default_kind\":\"\",\"is_in_primary_key\":0}\n",
            "{\"table\":\"author\",\"name\":\"payload\",\"type\":\"JSON\",",
            "\"default_kind\":\"\",\"is_in_primary_key\":0}\n"
        );
    }
    "{\"author\":[{\"id\":1,\"name\":\"Alice\"},{\"id\":2,\"name\":\"Bob\"}]}\n"
}

fn metadata_for(url: &str, database: &str, mutations: bool) -> Metadata {
    let insert_permissions = if mutations {
        json!([{
            "role": "user",
            "permission": { "columns": "*", "check": {} }
        }])
    } else {
        json!([])
    };
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "clickhouse",
            "configuration": { "connection_info": { "database_url": url } },
            "tables": [{
                "table": { "schema": database, "name": "author" },
                "configuration": { "custom_name": "author" },
                "select_permissions": [{
                    "role": "user",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": insert_permissions
            }]
        }]
    }))
    .expect("metadata deserializes")
}

fn metadata(url: &str) -> Metadata {
    metadata_for(url, "analytics", false)
}

fn metadata_with_mutations(url: &str, mutations: bool) -> Metadata {
    metadata_for(url, "analytics", mutations)
}

fn session() -> Session {
    Session {
        role: "user".to_string(),
        vars: HashMap::new(),
        backend_request: false,
    }
}

fn app_state(url: &str) -> Arc<AppState> {
    app_state_with_metadata(metadata(url))
}

fn app_state_with_metadata(metadata: Metadata) -> Arc<AppState> {
    Arc::new(AppState {
        pools: tokio::sync::RwLock::new(HashMap::new()),
        sqlite_paths: tokio::sync::RwLock::new(HashMap::new()),
        mysql_urls: tokio::sync::RwLock::new(HashMap::new()),
        engine: tokio::sync::RwLock::new(Engine {
            metadata,
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

fn with_database(url: &str, database: &str) -> String {
    let mut url = reqwest::Url::parse(url).expect("valid ClickHouse URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "database")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(retained)
        .append_pair("database", database);
    url.to_string()
}

#[tokio::test]
async fn clickhouse_read_only_capability_hides_and_rejects_mutations() {
    let stub = StubState::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(clickhouse_stub))
        .with_state(stub.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });
    let url = format!("http://{address}/?database=analytics");
    let state = app_state_with_metadata(metadata_with_mutations(&url, true));
    state
        .sync_sources()
        .await
        .expect("ClickHouse introspection");

    let (_, schema_body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": "{ __schema { mutationType { name } } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(
        schema_body,
        json!({ "data": { "__schema": { "mutationType": null } } })
    );

    let (_, mutation_body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": "mutation { insert_author(objects: [{id: 3, name: \"Carol\"}]) { affected_rows } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(
        mutation_body,
        json!({ "errors": [{
            "extensions": { "path": "$", "code": "validation-failed" },
            "message": "no mutations exist"
        }] })
    );
    assert_eq!(stub.requests.lock().await.len(), 1, "introspection only");
    server.abort();
}

#[tokio::test]
async fn clickhouse_capabilities_hide_and_reject_distinct_on() {
    let stub = StubState::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(clickhouse_stub))
        .with_state(stub.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });
    let state = app_state(&format!("http://{address}/?database=analytics"));
    state
        .sync_sources()
        .await
        .expect("ClickHouse introspection");

    let (_, schema_body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": "{ __type(name: \"query_root\") { fields { name args { name } } } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;
    let author = schema_body["data"]["__type"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["name"] == "author")
        .unwrap();
    assert!(
        !author["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg["name"] == "distinct_on")
    );

    let (_, query_body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": "{ author(distinct_on: [id]) { id } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;
    assert_eq!(
        query_body["errors"][0]["extensions"]["code"],
        "validation-failed"
    );
    assert!(
        query_body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("distinct_on")
    );
    assert_eq!(stub.requests.lock().await.len(), 1, "introspection only");
    server.abort();
}

#[tokio::test]
async fn clickhouse_capabilities_reject_postgres_only_predicates() {
    let stub = StubState::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(clickhouse_stub))
        .with_state(stub.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });
    let state = app_state(&format!("http://{address}/?database=analytics"));
    state
        .sync_sources()
        .await
        .expect("ClickHouse introspection");

    for query in [
        "{ author(where: {payload: {_has_key: \"x\"}}) { id } }",
        "{ author(where: {name: {_regex: \"^A\"}}) { id } }",
    ] {
        let (_, body) = gql::execute_full(
            &state,
            &session(),
            &json!({ "query": query }),
            false,
            &HeaderMap::new(),
        )
        .await;
        assert_eq!(
            body["errors"][0]["extensions"]["code"], "validation-failed",
            "{body}"
        );
    }
    assert_eq!(stub.requests.lock().await.len(), 1, "introspection only");
    server.abort();
}

#[tokio::test]
async fn clickhouse_source_is_introspected_and_queried_once() {
    let stub = StubState::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(clickhouse_stub))
        .with_state(stub.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });
    let state = app_state(&format!("http://{address}/?database=analytics"));

    state
        .sync_sources()
        .await
        .expect("ClickHouse introspection");
    let (status, body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": "{ author(where: {id: {_eq: 1}}) { id name } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "data": { "author": [
            { "id": 1, "name": "Alice" },
            { "id": 2, "name": "Bob" }
        ]}})
    );
    let requests = stub.requests.lock().await;
    assert_eq!(requests.len(), 2, "one introspection and one data query");
    assert!(requests[0].contains("system.columns"));
    assert!(
        !requests[1].contains(';'),
        "query must contain one statement"
    );
    assert!(requests[1].contains("CAST(1 AS UInt64)"), "{}", requests[1]);

    server.abort();
}

#[tokio::test]
#[ignore = "requires CLICKHOUSE_URL pointing at ClickHouse 25.8+"]
async fn clickhouse_real_server_when_configured() {
    let url = std::env::var("CLICKHOUSE_URL").expect("CLICKHOUSE_URL must be set");
    let client = reqwest::Client::new();
    let database = format!("donat_clickhouse_test_{}", std::process::id());
    let test_url = with_database(&url, &database);

    let create_database = format!("CREATE DATABASE {database}");
    let drop_database = format!("DROP DATABASE IF EXISTS {database}");
    let _ = client.post(&url).body(drop_database.clone()).send().await;
    let response = client
        .post(&url)
        .body(create_database)
        .send()
        .await
        .expect("create test database");
    assert!(response.status().is_success(), "create database failed");
    for sql in [
        "CREATE TABLE author (id Int32, name String) ENGINE = MergeTree ORDER BY id",
        "INSERT INTO author VALUES (1, 'Alice'), (2, 'Bob')",
    ] {
        let response = client
            .post(&test_url)
            .body(sql)
            .send()
            .await
            .expect("ClickHouse setup request");
        assert!(
            response.status().is_success(),
            "setup failed: {}",
            response.text().await.unwrap()
        );
    }

    let state = app_state_with_metadata(metadata_for(&test_url, &database, false));
    state
        .sync_sources()
        .await
        .expect("real ClickHouse introspection");
    let (status, body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": "{ author(order_by: {id: desc}) { id name } }" }),
        false,
        &HeaderMap::new(),
    )
    .await;

    let cleanup = client
        .post(&url)
        .body(drop_database)
        .send()
        .await
        .expect("cleanup request");
    assert!(cleanup.status().is_success(), "cleanup failed: {cleanup:?}");

    assert_eq!(status, axum::http::StatusCode::OK, "body: {body}");
    assert_eq!(
        body,
        json!({ "data": { "author": [
            { "id": 2, "name": "Bob" },
            { "id": 1, "name": "Alice" }
        ]}})
    );
}
