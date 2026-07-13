//! DB-less pipeline tests: metadata fixture + hand-built catalog ->
//! planner -> SQL snapshots. This is the core TDD loop for the engine:
//! everything above the executor is exercised here.

use std::collections::BTreeMap;
use std::path::Path;

use donat_catalog::{Catalog, ColumnInfo, ForeignKey, TableInfo};
use donat_metadata::Metadata;
use donat_schema::{Planner, Session};

fn fixture_metadata() -> Metadata {
    let dir = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../metadata/tests/fixtures/metadata"
    ));
    donat_metadata::load_metadata_dir(dir).expect("fixture metadata loads")
}

fn col(name: &str, pg_type: &str, nullable: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        native_type: None,
        nullable,
        has_default: false,
    }
}

fn fixture_catalog() -> Catalog {
    let mut tables = BTreeMap::new();
    tables.insert(
        "public.author".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "author".into(),
            columns: vec![
                col("id", "int4", false),
                col("name", "text", false),
                // present in the catalog but NOT in the role's column mask
                col("secret", "text", true),
            ],
            primary_key: vec!["id".into()],
            foreign_keys: vec![],
        },
    );
    tables.insert(
        "public.article".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "article".into(),
            columns: vec![
                col("id", "int4", false),
                col("title", "text", false),
                col("author_id", "int4", false),
                col("published", "bool", false),
            ],
            primary_key: vec!["id".into()],
            foreign_keys: vec![ForeignKey {
                constraint_name: "article_author_id_fkey".into(),
                column_mapping: BTreeMap::from([("author_id".into(), "id".into())]),
                referenced_schema: "public".into(),
                referenced_table: "author".into(),
            }],
        },
    );
    Catalog {
        tables,
        functions: BTreeMap::new(),
    }
}

fn user_session() -> Session {
    Session {
        role: "user".into(),
        vars: std::collections::HashMap::from([("x-donat-user-id".to_string(), "1".to_string())]),
        backend_request: false,
    }
}

fn plan_sql(query: &str) -> String {
    plan_sql_with(query, &user_session())
}

fn plan_sql_with(query: &str, session: &Session) -> String {
    let metadata = fixture_metadata();
    let catalog = fixture_catalog();
    let planner = Planner::new(&metadata, &catalog);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    let plan = planner
        .plan(&doc, None, &serde_json::Map::new(), session)
        .expect("planning succeeds");
    match plan {
        donat_schema::Plan::Query(roots) => donat_sqlgen::operation_to_sql(&roots),
        donat_schema::Plan::Mutation(_) => panic!("expected a query plan"),
    }
}

fn plan_err(query: &str) -> String {
    let metadata = fixture_metadata();
    let catalog = fixture_catalog();
    let planner = Planner::new(&metadata, &catalog);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    planner
        .plan(&doc, None, &serde_json::Map::new(), &user_session())
        .expect_err("planning must fail")
        .message
}

#[test]
fn simple_select_with_permission_filter() {
    insta::assert_snapshot!(plan_sql("query { author { id name } }"));
}

#[test]
fn select_with_where_order_limit() {
    insta::assert_snapshot!(plan_sql(
        r#"query {
            article(where: { published: { _eq: true } },
                    order_by: { id: desc }, limit: 5, offset: 2) {
                id
                title
            }
        }"#
    ));
}

#[test]
fn object_relationship() {
    insta::assert_snapshot!(plan_sql("query { article { id author { name } } }"));
}

#[test]
fn array_relationship_with_args() {
    insta::assert_snapshot!(plan_sql(
        r#"query {
            author {
                id
                articles(where: { title: { _like: "%rust%" } }, order_by: { id: asc }) {
                    title
                }
            }
        }"#
    ));
}

#[test]
fn aliases_typename_and_fragments() {
    insta::assert_snapshot!(plan_sql(
        r#"query {
            __typename
            writers: author {
                __typename
                ident: id
                ...AuthorBits
            }
        }
        fragment AuthorBits on author { name }"#
    ));
}

#[test]
fn aggregate_with_count_and_nodes() {
    insta::assert_snapshot!(plan_sql(
        r#"query {
            article_aggregate(where: { published: { _eq: true } }) {
                aggregate { count }
                nodes { id }
            }
        }"#
    ));
}

#[test]
fn by_pk_lookup() {
    insta::assert_snapshot!(plan_sql("query { author_by_pk(id: 7) { id name } }"));
}

#[test]
fn relationship_predicate_in_where() {
    insta::assert_snapshot!(plan_sql(
        r#"query {
            article(where: { author: { name: { _eq: "tolstoy" } } }) { id }
        }"#
    ));
}

#[test]
fn distinct_on_columns_lead_order_by() {
    // The planner must prepend distinct_on columns to ORDER BY (Postgres
    // requires DISTINCT ON expressions to match the leftmost ORDER BY).
    insta::assert_snapshot!(plan_sql(
        r#"query {
            article(distinct_on: published, order_by: { id: desc }) { id }
        }"#
    ));
}

#[test]
fn string_literals_are_escaped() {
    // sqlgen inlines literals; quotes must be doubled, never raw.
    let sql = plan_sql(r#"query { article(where: { title: { _eq: "O'Brien" } }) { id } }"#);
    assert!(
        sql.contains("('O''Brien')::\"text\""),
        "escaped literal missing in: {sql}"
    );
    assert!(
        !sql.contains("'O'Brien'"),
        "raw unescaped literal leaked into: {sql}"
    );
}

#[test]
fn injection_payload_in_where_eq_is_escaped() {
    // Classic boolean-injection payload through a string filter.
    let sql = plan_sql(r#"query { article(where: { title: { _eq: "x' OR '1'='1" } }) { id } }"#);
    assert!(
        sql.contains("'x'' OR ''1''=''1'"),
        "payload not doubled in: {sql}"
    );
    assert!(
        !sql.contains("'x' OR '1'='1'"),
        "raw breakout leaked into: {sql}"
    );
}

#[test]
fn injection_payload_in_like_pattern_is_escaped() {
    let sql = plan_sql(r#"query { article(where: { title: { _like: "%' OR '1'='1%" } }) { id } }"#);
    assert!(
        sql.contains("'%'' OR ''1''=''1%'"),
        "payload not doubled in: {sql}"
    );
    assert!(
        !sql.contains("'%' OR '1'='1%'"),
        "raw breakout leaked into: {sql}"
    );
}

#[test]
fn injection_stacked_statement_in_in_list_is_escaped() {
    // A stacked-query payload inside an _in list must stay a single quoted
    // literal: the opening quote of the next "statement" is doubled, so it
    // never closes the literal.
    let sql = plan_sql(
        r#"query { article(where: { title: { _in: ["a", "b'); DROP TABLE article; --"] } }) { id } }"#,
    );
    assert!(
        sql.contains("'b''); DROP TABLE article; --'"),
        "payload not doubled in: {sql}"
    );
    assert!(
        !sql.contains("'b'); DROP TABLE article; --"),
        "raw quote breakout leaked into: {sql}"
    );
}

#[test]
fn injection_payload_in_session_var_is_escaped() {
    // A session-variable value flows into the permission filter; an injection
    // payload there must be quoted, not break out of the literal.
    let session = Session {
        role: "user".into(),
        vars: std::collections::HashMap::from([(
            "x-donat-user-id".to_string(),
            "1' OR '1'='1".to_string(),
        )]),
        backend_request: false,
    };
    let sql = plan_sql_with("query { article { id } }", &session);
    assert!(
        sql.contains("'1'' OR ''1''=''1'"),
        "session var not doubled in: {sql}"
    );
    assert!(
        !sql.contains("'1' OR '1'='1'"),
        "raw breakout leaked into: {sql}"
    );
}

#[test]
fn unknown_role_sees_nothing() {
    let session = Session {
        role: "stranger".into(),
        vars: Default::default(),
        backend_request: false,
    };
    let metadata = fixture_metadata();
    let catalog = fixture_catalog();
    let planner = Planner::new(&metadata, &catalog);
    let doc = graphql_parser::parse_query::<String>("query { author { id } }")
        .unwrap()
        .into_static();
    let err = planner
        .plan(&doc, None, &serde_json::Map::new(), &session)
        .expect_err("no permission, no field");
    assert_eq!(
        err.message,
        "field 'author' not found in type: 'query_root'"
    );
}

#[test]
fn column_outside_permission_mask_is_rejected() {
    // 'secret' exists in the catalog but the user role's mask is [id, name].
    let msg = plan_err("query { author { id secret } }");
    assert_eq!(msg, "field 'secret' not found in type: 'author'");
}

#[test]
fn unknown_field_is_rejected() {
    let msg = plan_err("query { author { id surname } }");
    assert_eq!(msg, "field 'surname' not found in type: 'author'");
}

#[test]
fn missing_session_variable_errors() {
    let session = Session {
        role: "user".into(),
        vars: Default::default(),
        backend_request: false,
    };
    let metadata = fixture_metadata();
    let catalog = fixture_catalog();
    let planner = Planner::new(&metadata, &catalog);
    let doc = graphql_parser::parse_query::<String>("query { author { id } }")
        .unwrap()
        .into_static();
    let err = planner
        .plan(&doc, None, &serde_json::Map::new(), &session)
        .expect_err("session var required by the row filter");
    assert_eq!(err.message, "missing session variable: \"x-donat-user-id\"");
}
