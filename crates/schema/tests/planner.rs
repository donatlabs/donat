//! DB-less planner unit tests: in-memory metadata + catalog -> Planner.
//!
//! Covers pure planning logic the conformance suite only hits indirectly:
//! bool_exp parsing (legacy `$op` spellings, session variables), argument
//! handling (variables, fragments, by_pk/aggregate gating, permission
//! limits), exact v1 error shapes, and inherited-role resolution.

use std::collections::{BTreeMap, HashMap};

use dist_catalog::{Catalog, ColumnInfo, ForeignKey, TableInfo};
use dist_ir::{BoolExp, CompareOp, FieldValue, RootField, Scalar, SelectQuery};
use dist_metadata::Metadata;
use dist_schema::{Plan, PlanError, Planner, Session};
use serde_json::{Value as Json, json};

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
                    "array_relationships": [{
                        "name": "articles",
                        "using": { "foreign_key_constraint_on": {
                            "table": { "schema": "public", "name": "article" },
                            "column": "author_id"
                        }}
                    }],
                    "insert_permissions": [
                        { "role": "user", "permission": { "check": {}, "columns": ["name"] } }
                    ],
                    "select_permissions": [
                        { "role": "user", "permission": {
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
                        { "role": "user", "permission": { "columns": ["name"], "filter": {} } },
                        { "role": "preset_user", "permission": {
                            "columns": ["name"], "filter": {}, "set": { "name": "preset" }
                        }}
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
                            "filter": { "id": { "_in": "X-Hasura-Allowed-Ids" } }
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
            columns: vec![col("id", "int4"), col("name", "text"), col("secret", "text")],
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
                col("id", "int4"),
                col("title", "text"),
                col("author_id", "int4"),
                col("published", "bool"),
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
    Catalog { tables, functions: BTreeMap::new() }
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
    session("user", &[("x-hasura-user-id", "7")])
}

fn plan_gql(query: &str, sess: &Session, variables: Json) -> Result<Plan, PlanError> {
    let md = metadata();
    let cat = catalog();
    let planner = Planner::new(&md, &cat);
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    let vars = variables.as_object().cloned().unwrap_or_default();
    planner.plan(&doc, None, &vars, sess)
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
    let BoolExp::Or(items) = pred else { panic!("expected Or, got {pred:?}") };
    assert_eq!(items.len(), 2);
    assert!(matches!(&items[0], BoolExp::Compare { column, op: CompareOp::Gt(_), .. } if column == "id"));
    let BoolExp::Not(inner) = &items[1] else { panic!("expected Not") };
    assert!(matches!(&**inner, BoolExp::Compare { op: CompareOp::Eq(_), .. }));
}

#[test]
fn legacy_dollar_comparison_ops_parse() {
    let pred = article_where(json!({ "id": { "$gt": 5 } }), &user()).unwrap().unwrap();
    assert!(matches!(pred, BoolExp::Compare { op: CompareOp::Gt(_), .. }));
    // `$ne` is the legacy alias of `_neq`.
    let pred = article_where(json!({ "id": { "$ne": 3 } }), &user()).unwrap().unwrap();
    assert!(matches!(pred, BoolExp::Compare { op: CompareOp::Neq(_), .. }));
}

#[test]
fn bare_value_is_implicit_eq() {
    let pred = article_where(json!({ "id": 7 }), &user()).unwrap().unwrap();
    let BoolExp::Compare { column, op: CompareOp::Eq(Scalar::Json(v)), .. } = pred else {
        panic!("expected implicit _eq compare")
    };
    assert_eq!(column, "id");
    assert_eq!(v, json!(7));
}

#[test]
fn unknown_operator_error_shape() {
    let err = article_where(json!({ "id": { "_bogus": 1 } }), &user()).unwrap_err();
    assert_eq!(err.code, "validation-failed");
    assert_eq!(err.message, "unexpected operator \"_bogus\" for column 'id'");
}

#[test]
fn unknown_column_in_bool_exp_error_shape() {
    let err = article_where(json!({ "nope": { "_eq": 1 } }), &user()).unwrap_err();
    assert_eq!(err.code, "validation-failed");
    assert_eq!(err.message, "field 'nope' not found in type: 'article_bool_exp'");
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
    let BoolExp::Exists { table, predicate } = pred else { panic!("expected Exists") };
    assert_eq!(table.name, "author");
    assert!(matches!(&*predicate, BoolExp::Compare { op: CompareOp::Eq(_), .. }));
}

#[test]
fn session_var_substituted_in_permission_filter() {
    // The author filter references X-Hasura-User-Id (mixed case); lookup is
    // case-insensitive and the substituted value lands as a string literal.
    let q = gql_select("query { author { id } }", &user());
    let Some(BoolExp::Compare { column, op: CompareOp::Eq(Scalar::Json(v)), .. }) = q.predicate
    else {
        panic!("expected the permission filter as the only predicate")
    };
    assert_eq!(column, "id");
    assert_eq!(v, json!("7"));
}

#[test]
fn missing_session_var_error_shape() {
    let err = gql_err("query { author { id } }", &session("user", &[]));
    assert_eq!(err.code, "not-found");
    // Hasura reports path "$" regardless of filter depth, name lower-cased.
    assert_eq!(err.path, "$");
    assert_eq!(err.message, "missing session variable: \"x-hasura-user-id\"");
}

#[test]
fn session_var_not_resolved_in_user_where() {
    // Clients cannot reference session variables; the string stays literal.
    let pred = article_where(
        json!({ "title": { "_eq": "X-Hasura-User-Id" } }),
        &session("user", &[]),
    )
    .unwrap()
    .unwrap();
    let BoolExp::Compare { op: CompareOp::Eq(Scalar::Json(v)), .. } = pred else {
        panic!("expected compare")
    };
    assert_eq!(v, json!("X-Hasura-User-Id"));
}

#[test]
fn in_session_var_accepts_array_spellings() {
    // A session variable used with _in may hold a Postgres array literal...
    let q = v1_select(
        json!({ "table": "article", "columns": ["id"] }),
        &session("tagged", &[("x-hasura-allowed-ids", "{1,2}")]),
    )
    .unwrap();
    let Some(BoolExp::Compare { op: CompareOp::In(items), .. }) = q.predicate else {
        panic!("expected In predicate")
    };
    assert_eq!(
        items.iter().map(Scalar::as_json).collect::<Vec<_>>(),
        vec![&json!("1"), &json!("2")]
    );

    // ...or a JSON array.
    let q = v1_select(
        json!({ "table": "article", "columns": ["id"] }),
        &session("tagged", &[("x-hasura-allowed-ids", "[1,2]")]),
    )
    .unwrap();
    let Some(BoolExp::Compare { op: CompareOp::In(items), .. }) = q.predicate else {
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
    assert!(matches!(pred, BoolExp::Compare { op: CompareOp::IsNull(true), .. }));
    let pred = article_where(json!({ "title": { "_is_null": false } }), &user())
        .unwrap()
        .unwrap();
    assert!(matches!(pred, BoolExp::Compare { op: CompareOp::IsNull(false), .. }));
}

#[test]
fn column_compare_root_and_relationship_paths() {
    // ["$", col] compares against the bool_exp's root table.
    let pred = article_where(json!({ "id": { "$ceq": ["$", "author_id"] } }), &user())
        .unwrap()
        .unwrap();
    let BoolExp::Compare { op: CompareOp::CompareColumn { sql_op, column, root }, .. } = pred
    else {
        panic!("expected CompareColumn")
    };
    assert_eq!((sql_op.as_str(), column.as_str(), root), ("=", "author_id", true));

    // [rel, col] compares against a column of the related table.
    let pred = article_where(json!({ "id": { "_ceq": ["author", "id"] } }), &user())
        .unwrap()
        .unwrap();
    let BoolExp::Compare { op: CompareOp::CompareColumnRel { table, column, .. }, .. } = pred
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
    assert_eq!(first_select(plan_gql(q, &user(), json!({})).unwrap()).limit, Some(3));
    // ...and a provided value overrides it.
    assert_eq!(
        first_select(plan_gql(q, &user(), json!({ "lim": 7 })).unwrap()).limit,
        Some(7)
    );
}

#[test]
fn missing_required_variable_error() {
    let err = gql_err("query($lim: Int!) { article(limit: $lim) { id } }", &user());
    assert_eq!(err.message, "expecting a value for non-nullable variable: \"lim\"");
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
    assert_eq!(err.message, "fragment \"Bits\" is defined on 'author', not 'article'");
    // An undefined fragment is reported by name.
    let err = gql_err("query { article { ...Nope } }", &user());
    assert_eq!(err.message, "fragment \"Nope\" not found");
}

#[test]
fn by_pk_hidden_when_role_cannot_select_all_pk_columns() {
    // Role "nopk" may select author.name but not the pk column id.
    let err = gql_err("query { author_by_pk(id: 1) { name } }", &session("nopk", &[]));
    assert_eq!(err.message, "field 'author_by_pk' not found in type: 'query_root'");
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
    // ...but Hasura omits count(columns:) from such a role's schema.
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
    assert_eq!(err.message, "field 'max' not found in type: 'article_aggregate_fields'");
}

#[test]
fn aggregate_root_requires_allow_aggregations() {
    // The user's author permission has no allow_aggregations.
    let err = gql_err("query { author_aggregate { aggregate { count } } }", &user());
    assert_eq!(err.message, "field 'author_aggregate' not found in type: 'query_root'");
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
        [dist_ir::OrderBy {
            target: dist_ir::OrderByTarget::RelationshipAggregate { .. },
            ..
        }]
    ));
}

#[test]
fn permission_limit_caps_user_limit() {
    // article select for "user" carries limit: 100.
    assert_eq!(gql_select("query { article { id } }", &user()).limit, Some(100));
    assert_eq!(gql_select("query { article(limit: 5) { id } }", &user()).limit, Some(5));
    assert_eq!(gql_select("query { article(limit: 500) { id } }", &user()).limit, Some(100));
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
    let Some(BoolExp::Or(parts)) = q.predicate else { panic!("expected OR of parent filters") };
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
    assert!(matches!(q.fields[1].value, FieldValue::ColumnGuarded { .. }));
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
    assert_eq!(err.message, "field 'delete_article' not found in type: 'mutation_root'");
}

#[test]
fn identical_parent_permissions_are_inherited() {
    let plan = plan_gql(
        "mutation { delete_article(where: {}) { affected_rows } }",
        &session("twins", &[]),
        json!({}),
    )
    .expect("identical parent permissions resolve");
    let Plan::Mutation(roots) = plan else { panic!("expected a mutation plan") };
    assert_eq!(roots.len(), 1);
    assert!(matches!(&roots[0], dist_ir::MutationRoot::Delete { .. }));
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
    // Hasura's exact message ends with ". " — the trailing space matters.
    assert_eq!(err.message, "insert on \"author\" for role \"stranger\" is not allowed. ");
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
    let err = v1_select(json!({ "table": "author", "columns": ["id", "secret"] }), &user())
        .unwrap_err();
    assert_eq!(err.code, "permission-denied");
    assert_eq!(err.path, "$.args.columns[1]");
    assert_eq!(
        err.message,
        "role \"user\" does not have permission to select column \"secret\""
    );
    // ...an unknown column is a validation failure.
    let err = v1_select(json!({ "table": "author", "columns": ["nope"] }), &user())
        .unwrap_err();
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
        BoolExp::Compare { op: CompareOp::StDWithin { three_d: false, .. }, .. }
    ));

    let pred = article_where(
        json!({ "id": { "_st_3d_d_within": { "distance": 5, "from": "POINT(1 2 3)" } } }),
        &user(),
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        pred,
        BoolExp::Compare { op: CompareOp::StDWithin { three_d: true, .. }, .. }
    ));
}
