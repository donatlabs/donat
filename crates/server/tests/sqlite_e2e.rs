//! End-to-end validation of the SQLite backend binding against a REAL
//! in-memory SQLite database.
//!
//! This exercises the full pipeline:
//!   sqlite_introspect -> Metadata -> donat_schema::Planner::plan
//!   -> donat_sqlgen::operation_to_sql_with(.., AnyDialect::Sqlite)
//!   -> execute the ONE generated SQL string on a real rusqlite connection
//!   -> parse the returned JSON -> assert.
//!
//! It is the conformance slice the `SqliteDialect` JSON-assembly/scalar
//! renderings were flagged "validated against real sqlite in the harness
//! slice" — i.e. the failing test that drives any dialect fix.

use std::collections::HashMap;

use donat_backend::{AnyDialect, SqliteDialect};
use donat_catalog::{Catalog, sqlite_introspect};
use donat_metadata::Metadata;
use donat_schema::{Plan, Planner, Session};
use rusqlite::Connection;
use serde_json::{Value as Json, json};

/// Create the schema and seed deterministic rows.
fn seed(conn: &Connection) {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE author (
            id   INTEGER PRIMARY KEY,
            name TEXT
        );
        CREATE TABLE article (
            id        INTEGER PRIMARY KEY,
            title     TEXT,
            author_id INTEGER REFERENCES author(id)
        );
        INSERT INTO author (id, name) VALUES
            (1, 'Alice'),
            (2, 'Bob'),
            (3, 'Carol');
        INSERT INTO article (id, title, author_id) VALUES
            (1, 'A1', 1),
            (2, 'A2', 1),
            (3, 'B1', 2),
            (4, 'B2', 2),
            (5, 'C1', 3);
        "#,
    )
    .expect("seed schema + data");
}

/// In-memory metadata tracking both tables under schema `main`, with a
/// `user` role granted select on every column (filter `{}`), the FK-based
/// object relationship `article.author` and array relationship
/// `author.articles`, and aggregations enabled.
fn metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": [
                {
                    "table": { "schema": "main", "name": "author" },
                    "configuration": { "custom_name": "author" },
                    "array_relationships": [{
                        "name": "articles",
                        "using": { "foreign_key_constraint_on": {
                            "table": { "schema": "main", "name": "article" },
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
                    "table": { "schema": "main", "name": "article" },
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

/// Plan `gql`, render it for SQLite, run the single SQL statement against
/// `conn`, and parse the single text column `root` as JSON.
fn run(conn: &Connection, md: &Metadata, catalog: &Catalog, gql: &str) -> Json {
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
    let sql = donat_sqlgen::operation_to_sql_with(&roots, AnyDialect::Sqlite(SqliteDialect));
    let text: String = conn
        .query_row(&sql, [], |r| r.get::<_, String>(0))
        .unwrap_or_else(|e| panic!("SQLite failed to execute:\n{sql}\n\nerror: {e}"));
    serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("returned text was not JSON:\n{text}\n\nerror: {e}"))
}

#[test]
fn sqlite_e2e_full_pipeline() {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    seed(&conn);
    let catalog = sqlite_introspect(&conn).expect("introspect");
    let md = metadata();

    // 1. Plain root selection of all rows.
    let v = run(
        &conn,
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
        &conn,
        &md,
        &catalog,
        "query { author(where: { id: { _eq: 1 } }) { id name } }",
    );
    assert_eq!(v, json!({ "author": [ { "id": 1, "name": "Alice" } ] }));

    // 3. Object relationship (article -> author).
    let v = run(
        &conn,
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
        &conn,
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
        &conn,
        &md,
        &catalog,
        "query { article_aggregate { aggregate { count } } }",
    );
    assert_eq!(
        v,
        json!({ "article_aggregate": { "aggregate": { "count": 5 } } })
    );
}
