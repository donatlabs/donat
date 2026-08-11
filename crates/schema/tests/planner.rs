//! DB-less planner unit tests: in-memory metadata + catalog -> Planner.
//!
//! Covers pure planning logic the conformance suite only hits indirectly:
//! bool_exp parsing (legacy `$op` spellings, session variables), argument
//! handling (variables, fragments, by_pk/aggregate gating, permission
//! limits), exact v1 error shapes, and inherited-role resolution.

use std::collections::{BTreeMap, HashMap};

use donat_catalog_types::{Catalog, ColumnInfo, ForeignKey, TableInfo};
use donat_ir::{
    BoolExp, CompareOp, FieldValue, MutationRoot, OrderByTarget, RootField, Scalar, SelectQuery,
    SetOp,
};
use donat_metadata::{Metadata, SourceKind};
use donat_schema::{Plan, PlanError, Planner, Session, execute_introspection};
use serde_json::{Map as JsonMap, Value as Json, json};

fn metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": [
                {
                    "table": { "schema": "public", "name": "author" },
                    "configuration": {
                        "custom_column_names": { "display_name": "displayName" },
                        "column_config": {
                            "display_name": { "custom_name": "legacyDisplayName" }
                        }
                    },
                    "array_relationships": [{
                        "name": "articles",
                        "using": { "foreign_key_constraint_on": {
                            "table": { "schema": "public", "name": "article" },
                            "column": "author_id"
                        }}
                    }],
                    "object_relationships": [{
                        "name": "profile",
                        "using": { "manual_configuration": {
                            "remote_table": { "schema": "public", "name": "profile" },
                            "column_mapping": { "id": "author_id" },
                            "insertion_order": "after_parent"
                        }}
                    }],
                    "insert_permissions": [
                        { "role": "user", "permission": { "check": {}, "columns": ["name"] } },
                        { "role": "upserter", "permission": {
                            "check": {}, "columns": ["id", "name", "display_name"]
                        } }
                    ],
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": ["id", "name", "display_name"],
                            "filter": { "id": { "_eq": "X-Donat-User-Id" } }
                        }},
                        { "role": "hasura_user", "permission": {
                            "columns": ["id", "name"],
                            "filter": { "id": { "_eq": "X-Hasura-User-Id" } }
                        }},
                        { "role": "nopk", "permission": { "columns": ["name"], "filter": {} } },
                        { "role": "s1", "permission": {
                            "columns": ["id"], "filter": { "id": { "_eq": 1 } }, "limit": 10
                        }},
                        { "role": "s2", "permission": {
                            "columns": ["id", "name"], "filter": { "id": { "_eq": 2 } }, "limit": 20
                        }},
                        { "role": "s3", "permission": { "columns": ["id"], "filter": {} } }
                    ],
                    "update_permissions": [
                        { "role": "user", "permission": { "columns": ["name", "system_meta"], "filter": {} } },
                        { "role": "preset_user", "permission": {
                            "columns": ["name"], "filter": {}, "set": { "name": "preset" }
                        }},
                        { "role": "upserter", "permission": {
                            "columns": ["display_name"],
                            "filter": { "id": { "_eq": "X-Donat-User-Id" } }
                        }}
                    ]
                },
                {
                    "table": { "schema": "public", "name": "profile" },
                    "object_relationships": [{
                        "name": "author",
                        "using": { "foreign_key_constraint_on": "author_id" }
                    }],
                    "insert_permissions": [
                        { "role": "user", "permission": { "check": {}, "columns": ["author_id", "bio"] } }
                    ],
                    "select_permissions": [
                        { "role": "user", "permission": { "columns": ["author_id", "bio"], "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "article" },
                    "object_relationships": [{
                        "name": "author",
                        "using": { "foreign_key_constraint_on": "author_id" }
                    }],
                    "select_permissions": [
                        { "role": "user", "permission": {
                            "columns": "*", "filter": {}, "limit": 100, "allow_aggregations": true
                        }},
                        { "role": "counter", "permission": {
                            "columns": [], "filter": {}, "allow_aggregations": true
                        }},
                        { "role": "tagged", "permission": {
                            "columns": ["id", "title"],
                            "filter": { "id": { "_in": "X-Donat-Allowed-Ids" } }
                        }}
                    ],
                    "delete_permissions": [
                        { "role": "p1", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "p2", "permission": { "filter": { "published": { "_eq": false } } } },
                        { "role": "q1", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "q2", "permission": { "filter": { "published": { "_eq": true } } } },
                        { "role": "kidfix", "permission": { "filter": {} } }
                    ]
                }
            ]
        }],
        "inherited_roles": [
            { "role_name": "kid", "role_set": ["p1", "p2"] },
            { "role_name": "kidfix", "role_set": ["p1", "p2"] },
            { "role_name": "twins", "role_set": ["q1", "q2"] },
            { "role_name": "inh", "role_set": ["s1", "s2"] },
            { "role_name": "inh2", "role_set": ["s1", "s3"] }
        ]
    }))
    .expect("metadata deserializes")
}

fn col(name: &str, pg_type: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        pg_typmod: -1,
        native_type: None,
        nullable: false,
        has_default: false,
    }
}

fn catalog() -> Catalog {
    let mut tables = BTreeMap::new();
    tables.insert(
        "public.author".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "author".into(),
            relation_kind: donat_catalog_types::RelationKind::Table,
            columns: vec![
                col("id", "int4"),
                col("name", "text"),
                col("display_name", "text"),
                col("secret", "text"),
                col("system_meta", "jsonb"),
            ],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys: vec![],
        },
    );
    tables.insert(
        "public.profile".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "profile".into(),
            relation_kind: donat_catalog_types::RelationKind::Table,
            columns: vec![col("author_id", "int4"), col("bio", "text")],
            primary_key: vec!["author_id".into()],
            unique_keys: vec![],
            foreign_keys: vec![ForeignKey {
                constraint_name: "profile_author_id_fkey".into(),
                column_mapping: BTreeMap::from([("author_id".into(), "id".into())]),
                referenced_schema: "public".into(),
                referenced_table: "author".into(),
            }],
        },
    );
    tables.insert(
        "public.article".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "article".into(),
            relation_kind: donat_catalog_types::RelationKind::Table,
            columns: vec![
                col("id", "int4"),
                col("title", "text"),
                col("author_id", "int4"),
                col("published", "bool"),
            ],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
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

fn session(role: &str, vars: &[(&str, &str)]) -> Session {
    Session {
        role: role.to_string(),
        vars: vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<HashMap<_, _>>(),
        backend_request: false,
    }
}

fn user() -> Session {
    session("user", &[("x-donat-user-id", "7")])
}

fn plan_gql(query: &str, sess: &Session, variables: Json) -> Result<Plan, PlanError> {
    plan_gql_for_source(SourceKind::Postgres, query, sess, variables)
}

fn plan_gql_for_source(
    source_kind: SourceKind,
    query: &str,
    sess: &Session,
    variables: Json,
) -> Result<Plan, PlanError> {
    let mut md = metadata();
    md.sources[0].kind = source_kind;
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    let vars = variables.as_object().cloned().unwrap_or_default();
    planner.plan(&doc, None, &vars, sess)
}

fn introspect_for_source(source_kind: SourceKind, query: &str) -> Json {
    let mut md = metadata();
    md.sources[0].kind = source_kind;
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    execute_introspection(&planner, &user(), &doc, None, &JsonMap::new())
        .expect("introspection query")
        .expect("introspection succeeds")
}

fn first_select(plan: Plan) -> SelectQuery {
    match plan {
        Plan::Query(roots) => match roots.into_iter().next().expect("one root") {
            RootField::Select { query, .. } => query,
            other => panic!("expected a select root, got {other:?}"),
        },
        Plan::Mutation(_) => panic!("expected a query plan"),
    }
}

fn gql_select(query: &str, sess: &Session) -> SelectQuery {
    first_select(plan_gql(query, sess, json!({})).expect("planning succeeds"))
}

fn gql_err(query: &str, sess: &Session) -> PlanError {
    plan_gql(query, sess, json!({})).expect_err("planning must fail")
}

fn v1_select(args: Json, sess: &Session) -> Result<SelectQuery, PlanError> {
    let md = metadata();
    let cat = catalog();
    Planner::new(&md, &cat).plan_v1_select(&args, sess)
}

/// The user's article select has an unrestricted filter, so the planned
/// predicate is exactly the parsed `where`.
fn article_where(where_exp: Json, sess: &Session) -> Result<Option<BoolExp>, PlanError> {
    v1_select(
        json!({ "table": "article", "columns": ["id"], "where": where_exp }),
        sess,
    )
    .map(|q| q.predicate)
}

#[test]
fn custom_column_names_are_exposed_in_introspection() {
    let md = metadata();
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc =
        graphql_parser::parse_query::<String>(r#"{ __type(name: "author") { fields { name } } }"#)
            .expect("query parses")
            .into_static();
    let data = execute_introspection(&planner, &user(), &doc, None, &JsonMap::new())
        .expect("introspection query")
        .expect("introspection succeeds");

    let fields = data["__type"]["fields"]
        .as_array()
        .expect("fields array")
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect::<Vec<_>>();
    assert!(fields.contains(&"displayName"), "{fields:?}");
    assert!(!fields.contains(&"display_name"), "{fields:?}");
}

#[test]
fn custom_column_names_plan_to_physical_columns() {
    let query = r#"
        query {
          author(
            where: { displayName: { _eq: "Ada" } }
            order_by: { displayName: asc }
          ) {
            displayName
          }
        }
    "#;
    let select = gql_select(query, &user());

    let [field] = select.fields.as_slice() else {
        panic!("expected one selected field, got {:?}", select.fields);
    };
    assert!(matches!(
        &field.value,
        FieldValue::Column { column, .. } if column == "display_name"
    ));
    assert!(matches!(
        &select.order_by[0].target,
        OrderByTarget::Column(column) if column == "display_name"
    ));
    assert!(matches!(
        select.predicate,
        Some(BoolExp::And(items)) if items.iter().any(|item| {
            matches!(item, BoolExp::Compare { column, .. } if column == "display_name")
        })
    ));
}

#[test]
fn insert_input_exposes_after_parent_object_relationships() {
    let md = metadata();
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>(
        r#"{ __type(name: "author_insert_input") { inputFields { name type { name } } } }"#,
    )
    .expect("query parses")
    .into_static();
    let data = execute_introspection(&planner, &user(), &doc, None, &JsonMap::new())
        .expect("introspection query")
        .expect("introspection succeeds");

    let fields = data["__type"]["inputFields"]
        .as_array()
        .expect("inputFields array")
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect::<Vec<_>>();
    assert!(fields.contains(&"profile"), "{fields:?}");
}

#[test]
fn insert_one_accepts_after_parent_object_relationship_data() {
    let plan = plan_gql(
        r#"
        mutation {
          insert_author_one(object: { name: "Ada", profile: { data: { bio: "math" } } }) {
            id
            name
          }
        }
        "#,
        &user(),
        json!({}),
    )
    .expect("nested object insert plans");

    let Plan::Mutation(roots) = plan else {
        panic!("expected mutation plan")
    };
    let [MutationRoot::Insert { insert, .. }] = roots.as_slice() else {
        panic!("expected one insert root, got {roots:?}")
    };
    assert_eq!(insert.nested_object_inserts.len(), 1);
    let nested = &insert.nested_object_inserts[0];
    assert_eq!(nested.relationship_name, "profile");
    assert_eq!(nested.table.name, "profile");
    assert_eq!(
        nested.column_mapping,
        vec![("id".to_string(), "author_id".to_string())]
    );
    assert_eq!(
        nested.columns,
        vec![("bio".to_string(), "text".to_string())]
    );
}

#[test]
fn sqlite_and_mysql_hide_and_reject_after_parent_nested_inserts() {
    let nested_insert = r#"
        mutation {
          insert_author_one(object: { name: "Ada", profile: { data: { bio: "math" } } }) {
            id
          }
        }
    "#;

    for source_kind in [SourceKind::Sqlite, SourceKind::Mysql] {
        let data = introspect_for_source(
            source_kind,
            r#"{ __type(name: "author_insert_input") { inputFields { name } } }"#,
        );
        let fields = data["__type"]["inputFields"]
            .as_array()
            .expect("insert input fields")
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!fields.contains(&"profile"), "{source_kind:?}: {fields:?}");

        let error = plan_gql_for_source(source_kind, nested_insert, &user(), json!({}))
            .expect_err("unsupported nested insert must fail during planning");
        assert_eq!(
            error.message, "field 'profile' not found in type: 'author_insert_input'",
            "{source_kind:?}"
        );
    }
}

#[test]
fn update_by_pk_accepts_jsonb_append_input() {
    let plan = plan_gql(
        r#"
        mutation {
          update_author_by_pk(
            pk_columns: { id: 1 }
            _append: { system_meta: { deleted: { by: "user-1" } } }
          ) {
            id
          }
        }
        "#,
        &user(),
        json!({}),
    )
    .expect("jsonb append update plans");

    let Plan::Mutation(roots) = plan else {
        panic!("expected mutation plan")
    };
    let [MutationRoot::Update { update, .. }] = roots.as_slice() else {
        panic!("expected one update root, got {roots:?}")
    };
    assert!(
        update.sets.iter().any(|op| matches!(
            op,
            SetOp::JsonbAppend { column, .. } if column == "system_meta"
        )),
        "{:?}",
        update.sets
    );
}

#[test]
fn update_introspection_exposes_jsonb_append_input() {
    let md = metadata();
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>(
        r#"
        {
          __type(name: "author_append_input") {
            inputFields { name }
          }
        }
        "#,
    )
    .expect("query parses")
    .into_static();
    let data = execute_introspection(&planner, &user(), &doc, None, &JsonMap::new())
        .expect("introspection query")
        .expect("introspection succeeds");

    let fields = data["__type"]["inputFields"]
        .as_array()
        .expect("inputFields array")
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(fields, vec!["system_meta"]);
}

#[test]
fn sqlite_and_mysql_hide_and_reject_jsonb_append_updates() {
    let append_update = r#"
        mutation {
          update_author_by_pk(
            pk_columns: { id: 1 }
            _append: { system_meta: { deleted: true } }
          ) { id }
        }
    "#;

    for source_kind in [SourceKind::Sqlite, SourceKind::Mysql] {
        let mutation = introspect_for_source(
            source_kind,
            r#"{ __type(name: "mutation_root") { fields { name args { name } } } }"#,
        );
        let update = mutation["__type"]["fields"]
            .as_array()
            .expect("mutation fields")
            .iter()
            .find(|field| field["name"] == "update_author")
            .expect("update_author field");
        let args = update["args"]
            .as_array()
            .expect("update arguments")
            .iter()
            .filter_map(|arg| arg["name"].as_str())
            .collect::<Vec<_>>();
        assert!(!args.contains(&"_append"), "{source_kind:?}: {args:?}");

        let append_type = introspect_for_source(
            source_kind,
            r#"{ __type(name: "author_append_input") { name } }"#,
        );
        assert_eq!(append_type["__type"], Json::Null, "{source_kind:?}");

        let error = plan_gql_for_source(source_kind, append_update, &user(), json!({}))
            .expect_err("unsupported jsonb append must fail during planning");
        assert_eq!(
            error.message, "unexpected argument: \"_append\"",
            "{source_kind:?}"
        );
    }
}

#[test]
fn sqlite_and_mysql_reject_deferred_upserts_during_planning() {
    for source_kind in [SourceKind::Sqlite, SourceKind::Mysql] {
        let error = plan_gql_for_source(
            source_kind,
            r#"
            mutation {
              insert_author(
                objects: [{ name: "Ada" }]
                on_conflict: { constraint: author_pkey, update_columns: [name] }
              ) {
                affected_rows
              }
            }
            "#,
            &user(),
            json!({}),
        )
        .expect_err("deferred upsert must be rejected by the planner");

        assert_eq!(
            error.message, "unexpected argument: \"on_conflict\"",
            "{source_kind:?}"
        );
    }
}

#[test]
fn postgres_still_plans_upserts() {
    let plan = plan_gql(
        r#"
        mutation {
          insert_author(
            objects: [{ name: "Ada" }]
            on_conflict: { constraint: author_pkey, update_columns: [name] }
          ) {
            affected_rows
          }
        }
        "#,
        &user(),
        json!({}),
    )
    .expect("Postgres upsert plans");

    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Insert { insert, .. } = &roots[0] else {
        panic!("expected an insert root")
    };
    assert!(insert.on_conflict.is_some());
}

/// The columns a `DO UPDATE` names are an update, and the role's update
/// permission is what says which columns those are.
///
/// `update_columns` used to be checked against the catalog alone, so a role
/// holding an insert permission could overwrite an existing row's column by
/// upserting onto it — past the update permission's own filter, presets and
/// validators. A column outside that permission is refused exactly as an
/// unknown one is, because the enum a role sees is the enum of the columns it
/// may update.
#[test]
fn an_upsert_writes_only_columns_the_role_may_update() {
    // `profile` has an insert permission for `user` and no update permission
    // at all: no column of an existing row is this role's to write.
    let error = gql_err(
        r#"mutation {
             insert_profile(
               objects: [{ author_id: 1, bio: "rewritten" }]
               on_conflict: { constraint: profile_pkey, update_columns: [bio] }
             ) { affected_rows }
           }"#,
        &user(),
    );
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "erroneous column name");
    assert_eq!(error.path, "$.selectionSet.insert_profile.args.on_conflict");

    // An empty list is `DO NOTHING`: it writes nothing, so it stays available
    // to a role that may not update at all.
    let plan = plan_gql(
        r#"mutation {
             insert_profile(
               objects: [{ author_id: 1, bio: "new" }]
               on_conflict: { constraint: profile_pkey, update_columns: [] }
             ) { affected_rows }
           }"#,
        &user(),
        json!({}),
    )
    .expect("an ignoring upsert needs no update permission");
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Insert { insert, .. } = &roots[0] else {
        panic!("expected an insert root")
    };
    let conflict = insert
        .on_conflict
        .as_ref()
        .expect("the clause is still planned");
    assert!(conflict.update_columns.is_empty());
    assert!(conflict.set_ops.is_empty());

    // `secret` is a column of the table and outside the user's update
    // permission (`name`, `system_meta`).
    let error = gql_err(
        r#"mutation {
             insert_author(
               objects: [{ name: "Ada" }]
               on_conflict: { constraint: author_pkey, update_columns: [secret] }
             ) { affected_rows }
           }"#,
        &user(),
    );
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "erroneous column name");
    assert_eq!(error.path, "$.selectionSet.insert_author.args.on_conflict");

    // `upserter` may insert `name` and `display_name` but update only
    // `display_name`: the insert permission does not widen the update one.
    let upserter = session("upserter", &[("x-donat-user-id", "7")]);
    let error = plan_gql(
        r#"mutation {
             insert_author(
               objects: [{ name: "Ada" }]
               on_conflict: { constraint: author_pkey, update_columns: [name] }
             ) { affected_rows }
           }"#,
        &upserter,
        json!({}),
    )
    .expect_err("an insertable column is not thereby an updatable one");
    assert_eq!(error.message, "erroneous column name");

    let plan = plan_gql(
        r#"mutation {
             insert_author(
               objects: [{ name: "Ada", displayName: "Ada L." }]
               on_conflict: { constraint: author_pkey, update_columns: [displayName] }
             ) { affected_rows }
           }"#,
        &upserter,
        json!({}),
    )
    .expect("the one column the role may update plans");
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Insert { insert, .. } = &roots[0] else {
        panic!("expected an insert root")
    };
    let conflict = insert.on_conflict.as_ref().expect("the clause is planned");
    assert_eq!(conflict.update_columns, vec!["display_name".to_string()]);
    assert!(
        conflict.predicate.is_some(),
        "the update permission's filter gates which existing rows may change"
    );
}

/// The same gate on `/v1`, where `action: update` names no columns at all and
/// the planner chooses them: it re-applies the inserted columns the role may
/// update, and nothing else.
#[test]
fn a_v1_upsert_updates_only_columns_the_role_may_update() {
    let md = metadata();
    let cat = catalog();
    let planner = Planner::new(&md, &cat);

    let insert = planner
        .plan_v1_insert(
            &json!({
                "table": "profile",
                "objects": [{ "author_id": 1, "bio": "rewritten" }],
                "on_conflict": { "constraint": "profile_pkey", "action": "update" }
            }),
            &user(),
        )
        .expect("the insert itself is permitted");
    let conflict = insert.on_conflict.expect("the clause is planned");
    assert!(
        conflict.update_columns.is_empty() && conflict.set_ops.is_empty(),
        "a role with no update permission writes nothing on conflict: {conflict:?}"
    );

    let upserter = session("upserter", &[("x-donat-user-id", "7")]);
    let insert = planner
        .plan_v1_insert(
            &json!({
                "table": "author",
                "objects": [{ "name": "Ada", "display_name": "Ada L." }],
                "on_conflict": { "constraint": "author_pkey", "action": "update" }
            }),
            &upserter,
        )
        .expect("the upsert plans");
    let conflict = insert.on_conflict.expect("the clause is planned");
    assert_eq!(
        conflict.update_columns,
        vec!["display_name".to_string()],
        "an inserted column the role may not update is not re-applied"
    );
    assert!(conflict.predicate.is_some(), "the filter still gates");
}

// ---------------------------------------------------------------------
// predicate.rs: bool_exp parsing
// ---------------------------------------------------------------------

#[test]
fn legacy_dollar_logical_ops_parse() {
    let pred = article_where(
        json!({ "$or": [ { "id": { "_gt": 5 } }, { "$not": { "published": true } } ] }),
        &user(),
    )
    .unwrap()
    .expect("predicate present");
    let BoolExp::Or(items) = pred else {
        panic!("expected Or, got {pred:?}")
    };
    assert_eq!(items.len(), 2);
    assert!(
        matches!(&items[0], BoolExp::Compare { column, op: CompareOp::Gt(_), .. } if column == "id")
    );
    let BoolExp::Not(inner) = &items[1] else {
        panic!("expected Not")
    };
    assert!(matches!(
        &**inner,
        BoolExp::Compare {
            op: CompareOp::Eq(_),
            ..
        }
    ));
}

#[test]
fn legacy_dollar_comparison_ops_parse() {
    let pred = article_where(json!({ "id": { "$gt": 5 } }), &user())
        .unwrap()
        .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare {
            op: CompareOp::Gt(_),
            ..
        }
    ));
    // `$ne` is the legacy alias of `_neq`.
    let pred = article_where(json!({ "id": { "$ne": 3 } }), &user())
        .unwrap()
        .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare {
            op: CompareOp::Neq(_),
            ..
        }
    ));
}

#[test]
fn bare_value_is_implicit_eq() {
    let pred = article_where(json!({ "id": 7 }), &user()).unwrap().unwrap();
    let BoolExp::Compare {
        column,
        op: CompareOp::Eq(Scalar::Json(v)),
        ..
    } = pred
    else {
        panic!("expected implicit _eq compare")
    };
    assert_eq!(column, "id");
    assert_eq!(v, json!(7));
}

#[test]
fn unknown_operator_error_shape() {
    let err = article_where(json!({ "id": { "_bogus": 1 } }), &user()).unwrap_err();
    assert_eq!(err.code, "validation-failed");
    assert_eq!(
        err.message,
        "unexpected operator \"_bogus\" for column 'id'"
    );
}

#[test]
fn unknown_column_in_bool_exp_error_shape() {
    let err = article_where(json!({ "nope": { "_eq": 1 } }), &user()).unwrap_err();
    assert_eq!(err.code, "validation-failed");
    assert_eq!(
        err.message,
        "field 'nope' not found in type: 'article_bool_exp'"
    );
}

#[test]
fn exists_requires_table() {
    let err = article_where(json!({ "_exists": { "_where": {} } }), &user()).unwrap_err();
    assert_eq!(err.message, "_exists needs a _table");
}

#[test]
fn exists_predicate_parses() {
    let pred = article_where(
        json!({ "_exists": {
            "_table": { "schema": "public", "name": "author" },
            "_where": { "name": { "_eq": "x" } }
        }}),
        &user(),
    )
    .unwrap()
    .unwrap();
    let BoolExp::Exists { table, predicate } = pred else {
        panic!("expected Exists")
    };
    assert_eq!(table.name, "author");
    assert!(matches!(
        &*predicate,
        BoolExp::Compare {
            op: CompareOp::Eq(_),
            ..
        }
    ));
}

#[test]
fn session_var_substituted_in_permission_filter() {
    // The author filter references X-Donat-User-Id (mixed case); lookup is
    // case-insensitive and the substituted value lands as a string literal.
    let q = gql_select("query { author { id } }", &user());
    let Some(BoolExp::Compare {
        column,
        op: CompareOp::Eq(Scalar::Json(v)),
        ..
    }) = q.predicate
    else {
        panic!("expected the permission filter as the only predicate")
    };
    assert_eq!(column, "id");
    assert_eq!(v, json!("7"));
}

#[test]
fn hasura_session_var_substituted_in_permission_filter() {
    let q = gql_select(
        "query { author { id } }",
        &session("hasura_user", &[("x-hasura-user-id", "42")]),
    );
    let Some(BoolExp::Compare {
        column,
        op: CompareOp::Eq(Scalar::Json(v)),
        ..
    }) = q.predicate
    else {
        panic!("expected the permission filter as the only predicate")
    };
    assert_eq!(column, "id");
    assert_eq!(v, json!("42"));
}

#[test]
fn missing_hasura_session_var_error_shape() {
    let err = gql_err("query { author { id } }", &session("hasura_user", &[]));
    assert_eq!(err.code, "not-found");
    assert_eq!(err.path, "$");
    assert_eq!(
        err.message,
        "missing session variable: \"x-hasura-user-id\""
    );
}

#[test]
fn missing_session_var_error_shape() {
    let err = gql_err("query { author { id } }", &session("user", &[]));
    assert_eq!(err.code, "not-found");
    // Donat reports path "$" regardless of filter depth, name lower-cased.
    assert_eq!(err.path, "$");
    assert_eq!(err.message, "missing session variable: \"x-donat-user-id\"");
}

#[test]
fn session_var_not_resolved_in_user_where() {
    // Clients cannot reference session variables; the string stays literal.
    let pred = article_where(
        json!({ "title": { "_eq": "X-Donat-User-Id" } }),
        &session("user", &[]),
    )
    .unwrap()
    .unwrap();
    let BoolExp::Compare {
        op: CompareOp::Eq(Scalar::Json(v)),
        ..
    } = pred
    else {
        panic!("expected compare")
    };
    assert_eq!(v, json!("X-Donat-User-Id"));
}

#[test]
fn in_session_var_accepts_array_spellings() {
    // A session variable used with _in may hold a Postgres array literal...
    let q = v1_select(
        json!({ "table": "article", "columns": ["id"] }),
        &session("tagged", &[("x-donat-allowed-ids", "{1,2}")]),
    )
    .unwrap();
    let Some(BoolExp::Compare {
        op: CompareOp::In(items),
        ..
    }) = q.predicate
    else {
        panic!("expected In predicate")
    };
    assert_eq!(
        items.iter().map(Scalar::as_json).collect::<Vec<_>>(),
        vec![&json!("1"), &json!("2")]
    );

    // ...or a JSON array.
    let q = v1_select(
        json!({ "table": "article", "columns": ["id"] }),
        &session("tagged", &[("x-donat-allowed-ids", "[1,2]")]),
    )
    .unwrap();
    let Some(BoolExp::Compare {
        op: CompareOp::In(items),
        ..
    }) = q.predicate
    else {
        panic!("expected In predicate")
    };
    assert_eq!(
        items.iter().map(Scalar::as_json).collect::<Vec<_>>(),
        vec![&json!(1), &json!(2)]
    );
}

#[test]
fn in_rejects_non_array_value() {
    let err = article_where(json!({ "id": { "_in": 5 } }), &user()).unwrap_err();
    assert_eq!(err.message, "expected an array of values");
}

#[test]
fn is_null_parses_bool_operand() {
    let pred = article_where(json!({ "title": { "_is_null": true } }), &user())
        .unwrap()
        .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare {
            op: CompareOp::IsNull(true),
            ..
        }
    ));
    let pred = article_where(json!({ "title": { "_is_null": false } }), &user())
        .unwrap()
        .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare {
            op: CompareOp::IsNull(false),
            ..
        }
    ));
}

fn permission_is_null(session_value: &str) -> Result<SelectQuery, PlanError> {
    let mut metadata = metadata();
    metadata.sources[0].tables[2].select_permissions.push(
        serde_json::from_value(json!({
            "role": "null_filter",
            "permission": {
                "columns": ["id", "title"],
                "filter": {
                    "title": { "_is_null": "X-Donat-Is-Null" }
                }
            }
        }))
        .expect("permission fixture deserializes"),
    );
    let catalog = catalog();
    Planner::new(&metadata, &catalog).plan_v1_select(
        &json!({ "table": "article", "columns": ["id"] }),
        &session("null_filter", &[("x-donat-is-null", session_value)]),
    )
}

#[test]
fn is_null_permission_session_string_is_strict_boolean() {
    for (value, expected) in [("TRUE", true), ("false", false)] {
        let query = permission_is_null(value)
            .unwrap_or_else(|error| panic!("`{value}` must parse: {error:?}"));
        assert!(matches!(
            query.predicate,
            Some(BoolExp::Compare {
                op: CompareOp::IsNull(actual),
                ..
            }) if actual == expected
        ));
    }

    let error = permission_is_null("yes").expect_err("non-boolean session text must fail closed");
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "expected a boolean");
}

#[test]
fn column_compare_root_and_relationship_paths() {
    // ["$", col] compares against the bool_exp's root table.
    let pred = article_where(json!({ "id": { "$ceq": ["$", "author_id"] } }), &user())
        .unwrap()
        .unwrap();
    let BoolExp::Compare {
        op:
            CompareOp::CompareColumn {
                sql_op,
                column,
                root,
            },
        ..
    } = pred
    else {
        panic!("expected CompareColumn")
    };
    assert_eq!(
        (sql_op.as_str(), column.as_str(), root),
        ("=", "author_id", true)
    );

    // [rel, col] compares against a column of the related table.
    let pred = article_where(json!({ "id": { "_ceq": ["author", "id"] } }), &user())
        .unwrap()
        .unwrap();
    let BoolExp::Compare {
        op: CompareOp::CompareColumnRel { table, column, .. },
        ..
    } = pred
    else {
        panic!("expected CompareColumnRel")
    };
    assert_eq!((table.name.as_str(), column.as_str()), ("author", "id"));
}

// ---------------------------------------------------------------------
// plan.rs: variables, fragments, gating, permission limits
// ---------------------------------------------------------------------

#[test]
fn variable_defaults_and_overrides() {
    // The definition's default applies when the variable is absent...
    let q = "query($lim: Int = 3) { article(limit: $lim) { id } }";
    assert_eq!(
        first_select(plan_gql(q, &user(), json!({})).unwrap()).limit,
        Some(3)
    );
    // ...and a provided value overrides it.
    assert_eq!(
        first_select(plan_gql(q, &user(), json!({ "lim": 7 })).unwrap()).limit,
        Some(7)
    );
}

#[test]
fn missing_required_variable_error() {
    let err = gql_err("query($lim: Int!) { article(limit: $lim) { id } }", &user());
    assert_eq!(
        err.message,
        "expecting a value for non-nullable variable: \"lim\""
    );
}

#[test]
fn skip_and_include_directives_drop_fields() {
    let q = gql_select(
        "query { article { id title @skip(if: true) published @include(if: false) } }",
        &user(),
    );
    assert_eq!(q.fields.len(), 1);
    assert_eq!(q.fields[0].alias, "id");
}

#[test]
fn inline_fragment_with_other_type_condition_is_skipped() {
    let q = gql_select("query { article { id ... on author { name } } }", &user());
    assert_eq!(q.fields.len(), 1);
    assert_eq!(q.fields[0].alias, "id");
}

#[test]
fn fragment_errors() {
    // A spread whose type condition mismatches the parent type is an error.
    let err = gql_err(
        "query { article { ...Bits } } fragment Bits on author { id }",
        &user(),
    );
    assert_eq!(
        err.message,
        "fragment \"Bits\" is defined on 'author', not 'article'"
    );
    // An undefined fragment is reported by name.
    let err = gql_err("query { article { ...Nope } }", &user());
    assert_eq!(err.message, "fragment \"Nope\" not found");
}

#[test]
fn by_pk_hidden_when_role_cannot_select_all_pk_columns() {
    // Role "nopk" may select author.name but not the pk column id.
    let err = gql_err(
        "query { author_by_pk(id: 1) { name } }",
        &session("nopk", &[]),
    );
    assert_eq!(
        err.message,
        "field 'author_by_pk' not found in type: 'query_root'"
    );
}

#[test]
fn by_pk_missing_pk_argument_error() {
    let err = gql_err("query { author_by_pk { id } }", &user());
    assert_eq!(err.message, "missing required field argument: \"id\"");
}

#[test]
fn distinct_on_rejects_unknown_column() {
    let err = gql_err("query { article(distinct_on: nope) { id } }", &user());
    assert_eq!(err.message, "field 'nope' not found in type: 'article'");
}

#[test]
fn columnless_role_count_columns_arg_rejected() {
    // Role "counter" has columns: [] — plain count works...
    plan_gql(
        "query { article_aggregate { aggregate { count } } }",
        &session("counter", &[]),
        json!({}),
    )
    .expect("plain count plans");
    // ...but Donat omits count(columns:) from such a role's schema.
    let err = gql_err(
        "query { article_aggregate { aggregate { count(columns: id) } } }",
        &session("counter", &[]),
    );
    assert_eq!(err.message, "'count' has no argument named 'columns'");
}

#[test]
fn columnless_role_has_no_column_aggregate_ops() {
    let err = gql_err(
        "query { article_aggregate { aggregate { max { id } } } }",
        &session("counter", &[]),
    );
    assert_eq!(
        err.message,
        "field 'max' not found in type: 'article_aggregate_fields'"
    );
}

#[test]
fn aggregate_root_requires_allow_aggregations() {
    // The user's author permission has no allow_aggregations.
    let err = gql_err(
        "query { author_aggregate { aggregate { count } } }",
        &user(),
    );
    assert_eq!(
        err.message,
        "field 'author_aggregate' not found in type: 'query_root'"
    );
}

#[test]
fn order_by_relationship_aggregate_rejects_unknown_function() {
    // SEC-01: sqlgen interpolates the order-by relationship-aggregate function
    // name into SQL verbatim (`format!("{function}(..)")`), so an
    // un-whitelisted name lets a client invoke arbitrary single-arg SQL
    // functions (e.g. pg_sleep). Only the fixed aggregate set (plus `count`)
    // is valid; anything else must be a field-not-found, exactly like the
    // aggregate-fields path.
    let err = gql_err(
        "query { author(order_by: { articles_aggregate: { evilfunc: { id: asc } } }) { id } }",
        &user(),
    );
    assert_eq!(err.code, "validation-failed");
    assert_eq!(
        err.message,
        "field 'evilfunc' not found in type: 'article_aggregate_order_by'"
    );
}

#[test]
fn order_by_relationship_aggregate_allows_whitelisted_function() {
    // Guard: a legitimate aggregate function must still plan after the fix.
    let q = gql_select(
        "query { author(order_by: { articles_aggregate: { max: { id: asc } } }) { id } }",
        &user(),
    );
    assert!(matches!(
        q.order_by.as_slice(),
        [donat_ir::OrderBy {
            target: donat_ir::OrderByTarget::RelationshipAggregate { .. },
            ..
        }]
    ));
}

#[test]
fn clickhouse_order_by_rejects_tracked_relationships() {
    for (relationship, nested) in [
        ("articles_aggregate", "articles_aggregate: { count: asc }"),
        ("profile", "profile: { id: asc }"),
    ] {
        let query = format!("query {{ author(order_by: {{ {nested} }}) {{ id }} }}");
        let err = plan_gql_for_source(SourceKind::Clickhouse, &query, &user(), json!({}))
            .expect_err("ClickHouse must not plan relationship order_by");
        assert_eq!(err.code, "validation-failed");
        assert_eq!(
            err.message,
            format!("field '{relationship}' not found in type: 'author'")
        );
    }
}

#[test]
fn permission_limit_caps_user_limit() {
    // article select for "user" carries limit: 100.
    assert_eq!(
        gql_select("query { article { id } }", &user()).limit,
        Some(100)
    );
    assert_eq!(
        gql_select("query { article(limit: 5) { id } }", &user()).limit,
        Some(5)
    );
    assert_eq!(
        gql_select("query { article(limit: 500) { id } }", &user()).limit,
        Some(100)
    );
}

#[test]
fn aggregate_permission_limit_caps_nodes_only() {
    let q = gql_select(
        "query { article_aggregate { aggregate { count } nodes { id } } }",
        &user(),
    );
    // The aggregate runs over the full filtered set; only nodes are capped.
    assert_eq!(q.limit, None);
    assert_eq!(q.nodes_limit, Some(100));
}

#[test]
fn by_pk_ignores_permission_limit() {
    let q = gql_select("query { article_by_pk(id: 1) { id } }", &user());
    assert!(q.single);
    assert_eq!(q.limit, None);
    assert_eq!(q.nodes_limit, None);
}

// ---------------------------------------------------------------------
// Inherited roles
// ---------------------------------------------------------------------

#[test]
fn inherited_role_limit_and_filter_combine_parents() {
    // inh = [s1 (limit 10), s2 (limit 20)]: max limit, OR of filters.
    let q = gql_select("query { author { id } }", &session("inh", &[]));
    assert_eq!(q.limit, Some(20));
    let Some(BoolExp::Or(parts)) = q.predicate else {
        panic!("expected OR of parent filters")
    };
    assert_eq!(parts.len(), 2);

    // inh2 = [s1 (limit 10), s3 (no limit, unrestricted)]: unlimited wins.
    let q = gql_select("query { author { id } }", &session("inh2", &[]));
    assert_eq!(q.limit, None);
    assert!(q.predicate.is_none());
}

#[test]
fn inherited_role_partially_granted_column_is_guarded() {
    // author.name is granted by s2 only, so it gets a cell-level guard;
    // id is granted by both parents and stays plain.
    let q = gql_select("query { author { id name } }", &session("inh", &[]));
    assert!(matches!(q.fields[0].value, FieldValue::Column { .. }));
    assert!(matches!(
        q.fields[1].value,
        FieldValue::ColumnGuarded { .. }
    ));
}

#[test]
fn mutation_permission_conflicts_reported() {
    let md = metadata();
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let conflicts = planner.mutation_permission_conflicts();
    // kid's parents p1/p2 disagree on the delete filter.
    assert!(conflicts.contains(&("kid".to_string(), "article".to_string(), "delete")));
    // Identical parent permissions don't conflict; a direct permission
    // overrides conflicting parents.
    assert!(!conflicts.iter().any(|(role, ..)| role == "twins"));
    assert!(!conflicts.iter().any(|(role, ..)| role == "kidfix"));
}

#[test]
fn conflicting_inherited_mutation_permission_hides_field() {
    let err = gql_err(
        "mutation { delete_article(where: {}) { affected_rows } }",
        &session("kid", &[]),
    );
    assert_eq!(
        err.message,
        "field 'delete_article' not found in type: 'mutation_root'"
    );
}

#[test]
fn identical_parent_permissions_are_inherited() {
    let plan = plan_gql(
        "mutation { delete_article(where: {}) { affected_rows } }",
        &session("twins", &[]),
        json!({}),
    )
    .expect("identical parent permissions resolve");
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    assert_eq!(roots.len(), 1);
    assert!(matches!(&roots[0], donat_ir::MutationRoot::Delete { .. }));
}

#[test]
fn role_without_any_mutation_permission_gets_no_mutations_exist() {
    let err = gql_err(
        "mutation { delete_article(where: {}) { affected_rows } }",
        &session("stranger", &[]),
    );
    assert_eq!(err.message, "no mutations exist");
    assert_eq!(err.path, "$");
}

// ---------------------------------------------------------------------
// v1.rs: exact legacy data-API error shapes
// ---------------------------------------------------------------------

#[test]
fn v1_count_permission_denied_shape() {
    let md = metadata();
    let cat = catalog();
    let err = Planner::new(&md, &cat)
        .plan_v1_count(&json!({ "table": "author" }), &session("stranger", &[]))
        .unwrap_err();
    assert_eq!(err.code, "permission-denied");
    assert_eq!(err.path, "$.args");
    assert_eq!(
        err.message,
        "select on \"author\" for role \"stranger\" is not allowed. ; \"count\" is only allowed if the role has \"select\" permissions on the table"
    );
}

#[test]
fn v1_insert_not_allowed_shape_keeps_trailing_space() {
    let md = metadata();
    let cat = catalog();
    let err = Planner::new(&md, &cat)
        .plan_v1_insert(
            &json!({ "table": "author", "objects": [{ "name": "x" }] }),
            &session("stranger", &[]),
        )
        .unwrap_err();
    assert_eq!(err.code, "permission-denied");
    assert_eq!(err.path, "$.args");
    // Donat's exact message ends with ". " — the trailing space matters.
    assert_eq!(
        err.message,
        "insert on \"author\" for role \"stranger\" is not allowed. "
    );
}

#[test]
fn sqlite_and_mysql_v1_reject_deferred_upserts_during_planning() {
    for source_kind in [SourceKind::Sqlite, SourceKind::Mysql] {
        let mut md = metadata();
        md.sources[0].kind = source_kind;
        let cat = catalog();
        let error = Planner::new(&md, &cat)
            .plan_v1_insert(
                &json!({
                    "table": "author",
                    "objects": [{ "name": "Ada" }],
                    "on_conflict": {
                        "constraint": "author_pkey",
                        "action": "update"
                    }
                }),
                &user(),
            )
            .expect_err("deferred v1 upsert must be rejected by the planner");

        assert_eq!(error.code, "validation-failed", "{source_kind:?}");
        assert_eq!(error.path, "$", "{source_kind:?}");
        assert_eq!(
            error.message, "on_conflict is not supported by this backend",
            "{source_kind:?}"
        );
    }
}

#[test]
fn v1_update_disallowed_column_shape() {
    let md = metadata();
    let cat = catalog();
    // user's update mask is [name]; secret exists but is not updatable.
    let err = Planner::new(&md, &cat)
        .plan_v1_update(
            &json!({ "table": "author", "$set": { "secret": "x" }, "where": {} }),
            &user(),
        )
        .unwrap_err();
    assert_eq!(err.code, "permission-denied");
    assert_eq!(err.path, "$.args[\"$set\"]");
    assert_eq!(
        err.message,
        "role \"user\" does not have permission to update column \"secret\""
    );
}

#[test]
fn v1_update_preset_column_shape() {
    let md = metadata();
    let cat = catalog();
    // preset_user's update permission presets name, so $set on it fails.
    let err = Planner::new(&md, &cat)
        .plan_v1_update(
            &json!({ "table": "author", "$set": { "name": "y" }, "where": {} }),
            &session("preset_user", &[]),
        )
        .unwrap_err();
    assert_eq!(err.code, "not-supported");
    assert_eq!(err.path, "$.args[\"$set\"]");
    assert_eq!(
        err.message,
        "column \"name\" is not updatable for role \"preset_user\"; its value is predefined in permission"
    );
}

#[test]
fn v1_select_distinguishes_hidden_and_unknown_columns() {
    // A column outside the mask is permission-denied with its index path...
    let err = v1_select(
        json!({ "table": "author", "columns": ["id", "secret"] }),
        &user(),
    )
    .unwrap_err();
    assert_eq!(err.code, "permission-denied");
    assert_eq!(err.path, "$.args.columns[1]");
    assert_eq!(
        err.message,
        "role \"user\" does not have permission to select column \"secret\""
    );
    // ...an unknown column is a validation failure.
    let err = v1_select(json!({ "table": "author", "columns": ["nope"] }), &user()).unwrap_err();
    assert_eq!(err.code, "validation-failed");
    assert_eq!(err.path, "$");
    assert_eq!(err.message, "column \"nope\" not found");
}

#[test]
fn st_d_within_parses_2d_and_3d_variants() {
    // Upstream's only 3D fixture (boolexp/postgis/query_geometry_3d_spatial_ops.yaml)
    // is a no-role (admin) request — out of conformance scope — so the
    // 2D/3D split is pinned here at the parse level.
    let pred = article_where(
        json!({ "id": { "_st_d_within": { "distance": 5, "from": "POINT(1 2)" } } }),
        &user(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare {
            op: CompareOp::StDWithin { three_d: false, .. },
            ..
        }
    ));

    let pred = article_where(
        json!({ "id": { "_st_3d_d_within": { "distance": 5, "from": "POINT(1 2 3)" } } }),
        &user(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare {
            op: CompareOp::StDWithin { three_d: true, .. },
            ..
        }
    ));
}

// ---------------------------------------------------------------------
// validators.rs: per-role value validators
// ---------------------------------------------------------------------

/// Metadata where `user` declares a validator and `assistant` inherits the
/// permission without declaring anything of its own.
fn metadata_with_inherited_validator() -> Metadata {
    let mut md = metadata();
    let author = md.sources[0]
        .tables
        .iter_mut()
        .find(|entry| entry.table.name() == "author")
        .expect("the author table is in the fixture");
    author.insert_permissions[0].permission.validate = serde_json::from_value(json!([{
        "expression": "size(name) >= 3",
        "message": "name must be at least 3 characters"
    }]))
    .expect("a validate list parses");
    md.inherited_roles.push(
        serde_json::from_value(json!({ "role_name": "assistant", "role_set": ["user"] })).unwrap(),
    );
    md
}

fn plan_author_insert(md: &Metadata, role: &str) -> Result<Plan, PlanError> {
    let cat = catalog();
    let planner = Planner::new(md, &cat);
    let doc = graphql_parser::parse_query::<String>(
        r#"mutation { insert_author(objects: [{ name: "Ada" }]) { affected_rows } }"#,
    )
    .expect("query parses")
    .into_static();
    planner.plan(
        &doc,
        None,
        &JsonMap::new(),
        &session(role, &[("x-donat-user-id", "7")]),
    )
}

fn insert_validator_messages(plan: Plan) -> Vec<String> {
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Insert { insert, .. } = &roots[0] else {
        panic!("expected an insert root")
    };
    insert
        .validators
        .iter()
        .map(|validator| validator.message.clone())
        .collect()
}

/// The role that declared the validators is held to them.
#[test]
fn a_declaring_role_carries_its_validators_into_the_plan() {
    let md = metadata_with_inherited_validator();
    let plan = plan_author_insert(&md, "user").expect("the declaring role plans an insert");
    assert_eq!(
        insert_validator_messages(plan),
        vec!["name must be at least 3 characters".to_string()]
    );
}

/// An inherited role is granted the parent's write permission wholesale —
/// columns, check, presets. It must inherit the parent's value contract with
/// them. Keying the validator lookup on the request role instead of the
/// declaring role made inheritance a way to shed the parent's checks, which
/// is exactly the shape of hole this asserts against.
#[test]
fn an_inherited_role_cannot_shed_the_parents_validators() {
    let md = metadata_with_inherited_validator();
    let plan = plan_author_insert(&md, "assistant")
        .expect("the inherited role plans an insert through the parent's permission");
    assert_eq!(
        insert_validator_messages(plan),
        vec!["name must be at least 3 characters".to_string()],
        "an inherited role must be held to the permission's validators"
    );
}

// ---------------------------------------------------------------------
// validators.rs: the `phone` validator (spec 021 §1)
// ---------------------------------------------------------------------

/// A table whose `phone` column is declared as a phone number in region DE.
///
/// `region` is an ordinary text column of the same table, and it is there to
/// be written by the caller: the declared region must be unreachable from
/// anything a request carries, including a column that happens to be spelled
/// like it.
fn phone_metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": [{
                "table": { "schema": "public", "name": "contact" },
                "insert_permissions": [{
                    "role": "user",
                    "permission": {
                        "check": {},
                        "columns": ["id", "phone", "region"],
                        "validate": [{
                            "phone": { "column": "phone", "region": "DE" },
                            "message": "phone must be a valid phone number"
                        }]
                    }
                }],
                "update_permissions": [{
                    "role": "user",
                    "permission": {
                        "columns": ["phone", "region"],
                        "filter": {},
                        "validate": [{
                            "phone": { "column": "phone", "region": "DE" },
                            "message": "phone must be a valid phone number"
                        }]
                    }
                }],
                "select_permissions": [{
                    "role": "user",
                    "permission": { "columns": ["id", "phone", "region"], "filter": {} }
                }]
            }]
        }],
        "inherited_roles": [{ "role_name": "assistant", "role_set": ["user"] }]
    }))
    .expect("phone metadata parses")
}

fn phone_catalog() -> Catalog {
    let mut tables = BTreeMap::new();
    tables.insert(
        "public.contact".to_string(),
        TableInfo {
            schema: "public".into(),
            name: "contact".into(),
            relation_kind: donat_catalog_types::RelationKind::Table,
            columns: vec![
                col("id", "int4"),
                col("phone", "text"),
                col("region", "text"),
            ],
            primary_key: vec!["id".into()],
            unique_keys: vec![],
            foreign_keys: vec![],
        },
    );
    Catalog {
        tables,
        functions: BTreeMap::new(),
    }
}

fn plan_with(md: &Metadata, query: &str, sess: &Session) -> Result<Plan, PlanError> {
    let cat = phone_catalog();
    let planner = Planner::new(md, &cat);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    planner.plan(&doc, None, &JsonMap::new(), sess)
}

fn insert_root(plan: Plan) -> donat_ir::InsertMutation {
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Insert { insert, .. } = roots.into_iter().next().expect("one root") else {
        panic!("expected an insert root")
    };
    insert
}

/// The value the statement will carry for `phone`, per inserted row.
fn planned_phones(insert: &donat_ir::InsertMutation) -> Vec<String> {
    let index = insert
        .columns
        .iter()
        .position(|(column, _)| column == "phone")
        .expect("the insert writes the phone column");
    insert
        .rows
        .iter()
        .map(
            |row| match row[index].as_ref().expect("a value was submitted") {
                Scalar::Json(Json::String(value)) => value.clone(),
                other => panic!("expected a string, got {other:?}"),
            },
        )
        .collect()
}

/// Five spellings of one Berlin number go in; one stored value comes out. An
/// unusable number is refused with the ordinary validator error shape — the
/// author's message, verbatim, under `validation-failed`.
#[test]
fn phone_validator_rejects_and_normalizes() {
    let md = phone_metadata();
    let insert = insert_root(
        plan_with(
            &md,
            r#"mutation { insert_contact(objects: [
                { phone: "030 1234567" },
                { phone: "030-123 4567" },
                { phone: "(030) 1234567" },
                { phone: "+49 30 1234567" },
                { phone: "+49 (0)30 123 4567" }
            ]) { affected_rows } }"#,
            &user(),
        )
        .expect("five spellings of a valid number plan"),
    );
    assert_eq!(planned_phones(&insert), vec!["+49301234567".to_string(); 5]);

    let error = plan_with(
        &md,
        r#"mutation { insert_contact(objects: [{ phone: "+49 1111 111111" }]) { affected_rows } }"#,
        &user(),
    )
    .expect_err("a number no numbering plan assigns is refused");
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "phone must be a valid phone number");
    assert_eq!(error.path, "$.selectionSet.insert_contact.args.objects");

    // An update is held to its own list, over the value it sets.
    let plan = plan_with(
        &md,
        r#"mutation { update_contact(where: {}, _set: { phone: "030-123 4567" }) { affected_rows } }"#,
        &user(),
    )
    .expect("an update normalizes what it writes");
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Update { update, .. } = &roots[0] else {
        panic!("expected an update root")
    };
    let donat_ir::SetOp::Set { value, .. } = &update.sets[0] else {
        panic!("expected a _set operation")
    };
    assert_eq!(value.as_json(), &json!("+49301234567"));

    let error = plan_with(
        &md,
        r#"mutation { update_contact(where: {}, _set: { phone: "nonsense" }) { affected_rows } }"#,
        &user(),
    )
    .expect_err("an update is held to the same contract");
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "phone must be a valid phone number");
}

/// The region is the one the metadata declared, and nothing about the request
/// reaches it: not the role, not a header, not a session variable, not a
/// column the caller wrote called `region`.
#[test]
fn phone_region_is_deploy_time() {
    let md = phone_metadata();
    let national = "030 1234567";

    let requests = [
        session("user", &[("x-donat-user-id", "7")]),
        // An inherited role plans through the parent's permission, so it is
        // held to the region that permission declared.
        session("assistant", &[("x-donat-user-id", "7")]),
        // Headers and session variables that name a region are just data.
        session(
            "user",
            &[
                ("x-donat-user-id", "7"),
                ("x-donat-region", "US"),
                ("x-donat-default-region", "US"),
            ],
        ),
    ];
    for sess in requests {
        let insert = insert_root(
            plan_with(
                &md,
                &format!(
                    r#"mutation {{ insert_contact(objects: [{{ phone: "{national}", region: "US" }}]) {{ affected_rows }} }}"#
                ),
                &sess,
            )
            .unwrap_or_else(|error| panic!("{} plans: {error:?}", sess.role)),
        );
        assert_eq!(
            planned_phones(&insert),
            vec!["+49301234567".to_string()],
            "the declared region decides, whoever asks and whatever they send"
        );
    }

    // The same value under a permission that declares US is refused, which is
    // what makes the assertions above a statement about the metadata rather
    // than about this particular number.
    let mut us = phone_metadata();
    us.sources[0].tables[0].insert_permissions[0]
        .permission
        .validate[0]
        .phone
        .as_mut()
        .expect("the entry declares a phone validator")
        .region = "US".to_owned();
    let error = plan_with(
        &us,
        r#"mutation { insert_contact(objects: [{ phone: "030 1234567" }]) { affected_rows } }"#,
        &user(),
    )
    .expect_err("a German national number is not a US number");
    assert_eq!(error.code, "validation-failed");

    // A region that could only be resolved from a request refuses publication
    // rather than being read from one.
    let mut deferred = phone_metadata();
    deferred.sources[0].tables[0].insert_permissions[0]
        .permission
        .validate[0]
        .phone
        .as_mut()
        .expect("the entry declares a phone validator")
        .region = "X-Donat-Region".to_owned();
    let error = plan_with(
        &deferred,
        r#"mutation { insert_contact(objects: [{ phone: "030 1234567" }]) { affected_rows } }"#,
        &user(),
    )
    .expect_err("a session-variable spelling is not a region");
    assert!(error.message.contains("region code"), "{}", error.message);
}

/// The check happens in Rust, before the statement exists. It therefore adds
/// no gate to the plan and no statement to the operation: the SQL is the same
/// shape it would have been without the validator, carrying the normalized
/// literal instead of the submitted one.
#[test]
fn phone_validation_adds_no_statement() {
    let md = phone_metadata();
    let query =
        r#"mutation { insert_contact(objects: [{ phone: "030-123 4567" }]) { affected_rows } }"#;

    let plan = plan_with(&md, query, &user()).expect("the insert plans");
    let Plan::Mutation(roots) = plan else {
        panic!("expected a mutation plan")
    };
    let MutationRoot::Insert { insert, .. } = &roots[0] else {
        panic!("expected an insert root")
    };
    assert!(
        insert.validators.is_empty(),
        "a phone validator lowers to no SQL gate"
    );
    let sql = donat_sqlgen::mutation_to_sql(&roots[0]);

    // The same mutation with the validator removed, for comparison.
    let mut plain = phone_metadata();
    plain.sources[0].tables[0].insert_permissions[0]
        .permission
        .validate
        .clear();
    let plain_plan = plan_with(&plain, query, &user()).expect("the undeclared insert plans");
    let Plan::Mutation(plain_roots) = plain_plan else {
        panic!("expected a mutation plan")
    };
    let plain_sql = donat_sqlgen::mutation_to_sql(&plain_roots[0]);

    assert_eq!(
        statement_count(&sql),
        1,
        "one operation is one statement:\n{sql}"
    );
    assert_eq!(statement_count(&plain_sql), 1);
    assert_eq!(
        sql.replace("+49301234567", "030-123 4567"),
        plain_sql,
        "the only difference a phone validator makes to the SQL is the literal it normalized"
    );
}

/// Statements in a rendered operation. The values in these tests contain no
/// semicolons, so splitting on them is exact.
fn statement_count(sql: &str) -> usize {
    sql.trim().trim_end_matches(';').split(';').count()
}

/// A preset is a value the permission chose, not one the caller sent, and the
/// declared contract covers it too — `validate` has always run after presets.
#[test]
fn a_preset_phone_value_is_normalized_like_a_submitted_one() {
    let mut md = phone_metadata();
    md.sources[0].tables[0].insert_permissions[0]
        .permission
        .set
        .insert("phone".to_owned(), json!("(030) 1234567"));

    let insert = insert_root(
        plan_with(
            &md,
            r#"mutation { insert_contact(objects: [{ id: 1 }]) { affected_rows } }"#,
            &user(),
        )
        .expect("the preset insert plans"),
    );
    assert_eq!(planned_phones(&insert), vec!["+49301234567".to_string()]);
}

/// A `/v1` upsert is held to the update permission's validators, exactly as
/// the GraphQL one is.
///
/// The v1 insert path builds its `on_conflict` update columns, filter and
/// presets from the role's update permission — so the rows it touches are
/// written under that permission and must satisfy its value contract. It
/// resolved only the insert permission's list, so a `phone` declared on the
/// update permission was neither enforced nor normalized for a caller who
/// changed endpoint. A validator a caller can step around by changing endpoint
/// is not a validator.
#[test]
fn a_v1_upsert_is_held_to_the_update_permissions_validators() {
    let mut md = phone_metadata();
    // Only the update permission declares the validator, which is what makes
    // this a statement about the DO UPDATE branch rather than about the insert.
    md.sources[0].tables[0].insert_permissions[0]
        .permission
        .validate
        .clear();
    let cat = phone_catalog();
    let planner = Planner::new(&md, &cat);
    let upsert = |phone: &str| {
        planner.plan_v1_insert(
            &json!({
                "table": "contact",
                "objects": [{ "phone": phone }],
                "on_conflict": { "constraint": "contact_phone_key", "action": "update" }
            }),
            &user(),
        )
    };

    let insert = upsert("030-123 4567").expect("an upsert of a valid number plans");
    assert_eq!(
        planned_phones(&insert),
        vec!["+49301234567".to_string()],
        "an upsert reaches the statement in the form the update contract declares"
    );

    let error = upsert("nonsense").expect_err("an upsert is held to the update contract");
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "phone must be a valid phone number");
    assert_eq!(error.path, "$");

    // `action: ignore` writes nothing on conflict, so it is an insert and is
    // held to the insert permission's own list — here, none.
    let ignored = planner
        .plan_v1_insert(
            &json!({
                "table": "contact",
                "objects": [{ "phone": "nonsense" }],
                "on_conflict": { "constraint": "contact_phone_key", "action": "ignore" }
            }),
            &user(),
        )
        .expect("an ignoring upsert plans");
    assert_eq!(planned_phones(&ignored), vec!["nonsense".to_string()]);
}

/// The value the DO UPDATE branch will write for `phone`, from the update
/// permission's preset.
fn conflict_phone(insert: &donat_ir::InsertMutation) -> String {
    let conflict = insert
        .on_conflict
        .as_ref()
        .expect("the upsert carries an on_conflict clause");
    conflict
        .set_ops
        .iter()
        .find_map(|op| match op {
            donat_ir::SetOp::Set { column, value, .. } if column == "phone" => {
                match value.as_json() {
                    Json::String(value) => Some(value.clone()),
                    other => panic!("expected a string, got {other:?}"),
                }
            }
            _ => None,
        })
        .expect("the update permission's preset reaches the DO UPDATE branch")
}

/// One number, one spelling, whichever branch of the upsert wrote it.
///
/// An update permission's preset is merged into the `DO UPDATE` set list, and
/// it was never normalized: the INSERT branch stored `+49301234567` and the
/// `DO UPDATE` branch stored whatever the preset was spelled as, in the same
/// column — which is exactly the uniqueness ADR 038 is about.
#[test]
fn an_upsert_preset_is_normalized_like_every_other_phone_value() {
    let mut md = phone_metadata();
    md.sources[0].tables[0].update_permissions[0]
        .permission
        .set
        .insert("phone".to_owned(), json!("(030) 1234567"));
    let cat = phone_catalog();
    let planner = Planner::new(&md, &cat);

    let insert = insert_root(
        plan_with(
            &md,
            r#"mutation { insert_contact(
                objects: [{ id: 1, phone: "030 1234567" }]
                on_conflict: { constraint: contact_pkey, update_columns: [region] }
            ) { affected_rows } }"#,
            &user(),
        )
        .expect("the upsert plans"),
    );
    assert_eq!(planned_phones(&insert), vec!["+49301234567".to_string()]);
    assert_eq!(conflict_phone(&insert), "+49301234567");

    // The same on `/v1`, whose upsert merges the same presets.
    let insert = planner
        .plan_v1_insert(
            &json!({
                "table": "contact",
                "objects": [{ "id": 1, "phone": "030 1234567" }],
                "on_conflict": { "constraint": "contact_pkey", "action": "update" }
            }),
            &user(),
        )
        .expect("the v1 upsert plans");
    assert_eq!(planned_phones(&insert), vec!["+49301234567".to_string()]);
    assert_eq!(conflict_phone(&insert), "+49301234567");

    // A preset that is not a number at all is refused with the entry's own
    // message, exactly as a submitted value is.
    let mut unusable = md.clone();
    unusable.sources[0].tables[0].update_permissions[0]
        .permission
        .set
        .insert("phone".to_owned(), json!("nonsense"));
    let error = plan_with(
        &unusable,
        r#"mutation { insert_contact(
            objects: [{ id: 1, phone: "030 1234567" }]
            on_conflict: { constraint: contact_pkey, update_columns: [region] }
        ) { affected_rows } }"#,
        &user(),
    )
    .expect_err("an unusable preset is refused");
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.message, "phone must be a valid phone number");
}
