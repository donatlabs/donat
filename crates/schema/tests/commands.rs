use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use donat_catalog::{Catalog, ColumnInfo, FunctionInfo, RelationKind, TableInfo};
use donat_ir::MutationRoot;
use donat_metadata::{Metadata, SourceKind};
use donat_rules::{RuleCatalog, RuleDefinition, RuleType, compile_catalog};
use donat_schema::{
    CompiledMultiSourceSchema, MultiSourcePlan, MultiSourcePlanner, PlanError, Session,
    compile_command_catalog, execute_multi_source_introspection,
};
use serde_json::{Map as JsonMap, Value as Json, json};

fn column(name: &str, pg_type: &str) -> ColumnInfo {
    column_with(name, pg_type, -1, false)
}

fn column_with(name: &str, pg_type: &str, pg_typmod: i32, nullable: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        pg_typmod,
        native_type: None,
        nullable,
        has_default: false,
    }
}

fn catalog(relation_kind: RelationKind) -> Catalog {
    Catalog {
        tables: BTreeMap::from([(
            "public.orders".to_string(),
            TableInfo {
                schema: "public".to_string(),
                name: "orders".to_string(),
                relation_kind,
                columns: vec![
                    column("id", "uuid"),
                    column("customer_id", "uuid"),
                    column("status", "text"),
                    column("quantity", "int4"),
                    column("order_date", "date"),
                    column("created_at", "timestamp"),
                ],
                primary_key: vec!["id".to_string()],
                foreign_keys: vec![],
            },
        )]),
        functions: BTreeMap::new(),
    }
}

fn metadata(commands: Vec<Json>) -> Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "custom_types": {
            "input_objects": [{
                "name": "OrderInput",
                "fields": [{ "name": "status", "type": "String!" }]
            }]
        },
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": { "connection_info": { "database_url": "postgres://unused" } },
            "tables": [{
                "table": { "schema": "public", "name": "orders" },
                "select_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {} }
                }],
                "insert_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "check": {} }
                }],
                "update_permissions": [{
                    "role": "customer",
                    "permission": { "columns": "*", "filter": {}, "check": {} }
                }],
                "delete_permissions": [{
                    "role": "customer",
                    "permission": { "filter": {} }
                }]
            }]
        }],
        "commands": commands
    }))
    .expect("command metadata deserializes")
}

fn valid_command() -> Json {
    json!({
        "name": "create_order",
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "arguments": [
            { "name": "id", "type": "uuid!" },
            { "name": "customer_id", "type": "uuid!" },
            { "name": "status", "type": "String!" },
            { "name": "quantity", "type": "Int!" },
            { "name": "request_id", "type": "uuid!" }
        ],
        "steps": [{
            "name": "order",
            "insert": {
                "table": { "schema": "public", "name": "orders" },
                "object": {
                    "id": { "arg": "id" },
                    "customer_id": { "arg": "customer_id" },
                    "status": { "arg": "status" },
                    "quantity": { "arg": "quantity" }
                },
                "returning": ["id", "customer_id", "status", "quantity"]
            }
        }],
        "result": {
            "order_id": { "step": "order", "column": "id" },
            "status": { "step": "order", "column": "status" }
        }
    })
}

fn rules() -> RuleCatalog {
    compile_catalog(
        &[RuleDefinition {
            name: "customer_is_allowed".to_string(),
            bindings: BTreeMap::from([("customer_id".to_string(), RuleType::Uuid)]),
            result: RuleType::Bool,
            expression: "true".to_string(),
        }],
        &[],
    )
    .expect("rule catalog compiles")
}

fn compile(metadata: &Metadata, relation_kind: RelationKind) -> Result<(), PlanError> {
    let catalogs = HashMap::from([("default".to_string(), catalog(relation_kind))]);
    compile_command_catalog(metadata, &catalogs, &rules(), true).map(|_| ())
}

fn compile_with_catalog(metadata: &Metadata, catalog: Catalog) -> Result<(), PlanError> {
    let catalogs = HashMap::from([("default".to_string(), catalog)]);
    compile_command_catalog(metadata, &catalogs, &rules(), true).map(|_| ())
}

fn command_catalog(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
) -> Arc<donat_schema::CompiledCommandCatalog> {
    Arc::new(
        compile_command_catalog(metadata, catalogs, &rules(), true)
            .expect("command catalog compiles before runtime schema construction"),
    )
}

fn runtime_schema(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    commands: Arc<donat_schema::CompiledCommandCatalog>,
) -> CompiledMultiSourceSchema {
    CompiledMultiSourceSchema::compile_with_command_catalog(metadata, catalogs, commands, true)
        .expect("runtime schema compiles from the validated command catalog")
}

fn session(role: &str) -> Session {
    Session {
        role: role.to_string(),
        vars: HashMap::new(),
        backend_request: false,
    }
}

fn introspect_runtime(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    commands: Arc<donat_schema::CompiledCommandCatalog>,
    role: &str,
    query: &str,
) -> Json {
    let compiled = runtime_schema(metadata, catalogs, commands);
    let planner = MultiSourcePlanner::from_compiled(metadata, catalogs, &compiled)
        .expect("planner uses the immutable runtime schema");
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("introspection query parses")
        .into_static();
    execute_multi_source_introspection(&planner, &session(role), &doc, None, &JsonMap::new())
        .expect("query is introspection")
        .expect("introspection succeeds")
}

fn plan_runtime(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    commands: Arc<donat_schema::CompiledCommandCatalog>,
    role: &str,
    query: &str,
) -> Result<MultiSourcePlan, PlanError> {
    plan_runtime_with_variables(metadata, catalogs, commands, role, query, JsonMap::new())
}

fn plan_runtime_with_variables(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    commands: Arc<donat_schema::CompiledCommandCatalog>,
    role: &str,
    query: &str,
    variables: JsonMap<String, Json>,
) -> Result<MultiSourcePlan, PlanError> {
    let compiled = runtime_schema(metadata, catalogs, commands);
    let planner = MultiSourcePlanner::from_compiled(metadata, catalogs, &compiled)
        .expect("planner uses the immutable runtime schema");
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("mutation parses")
        .into_static();
    planner.plan(&doc, None, &variables, &session(role))
}

fn literal_target_catalog(pg_type: &str, pg_typmod: i32, nullable: bool) -> Catalog {
    let mut catalog = catalog(RelationKind::Table);
    let table = catalog
        .tables
        .get_mut("public.orders")
        .expect("orders catalog entry exists");
    let status = table
        .columns
        .iter_mut()
        .find(|column| column.name == "status")
        .expect("status catalog column exists");
    *status = column_with("status", pg_type, pg_typmod, nullable);
    catalog
}

fn command_with_status_literal(literal: Json) -> Json {
    let mut command = valid_command();
    command["steps"][0]["insert"]["object"]["status"] = json!({ "literal": literal });
    command
}

fn assert_status_literal(
    pg_type: &str,
    pg_typmod: i32,
    nullable: bool,
    literal: Json,
    accepts: bool,
) {
    let result = compile_with_catalog(
        &metadata(vec![command_with_status_literal(literal.clone())]),
        literal_target_catalog(pg_type, pg_typmod, nullable),
    );
    if accepts {
        result.unwrap_or_else(|error| {
            panic!(
                "{pg_type} ({pg_typmod}) literal should compile but failed at {}: {}",
                error.path, error.message
            )
        });
    } else {
        let error = match result {
            Ok(()) => panic!(
                "{pg_type} ({pg_typmod}) literal should be rejected but compiled: {literal:?}"
            ),
            Err(error) => error,
        };
        assert_eq!(error.code, "validation-failed");
        assert_eq!(error.path, "commands[0].steps[0]");
        assert!(
            error.message.contains(pg_type),
            "database type is reported in {:?}",
            error.message
        );
    }
}

fn assert_rejected(command: Json, expected: &str) {
    let error = compile(&metadata(vec![command]), RelationKind::Table)
        .expect_err("invalid command metadata is rejected before serving");
    assert_eq!(error.code, "validation-failed");
    assert!(
        error.message.contains(expected),
        "expected {expected:?} in {:?}",
        error.message
    );
}

#[test]
fn command_schema_is_role_specific_and_preserves_the_metadata_field_name() {
    let metadata = metadata(vec![valid_command()]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let query = r#"
        {
          root: __type(name: "mutation_root") {
            fields {
              name
              args { name type { kind name ofType { kind name } } }
              type { kind name }
            }
          }
          result: __type(name: "CreateOrderResult") {
            fields { name type { kind name ofType { kind name } } }
          }
        }
    "#;

    let customer = introspect_runtime(&metadata, &catalogs, commands.clone(), "customer", query);
    let command_fields = customer["root"]["fields"]
        .as_array()
        .expect("mutation root fields")
        .iter()
        .filter(|field| field["name"] == "create_order")
        .collect::<Vec<_>>();
    assert_eq!(command_fields.len(), 1, "{customer:#}");
    assert_eq!(command_fields[0]["type"]["kind"], "OBJECT");
    assert_eq!(command_fields[0]["type"]["name"], "CreateOrderResult");
    assert_eq!(
        command_fields[0]["args"],
        json!([
            {"name": "id", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "uuid"}}},
            {"name": "customer_id", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "uuid"}}},
            {"name": "status", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "String"}}},
            {"name": "quantity", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "Int"}}},
            {"name": "request_id", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "uuid"}}}
        ])
    );
    assert_eq!(
        customer["result"]["fields"],
        json!([
            {"name": "order_id", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "uuid"}}},
            {"name": "status", "type": {"kind": "NON_NULL", "name": null, "ofType": {"kind": "SCALAR", "name": "String"}}}
        ])
    );

    let unknown = introspect_runtime(&metadata, &catalogs, commands, "unknown", query);
    assert!(
        unknown["root"].is_null()
            || unknown["root"]["fields"]
                .as_array()
                .is_none_or(|fields| fields.iter().all(|field| field["name"] != "create_order")),
        "an unpermitted role must not see the command: {unknown:#}"
    );
}

#[test]
fn command_planning_rejects_undeclared_output_and_missing_runtime_table_permission() {
    let metadata = metadata(vec![valid_command()]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let undeclared = plan_runtime(
        &metadata,
        &catalogs,
        commands.clone(),
        "customer",
        r#"
            mutation {
              create_order(
                id: "550e8400-e29b-41d4-a716-446655440000"
                customer_id: "550e8400-e29b-41d4-a716-446655440001"
                status: "new"
                quantity: 1
                request_id: "550e8400-e29b-41d4-a716-446655440002"
              ) { missing }
            }
        "#,
    )
    .expect_err("undeclared command result selections are rejected");
    assert!(undeclared.message.contains("missing"), "{undeclared:?}");

    let mut revoked = metadata.clone();
    revoked.sources[0].tables[0].insert_permissions.clear();
    let permission_error = plan_runtime(
        &revoked,
        &catalogs,
        commands,
        "customer",
        r#"
            mutation {
              create_order(
                id: "550e8400-e29b-41d4-a716-446655440000"
                customer_id: "550e8400-e29b-41d4-a716-446655440001"
                status: "new"
                quantity: 1
                request_id: "550e8400-e29b-41d4-a716-446655440002"
              ) { order_id }
            }
        "#,
    )
    .expect_err("command permission never bypasses a current table permission");
    assert!(
        permission_error.message.contains("insert permission"),
        "{permission_error:?}"
    );
}

#[test]
fn command_planning_preserves_aliases_and_emits_a_sql_free_command_root() {
    let metadata = metadata(vec![valid_command()]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let plan = plan_runtime(
        &metadata,
        &catalogs,
        commands,
        "customer",
        r#"
            mutation {
              submitted: create_order(
                id: "550e8400-e29b-41d4-a716-446655440000"
                customer_id: "550e8400-e29b-41d4-a716-446655440001"
                status: "new"
                quantity: 1
                request_id: "550e8400-e29b-41d4-a716-446655440002"
              ) { order: order_id }
            }
        "#,
    )
    .expect("a permitted command plans");
    let MultiSourcePlan::Mutation { source, roots, .. } = plan else {
        panic!("command must be a mutation plan");
    };
    assert_eq!(source.as_deref(), Some("default"));
    let [MutationRoot::Command { alias, command }] = roots.as_slice() else {
        panic!("expected one command root, got {roots:#?}");
    };
    assert_eq!(alias, "submitted");
    assert_eq!(command.definition.name, "create_order");
    assert_eq!(command.arguments["quantity"].as_json(), &json!(1));
    let serialized = serde_json::to_value(command).expect("command IR serializes");
    assert_eq!(serialized["selection"][0]["Scalar"]["alias"], "order");
    assert!(
        !serialized.to_string().to_ascii_lowercase().contains("sql"),
        "command IR carries only validated metadata, values, and projection: {serialized:#}"
    );
}

#[test]
fn command_arguments_validate_resolved_variables_against_the_declared_type() {
    let metadata = metadata(vec![valid_command()]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let error = plan_runtime_with_variables(
        &metadata,
        &catalogs,
        commands,
        "customer",
        r#"
            mutation Create($quantity: Int!) {
              create_order(
                id: "550e8400-e29b-41d4-a716-446655440000"
                customer_id: "550e8400-e29b-41d4-a716-446655440001"
                status: "new"
                quantity: $quantity
                request_id: "550e8400-e29b-41d4-a716-446655440002"
              ) { order_id }
            }
        "#,
        JsonMap::from_iter([(String::from("quantity"), json!("not-an-int"))]),
    )
    .expect_err("resolved variables must satisfy the declared command argument type");
    assert_eq!(error.code, "validation-failed");
    assert_eq!(error.path, "$.selectionSet.create_order.args.quantity");
    assert!(error.message.contains("Int"), "{error:?}");
}

#[test]
fn command_schema_preserves_list_item_nullability_for_declared_input_objects() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({ "name": "lines", "type": "[OrderInput!]!" }));
    let metadata = metadata(vec![command]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let data = introspect_runtime(
        &metadata,
        &catalogs,
        commands,
        "customer",
        r#"
            {
              root: __type(name: "mutation_root") {
                fields {
                  name
                  args {
                    name
                    type { kind name ofType { kind name ofType { kind name } } }
                  }
                }
              }
              line: __type(name: "OrderInput") {
                kind
                inputFields { name type { kind name ofType { kind name } } }
              }
            }
        "#,
    );
    let lines = data["root"]["fields"]
        .as_array()
        .expect("root fields")
        .iter()
        .find(|field| field["name"] == "create_order")
        .and_then(|field| field["args"].as_array())
        .and_then(|arguments| {
            arguments
                .iter()
                .find(|argument| argument["name"] == "lines")
        })
        .expect("lines command argument");
    assert_eq!(
        lines["type"],
        json!({
            "kind": "NON_NULL", "name": null,
            "ofType": {
                "kind": "LIST", "name": null,
                "ofType": { "kind": "NON_NULL", "name": null }
            }
        })
    );
    assert_eq!(data["line"]["kind"], "INPUT_OBJECT");
    assert_eq!(data["line"]["inputFields"][0]["name"], "status");
}

#[test]
fn command_roots_are_absent_from_non_postgres_sources() {
    let metadata = metadata(vec![valid_command()]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let mut non_postgres = metadata.clone();
    non_postgres.sources[0].kind = SourceKind::Sqlite;
    let data = introspect_runtime(
        &non_postgres,
        &catalogs,
        commands,
        "customer",
        r#"{ __type(name: "mutation_root") { fields { name } } }"#,
    );
    assert!(
        data["__type"].is_null()
            || data["__type"]["fields"]
                .as_array()
                .is_none_or(|fields| fields.iter().all(|field| field["name"] != "create_order")),
        "a command must not be exposed through a non-Postgres source: {data:#}"
    );
}

#[test]
fn compiles_an_immutable_catalog_per_postgres_source() {
    let metadata = metadata(vec![valid_command()]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let catalog = compile_command_catalog(&metadata, &catalogs, &rules(), true)
        .expect("valid Postgres command catalog compiles");
    assert!(
        catalog
            .source("default")
            .and_then(|source| source.command("create_order"))
            .is_some(),
        "the validated command is retained in its source-local immutable catalog"
    );
}

#[test]
fn rejects_duplicate_command_names_within_a_source() {
    let command = valid_command();
    let error = compile(
        &metadata(vec![command.clone(), command]),
        RelationKind::Table,
    )
    .expect_err("duplicate command name is rejected");
    assert!(
        error
            .message
            .contains("duplicate command name 'create_order'")
    );
}

#[test]
fn rejects_non_table_command_targets() {
    let error = compile(&metadata(vec![valid_command()]), RelationKind::View)
        .expect_err("a view cannot be a command mutation target");
    assert!(error.message.contains("must be an ordinary table"));
}

#[test]
fn rejects_unknown_columns_arguments_and_forward_step_references() {
    let mut unknown_column = valid_command();
    unknown_column["steps"][0]["insert"]["object"]["missing"] = json!({ "arg": "status" });
    assert_rejected(unknown_column, "unknown column 'missing'");

    let mut unknown_argument = valid_command();
    unknown_argument["steps"][0]["insert"]["object"]["id"] = json!({ "arg": "missing" });
    assert_rejected(unknown_argument, "unknown argument 'missing'");

    let mut forward_reference = valid_command();
    forward_reference["steps"][0]["insert"]["object"]["customer_id"] =
        json!({ "step": "later", "column": "id" });
    forward_reference["steps"] = json!([
        forward_reference["steps"][0].clone(),
        {
            "name": "later",
            "insert": {
                "table": { "schema": "public", "name": "orders" },
                "object": {
                    "id": { "arg": "id" },
                    "customer_id": { "arg": "customer_id" },
                    "status": { "arg": "status" },
                    "quantity": { "arg": "quantity" }
                },
                "returning": ["id"]
            }
        }
    ]);
    assert_rejected(forward_reference, "must reference an earlier step");
}

#[test]
fn rejects_mutable_result_shapes_and_unknown_returning_columns() {
    let mut mutable_result = valid_command();
    mutable_result["result"]["echo"] = json!({ "arg": "status" });
    assert_rejected(
        mutable_result,
        "result fields must be step columns or literals",
    );

    let mut unknown_returning = valid_command();
    unknown_returning["steps"][0]["insert"]["returning"] = json!(["id", "missing"]);
    assert_rejected(unknown_returning, "unknown column 'missing'");
}

#[test]
fn rejects_update_and_delete_without_every_primary_key_predicate() {
    for operation in [
        json!({
            "name": "change",
            "update": {
                "table": { "schema": "public", "name": "orders" },
                "where": {},
                "set": { "status": { "arg": "status" } },
                "returning": ["id"]
            }
        }),
        json!({
            "name": "remove",
            "delete": {
                "table": { "schema": "public", "name": "orders" },
                "where": {},
                "returning": ["id"]
            }
        }),
    ] {
        let mut command = valid_command();
        command["steps"] = json!([operation]);
        command["result"] = json!({ "order_id": { "step": "change", "column": "id" } });
        if command["steps"][0].get("delete").is_some() {
            command["result"] = json!({ "order_id": { "step": "remove", "column": "id" } });
        }
        assert_rejected(command, "requires every primary-key column");
    }
}

#[test]
fn rejects_object_or_list_values_bound_to_scalar_columns() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({
            "name": "payload",
            "type": "OrderInput!"
        }));
    command["steps"][0]["insert"]["object"]["status"] = json!({ "arg": "payload" });
    assert_rejected(command, "is not assignable to column 'status'");
}

#[test]
fn rejects_multi_row_step_columns_bound_to_a_scalar_destination() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({
            "name": "items",
            "type": "[OrderInput!]!"
        }));
    command["steps"] = json!([
        {
            "name": "lines",
            "insert_many": {
                "table": { "schema": "public", "name": "orders" },
                "for_each": { "arg": "items" },
                "object": { "status": { "item": "status" } },
                "returning": ["id"]
            }
        },
        {
            "name": "order",
            "insert": {
                "table": { "schema": "public", "name": "orders" },
                "object": {
                    "id": { "arg": "id" },
                    "customer_id": { "step": "lines", "column": "id" },
                    "status": { "arg": "status" },
                    "quantity": { "arg": "quantity" }
                },
                "returning": ["id"]
            }
        }
    ]);
    command["result"] = json!({ "order_id": { "step": "order", "column": "id" } });
    assert_rejected(
        command,
        "list<uuid> is not assignable to column 'customer_id'",
    );
}

#[test]
fn rejects_wrong_scalar_and_rule_binding_types() {
    let mut scalar_mismatch = valid_command();
    scalar_mismatch["steps"][0]["insert"]["object"]["quantity"] = json!({ "arg": "status" });
    assert_rejected(scalar_mismatch, "is not assignable to column 'quantity'");

    let mut rule_mismatch = valid_command();
    rule_mismatch["guards"] = json!([{
        "rule": "customer_is_allowed",
        "with": { "customer_id": { "arg": "status" } }
    }]);
    assert_rejected(
        rule_mismatch,
        "is not assignable to rule binding 'customer_id'",
    );
}

#[test]
fn rejects_malformed_literals_for_resolved_column_scalars() {
    for (column, literal, database_type) in [
        ("id", json!("not-a-uuid"), "uuid"),
        ("quantity", json!("not-an-integer"), "int4"),
        ("order_date", json!("2026-99-99"), "date"),
        ("created_at", json!("not-a-timestamp"), "timestamp"),
    ] {
        let mut command = valid_command();
        command["steps"][0]["insert"]["object"][column] = json!({ "literal": literal });
        assert_rejected(
            command,
            &format!("PostgreSQL column type '{database_type}'"),
        );
    }
}

#[test]
fn command_literals_obey_concrete_postgres_scalar_descriptors() {
    let numeric_typmod = |precision: i32, scale: i32| ((precision << 16) | (scale & 0x7ff)) + 4;

    for (pg_type, pg_typmod, literal) in [
        ("bool", -1, json!(true)),
        ("int2", -1, json!(-32768)),
        ("int2", -1, json!(32767)),
        ("int2", -1, json!("-32768")),
        ("int2", -1, json!("32767")),
        ("int4", -1, json!(-2147483648_i64)),
        ("int4", -1, json!(2147483647_i64)),
        ("int4", -1, json!("-2147483648")),
        ("int4", -1, json!("2147483647")),
        ("int8", -1, json!(-9223372036854775808_i64)),
        ("int8", -1, json!(9223372036854775807_i64)),
        ("int8", -1, json!("-9223372036854775808")),
        ("int8", -1, json!("9223372036854775807")),
        ("float4", -1, json!(1.25)),
        ("float4", -1, json!("1.25")),
        ("float8", -1, json!(1.25)),
        ("float8", -1, json!("1.25")),
        ("numeric", numeric_typmod(5, 2), json!("999.994")),
        ("numeric", numeric_typmod(5, 2), json!("-999.994")),
        (
            "decimal",
            -1,
            json!("123456789012345678901234567890.123456789"),
        ),
        ("numeric", numeric_typmod(3, -2), json!("99949")),
        ("uuid", -1, json!("550e8400-e29b-41d4-a716-446655440000")),
        ("date", -1, json!("2026-02-28")),
        ("timestamp", -1, json!("2026-02-28T12:34:56.123456")),
        ("timestamp", 0, json!("2026-02-28 12:34:56")),
        ("timestamp", 3, json!("2026-02-28T12:34:56.123")),
        ("timestamp", 6, json!("2026-02-28T12:34:56.123456")),
        ("timestamptz", -1, json!("2026-02-28T12:34:56.123456+03:00")),
        (
            "timestamp with time zone",
            3,
            json!("2026-02-28T12:34:56.123Z"),
        ),
        ("text", -1, json!("unbounded")),
        ("varchar", 7, json!("åßç")),
        ("bpchar", 6, json!("åß")),
        ("name", -1, json!("a".repeat(63))),
        ("citext", -1, json!("Case Insensitive")),
        ("varchar", 7, Json::Null),
    ] {
        assert_status_literal(
            pg_type,
            pg_typmod,
            pg_type == "varchar" && literal.is_null(),
            literal,
            true,
        );
    }

    for (pg_type, pg_typmod, nullable, literal) in [
        ("bool", -1, false, json!("true")),
        ("int2", -1, false, json!(-32769)),
        ("int2", -1, false, json!("32768")),
        ("int4", -1, false, json!(-2147483649_i64)),
        ("int4", -1, false, json!("2147483648")),
        ("int8", -1, false, json!(9223372036854775808_u64)),
        ("int8", -1, false, json!("-9223372036854775809")),
        ("int8", -1, false, json!("+1")),
        ("int8", -1, false, json!("1.0")),
        ("int8", -1, false, json!("1e2")),
        ("float4", -1, false, json!("NaN")),
        ("float4", -1, false, json!("Infinity")),
        ("float8", -1, false, json!("-Infinity")),
        ("float4", -1, false, json!("3.5e39")),
        ("numeric", numeric_typmod(5, 2), false, json!("999.995")),
        ("numeric", numeric_typmod(5, 2), false, json!("1e2")),
        ("numeric", numeric_typmod(3, -2), false, json!("99950")),
        ("numeric", 4, false, json!("1")),
        ("uuid", -1, false, json!("550e8400e29b41d4a716446655440000")),
        ("date", -1, false, json!("2026-02-29")),
        ("timestamp", -1, false, json!("2026-02-28T12:34:56.1234567")),
        ("timestamp", 0, false, json!("2026-02-28T12:34:56.1")),
        ("timestamp", 3, false, json!("2026-02-28T12:34:56.1234")),
        ("timestamp", 7, false, json!("2026-02-28T12:34:56")),
        ("timestamptz", 3, false, json!("2026-02-28T12:34:56.1234Z")),
        ("timestamp", -1, false, json!("2026-02-28T12:34:56Z")),
        ("timestamptz", -1, false, json!("2026-02-28T12:34:56")),
        ("varchar", 7, false, json!("åßçd")),
        ("varchar", 3, false, json!("x")),
        ("bpchar", 3, false, json!("x")),
        ("name", -1, false, json!("a".repeat(64))),
        ("text", 4, false, json!("x")),
        ("citext", 4, false, json!("x")),
        ("varchar", 7, false, Json::Null),
        ("jsonb", -1, false, json!("payload")),
        ("order_status", -1, false, json!("new")),
        ("_int4", -1, false, json!([1, 2])),
        ("domain_order_id", -1, false, json!("1")),
        ("extension_scalar", -1, false, json!("1")),
        ("bool", -1, false, json!({ "unexpected": true })),
        ("bool", -1, false, json!([true])),
    ] {
        assert_status_literal(pg_type, pg_typmod, nullable, literal, false);
    }
}

#[test]
fn literal_diagnostics_identify_object_and_primary_key_target_columns() {
    let object_error = compile_with_catalog(
        &metadata(vec![command_with_status_literal(json!("32768"))]),
        literal_target_catalog("int2", -1, false),
    )
    .expect_err("an object literal outside int2 range is rejected");
    assert_eq!(object_error.path, "commands[0].steps[0]");
    assert!(
        object_error.message.contains("column 'status'") && object_error.message.contains("int2"),
        "object literal diagnostic must name its concrete target: {}",
        object_error.message
    );

    let mut primary_key_command = valid_command();
    primary_key_command["steps"] = json!([{
        "name": "order",
        "update": {
            "table": { "schema": "public", "name": "orders" },
            "where": { "id": { "literal": "9223372036854775808" } },
            "set": { "status": { "arg": "status" } },
            "returning": ["id"]
        }
    }]);
    primary_key_command["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    let mut primary_key_catalog = catalog(RelationKind::Table);
    let primary_key_column = primary_key_catalog
        .tables
        .get_mut("public.orders")
        .expect("orders catalog entry exists")
        .columns
        .iter_mut()
        .find(|column| column.name == "id")
        .expect("id catalog column exists");
    *primary_key_column = column_with("id", "int8", -1, false);

    let primary_key_error =
        compile_with_catalog(&metadata(vec![primary_key_command]), primary_key_catalog)
            .expect_err("an out-of-range primary-key literal is rejected");
    assert_eq!(primary_key_error.path, "commands[0].steps[0]");
    assert!(
        primary_key_error.message.contains("column 'id'")
            && primary_key_error.message.contains("int8"),
        "primary-key literal diagnostic must name its concrete target: {}",
        primary_key_error.message
    );
}

#[test]
fn accepts_table_permissions_inherited_by_the_explicit_command_role() {
    let mut inherited = metadata(vec![valid_command()]);
    inherited.inherited_roles = serde_json::from_value(json!([
        { "role_name": "customer", "role_set": ["table_writer"] }
    ]))
    .expect("inherited roles deserialize");
    for entry in &mut inherited.sources[0].tables {
        for permission in &mut entry.select_permissions {
            permission.role = "table_writer".to_string();
        }
        for permission in &mut entry.insert_permissions {
            permission.role = "table_writer".to_string();
        }
    }
    compile(&inherited, RelationKind::Table)
        .expect("the explicit command role inherits normal table permissions");
}

#[test]
fn rejects_graphql_unsafe_argument_and_result_field_names() {
    let mut invalid_argument = valid_command();
    invalid_argument["arguments"][0]["name"] = json!("bad-name");
    assert_rejected(invalid_argument, "command argument name 'bad-name'");

    let mut invalid_result = valid_command();
    invalid_result["result"] = json!({
        "__typename": { "step": "order", "column": "id" }
    });
    assert_rejected(invalid_result, "command result field '__typename'");
}

#[test]
fn rejects_commands_without_explicit_command_or_table_permissions() {
    let mut missing_command_permission = valid_command();
    missing_command_permission["permissions"] = json!([]);
    assert_rejected(
        missing_command_permission,
        "must declare at least one explicit role",
    );

    let mut missing_table_permission = metadata(vec![valid_command()]);
    missing_table_permission.sources[0].tables[0]
        .insert_permissions
        .clear();
    let error = compile(&missing_table_permission, RelationKind::Table)
        .expect_err("command permissions do not bypass table permissions");
    assert!(error.message.contains("lacks insert permission"));

    let mut backend_only_permission = metadata(vec![valid_command()]);
    backend_only_permission.sources[0].tables[0].insert_permissions[0]
        .permission
        .backend_only = true;
    let error = compile(&backend_only_permission, RelationKind::Table)
        .expect_err("commands cannot use backend-only insert permissions");
    assert!(error.message.contains("lacks insert permission"));
}

#[test]
fn rejects_invalid_graphql_command_names_and_mutation_root_collisions() {
    let mut invalid_name = valid_command();
    invalid_name["name"] = json!("create-order");
    assert_rejected(invalid_name, "must be a valid GraphQL name");

    let mut table_collision = valid_command();
    table_collision["name"] = json!("insert_orders");
    assert_rejected(
        table_collision,
        "collides with an existing mutation root field",
    );

    let mut action_collision = metadata(vec![valid_command()]);
    action_collision.actions.push(
        serde_json::from_value(json!({
            "name": "create_order",
            "definition": {
                "type": "mutation",
                "handler": "http://example.invalid/action"
            },
            "permissions": [{ "role": "customer" }]
        }))
        .expect("action metadata deserializes"),
    );
    let error = compile(&action_collision, RelationKind::Table)
        .expect_err("command cannot collide with an action mutation root");
    assert!(error.message.contains("collides with action mutation"));

    let mut function_collision = metadata(vec![valid_command()]);
    function_collision.sources[0].functions.push(
        serde_json::from_value(json!({
            "function": { "schema": "public", "name": "create_order_function" },
            "configuration": {
                "custom_name": "create_order",
                "exposed_as": "mutation"
            }
        }))
        .expect("function metadata deserializes"),
    );
    let mut catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    catalogs
        .get_mut("default")
        .expect("default catalog")
        .functions
        .insert(
            "public.create_order_function".to_string(),
            FunctionInfo {
                schema: "public".to_string(),
                name: "create_order_function".to_string(),
                args: vec![],
                returns_table: Some(("public".to_string(), "orders".to_string())),
                returns_set: true,
                returns_scalar: None,
            },
        );
    let error = compile_command_catalog(&function_collision, &catalogs, &rules(), true)
        .expect_err("command cannot collide with a function mutation root");
    assert!(
        error
            .message
            .contains("collides with an existing mutation root field")
    );
}

#[test]
fn rejects_effects_without_command_idempotency_and_malformed_local_bindings() {
    let mut missing_command_idempotency = valid_command();
    missing_command_idempotency["effects"] = json!([{
        "start_process": {
            "process": "checkout",
            "input": { "order_id": { "step": "order", "column": "id" } },
            "idempotency_key": { "argument": "request_id" }
        }
    }]);
    assert_rejected(
        missing_command_idempotency,
        "effects require command idempotency",
    );

    let mut malformed_bindings = valid_command();
    malformed_bindings["idempotency"] = json!({
        "key": { "argument": "request_id" },
        "scope": [{ "argument": "customer_id" }]
    });
    malformed_bindings["effects"] = json!([{
        "signal_process": {
            "process": "checkout",
            "signal": "approval_recorded",
            "correlate": { "order_id": { "arg": "missing" } },
            "payload": { "approved": { "literal": true } },
            "idempotency_key": { "argument": "request_id" }
        }
    }]);
    assert_rejected(malformed_bindings, "unknown argument 'missing'");
}

#[test]
fn rejects_non_scalar_idempotency_scope_and_non_postgres_command_sources() {
    let mut invalid_scope = valid_command();
    invalid_scope["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({
            "name": "items",
            "type": "[String!]!"
        }));
    invalid_scope["idempotency"] = json!({
        "key": { "argument": "request_id" },
        "scope": [{ "argument": "items" }]
    });
    assert_rejected(invalid_scope, "idempotency scope must be scalar");

    let mut sqlite_metadata = metadata(vec![valid_command()]);
    sqlite_metadata.sources[0].kind = SourceKind::Sqlite;
    let error =
        compile(&sqlite_metadata, RelationKind::Table).expect_err("commands remain Postgres-only");
    assert!(error.message.contains("requires a Postgres source"));
}
