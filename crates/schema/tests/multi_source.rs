use std::collections::{BTreeMap, HashMap};

use donat_catalog::{Catalog, ColumnInfo, FunctionInfo, TableInfo};
use donat_ir::{BoolExp, MutationRoot, RootField};
use donat_metadata::Metadata;
use donat_schema::{
    MultiSourcePlan, MultiSourcePlanner, PlanError, QueryResponseSlot, Session,
    execute_multi_source_introspection,
};
use serde_json::{Map as JsonMap, Value as Json, json};

fn metadata() -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [
            {
                "name": "default",
                "kind": "postgres",
                "configuration": { "connection_info": { "database_url": "postgres://unused" } },
                "tables": [{
                    "table": { "schema": "public", "name": "item" },
                    "configuration": { "custom_name": "public_item" },
                    "select_permissions": [{ "role": "user", "permission": {
                        "columns": ["id", "name"],
                        "filter": { "id": { "_eq": "X-Donat-User-Id" } }
                    }}],
                    "insert_permissions": [{ "role": "user", "permission": {
                        "columns": ["id", "name"], "check": {}
                    }}]
                }]
            },
            {
                "name": "clickhouse",
                "kind": "clickhouse",
                "configuration": { "connection_info": { "database_url": "http://unused" } },
                "tables": [{
                    "table": { "schema": "logs", "name": "event" },
                    "configuration": { "custom_name": "logs_event" },
                    "select_permissions": [{ "role": "user", "permission": {
                        "columns": ["id", "message"],
                        "filter": {}
                    }}]
                }]
            },
            {
                "name": "secondary",
                "kind": "postgres",
                "configuration": { "connection_info": { "database_url": "postgres://unused" } },
                "tables": [{
                    "table": { "schema": "public", "name": "note" },
                    "configuration": { "custom_name": "public_note" },
                    "select_permissions": [{ "role": "user", "permission": {
                        "columns": ["id", "body"], "filter": {}
                    }}],
                    "insert_permissions": [{ "role": "user", "permission": {
                        "columns": ["id", "body"], "check": {}
                    }}]
                }]
            }
        ]
    }))
    .expect("metadata deserializes")
}

fn col(name: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: "int4".to_string(),
        native_type: None,
        nullable: false,
        has_default: false,
    }
}

fn catalog(schema: &str, table: &str, columns: &[&str]) -> Catalog {
    Catalog {
        tables: BTreeMap::from([(
            format!("{schema}.{table}"),
            TableInfo {
                schema: schema.to_string(),
                name: table.to_string(),
                columns: columns.iter().map(|name| col(name)).collect(),
                primary_key: vec!["id".to_string()],
                foreign_keys: vec![],
            },
        )]),
        functions: BTreeMap::new(),
    }
}

fn catalogs() -> HashMap<String, Catalog> {
    HashMap::from([
        (
            "default".to_string(),
            catalog("public", "item", &["id", "name"]),
        ),
        (
            "clickhouse".to_string(),
            catalog("logs", "event", &["id", "message"]),
        ),
        (
            "secondary".to_string(),
            catalog("public", "note", &["id", "body"]),
        ),
    ])
}

fn session(role: &str) -> Session {
    Session {
        role: role.to_string(),
        vars: HashMap::from([("x-donat-user-id".to_string(), "7".to_string())]),
        backend_request: false,
    }
}

fn plan(query: &str, role: &str) -> Result<MultiSourcePlan, PlanError> {
    plan_with_variables(query, role, json!({}))
}

fn plan_with_variables(
    query: &str,
    role: &str,
    variables: Json,
) -> Result<MultiSourcePlan, PlanError> {
    let metadata = metadata();
    let catalogs = catalogs();
    let planner = MultiSourcePlanner::new(&metadata, &catalogs).expect("planner constructs");
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("query parses")
        .into_static();
    planner.plan(
        &doc,
        None,
        &variables.as_object().cloned().unwrap_or_default(),
        &session(role),
    )
}

#[test]
fn partitions_mixed_roots_and_preserves_response_order() {
    let planned = plan(
        r#"
        query Mixed($enabled: Boolean! = true) {
          event: logs_event { id }
          __typename @include(if: $enabled)
          item: public_item { id }
        }
        "#,
        "user",
    )
    .expect("mixed query plans");

    let MultiSourcePlan::Query { sources, response } = planned else {
        panic!("expected query plan");
    };
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].source, "clickhouse");
    assert_eq!(sources[1].source, "default");
    assert_eq!(
        response,
        vec![
            QueryResponseSlot::SourceField {
                key: "event".to_string()
            },
            QueryResponseSlot::LocalTypename {
                key: "__typename".to_string(),
                value: "query_root".to_string()
            },
            QueryResponseSlot::SourceField {
                key: "item".to_string()
            },
        ]
    );
}

#[test]
fn merges_compatible_direct_and_fragment_roots() {
    let planned = plan(
        r#"
        query {
          public_item { id }
          ...MoreItem
        }
        fragment MoreItem on query_root { public_item { name } }
        "#,
        "user",
    )
    .expect("compatible roots merge");

    let MultiSourcePlan::Query { sources, response } = planned else {
        panic!("expected query plan");
    };
    assert_eq!(
        response,
        vec![QueryResponseSlot::SourceField {
            key: "public_item".to_string()
        }]
    );
    let [source] = sources.as_slice() else {
        panic!("one source expected")
    };
    assert_eq!(source.source, "default");
    let [RootField::Select { query, .. }] = source.roots.as_slice() else {
        panic!("one select root expected");
    };
    assert_eq!(query.fields.len(), 2);
}

#[test]
fn rejects_conflicting_aliases_and_arguments_before_delegation() {
    for query in [
        "{ shared: public_item { id } shared: logs_event { id } }",
        "{ public_item(limit: 1) { id } public_item(limit: 2) { id } }",
    ] {
        let error = plan(query, "user").expect_err("response-key collision must fail");
        assert_eq!(error.code, "validation-failed");
        assert!(error.path.starts_with("$.selectionSet."));
    }
}

#[test]
fn rejects_distinct_variable_arguments_even_when_runtime_values_match() {
    let error = plan_with_variables(
        r#"
        query DifferentVariables($a: Int!, $b: Int!) {
          public_item(limit: $a) { id }
          public_item(limit: $b) { name }
        }
        "#,
        "user",
        json!({ "a": 1, "b": 1 }),
    )
    .expect_err("variable identity is part of GraphQL field compatibility");

    assert_eq!(error.code, "validation-failed");
    assert!(
        error
            .message
            .contains("response key 'public_item' conflict")
    );
}

#[test]
fn accepts_compatible_arguments_in_different_name_order() {
    let planned = plan(
        r#"
        {
          public_item(limit: 1, where: { id: { _eq: 7 } }) { id }
          public_item(where: { id: { _eq: 7 } }, limit: 1) { name }
        }
        "#,
        "user",
    )
    .expect("argument-name order does not affect compatibility");

    let MultiSourcePlan::Query { sources, .. } = planned else {
        panic!("query expected")
    };
    let [RootField::Select { query, .. }] = sources[0].roots.as_slice() else {
        panic!("one merged root expected")
    };
    assert_eq!(query.fields.len(), 2);
}

#[test]
fn rejects_nested_response_key_conflicts_before_child_planning() {
    let error = plan(
        r#"
        {
          public_item { value: id }
          public_item { value: name }
        }
        "#,
        "user",
    )
    .expect_err("nested response-key conflict must fail in composite collection");

    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.path, "$.selectionSet.public_item.selectionSet.name");
    assert!(error.message.contains("response key 'value' conflict"));
}

#[test]
fn rejects_nested_conflicts_from_included_fragments() {
    let error = plan(
        r#"
        query NestedFragment($include: Boolean! = true) {
          public_item {
            value: id
            ...ConflictingFields @include(if: $include)
          }
        }
        fragment ConflictingFields on public_item { value: name }
        "#,
        "user",
    )
    .expect_err("included nested fragment conflicts before child planning");

    assert_eq!(error.path, "$.selectionSet.public_item.selectionSet.name");
    assert!(error.message.contains("response key 'value' conflict"));
}

#[test]
fn typename_only_operations_are_source_less() {
    let query = plan("query { __typename }", "user").expect("typename query plans");
    let MultiSourcePlan::Query { sources, response } = query else {
        panic!("query expected")
    };
    assert!(sources.is_empty());
    assert_eq!(response.len(), 1);

    let mutation = plan("mutation { __typename }", "user").expect("typename mutation plans");
    let MultiSourcePlan::Mutation {
        source,
        roots,
        response,
    } = mutation
    else {
        panic!("mutation expected")
    };
    assert_eq!(source, None);
    assert!(roots.is_empty());
    assert_eq!(
        response,
        vec![QueryResponseSlot::LocalTypename {
            key: "__typename".to_string(),
            value: "mutation_root".to_string()
        }]
    );
}

#[test]
fn plans_one_source_mutation_and_rejects_cross_source_mutation() {
    let mutation = plan(
        "mutation { insert_public_item_one(object: { id: 1, name: \"x\" }) { id } }",
        "user",
    )
    .expect("single-source mutation plans");
    let MultiSourcePlan::Mutation { source, roots, .. } = mutation else {
        panic!("mutation expected")
    };
    assert_eq!(source.as_deref(), Some("default"));
    assert!(matches!(roots.as_slice(), [MutationRoot::Insert { .. }]));

    let error = plan(
        "mutation { insert_public_item_one(object: { id: 1, name: \"x\" }) { id } insert_public_note_one(object: { id: 2, body: \"y\" }) { id } }",
        "user",
    )
    .expect_err("cross-source mutation must fail");
    assert_eq!(error.code, "validation-failed");
    assert!(error.message.contains("multiple sources"));

    let error = plan(
        "mutation { insert_logs_event_one(object: { id: 1, message: \"x\" }) { id } }",
        "user",
    )
    .expect_err("ClickHouse owns no mutation roots");
    assert_eq!(
        error.message,
        "field 'insert_logs_event_one' not found in type: 'mutation_root'"
    );
}

#[test]
fn child_planners_enforce_role_visibility_admin_and_session_predicates() {
    let hidden = plan("{ public_item { id } logs_event { id } }", "admin")
        .expect_err("admin has no implicit permissions");
    assert_eq!(hidden.code, "validation-failed");

    let planned = plan("{ public_item { id } }", "user").expect("explicit role plans");
    let MultiSourcePlan::Query { sources, .. } = planned else {
        panic!("query expected")
    };
    let [source] = sources.as_slice() else {
        panic!("one source expected")
    };
    let [RootField::Select { query, .. }] = source.roots.as_slice() else {
        panic!("select expected")
    };
    assert!(matches!(query.predicate, Some(BoolExp::Compare { ref column, .. }) if column == "id"));

    let mut metadata = metadata();
    metadata.sources[1].tables[0].select_permissions.push(
        serde_json::from_value(json!({
            "role": "admin",
            "permission": { "columns": ["id"], "filter": {} }
        }))
        .expect("permission deserializes"),
    );
    let catalogs = catalogs();
    let planner = MultiSourcePlanner::new(&metadata, &catalogs).expect("planner constructs");
    let doc = graphql_parser::parse_query::<String>("{ logs_event { id } }")
        .expect("query parses")
        .into_static();
    assert!(
        planner
            .plan(&doc, None, &JsonMap::new(), &session("admin"))
            .is_ok()
    );
}

#[test]
fn composite_introspection_merges_roots_and_preserves_capability_arguments() {
    let metadata = metadata();
    let catalogs = catalogs();
    let planner = MultiSourcePlanner::new(&metadata, &catalogs).expect("planner constructs");
    let doc = graphql_parser::parse_query::<String>(
        r#"{
            query: __type(name: "query_root") { fields { name args { name } } }
            mutation: __type(name: "mutation_root") { fields { name } }
        }"#,
    )
    .expect("query parses")
    .into_static();
    let data =
        execute_multi_source_introspection(&planner, &session("user"), &doc, None, &JsonMap::new())
            .expect("introspection query")
            .expect("introspection succeeds");

    let fields = data["query"]["fields"].as_array().expect("query fields");
    let postgres = fields
        .iter()
        .find(|field| field["name"] == "public_item")
        .expect("postgres root");
    let clickhouse = fields
        .iter()
        .find(|field| field["name"] == "logs_event")
        .expect("clickhouse root");
    assert!(
        postgres["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg["name"] == "distinct_on")
    );
    assert!(
        !clickhouse["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg["name"] == "distinct_on")
    );
    assert!(
        data["mutation"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["name"] == "insert_public_item")
    );
}

#[test]
fn rejects_duplicate_root_and_incompatible_type_collisions() {
    let mut duplicate_root = metadata();
    duplicate_root.sources[2].tables[0].configuration = Some(
        serde_json::from_value(json!({
            "custom_name": "public_item"
        }))
        .expect("configuration deserializes"),
    );
    let error = MultiSourcePlanner::new(&duplicate_root, &catalogs()).expect_err("root collision");
    assert!(error.message.contains("public_item"));

    let mut duplicate_type = metadata();
    duplicate_type.sources[2].tables[0].configuration = Some(
        serde_json::from_value(json!({
            "custom_name": "public_item",
            "custom_root_fields": {
                "select": "secondary_item",
                "select_by_pk": "secondary_item_by_pk",
                "select_aggregate": "secondary_item_aggregate",
                "insert": "insert_secondary_item",
                "insert_one": "insert_secondary_item_one",
                "update": "update_secondary_item",
                "update_by_pk": "update_secondary_item_by_pk",
                "delete": "delete_secondary_item",
                "delete_by_pk": "delete_secondary_item_by_pk"
            }
        }))
        .expect("configuration deserializes"),
    );
    let error = MultiSourcePlanner::new(&duplicate_type, &catalogs())
        .expect_err("type collision must fail construction");
    assert!(error.message.contains("item"));
}

#[test]
fn rejects_role_specific_output_and_mutation_type_collisions_at_construction() {
    let make_metadata = |default_select: &[&str],
                         secondary_select: &[&str],
                         default_insert: Option<&[&str]>,
                         secondary_insert: Option<&[&str]>| {
        let insert_permissions = default_insert.map(|columns| {
            json!([{
                "role": "writer",
                "permission": {
                    "columns": columns,
                    "check": {}
                }
            }])
        });
        let secondary_insert_permissions = secondary_insert.map(|columns| {
            json!([{
                "role": "writer",
                "permission": {
                    "columns": columns,
                    "check": {}
                }
            }])
        });
        let default_select_permissions = if default_insert.is_some() {
            json!([
                {
                    "role": "user",
                    "permission": { "columns": default_select, "filter": {} }
                },
                {
                    "role": "writer",
                    "permission": { "columns": ["id", "name"], "filter": {} }
                }
            ])
        } else {
            json!([{
                "role": "user",
                "permission": { "columns": default_select, "filter": {} }
            }])
        };
        let secondary_select_permissions = if secondary_insert.is_some() {
            json!([
                {
                    "role": "user",
                    "permission": { "columns": secondary_select, "filter": {} }
                },
                {
                    "role": "writer",
                    "permission": { "columns": ["id", "name"], "filter": {} }
                }
            ])
        } else {
            json!([{
                "role": "user",
                "permission": { "columns": secondary_select, "filter": {} }
            }])
        };
        serde_json::from_value(json!({
            "version": 3,
            "sources": [{
                "name": "default",
                "kind": "postgres",
                "configuration": { "connection_info": { "database_url": "postgres://unused" } },
                "tables": [{
                    "table": { "schema": "public", "name": "item" },
                    "configuration": {
                        "custom_name": "shared_type",
                        "custom_root_fields": {
                            "select": "default_shared",
                            "select_by_pk": "default_shared_by_pk",
                            "select_aggregate": "default_shared_aggregate",
                            "insert": "insert_default_shared",
                            "insert_one": "insert_default_shared_one",
                            "update": "update_default_shared",
                            "update_by_pk": "update_default_shared_by_pk",
                            "delete": "delete_default_shared",
                            "delete_by_pk": "delete_default_shared_by_pk"
                        }
                    },
                    "select_permissions": default_select_permissions,
                    "insert_permissions": insert_permissions.unwrap_or_else(|| json!([]))
                }]
            }, {
                "name": "secondary",
                "kind": "postgres",
                "configuration": { "connection_info": { "database_url": "postgres://unused" } },
                "tables": [{
                    "table": { "schema": "public", "name": "item" },
                    "configuration": {
                        "custom_name": "shared_type",
                        "custom_root_fields": {
                            "select": "secondary_shared",
                            "select_by_pk": "secondary_shared_by_pk",
                            "select_aggregate": "secondary_shared_aggregate",
                            "insert": "insert_secondary_shared",
                            "insert_one": "insert_secondary_shared_one",
                            "update": "update_secondary_shared",
                            "update_by_pk": "update_secondary_shared_by_pk",
                            "delete": "delete_secondary_shared",
                            "delete_by_pk": "delete_secondary_shared_by_pk"
                        }
                    },
                    "select_permissions": secondary_select_permissions,
                    "insert_permissions": secondary_insert_permissions.unwrap_or_else(|| json!([]))
                }]
            }]
        }))
        .expect("role-projection metadata deserializes")
    };
    let shared_catalog = catalog("public", "item", &["id", "name"]);
    let catalogs = HashMap::from([
        ("default".to_string(), shared_catalog.clone()),
        ("secondary".to_string(), shared_catalog),
    ]);

    let output_collision = make_metadata(&["id"], &["name"], None, None);
    let error = MultiSourcePlanner::new(&output_collision, &catalogs)
        .expect_err("a real role's incompatible object projection must fail construction");
    assert!(error.message.contains("shared_type"));

    let mutation_collision = make_metadata(
        &["id", "name"],
        &["id", "name"],
        Some(&["id"]),
        Some(&["name"]),
    );
    let error = MultiSourcePlanner::new(&mutation_collision, &catalogs)
        .expect_err("a mutation role's incompatible input projection must fail construction");
    assert!(error.message.contains("shared_type_insert_input"));
}

#[test]
fn permits_conflicting_response_keys_in_mutually_exclusive_typed_fragments() {
    let mut metadata = metadata();
    metadata.sources.truncate(1);
    metadata.sources[0].tables.push(
        serde_json::from_value(json!({
            "table": { "schema": "public", "name": "other" },
            "configuration": { "custom_name": "public_other" },
            "select_permissions": [{
                "role": "user",
                "permission": { "columns": ["id", "body"], "filter": {} }
            }]
        }))
        .expect("second Relay type deserializes"),
    );
    let mut catalogs = catalogs();
    catalogs.remove("clickhouse");
    catalogs.remove("secondary");
    catalogs.get_mut("default").unwrap().tables.insert(
        "public.other".to_string(),
        TableInfo {
            schema: "public".to_string(),
            name: "other".to_string(),
            columns: vec![col("id"), col("body")],
            primary_key: vec!["id".to_string()],
            foreign_keys: vec![],
        },
    );
    let mut planner = MultiSourcePlanner::new(&metadata, &catalogs).expect("planner constructs");
    planner.set_relay(true).expect("single Relay source");
    let doc = graphql_parser::parse_query::<String>(
        r#"{
          node(id: "WyJkZWZhdWx0IiwicHVibGljIiwiaXRlbSIsMV0=") {
            ... on public_item { value: id }
            ... on public_other { value: body }
          }
        }"#,
    )
    .expect("Relay query parses")
    .into_static();

    planner
        .plan(&doc, None, &JsonMap::new(), &session("user"))
        .expect("mutually exclusive concrete fragments do not conflict");
}

#[test]
fn forwards_function_permission_inference_to_children() {
    let mut metadata = metadata();
    metadata.sources[0].functions.push(
        serde_json::from_value(json!({
            "function": { "schema": "public", "name": "all_items" },
            "permissions": [{ "role": "function_user" }]
        }))
        .expect("function metadata deserializes"),
    );
    let mut catalogs = catalogs();
    catalogs
        .get_mut("default")
        .expect("default catalog")
        .functions
        .insert(
            "public.all_items".to_string(),
            FunctionInfo {
                schema: "public".to_string(),
                name: "all_items".to_string(),
                args: vec![],
                returns_table: Some(("public".to_string(), "item".to_string())),
                returns_set: true,
                returns_scalar: None,
            },
        );
    let doc = graphql_parser::parse_query::<String>("{ all_items { id } }")
        .expect("query parses")
        .into_static();
    let mut planner = MultiSourcePlanner::new(&metadata, &catalogs).expect("planner constructs");

    planner
        .plan(&doc, None, &JsonMap::new(), &session("user"))
        .expect("inferred function permission is enabled by default");
    planner.set_infer_function_permissions(false);
    let error = planner
        .plan(&doc, None, &JsonMap::new(), &session("user"))
        .expect_err("explicit function permission is required when inference is disabled");
    assert_eq!(
        error.message,
        "field 'all_items' not found in type: 'query_root'"
    );
}

#[test]
fn forwards_relay_mode_only_to_capable_children() {
    let mut metadata = metadata();
    metadata.sources.truncate(2);
    let mut catalogs = catalogs();
    catalogs.remove("secondary");
    let mut planner = MultiSourcePlanner::new(&metadata, &catalogs).expect("planner constructs");
    planner
        .set_relay(true)
        .expect("relay ownership is unambiguous");

    let doc = graphql_parser::parse_query::<String>(
        "{ public_item_connection(first: 1) { edges { node { id } } } }",
    )
    .expect("query parses")
    .into_static();
    let planned = planner
        .plan(&doc, None, &JsonMap::new(), &session("user"))
        .expect("Postgres relay root plans");
    let MultiSourcePlan::Query { sources, .. } = planned else {
        panic!("query expected")
    };
    assert_eq!(sources[0].source, "default");

    let clickhouse_doc = graphql_parser::parse_query::<String>(
        "{ logs_event_connection(first: 1) { edges { node { id } } } }",
    )
    .expect("query parses")
    .into_static();
    let error = planner
        .plan(&clickhouse_doc, None, &JsonMap::new(), &session("user"))
        .expect_err("ClickHouse has no relay roots");
    assert_eq!(
        error.message,
        "field 'logs_event_connection' not found in type: 'query_root'"
    );
}
