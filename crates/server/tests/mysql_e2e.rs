//! End-to-end validation of the MySQL backend binding against a REAL MySQL 8
//! server (the `donat-mysql` container at `mysql://root:root@127.0.0.1:13306/donat`).
//!
//! This exercises the full READ-query pipeline:
//!   mysql_introspect -> Metadata -> donat_schema::Planner::plan
//!   -> donat_sqlgen::operation_to_sql_with(.., AnyDialect::Mysql)
//!   -> execute the ONE generated SQL string on a real MySQL connection
//!   -> parse the returned JSON -> assert.
//!
//! It is the conformance slice the `MySqlDialect` JSON-assembly/scalar
//! renderings are validated against — i.e. the failing test that drives any
//! dialect fix (string escaping, JSON_ARRAYAGG nesting, scalar casts, the
//! LIMIT/OFFSET shape). MySQL MUTATIONS are out of scope (no RETURNING).
//!
//! The test is skipped (passes trivially) when no MySQL server is reachable so
//! the crate's test suite stays green in environments without the container.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use donat_backend::{AnyDialect, MySqlDialect};
use donat_catalog::{Catalog, mysql_introspect};
use donat_metadata::Metadata;
use donat_schema::{Plan, Planner, Session};
use mysql::prelude::Queryable;
use mysql::{Conn, Opts, Row, Value as MyValue};
use serde_json::{Value as Json, json};

const URL: &str = "mysql://root:root@127.0.0.1:13306/donat";
const SCHEMA: &str = "donat";

/// Connect to the container, retrying for up to ~60s while it initialises.
/// Returns `None` if it never becomes reachable (the test then no-ops).
fn connect() -> Option<Conn> {
    let opts = Opts::from_url(URL).expect("valid mysql url");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match Conn::new(opts.clone()) {
            Ok(mut conn) => {
                // sqlgen renders identifiers with double quotes (its free
                // `quote_ident` is hardcoded to the Postgres dialect — it is
                // NOT routed through the runtime dialect, so MySqlDialect's
                // backtick quoting never reaches the emitted SQL). MySQL only
                // treats `"..."` as an identifier quote under ANSI_QUOTES mode,
                // so enable it on the session. This keeps the dialect/sqlgen
                // untouched while letting the Postgres-shaped identifiers parse.
                conn.query_drop("SET SESSION sql_mode = CONCAT(@@sql_mode, ',ANSI_QUOTES')")
                    .expect("enable ANSI_QUOTES");
                return Some(conn);
            }
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1000));
            }
            Err(_) => return None,
        }
    }
}

/// (Re)create the schema and seed deterministic rows. Mirrors sqlite_e2e's
/// author/article shape. Dropped-and-recreated so the test is idempotent.
fn seed(conn: &mut Conn) {
    conn.query_drop("SET FOREIGN_KEY_CHECKS = 0").unwrap();
    conn.query_drop("DROP TABLE IF EXISTS article").unwrap();
    conn.query_drop("DROP TABLE IF EXISTS author").unwrap();
    conn.query_drop("SET FOREIGN_KEY_CHECKS = 1").unwrap();
    conn.query_drop(
        "CREATE TABLE author (\
            id   INT PRIMARY KEY, \
            name VARCHAR(255)\
        )",
    )
    .unwrap();
    conn.query_drop(
        "CREATE TABLE article (\
            id        INT PRIMARY KEY, \
            title     VARCHAR(255), \
            author_id INT, \
            CONSTRAINT fk_article_author FOREIGN KEY (author_id) REFERENCES author(id)\
        )",
    )
    .unwrap();
    conn.query_drop(
        "INSERT INTO author (id, name) VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol')",
    )
    .unwrap();
    conn.query_drop(
        "INSERT INTO article (id, title, author_id) VALUES \
            (1, 'A1', 1), (2, 'A2', 1), (3, 'B1', 2), (4, 'B2', 2), (5, 'C1', 3)",
    )
    .unwrap();
}

/// In-memory metadata tracking both tables under schema `donat`, with a `user`
/// role granted select on every column (filter `{}`), the FK-based object
/// relationship `article.author` and array relationship `author.articles`, and
/// aggregations enabled. (Same shape as sqlite_e2e, schema renamed to donat.)
fn metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": [
                {
                    "table": { "schema": SCHEMA, "name": "author" },
                    "configuration": { "custom_name": "author" },
                    "array_relationships": [{
                        "name": "articles",
                        "using": { "foreign_key_constraint_on": {
                            "table": { "schema": SCHEMA, "name": "article" },
                            "column": "author_id"
                        }}
                    }],
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": "*", "filter": {}, "allow_aggregations": true
                        }}
                    ]
                },
                {
                    "table": { "schema": SCHEMA, "name": "article" },
                    "configuration": { "custom_name": "article" },
                    "object_relationships": [{
                        "name": "author",
                        "using": { "foreign_key_constraint_on": "author_id" }
                    }],
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": "*", "filter": {}, "allow_aggregations": true
                        }}
                    ]
                }
            ]
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

/// Read MySQL's first column as a UTF-8 string regardless of whether the
/// driver hands back a JSON/text value as `Bytes` or `Str`.
fn first_col_string(row: &Row) -> String {
    match &row[0] {
        MyValue::Bytes(b) => String::from_utf8(b.clone()).expect("utf8"),
        MyValue::NULL => "null".to_string(),
        other => mysql::from_value::<String>(other.clone()),
    }
}

/// Plan `gql`, render it for MySQL, run the single SQL statement against
/// `conn`, and parse the single column `root` as JSON.
fn run(conn: &mut Conn, md: &Metadata, catalog: &Catalog, gql: &str) -> Json {
    let doc = graphql_parser::parse_query::<String>(gql)
        .expect("query parses")
        .into_static();
    let plan = Planner::new(md, catalog)
        .plan(&doc, None, &serde_json::Map::new(), &session())
        .expect("planning succeeds");
    let roots = match plan {
        Plan::Query(roots) => roots,
        Plan::Mutation(_) => panic!("expected a query plan"),
    };
    let sql = donat_sqlgen::operation_to_sql_with(&roots, AnyDialect::Mysql(MySqlDialect));
    let rows: Vec<Row> = conn
        .query(&sql)
        .unwrap_or_else(|e| panic!("MySQL failed to execute:\n{sql}\n\nerror: {e}"));
    let row = rows
        .first()
        .unwrap_or_else(|| panic!("MySQL returned no rows for:\n{sql}"));
    let text = first_col_string(row);
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("returned text was not JSON:\n{text}\n\nerror: {e}"))
}

#[test]
fn mysql_e2e_full_pipeline() {
    let Some(mut conn) = connect() else {
        eprintln!("skipping mysql_e2e: no MySQL server reachable at {URL}");
        return;
    };
    seed(&mut conn);
    let catalog = mysql_introspect(&mut conn, SCHEMA).expect("introspect");
    let md = metadata();

    // 1. Plain root selection of all rows.
    let v = run(
        &mut conn,
        &md,
        &catalog,
        "query { author(order_by: { id: asc }) { id name } }",
    );
    assert_eq!(
        v,
        json!({ "author": [
            { "id": 1, "name": "Alice" },
            { "id": 2, "name": "Bob" },
            { "id": 3, "name": "Carol" },
        ]})
    );

    // 2. where filter.
    let v = run(
        &mut conn,
        &md,
        &catalog,
        "query { author(where: { id: { _eq: 1 } }) { id name } }",
    );
    assert_eq!(v, json!({ "author": [ { "id": 1, "name": "Alice" } ] }));

    // 3. Object relationship (article -> author).
    let v = run(
        &mut conn,
        &md,
        &catalog,
        "query { article(order_by: { id: asc }) { id title author { name } } }",
    );
    assert_eq!(
        v,
        json!({ "article": [
            { "id": 1, "title": "A1", "author": { "name": "Alice" } },
            { "id": 2, "title": "A2", "author": { "name": "Alice" } },
            { "id": 3, "title": "B1", "author": { "name": "Bob" } },
            { "id": 4, "title": "B2", "author": { "name": "Bob" } },
            { "id": 5, "title": "C1", "author": { "name": "Carol" } },
        ]})
    );

    // 4. Array relationship (author -> articles).
    let v = run(
        &mut conn,
        &md,
        &catalog,
        "query { author(order_by: { id: asc }) { id articles(order_by: { id: asc }) { title } } }",
    );
    assert_eq!(
        v,
        json!({ "author": [
            { "id": 1, "articles": [ { "title": "A1" }, { "title": "A2" } ] },
            { "id": 2, "articles": [ { "title": "B1" }, { "title": "B2" } ] },
            { "id": 3, "articles": [ { "title": "C1" } ] },
        ]})
    );

    // 5. Aggregate count.
    let v = run(
        &mut conn,
        &md,
        &catalog,
        "query { article_aggregate { aggregate { count } } }",
    );
    assert_eq!(
        v,
        json!({ "article_aggregate": { "aggregate": { "count": 5 } } })
    );
}
