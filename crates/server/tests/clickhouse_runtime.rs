use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{RawQuery, State};
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

#[derive(Clone, Default)]
struct MultiDatabaseStubState {
    requests: Arc<Mutex<Vec<(String, String)>>>,
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

async fn multi_database_clickhouse_stub(
    State(state): State<MultiDatabaseStubState>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> impl IntoResponse {
    let sql = String::from_utf8(body.to_vec()).expect("request body is SQL");
    state
        .requests
        .lock()
        .await
        .push((query.unwrap_or_default(), sql));
    concat!(
        "{\"database\":\"analytics\",\"table\":\"daily\",\"name\":\"id\",",
        "\"type\":\"UInt64\",\"default_kind\":\"\",\"is_in_primary_key\":1}\n",
        "{\"database\":\"logs\",\"table\":\"events\",\"name\":\"id\",",
        "\"type\":\"UInt64\",\"default_kind\":\"\",\"is_in_primary_key\":1}\n"
    )
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
                    "permission": {
                        "columns": "*",
                        "filter": {},
                        "allow_aggregations": true
                    }
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

fn multi_database_metadata(url: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "clickhouse",
            "configuration": { "connection_info": { "database_url": url } },
            "tables": [{
                "table": { "schema": "analytics", "name": "daily" },
                "configuration": { "custom_name": "analytics_daily" },
                "select_permissions": [{
                    "role": "user",
                    "permission": {
                        "columns": "*",
                        "filter": {},
                        "allow_aggregations": true
                    }
                }]
            }, {
                "table": { "schema": "logs", "name": "events" },
                "configuration": { "custom_name": "logs_events" },
                "select_permissions": [{
                    "role": "user",
                    "permission": {
                        "columns": "*",
                        "filter": {},
                        "allow_aggregations": true
                    }
                }]
            }]
        }]
    }))
    .expect("metadata deserializes")
}

fn metadata_with_mutations(url: &str, mutations: bool) -> Metadata {
    metadata_for(url, "analytics", mutations)
}

fn complex_metadata(url: &str, database: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "clickhouse",
            "configuration": { "connection_info": { "database_url": url } },
            "tables": [{
                "table": { "schema": database, "name": "complex_values" },
                "configuration": { "custom_name": "complex_values" },
                "select_permissions": [{
                    "role": "user",
                    "permission": {
                        "columns": "*",
                        "filter": {},
                        "allow_aggregations": true
                    }
                }]
            }]
        }]
    }))
    .expect("metadata deserializes")
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
async fn clickhouse_tracks_tables_across_databases_without_url_database() {
    let stub = MultiDatabaseStubState::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/", post(multi_database_clickhouse_stub))
        .with_state(stub.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("stub server");
    });
    let state = app_state_with_metadata(multi_database_metadata(&format!("http://{address}/")));

    state
        .sync_sources()
        .await
        .expect("multi-database ClickHouse introspection");

    let engine = state.engine.read().await;
    let tables = &engine.metadata.sources[0].tables;
    assert_eq!(tables.len(), 2, "tracked tables must not be pruned");
    assert!(
        tables
            .iter()
            .any(|table| { table.table.schema() == "analytics" && table.table.name() == "daily" })
    );
    assert!(
        tables
            .iter()
            .any(|table| { table.table.schema() == "logs" && table.table.name() == "events" })
    );
    drop(engine);

    let requests = stub.requests.lock().await;
    assert_eq!(requests.len(), 1, "one introspection request");
    let (query, sql) = &requests[0];
    assert!(
        sql.contains("WHERE database IN {databases:Array(String)}"),
        "{sql}"
    );
    let parsed =
        reqwest::Url::parse(&format!("http://localhost/?{query}")).expect("request query parses");
    let databases = parsed
        .query_pairs()
        .find(|(key, _)| key == "param_databases")
        .map(|(_, value)| value.into_owned())
        .expect("param_databases is present");
    assert_eq!(databases, "['analytics','logs']");

    server.abort();
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
async fn clickhouse_real_server_when_configured() {
    let Some(url) = std::env::var("CLICKHOUSE_URL").ok() else {
        if std::env::var_os("DONAT_EXTERNAL_DB_TESTS").is_some() {
            panic!("CLICKHOUSE_URL must be set when DONAT_EXTERNAL_DB_TESTS=1");
        }
        eprintln!("skipping real ClickHouse test: CLICKHOUSE_URL is not configured");
        return;
    };
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
        "CREATE TABLE author (id UInt64, name String) ENGINE = MergeTree ORDER BY id",
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
    let (aggregate_status, aggregate_body) = gql::execute_full(
        &state,
        &session(),
        &json!({ "query": r#"{
            author_aggregate {
                aggregate {
                    sum { id }
                    avg { id }
                    max { id }
                    min { id }
                    stddev { id }
                    stddev_samp { id }
                    stddev_pop { id }
                    variance { id }
                    var_samp { id }
                    var_pop { id }
                }
            }
            empty: author_aggregate(where: {id: {_gt: 100}}) {
                aggregate {
                    sum { id }
                    avg { id }
                    max { id }
                    min { id }
                    stddev { id }
                    stddev_samp { id }
                    stddev_pop { id }
                    variance { id }
                    var_samp { id }
                    var_pop { id }
                }
            }
        }"# }),
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
    assert_eq!(
        aggregate_status,
        axum::http::StatusCode::OK,
        "body: {aggregate_body}"
    );
    assert!(
        aggregate_body.get("errors").is_none(),
        "aggregate query failed: {aggregate_body}"
    );
    let aggregate = &aggregate_body["data"]["author_aggregate"]["aggregate"];
    for (op, expected) in [
        ("sum", 3.0),
        ("avg", 1.5),
        ("max", 2.0),
        ("min", 1.0),
        ("stddev", std::f64::consts::FRAC_1_SQRT_2),
        ("stddev_samp", std::f64::consts::FRAC_1_SQRT_2),
        ("stddev_pop", 0.5),
        ("variance", 0.5),
        ("var_samp", 0.5),
        ("var_pop", 0.25),
    ] {
        let actual = aggregate[op]["id"]
            .as_f64()
            .unwrap_or_else(|| panic!("missing numeric {op}: {aggregate_body}"));
        assert!(
            (actual - expected).abs() < 1e-12,
            "unexpected {op}: {actual}"
        );
        assert_eq!(
            aggregate_body["data"]["empty"]["aggregate"][op]["id"],
            serde_json::Value::Null,
            "empty {op} must be null: {aggregate_body}"
        );
    }
}

#[tokio::test]
async fn clickhouse_complex_type_filters_round_trip() {
    let Some(url) = std::env::var("CLICKHOUSE_URL").ok() else {
        if std::env::var_os("DONAT_EXTERNAL_DB_TESTS").is_some() {
            panic!("CLICKHOUSE_URL must be set when DONAT_EXTERNAL_DB_TESTS=1");
        }
        eprintln!("skipping real ClickHouse complex-type test: CLICKHOUSE_URL is not configured");
        return;
    };
    let client = reqwest::Client::new();
    let database = format!("donat_clickhouse_complex_{}", std::process::id());
    let test_url = with_database(&url, &database);

    let drop_database = format!("DROP DATABASE IF EXISTS {database}");
    let _ = client.post(&url).body(drop_database.clone()).send().await;
    let response = client
        .post(&url)
        .body(format!("CREATE DATABASE {database}"))
        .send()
        .await
        .expect("create complex-type test database");
    assert!(response.status().is_success(), "create database failed");
    for sql in [
        "CREATE TABLE complex_values (id UInt64, numeric_map Map(UInt64, UInt64), string_map Map(String, UInt64), labels Array(String), point Tuple(label String, value UInt64)) ENGINE = MergeTree ORDER BY id",
        "INSERT INTO complex_values VALUES (1, map(1, 2), map('a', 2), ['red', 'blue'], ('p', 7)), (2, map(3, 4), map('b', 4), ['green'], ('q', 8))",
    ] {
        let response = client
            .post(&test_url)
            .body(sql)
            .send()
            .await
            .expect("ClickHouse complex-type setup request");
        assert!(
            response.status().is_success(),
            "setup failed: {}",
            response.text().await.unwrap()
        );
    }

    let state = app_state_with_metadata(complex_metadata(&test_url, &database));
    state
        .sync_sources()
        .await
        .expect("real ClickHouse complex-type introspection");
    let (_, equality_body) = gql::execute_full(
        &state,
        &session(),
        &json!({
            "query": r#"query($where: complex_values_bool_exp!) {
                complex_values(where: $where) { id }
            }"#,
            "variables": {
                "where": {
                    "numeric_map": { "_eq": { "1": 2 } },
                    "string_map": { "_eq": { "a": 2 } },
                    "labels": { "_eq": ["red", "blue"] },
                    "point": { "_eq": { "label": "p", "value": 7 } }
                }
            }
        }),
        false,
        &HeaderMap::new(),
    )
    .await;
    let (_, in_body) = gql::execute_full(
        &state,
        &session(),
        &json!({
            "query": r#"query($where: complex_values_bool_exp!) {
                complex_values(where: $where) { id }
            }"#,
            "variables": {
                "where": {
                    "numeric_map": { "_in": [{ "1": 2 }, { "3": 4 }] }
                }
            }
        }),
        false,
        &HeaderMap::new(),
    )
    .await;

    let cleanup = client
        .post(&url)
        .body(drop_database)
        .send()
        .await
        .expect("complex-type cleanup request");
    assert!(cleanup.status().is_success(), "cleanup failed: {cleanup:?}");

    assert_eq!(
        equality_body,
        json!({ "data": { "complex_values": [{ "id": 1 }] } }),
        "complex equality filters failed"
    );
    assert_eq!(
        in_body,
        json!({ "data": { "complex_values": [{ "id": 1 }, { "id": 2 }] } }),
        "complex _in filter failed"
    );
}
