use std::sync::atomic::{AtomicU64, Ordering};

use donat_conformance::{BackendId, Suite};
use donat_metadata::Metadata;
use reqwest::blocking::Client;
use serde_json::{Value as Json, json};

static NEXT_DATABASE: AtomicU64 = AtomicU64::new(1);

struct ClickhouseDatabases {
    admin_url: String,
    names: Vec<String>,
}

impl Drop for ClickhouseDatabases {
    fn drop(&mut self) {
        let client = Client::new();
        for name in &self.names {
            let _ = client
                .post(&self.admin_url)
                .body(format!("DROP DATABASE IF EXISTS `{name}`"))
                .send();
        }
    }
}

fn clickhouse_admin_url() -> Option<String> {
    let configured = match std::env::var("CLICKHOUSE_URL") {
        Ok(url) => url,
        Err(_) if std::env::var_os("DONAT_EXTERNAL_DB_TESTS").is_some() => {
            panic!("CLICKHOUSE_URL must be set when DONAT_EXTERNAL_DB_TESTS=1")
        }
        Err(_) => return None,
    };
    let mut url = reqwest::Url::parse(&configured).expect("valid CLICKHOUSE_URL");
    let retained = url
        .query_pairs()
        .filter(|(key, _)| key != "database")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(retained);
    Some(url.to_string())
}

fn execute_clickhouse(client: &Client, url: &str, sql: impl Into<String>) {
    let sql = sql.into();
    let response = client
        .post(url)
        .body(sql.clone())
        .send()
        .expect("ClickHouse request");
    let status = response.status();
    let body = response.text().unwrap_or_default();
    assert!(status.is_success(), "ClickHouse failed: {sql}\n{body}");
}

fn metadata(url: &str, analytics: &str, logs: &str) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "clickhouse",
            "kind": "clickhouse",
            "configuration": {
                "connection_info": { "database_url": url }
            },
            "tables": [{
                "table": { "schema": analytics, "name": "daily" },
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
                "table": { "schema": logs, "name": "events" },
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
    .expect("multi-database ClickHouse metadata")
}

fn post_graphql_raw(base_url: &str, query: &str) -> (u16, String) {
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
fn clickhouse_tracks_tables_across_databases_without_url_database() {
    let Some(admin_url) = clickhouse_admin_url() else {
        eprintln!("skipping multi-database ClickHouse conformance: CLICKHOUSE_URL is not set");
        return;
    };

    let sequence = NEXT_DATABASE.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("{}_{}", std::process::id(), sequence);
    let analytics = format!("donat_conf_analytics_{suffix}");
    let logs = format!("donat_conf_logs_{suffix}");
    let databases = ClickhouseDatabases {
        admin_url: admin_url.clone(),
        names: vec![analytics.clone(), logs.clone()],
    };
    let client = Client::new();
    for name in &databases.names {
        execute_clickhouse(&client, &admin_url, format!("CREATE DATABASE `{name}`"));
    }
    execute_clickhouse(
        &client,
        &admin_url,
        format!(
            "CREATE TABLE `{analytics}`.`daily` (id UInt64, label String) \
             ENGINE = MergeTree ORDER BY id"
        ),
    );
    execute_clickhouse(
        &client,
        &admin_url,
        format!("INSERT INTO `{analytics}`.`daily` VALUES (1, 'daily')"),
    );
    execute_clickhouse(
        &client,
        &admin_url,
        format!(
            "CREATE TABLE `{logs}`.`events` (id UInt64, message String) \
             ENGINE = MergeTree ORDER BY id"
        ),
    );
    execute_clickhouse(
        &client,
        &admin_url,
        format!("INSERT INTO `{logs}`.`events` VALUES (2, 'event')"),
    );

    let suite = Suite::new("clickhouse-multi-database")
        .backend(BackendId::Postgres)
        .initial_metadata(metadata(&admin_url, &analytics, &logs))
        .start();
    let base_url = suite.base_url();

    let (status, body) = post_graphql_raw(
        &base_url,
        "query { analytics_daily { id label } logs_events { id message } }",
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"data":{"analytics_daily":[{"id":1,"label":"daily"}],"logs_events":[{"id":2,"message":"event"}]}}"#
    );

    let (status, body) = post_graphql_raw(
        &base_url,
        "query { __type(name: \"query_root\") { fields { name } } }",
    );
    assert_eq!(status, 200, "{body}");
    let body: Json = serde_json::from_str(&body).expect("introspection JSON");
    let fields = body["data"]["__type"]["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("query fields missing: {body}"));
    for expected in ["analytics_daily", "logs_events"] {
        assert!(
            fields.iter().any(|field| field["name"] == expected),
            "missing {expected}: {body}"
        );
    }
}
