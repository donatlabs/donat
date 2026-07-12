use donat_conformance::Suite;
use donat_metadata::Metadata;
use serde_json::json;

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

#[test]
#[ignore = "requires CLICKHOUSE_URL pointing at ClickHouse 25.8+"]
fn clickhouse_read_query() {
    let source_url = std::env::var("CLICKHOUSE_URL").expect("CLICKHOUSE_URL must be set");
    let database = format!("donat_conformance_{}", std::process::id());
    let database_url = with_database(&source_url, &database);
    let http = reqwest::blocking::Client::new();
    let drop_database = format!("DROP DATABASE IF EXISTS {database}");
    let _ = http.post(&source_url).body(drop_database.clone()).send();
    http.post(&source_url)
        .body(format!("CREATE DATABASE {database}"))
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    for sql in [
        "CREATE TABLE author (id UInt64, name String) ENGINE = MergeTree ORDER BY id",
        "INSERT INTO author VALUES (1, 'Alice'), (2, 'Bob')",
    ] {
        http.post(&database_url)
            .body(sql)
            .send()
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    let metadata: Metadata = serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "clickhouse",
            "configuration": { "connection_info": { "database_url": database_url } },
            "tables": [{
                "table": { "schema": database, "name": "author" },
                "configuration": { "custom_name": "author" },
                "select_permissions": [{
                    "role": "user",
                    "permission": { "columns": "*", "filter": {}, "allow_aggregations": true }
                }]
            }]
        }]
    }))
    .unwrap();
    let suite = Suite::new("clickhouse").initial_metadata(metadata).start();
    let (_, schema) = suite.post(
        "/v1/graphql",
        &json!({ "query": "{ __type(name: \"query_root\") { fields { name } } }" }),
        &[("X-Donat-Role".to_string(), "user".to_string())],
    );
    assert!(
        schema["data"]["__type"]["fields"]
            .as_array()
            .is_some_and(|fields| fields.iter().any(|field| field["name"] == "author")),
        "query_root fields: {schema}"
    );
    let (status, body) = suite.post(
        "/v1/graphql",
        &json!({ "query": "{ __typename author(order_by: {id: desc}) { __typename id name } }" }),
        &[("X-Donat-Role".to_string(), "user".to_string())],
    );

    let (_, by_pk) = suite.post(
        "/v1/graphql",
        &json!({ "query": "{ author_by_pk(id: 1) { id name } }" }),
        &[("X-Donat-Role".to_string(), "user".to_string())],
    );
    let (_, aggregate) = suite.post(
        "/v1/graphql",
        &json!({ "query": "{ author_aggregate { __typename aggregate { __typename count } nodes { id } } }" }),
        &[("X-Donat-Role".to_string(), "user".to_string())],
    );
    http.post(&source_url)
        .body(drop_database)
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();

    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({ "data": {
            "__typename": "query_root",
            "author": [
                { "__typename": "author", "id": 2, "name": "Bob" },
                { "__typename": "author", "id": 1, "name": "Alice" }
            ]
        }})
    );
    assert_eq!(
        by_pk,
        json!({ "data": { "author_by_pk": { "id": 1, "name": "Alice" } } })
    );
    assert_eq!(
        aggregate,
        json!({ "data": { "author_aggregate": {
            "__typename": "author_aggregate",
            "aggregate": { "__typename": "author_aggregate_fields", "count": 2 },
            "nodes": [{ "id": 1 }, { "id": 2 }]
        }}})
    );
}
