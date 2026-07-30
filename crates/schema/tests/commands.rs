use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use donat_catalog::{Catalog, ColumnInfo, FunctionArg, FunctionInfo, RelationKind, TableInfo};
use donat_ir::{
    CommandExecutionStep, CommandExecutionValue, CommandResultValue, MutationRoot, Scalar, TypeRef,
    ValueScalar, ValueType,
};
use donat_metadata::{Columns, CommandStepOperation, Metadata, ScalarType, SourceKind};
use donat_rules::{
    DecisionRow, DecisionTableDefinition, HitPolicy, RuleCatalog, RuleDefinition, RuleType,
    compile_catalog,
};
use donat_schema::{
    CompiledMultiSourceSchema, MultiSourcePlan, MultiSourcePlanner, PlanError, Session,
    compile_command_catalog, compile_command_source_catalog, execute_multi_source_introspection,
    validate_command_catalog,
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
                "fields": [
                    { "name": "status", "type": "String!" },
                    { "name": "quantity", "type": "Int!" }
                ]
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

fn two_source_metadata(commands: Vec<Json>) -> Metadata {
    let mut metadata = metadata(commands);
    let mut secondary = metadata.sources[0].clone();
    secondary.name = "secondary".to_string();
    secondary.tables[0].configuration = Some(
        serde_json::from_value(json!({ "custom_name": "secondary_orders" }))
            .expect("secondary table configuration deserializes"),
    );
    metadata.sources.push(secondary);
    metadata
}

fn two_source_catalogs() -> HashMap<String, Catalog> {
    let orders = catalog(RelationKind::Table);
    HashMap::from([
        ("default".to_string(), orders.clone()),
        ("secondary".to_string(), orders),
    ])
}

fn set_source_table_role(metadata: &mut Metadata, source_name: &str, role: &str) {
    let source = metadata
        .sources
        .iter_mut()
        .find(|source| source.name == source_name)
        .expect("source exists");
    for table in &mut source.tables {
        for permission in &mut table.select_permissions {
            permission.role = role.to_string();
        }
        for permission in &mut table.insert_permissions {
            permission.role = role.to_string();
        }
        for permission in &mut table.update_permissions {
            permission.role = role.to_string();
        }
        for permission in &mut table.delete_permissions {
            permission.role = role.to_string();
        }
    }
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

fn relational_batch_command() -> Json {
    json!({
        "name": "reserve_orders",
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "arguments": [
            { "name": "customer_id", "type": "uuid!" }
        ],
        "steps": [
            {
                "name": "selected",
                "select_many": {
                    "table": { "schema": "public", "name": "orders" },
                    "by": {
                        "customer_id": { "arg": "customer_id" }
                    },
                    "order_by": ["id"],
                    "returning": ["id", "customer_id", "status", "quantity"],
                    "require_non_empty": true
                }
            },
            {
                "name": "totals",
                "aggregate": {
                    "from": { "step": "selected" },
                    "values": {
                        "line_count": { "count": {} },
                        "quantity_sum": { "sum": { "column": "quantity" } },
                        "first_status": { "min": { "column": "status" } }
                    }
                }
            },
            {
                "name": "updated",
                "update_many": {
                    "table": { "schema": "public", "name": "orders" },
                    "for_each": { "step": "selected" },
                    "by": {
                        "id": { "item": "id" }
                    },
                    "set": {
                        "quantity": {
                            "rule": "double_quantity",
                            "with": {
                                "quantity": { "item": "quantity" }
                            }
                        }
                    },
                    "check": {
                        "rule": "customer_is_allowed",
                        "with": {
                            "customer_id": { "current_column": "customer_id" }
                        }
                    },
                    "returning": ["id", "quantity"],
                    "require_each": true
                }
            }
        ],
        "result": {
            "selected": { "step": "selected" },
            "totals": { "step": "totals" },
            "updated": { "step": "updated" }
        }
    })
}

fn select_one_command() -> Json {
    let mut command = valid_command();
    command["steps"] = json!([{
        "name": "order",
        "select_one": {
            "table": { "schema": "public", "name": "orders" },
            "by": {
                "customer_id": { "arg": "customer_id" }
            },
            "returning": ["id", "customer_id", "status"],
            "require_found": true
        }
    }]);
    command
}

fn select_many_command() -> Json {
    let mut command = valid_command();
    command["steps"] = json!([{
        "name": "orders",
        "select_many": {
            "table": { "schema": "public", "name": "orders" },
            "by": {
                "customer_id": { "arg": "customer_id" }
            },
            "order_by": ["id"],
            "returning": ["id", "customer_id", "status"]
        }
    }]);
    command["result"] = json!({
        "orders": { "step": "orders" }
    });
    command
}

fn rules() -> RuleCatalog {
    compile_catalog(
        &[
            RuleDefinition {
                name: "customer_is_allowed".to_string(),
                bindings: BTreeMap::from([("customer_id".to_string(), RuleType::Uuid)]),
                result: RuleType::Bool,
                expression: "true".to_string(),
            },
            RuleDefinition {
                name: "double_quantity".to_string(),
                bindings: BTreeMap::from([("quantity".to_string(), RuleType::Int)]),
                result: RuleType::Int,
                expression: "quantity * 2".to_string(),
            },
        ],
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

fn session_with_vars<'a>(
    role: &str,
    vars: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Session {
    Session {
        role: role.to_string(),
        vars: vars
            .into_iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
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

fn plan_runtime_for_session(
    metadata: &Metadata,
    catalogs: &HashMap<String, Catalog>,
    commands: Arc<donat_schema::CompiledCommandCatalog>,
    session: Session,
    query: &str,
) -> Result<MultiSourcePlan, PlanError> {
    let compiled = runtime_schema(metadata, catalogs, commands);
    let planner = MultiSourcePlanner::from_compiled(metadata, catalogs, &compiled)
        .expect("planner uses the immutable runtime schema");
    let doc = graphql_parser::parse_query::<String>(query)
        .expect("mutation parses")
        .into_static();
    planner.plan(&doc, None, &JsonMap::new(), &session)
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
fn relational_batch_rejects_unauthorized_read_and_update_targets() {
    let mut missing_read = metadata(vec![relational_batch_command()]);
    missing_read.sources[0].tables[0].select_permissions.clear();
    let error = compile(&missing_read, RelationKind::Table)
        .expect_err("select_many must require the explicit role's read permission");
    assert!(error.message.contains("lacks select permission"));

    let mut missing_update = metadata(vec![relational_batch_command()]);
    missing_update.sources[0].tables[0]
        .update_permissions
        .clear();
    let error = compile(&missing_update, RelationKind::Table)
        .expect_err("update_many must require the explicit role's update permission");
    assert!(error.message.contains("lacks update permission"));

    let mut hidden_returning = metadata(vec![relational_batch_command()]);
    hidden_returning.sources[0].tables[0].select_permissions[0]
        .permission
        .columns = Columns::List(vec![
        "id".to_owned(),
        "customer_id".to_owned(),
        "quantity".to_owned(),
    ]);
    let error = compile(&hidden_returning, RelationKind::Table)
        .expect_err("an undeclared read column must not enter a command result type");
    assert!(
        error
            .message
            .contains("lacks select permission for column 'status'")
    );
}

#[test]
fn relational_batch_rejects_invalid_equality_and_order_columns() {
    let mut empty_equality = metadata(vec![relational_batch_command()]);
    let CommandStepOperation::SelectMany { select_many } =
        &mut empty_equality.commands[0].steps[0].operation
    else {
        panic!("relational fixture starts with select_many");
    };
    select_many.by.clear();
    let error = compile(&empty_equality, RelationKind::Table)
        .expect_err("catalog validation must retain the non-empty equality invariant");
    assert!(error.message.contains("at least one equality"));

    let mut empty_order = metadata(vec![relational_batch_command()]);
    let CommandStepOperation::SelectMany { select_many } =
        &mut empty_order.commands[0].steps[0].operation
    else {
        panic!("relational fixture starts with select_many");
    };
    select_many.order_by.clear();
    let error = compile(&empty_order, RelationKind::Table)
        .expect_err("a row set without a declared total order must be rejected");
    assert!(error.message.contains("total order"));

    let mut duplicate_order = metadata(vec![relational_batch_command()]);
    let CommandStepOperation::SelectMany { select_many } =
        &mut duplicate_order.commands[0].steps[0].operation
    else {
        panic!("relational fixture starts with select_many");
    };
    select_many.order_by.push("id".to_owned());
    let error = compile(&duplicate_order, RelationKind::Table)
        .expect_err("a repeated order column cannot define the row-set identity");
    assert!(error.message.contains("duplicate order"));

    let mut unknown_order = relational_batch_command();
    unknown_order["steps"][0]["select_many"]["order_by"] = json!(["missing"]);
    assert_rejected(unknown_order, "unknown order column 'missing'");
}

#[test]
fn relational_batch_rejects_invalid_aggregate_sources_and_types() {
    let mut scalar_source = valid_command();
    scalar_source["steps"]
        .as_array_mut()
        .expect("steps array")
        .push(json!({
            "name": "totals",
            "aggregate": {
                "from": { "step": "order" },
                "values": { "count": { "count": {} } }
            }
        }));
    scalar_source["result"] = json!({ "totals": { "step": "totals" } });
    assert_rejected(
        scalar_source,
        "aggregate input must be a prior select_many row set",
    );

    let mut forward_source = relational_batch_command();
    forward_source["steps"][1]["aggregate"]["from"] = json!({ "step": "updated" });
    assert_rejected(
        forward_source,
        "step reference 'updated' must reference an earlier step",
    );

    let mut non_numeric_sum = relational_batch_command();
    non_numeric_sum["steps"][1]["aggregate"]["values"]["quantity_sum"] =
        json!({ "sum": { "column": "status" } });
    assert_rejected(non_numeric_sum, "sum requires a numeric column");

    let unsupported_min = relational_batch_command();
    let mut json_catalog = catalog(RelationKind::Table);
    json_catalog
        .tables
        .get_mut("public.orders")
        .expect("orders table")
        .columns
        .iter_mut()
        .find(|column| column.name == "status")
        .expect("status column")
        .pg_type = "jsonb".to_owned();
    let error = compile_with_catalog(&metadata(vec![unsupported_min]), json_catalog)
        .expect_err("min/max must reject database types without a closed ordering");
    assert!(error.message.contains("min requires an orderable column"));
}

#[test]
fn relational_batch_rejects_invalid_update_many_input_and_assignments() {
    let mut wrong_input = relational_batch_command();
    wrong_input["steps"][2]["update_many"]["for_each"] = json!({ "step": "totals" });
    assert_rejected(
        wrong_input,
        "update_many input must be a prior select_many row set",
    );

    let mut composite_catalog = catalog(RelationKind::Table);
    composite_catalog
        .tables
        .get_mut("public.orders")
        .expect("orders table")
        .primary_key = vec!["id".to_owned(), "customer_id".to_owned()];
    let error = compile_with_catalog(
        &metadata(vec![relational_batch_command()]),
        composite_catalog.clone(),
    )
    .expect_err("every update target primary-key column must be mapped");
    assert!(error.message.contains("every primary-key column"));

    let mut duplicate_input = relational_batch_command();
    duplicate_input["steps"][2]["update_many"]["by"]["customer_id"] = json!({ "item": "id" });
    let error = compile_with_catalog(&metadata(vec![duplicate_input]), composite_catalog)
        .expect_err("one input field cannot stand in for two primary-key components");
    assert!(error.message.contains("duplicate input key"));

    let mut mismatched_current = relational_batch_command();
    mismatched_current["steps"][2]["update_many"]["set"]["quantity"] =
        json!({ "current_column": "status" });
    assert_rejected(mismatched_current, "is not assignable to column 'quantity'");

    let mut mismatched_rule = relational_batch_command();
    mismatched_rule["steps"][2]["update_many"]["set"]["status"] = json!({
        "rule": "double_quantity",
        "with": { "quantity": { "item": "quantity" } }
    });
    mismatched_rule["steps"][2]["update_many"]["set"]
        .as_object_mut()
        .expect("set object")
        .remove("quantity");
    assert_rejected(mismatched_rule, "is not assignable to column 'status'");

    let mut current_outside_update_many = valid_command();
    current_outside_update_many["steps"][0]["insert"]["object"]["quantity"] =
        json!({ "current_column": "quantity" });
    assert_rejected(
        current_outside_update_many,
        "current_column values are allowed only inside update_many",
    );
}

#[test]
fn relational_batch_rejects_view_and_non_postgres_update_targets() {
    let error = compile(
        &metadata(vec![relational_batch_command()]),
        RelationKind::View,
    )
    .expect_err("select_many may read a view but update_many may not target it");
    assert!(error.message.contains("update_many target"));
    assert!(error.message.contains("ordinary table"));

    let mut sqlite = metadata(vec![relational_batch_command()]);
    sqlite.sources[0].kind = SourceKind::Sqlite;
    let error = compile(&sqlite, RelationKind::Table)
        .expect_err("relational update batches remain Postgres-only");
    assert!(error.message.contains("requires a Postgres source"));
}

#[test]
fn command_reads_accept_supported_tracked_view_kinds_without_primary_keys() {
    for kind in [RelationKind::View, RelationKind::MaterializedView] {
        let mut read_catalog = catalog(kind);
        read_catalog
            .tables
            .get_mut("public.orders")
            .expect("orders relation")
            .primary_key
            .clear();

        compile_with_catalog(&metadata(vec![select_one_command()]), read_catalog.clone())
            .unwrap_or_else(|error| panic!("select_one must read {kind:?}: {error:?}"));
        compile_with_catalog(&metadata(vec![select_many_command()]), read_catalog)
            .unwrap_or_else(|error| panic!("select_many must read {kind:?}: {error:?}"));
    }
}

#[test]
fn command_reads_over_views_still_require_explicit_select_permission() {
    for command in [select_one_command(), select_many_command()] {
        let mut metadata = metadata(vec![command]);
        metadata.sources[0].tables[0].select_permissions.clear();
        let error = compile(&metadata, RelationKind::View)
            .expect_err("a tracked view never bypasses the explicit role's select permission");
        assert_eq!(error.path, "commands[0].steps[0]");
        assert_eq!(
            error.message,
            "role 'customer' lacks select permission on table public.orders"
        );
    }
}

#[test]
fn every_command_write_form_rejects_non_ordinary_relations_at_the_step_path() {
    let relation = json!({ "schema": "public", "name": "orders" });
    let writes = [
        (
            "insert",
            json!({
                "insert": {
                    "table": relation.clone(),
                    "object": { "status": { "arg": "status" } }
                }
            }),
        ),
        (
            "insert_many",
            json!({
                "insert_many": {
                    "table": relation.clone(),
                    "for_each": { "arg": "customer_id" },
                    "object": { "status": { "arg": "status" } }
                }
            }),
        ),
        (
            "update",
            json!({
                "update": {
                    "table": relation.clone(),
                    "where": { "id": { "arg": "id" } },
                    "set": { "status": { "arg": "status" } }
                }
            }),
        ),
        (
            "update_many",
            json!({
                "update_many": {
                    "table": relation.clone(),
                    "for_each": { "arg": "customer_id" },
                    "by": { "id": { "item": "id" } },
                    "set": { "status": { "arg": "status" } }
                }
            }),
        ),
        (
            "delete",
            json!({
                "delete": {
                    "table": relation.clone(),
                    "where": { "id": { "arg": "id" } }
                }
            }),
        ),
        (
            "update_when",
            json!({
                "update_when": {
                    "when": {
                        "argument_equals": {
                            "argument": "status",
                            "value": "new"
                        }
                    },
                    "table": relation.clone(),
                    "where": { "id": { "arg": "id" } },
                    "set": { "status": { "arg": "status" } }
                }
            }),
        ),
        (
            "insert_when",
            json!({
                "insert_when": {
                    "when": {
                        "argument_equals": {
                            "argument": "status",
                            "value": "new"
                        }
                    },
                    "table": relation,
                    "object": { "status": { "arg": "status" } }
                }
            }),
        ),
    ];

    for (operation, body) in writes {
        let mut command = valid_command();
        command["steps"] = json!([{ "name": "write" }]);
        command["steps"][0][operation] = body[operation].clone();
        let error = compile(&metadata(vec![command]), RelationKind::View)
            .expect_err("command writes must remain limited to ordinary tables");
        assert_eq!(error.path, "commands[0].steps[0]", "{operation}");
        let prefix = if operation == "update_many" {
            "update_many target"
        } else {
            "command target"
        };
        assert_eq!(
            error.message,
            format!("{prefix} 'public.orders' must be an ordinary table, not View"),
            "{operation}"
        );
    }
}

#[test]
fn relational_batch_rejects_a_row_set_projected_as_one_scalar() {
    let mut command = relational_batch_command();
    command["result"]["selected"] = json!({ "step": "selected", "column": "id" });
    assert_rejected(
        command,
        "row-set result must reference the declared row object",
    );
}

#[test]
fn relational_batch_compiles_typed_ir_and_exact_graphql_result_shapes() {
    let metadata = metadata(vec![relational_batch_command()]);
    let catalogs = HashMap::from([("default".to_owned(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let mutation = r#"
        mutation {
          reserve_orders(customer_id: "00000000-0000-0000-0000-000000000001") {
            selected { id status }
            totals { line_count quantity_sum first_status }
            updated { id quantity }
          }
        }
    "#;
    let plan = plan_runtime(&metadata, &catalogs, commands.clone(), "customer", mutation)
        .expect("a valid relational batch plans into closed execution IR");
    let MultiSourcePlan::Mutation { roots, .. } = plan else {
        panic!("command must plan as a mutation");
    };
    let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
        panic!("expected one command root");
    };
    assert!(matches!(
        &command.steps[0],
        CommandExecutionStep::SelectMany {
            equality,
            order_by,
            returning,
            require_non_empty: true,
            ..
        } if equality.len() == 1 && order_by.len() == 1 && returning.len() == 4
    ));
    assert!(matches!(
        &command.steps[1],
        CommandExecutionStep::Aggregate { values, .. } if values.len() == 3
    ));
    assert!(matches!(
        &command.steps[2],
        CommandExecutionStep::UpdateMany {
            primary_key,
            assignments,
            check: Some(_),
            returning,
            require_each: true,
            ..
        } if primary_key.len() == 1 && assignments.len() == 1 && returning.len() == 2
    ));
    let serialized = serde_json::to_value(command).expect("relational command IR serializes");
    assert!(
        !serialized.to_string().contains("select_many"),
        "runtime IR carries resolved variants rather than raw command metadata: {serialized:#}"
    );

    let introspection = introspect_runtime(
        &metadata,
        &catalogs,
        commands,
        "customer",
        r#"
          {
            result: __type(name: "ReserveOrdersResult") {
              fields {
                name
                type { kind name ofType { kind name ofType { kind name } } }
              }
            }
            selected: __type(name: "ReserveOrdersSelectedRow") {
              fields { name }
            }
            totals: __type(name: "ReserveOrdersTotalsRow") {
              fields { name }
            }
          }
        "#,
    );
    let result_fields = introspection["result"]["fields"]
        .as_array()
        .expect("result fields");
    for name in ["selected", "updated"] {
        let field = result_fields
            .iter()
            .find(|field| field["name"] == name)
            .expect("row-set result field");
        assert_eq!(field["type"]["kind"], "NON_NULL");
        assert_eq!(field["type"]["ofType"]["kind"], "LIST");
        assert_eq!(field["type"]["ofType"]["ofType"]["kind"], "NON_NULL");
    }
    let totals = result_fields
        .iter()
        .find(|field| field["name"] == "totals")
        .expect("aggregate result field");
    assert_eq!(totals["type"]["kind"], "NON_NULL");
    assert_eq!(totals["type"]["ofType"]["name"], "ReserveOrdersTotalsRow");
    assert_eq!(
        introspection["selected"]["fields"],
        json!([
            { "name": "id" },
            { "name": "customer_id" },
            { "name": "status" },
            { "name": "quantity" }
        ]),
        "only select_many.returning columns enter the generated row object"
    );
    assert_eq!(
        introspection["totals"]["fields"],
        json!([
            { "name": "first_status" },
            { "name": "line_count" },
            { "name": "quantity_sum" }
        ])
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
fn command_planning_preserves_aliases_and_emits_a_resolved_command_root() {
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
    assert_eq!(command.name, "create_order");
    let [CommandExecutionStep::Insert { object, .. }] = command.steps.as_slice() else {
        panic!("expected one resolved insert step: {command:#?}");
    };
    assert!(matches!(
        object
            .iter()
            .find(|assignment| assignment.column.name == "quantity")
            .map(|assignment| &assignment.value),
        Some(CommandExecutionValue::Scalar { value, .. }) if value.as_json() == &json!(1)
    ));
    let serialized = serde_json::to_value(command).expect("command IR serializes");
    assert_eq!(serialized["selection"][0]["Scalar"]["alias"], "order");
    assert!(
        serialized.get("definition").is_none(),
        "command IR never carries raw metadata: {serialized:#}"
    );
}

#[test]
fn command_execution_identity_is_source_and_explicit_role_qualified() {
    let mut default_command = valid_command();
    default_command["permissions"] = json!([{ "role": "buyer" }]);
    let mut secondary_command = valid_command();
    secondary_command["source"] = json!("secondary");
    secondary_command["permissions"] = json!([{ "role": "merchant" }]);
    let mut metadata = two_source_metadata(vec![default_command, secondary_command]);
    set_source_table_role(&mut metadata, "default", "buyer");
    set_source_table_role(&mut metadata, "secondary", "merchant");
    let catalogs = two_source_catalogs();
    let commands = command_catalog(&metadata, &catalogs);
    let query = r#"
        mutation {
          create_order(
            id: "550e8400-e29b-41d4-a716-446655440000"
            customer_id: "550e8400-e29b-41d4-a716-446655440001"
            status: "new"
            quantity: 1
            request_id: "550e8400-e29b-41d4-a716-446655440002"
          ) { order_id }
        }
    "#;

    let identity_for = |role| {
        let plan = plan_runtime(&metadata, &catalogs, commands.clone(), role, query)
            .expect("the disjoint role resolves its source-local command");
        let MultiSourcePlan::Mutation { roots, .. } = plan else {
            panic!("command must plan as a mutation");
        };
        let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
            panic!("expected one command root");
        };
        command.identity.clone()
    };

    let buyer = identity_for("buyer");
    let merchant = identity_for("merchant");
    assert_eq!(buyer.source, "default");
    assert_eq!(buyer.name, "create_order");
    assert_eq!(buyer.role, "buyer");
    assert_eq!(merchant.source, "secondary");
    assert_eq!(merchant.name, "create_order");
    assert_eq!(merchant.role, "merchant");
    assert_ne!(
        buyer, merchant,
        "journal identity must not cross source/role"
    );
}

#[test]
fn command_planning_resolves_execution_facts_without_raw_metadata() {
    let mut command = valid_command();
    command["steps"][0]["insert"]["object"]["customer_id"] =
        json!({ "session_variable": "x-donat-user-id" });
    command["guards"] = json!([{
        "rule": "customer_is_allowed",
        "with": { "customer_id": { "arg": "customer_id" } },
        "message": "customer is not allowed to order"
    }]);
    command["idempotency"] = json!({
        "key": { "argument": "request_id" },
        "scope": [{ "session_variable": "x-donat-user-id" }],
        "retention": "30d"
    });
    command["result"]["order"] = json!({ "step": "order" });
    let mut metadata = metadata(vec![command]);
    metadata.sources[0].tables[0].insert_permissions[0]
        .permission
        .check = json!({ "customer_id": { "_eq": "X-Donat-User-Id" } });
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    assert_eq!(
        commands
            .source("default")
            .unwrap()
            .command("create_order")
            .unwrap()
            .descriptor()
            .required_session_variables["customer"]["x-donat-user-id"],
        required_scalar(ValueScalar::Uuid),
        "a typed command assignment contributes its concrete column contract"
    );
    let user_id = "550e8400-e29b-41d4-a716-446655440001";
    let plan = plan_runtime_for_session(
        &metadata,
        &catalogs,
        commands.clone(),
        session_with_vars("customer", [("x-donat-user-id", user_id)]),
        r#"
            mutation {
              submitted: create_order(
                id: "550e8400-e29b-41d4-a716-446655440000"
                customer_id: "550e8400-e29b-41d4-a716-446655440001"
                status: "new"
                quantity: 1
                request_id: "550e8400-e29b-41d4-a716-446655440002"
              ) { order: order_id order_row: order { id status } }
            }
        "#,
    )
    .expect("a permitted command plans into executable IR");
    let MultiSourcePlan::Mutation { roots, .. } = plan else {
        panic!("command must be a mutation plan");
    };
    let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
        panic!("expected one command root, got {roots:#?}");
    };

    assert_eq!(command.name, "create_order");
    let [CommandExecutionStep::Insert { object, check, .. }] = command.steps.as_slice() else {
        panic!("expected the resolved insert step: {command:#?}");
    };
    let customer_id = object
        .iter()
        .find(|assignment| assignment.column.name == "customer_id")
        .expect("customer_id assignment is resolved");
    assert!(matches!(
        &customer_id.value,
        CommandExecutionValue::Scalar { value, pg_type }
            if value == &Scalar::Json(json!(user_id))
                && pg_type == "uuid"
    ));
    assert!(check.is_some(), "the explicit role's insert check is in IR");
    let order = command
        .result
        .iter()
        .find(|field| field.name == "order")
        .expect("the declared row result is resolved");
    assert!(matches!(
        &order.value,
        CommandResultValue::StepRow { columns, .. }
            if columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>()
                == ["id", "customer_id", "status", "quantity"]
    ));
    assert_eq!(command.guards.len(), 1);
    assert!(
        !command.guards[0].sql.is_empty(),
        "the planner lowers a compiled rule instead of carrying its name/source"
    );
    assert_eq!(
        command.guards[0].error_path, "$.selectionSet.create_order",
        "runtime command rejections use the GraphQL command-field path, not a planner-internal path"
    );
    let idempotency = command
        .idempotency
        .as_ref()
        .expect("idempotency is resolved");
    assert!(
        matches!(
            idempotency.scope.as_slice(),
            [CommandExecutionValue::Scalar {
                value: Scalar::Json(value),
                pg_type,
            }] if value == &json!(user_id) && pg_type == "uuid"
        ),
        "the same session value keeps its typed contract in the idempotency scope"
    );
    assert_eq!(idempotency.retention_seconds, Some(30 * 24 * 60 * 60));
    let serialized = serde_json::to_value(command).expect("execution IR serializes");
    assert!(
        serialized.get("definition").is_none(),
        "raw metadata is absent"
    );
    assert!(
        !serialized.to_string().contains("customer_is_allowed"),
        "the raw rule name/source is absent from execution IR: {serialized:#}"
    );

    let missing = plan_runtime_for_session(
        &metadata,
        &catalogs,
        commands,
        session("customer"),
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
    .expect_err("a typed command session binding requires its header");
    assert_eq!(missing.code, "not-found");
    assert_eq!(
        missing.message,
        "missing session variable: \"x-donat-user-id\""
    );
}

#[test]
fn command_planning_keeps_insert_many_rule_item_bindings_resolved() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("command arguments are an array")
        .push(json!({ "name": "lines", "type": "[OrderInput!]!" }));
    command["steps"] = json!([{
        "name": "lines",
        "insert_many": {
            "table": { "schema": "public", "name": "orders" },
            "for_each": { "arg": "lines" },
            "object": {
                "quantity": {
                    "rule": "double_quantity",
                    "with": { "quantity": { "item": "quantity" } }
                }
            },
            "returning": ["quantity"]
        }
    }]);
    command["result"] = json!({ "lines": { "step": "lines" } });

    let metadata = metadata(vec![command]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let plan = plan_runtime(
        &metadata,
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
                lines: [{ status: "first", quantity: 2 }, { status: "second", quantity: 3 }]
              ) { lines { quantity } }
            }
        "#,
    )
    .expect("a rule may bind the current insert_many item");

    let MultiSourcePlan::Mutation { roots, .. } = plan else {
        panic!("command must plan as a mutation");
    };
    let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
        panic!("expected command mutation root: {roots:#?}");
    };
    let [
        CommandExecutionStep::InsertMany {
            item_fields,
            object,
            ..
        },
    ] = command.steps.as_slice()
    else {
        panic!(
            "expected one resolved insert_many step: {:#?}",
            command.steps
        );
    };
    assert!(
        item_fields.iter().any(|field| field.name == "quantity"),
        "the executable IR retains the typed current item binding"
    );
    assert!(matches!(
        object.first().map(|assignment| &assignment.value),
        Some(CommandExecutionValue::Rule { sql, .. }) if sql.contains("\"_cmd_item\".\"quantity\"")
    ));
}

#[test]
fn command_planning_normalizes_graphql_single_object_list_coercion() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("command arguments are an array")
        .push(json!({ "name": "lines", "type": "[OrderInput!]!" }));
    command["steps"] = json!([{
        "name": "lines",
        "insert_many": {
            "table": { "schema": "public", "name": "orders" },
            "for_each": { "arg": "lines" },
            "object": {
                "status": { "item": "status" },
                "quantity": { "item": "quantity" }
            },
            "returning": ["status", "quantity"]
        }
    }]);
    command["result"] = json!({ "lines": { "step": "lines" } });

    let metadata = metadata(vec![command]);
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let plan = plan_runtime(
        &metadata,
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
                lines: { status: "only", quantity: 2 }
              ) { lines { status quantity } }
            }
        "#,
    )
    .expect("GraphQL coerces one input object to a one-element list");

    let MultiSourcePlan::Mutation { roots, .. } = plan else {
        panic!("command must plan as a mutation");
    };
    let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
        panic!("expected one command root");
    };
    let [CommandExecutionStep::InsertMany { items, .. }] = command.steps.as_slice() else {
        panic!("expected one insert_many step");
    };
    assert_eq!(
        items.as_json(),
        &json!([{ "status": "only", "quantity": 2 }]),
        "the SQL-free boundary must carry canonical list-shaped input"
    );
}

#[test]
fn rejects_guard_rule_bindings_that_depend_on_command_steps() {
    let mut command = valid_command();
    command["guards"] = json!([{
        "rule": "customer_is_allowed",
        "with": { "customer_id": { "step": "order", "column": "customer_id" } },
        "message": "a guard must be a precondition"
    }]);
    let metadata = metadata(vec![command]);

    let error = compile(&metadata, RelationKind::Table)
        .expect_err("a guard cannot depend on a write that it must precede");
    assert!(
        error
            .message
            .contains("command guards cannot reference step 'order'"),
        "{error:?}"
    );
    assert_eq!(error.path, "commands[0].guards[0]");
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
fn command_compiler_maps_every_active_petshop_builtin_scalar_alias() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .extend([
            json!({ "name": "active_bool", "type": "bool!" }),
            json!({ "name": "active_int", "type": "int!" }),
            json!({ "name": "active_string", "type": "string!" }),
            json!({ "name": "active_bigint", "type": "bigint!" }),
            json!({ "name": "nullable_bigint", "type": "bigint" }),
            json!({ "name": "bigint_list", "type": "[bigint!]!" }),
            json!({ "name": "active_timestamptz", "type": "timestamptz!" }),
            json!({ "name": "active_uuid", "type": "uuid!" }),
        ]);
    let metadata = metadata(vec![command]);
    let catalog = compile_command_source_catalog(
        &metadata,
        "default",
        &catalog(RelationKind::Table),
        &rules(),
        true,
    )
    .expect("every active Petshop command scalar alias compiles");
    let arguments = &catalog
        .command("create_order")
        .expect("compiled command")
        .descriptor()
        .arguments
        .roots;

    for (name, scalar) in [
        ("active_bool", ValueScalar::Boolean),
        ("active_int", ValueScalar::Int32),
        ("active_string", ValueScalar::String),
        ("active_bigint", ValueScalar::Int64),
        ("active_timestamptz", ValueScalar::TimestampTz),
        ("active_uuid", ValueScalar::Uuid),
    ] {
        assert_eq!(
            arguments[name].type_ref,
            TypeRef {
                nullable: false,
                value_type: ValueType::Scalar { scalar },
            },
            "{name} maps to its closed static scalar"
        );
    }
    assert_eq!(
        arguments["nullable_bigint"].type_ref,
        TypeRef {
            nullable: true,
            value_type: ValueType::Scalar {
                scalar: ValueScalar::Int64,
            },
        },
        "omitting ! makes the bigint alias nullable without changing its scalar"
    );
    assert_eq!(
        arguments["bigint_list"].type_ref,
        TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(TypeRef {
                    nullable: false,
                    value_type: ValueType::Scalar {
                        scalar: ValueScalar::Int64,
                    },
                }),
            },
        },
        "the bigint alias remains closed under list and item nullability"
    );
}

#[test]
fn command_compiler_keeps_unknown_and_malformed_bigint_forms_closed() {
    for invalid in [
        "biginteger!",
        "BigInt!",
        "bigint!!",
        "[bigint",
        "bigint]",
        "[bigint!]!!",
    ] {
        let mut command = valid_command();
        command["arguments"]
            .as_array_mut()
            .expect("arguments array")
            .push(json!({ "name": "invalid_alias", "type": invalid }));
        let metadata = metadata(vec![command]);

        let error = compile(&metadata, RelationKind::Table)
            .expect_err("unknown and malformed aliases must remain deployment errors");
        assert_eq!(error.code, "validation-failed", "{invalid}: {error:?}");
        assert_eq!(
            error.path, "commands[0].arguments[5]",
            "{invalid}: {error:?}"
        );
    }
}

#[test]
fn command_planner_resolves_bigint_arguments_as_int8_values() {
    let mut command = valid_command();
    command["arguments"][3]["type"] = json!("bigint!");
    let metadata = metadata(vec![command]);
    let mut bigint_catalog = catalog(RelationKind::Table);
    bigint_catalog
        .tables
        .get_mut("public.orders")
        .expect("orders table")
        .columns
        .iter_mut()
        .find(|column| column.name == "quantity")
        .expect("quantity column")
        .pg_type = "int8".to_owned();
    let catalogs = HashMap::from([("default".to_string(), bigint_catalog)]);
    let commands = command_catalog(&metadata, &catalogs);

    let plan = plan_runtime(
        &metadata,
        &catalogs,
        commands,
        "customer",
        r#"
            mutation {
              create_order(
                id: "550e8400-e29b-41d4-a716-446655440000"
                customer_id: "550e8400-e29b-41d4-a716-446655440001"
                status: "new"
                quantity: 2147483648
                request_id: "550e8400-e29b-41d4-a716-446655440002"
              ) { order_id }
            }
        "#,
    )
    .expect("an i64 value outside GraphQL Int range plans through the bigint alias");
    let MultiSourcePlan::Mutation { roots, .. } = plan else {
        panic!("command must be a mutation plan");
    };
    let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
        panic!("expected one command root");
    };
    let [CommandExecutionStep::Insert { object, .. }] = command.steps.as_slice() else {
        panic!("expected the resolved insert step");
    };
    let quantity = object
        .iter()
        .find(|assignment| assignment.column.name == "quantity")
        .expect("quantity assignment is resolved");
    assert!(matches!(
        &quantity.value,
        CommandExecutionValue::Scalar { value, pg_type }
            if value == &Scalar::Json(json!(2147483648_i64)) && pg_type == "int8"
    ));
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
fn command_compiler_resolves_dotted_scalar_relation_references() {
    let mut command = valid_command();
    command["steps"][0]["insert"]["table"] = json!("public.orders");
    let metadata = metadata(vec![command]);

    compile(&metadata, RelationKind::Table)
        .expect("a scalar schema.name command target matches the tracked qualified table");
}

#[test]
fn missing_command_source_catalog_is_a_validation_error() {
    let metadata = metadata(vec![valid_command()]);
    let error = compile_command_catalog(&metadata, &HashMap::new(), &rules(), true)
        .expect_err("a missing source catalog must fail closed without panicking");

    assert_eq!(error.path, "commands[0]");
    assert_eq!(
        error.message,
        "catalog for command source 'default' is missing"
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
fn rejects_command_root_collisions_visible_to_the_same_role_across_sources() {
    let first = valid_command();
    let mut second = valid_command();
    second["source"] = json!("secondary");
    let metadata = two_source_metadata(vec![first, second]);
    let catalogs = two_source_catalogs();

    let diagnostics = validate_command_catalog(&metadata, &catalogs, &rules(), true);

    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[1]");
    assert_eq!(
        diagnostics[0].message,
        "command root 'create_order' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'secondary')"
    );
    assert_eq!(diagnostics[1].path, "commands[1]");
    assert_eq!(
        diagnostics[1].message,
        "generated command type 'CreateOrderResult' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'secondary')"
    );
}

#[test]
fn permits_same_command_root_in_disjoint_role_schemas_across_sources() {
    let first = valid_command();
    let mut second = valid_command();
    second["source"] = json!("secondary");
    second["permissions"] = json!([{ "role": "supplier" }]);
    let mut metadata = two_source_metadata(vec![first, second]);
    set_source_table_role(&mut metadata, "secondary", "supplier");
    let catalogs = two_source_catalogs();
    let commands = command_catalog(&metadata, &catalogs);

    for role in ["customer", "supplier"] {
        let data = introspect_runtime(
            &metadata,
            &catalogs,
            commands.clone(),
            role,
            r#"{ __type(name: "mutation_root") { fields { name } } }"#,
        );
        assert_eq!(
            data["__type"]["fields"]
                .as_array()
                .expect("role mutation schema")
                .iter()
                .filter(|field| field["name"] == "create_order")
                .count(),
            1,
            "{role} must see exactly its one source-local command root: {data:#}"
        );
    }

    let mutation = r#"
        mutation {
          create_order(
            id: "00000000-0000-0000-0000-000000000001"
            customer_id: "00000000-0000-0000-0000-000000000002"
            status: "new"
            quantity: 1
            request_id: "00000000-0000-0000-0000-000000000003"
          ) { order_id }
        }
    "#;
    for (role, expected_source) in [("customer", "default"), ("supplier", "secondary")] {
        let planned = plan_runtime(&metadata, &catalogs, commands.clone(), role, mutation)
            .expect("the explicit role routes its visible command root");
        let MultiSourcePlan::Mutation { source, .. } = planned else {
            panic!("command must plan as a mutation");
        };
        assert_eq!(source.as_deref(), Some(expected_source));
    }
}

#[test]
fn rejects_pascal_cased_command_result_type_collisions_in_one_role_schema() {
    let mut snake_case = valid_command();
    snake_case["name"] = json!("foo_bar");
    snake_case["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    let mut camel_case = valid_command();
    camel_case["name"] = json!("fooBar");
    camel_case["result"] = json!({
        "status": { "step": "order", "column": "status" }
    });
    let metadata = metadata(vec![snake_case, camel_case]);

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[1]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'default')"
    );
}

#[test]
fn rejects_identical_pascal_cased_command_result_type_collisions_in_one_role_schema() {
    let mut snake_case = valid_command();
    snake_case["name"] = json!("foo_bar");
    snake_case["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    let mut camel_case = valid_command();
    camel_case["name"] = json!("fooBar");
    camel_case["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    let metadata = metadata(vec![snake_case, camel_case]);

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[1]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'default')"
    );
}

#[test]
fn rejects_command_result_type_colliding_with_its_custom_input_argument_type() {
    let mut command = valid_command();
    command["name"] = json!("foo_bar");
    command["arguments"]
        .as_array_mut()
        .expect("command arguments are an array")
        .push(json!({ "name": "payload", "type": "FooBarResult!" }));
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects[0].name = "FooBarResult".to_string();

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in commands[0] (source 'default') and custom_types.input_objects[0]"
    );
}

#[test]
fn rejects_command_result_type_colliding_with_a_public_action_input_type() {
    let mut command = valid_command();
    command["name"] = json!("foo_bar");
    command["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects[0].name = "FooBarResult".to_string();
    metadata.actions.push(
        serde_json::from_value(json!({
            "name": "public_submit",
            "definition": {
                "handler": "https://example.invalid/public-submit",
                "arguments": [{ "name": "input", "type": "FooBarResult!" }],
                "output_type": "String"
            },
            "permissions": []
        }))
        .expect("public action metadata deserializes"),
    );

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in commands[0] (source 'default') and actions[0] (action 'public_submit') -> custom_types.input_objects[0]"
    );
}

#[test]
fn rejects_command_row_type_colliding_with_a_public_action_output_type() {
    let mut command = valid_command();
    command["name"] = json!("foo_bar");
    command["steps"][0]["insert"]["returning"] = json!(["id", "status"]);
    command["result"] = json!({ "order": { "step": "order" } });
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.objects.push(
        serde_json::from_value(json!({
            "name": "FooBarOrderRow",
            "fields": [{ "name": "id", "type": "uuid!" }]
        }))
        .expect("public action output type metadata deserializes"),
    );
    metadata.actions.push(
        serde_json::from_value(json!({
            "name": "public_order",
            "definition": {
                "handler": "https://example.invalid/public-order",
                "output_type": "FooBarOrderRow"
            },
            "permissions": []
        }))
        .expect("public action metadata deserializes"),
    );

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0].steps[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarOrderRow' is visible to role 'customer' in commands[0].steps[0] (step 'order') and actions[0] (action 'public_order') -> custom_types.objects[0]"
    );
}

#[test]
fn permits_a_command_type_matching_an_action_hidden_from_its_role() {
    let mut command = valid_command();
    command["name"] = json!("foo_bar");
    command["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects[0].name = "FooBarResult".to_string();
    metadata.actions.push(
        serde_json::from_value(json!({
            "name": "supplier_submit",
            "definition": {
                "handler": "https://example.invalid/supplier-submit",
                "arguments": [{ "name": "input", "type": "FooBarResult!" }],
                "output_type": "String"
            },
            "permissions": [{ "role": "supplier" }]
        }))
        .expect("permissioned action metadata deserializes"),
    );

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert!(
        diagnostics.is_empty(),
        "an action unavailable to customer must not reserve its types: {diagnostics:#?}"
    );
}

#[test]
fn accepts_a_command_argument_with_a_directly_recursive_input_object() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("command arguments are an array")
        .push(json!({ "name": "tree", "type": "TreeInput!" }));
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects[0] = serde_json::from_value(json!({
        "name": "TreeInput",
        "fields": [{ "name": "children", "type": "[TreeInput!]" }]
    }))
    .expect("recursive input type metadata deserializes");
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);

    let commands = command_catalog(&metadata, &catalogs);
    let _schema = runtime_schema(&metadata, &catalogs, commands.clone());

    let valid = r#"
        mutation {
          create_order(
            id: "00000000-0000-0000-0000-000000000001"
            customer_id: "00000000-0000-0000-0000-000000000002"
            status: "new"
            quantity: 1
            request_id: "00000000-0000-0000-0000-000000000003"
            tree: { children: [{ children: [] }] }
          ) { order_id }
        }
    "#;
    plan_runtime(&metadata, &catalogs, commands.clone(), "customer", valid)
        .expect("a finite recursive input value plans successfully");

    let invalid = r#"
        mutation {
          create_order(
            id: "00000000-0000-0000-0000-000000000001"
            customer_id: "00000000-0000-0000-0000-000000000002"
            status: "new"
            quantity: 1
            request_id: "00000000-0000-0000-0000-000000000003"
            tree: { children: ["not an input object"] }
          ) { order_id }
        }
    "#;
    let error = plan_runtime(&metadata, &catalogs, commands, "customer", invalid)
        .expect_err("nested recursive input values keep their declared validation");
    assert_eq!(error.message, "argument must be input object 'TreeInput'");
}

#[test]
fn accepts_a_command_argument_with_an_indirectly_recursive_input_object() {
    let mut command = valid_command();
    command["arguments"]
        .as_array_mut()
        .expect("command arguments are an array")
        .push(json!({ "name": "root", "type": "FirstInput!" }));
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects = vec![
        serde_json::from_value(json!({
            "name": "FirstInput",
            "fields": [{ "name": "second", "type": "SecondInput!" }]
        }))
        .expect("first recursive input metadata deserializes"),
        serde_json::from_value(json!({
            "name": "SecondInput",
            "fields": [{ "name": "firsts", "type": "[FirstInput!]" }]
        }))
        .expect("second recursive input metadata deserializes"),
    ];
    let catalogs = HashMap::from([("default".to_string(), catalog(RelationKind::Table))]);

    let commands = command_catalog(&metadata, &catalogs);
    let _schema = runtime_schema(&metadata, &catalogs, commands.clone());
    let mutation = r#"
        mutation {
          create_order(
            id: "00000000-0000-0000-0000-000000000001"
            customer_id: "00000000-0000-0000-0000-000000000002"
            status: "new"
            quantity: 1
            request_id: "00000000-0000-0000-0000-000000000003"
            root: { second: { firsts: [{ second: { firsts: [] } }] } }
          ) { order_id }
        }
    "#;
    plan_runtime(&metadata, &catalogs, commands, "customer", mutation)
        .expect("a finite indirectly recursive input value plans successfully");
}

#[test]
fn detects_a_generated_type_collision_reachable_inside_a_recursive_input_cycle() {
    let mut command = valid_command();
    command["name"] = json!("foo_bar");
    command["result"] = json!({
        "order_id": { "step": "order", "column": "id" }
    });
    command["arguments"]
        .as_array_mut()
        .expect("command arguments are an array")
        .push(json!({ "name": "root", "type": "RootInput!" }));
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects = vec![
        serde_json::from_value(json!({
            "name": "RootInput",
            "fields": [{ "name": "result", "type": "FooBarResult!" }]
        }))
        .expect("recursive input root metadata deserializes"),
        serde_json::from_value(json!({
            "name": "FooBarResult",
            "fields": [{ "name": "roots", "type": "[RootInput!]" }]
        }))
        .expect("recursive input nested metadata deserializes"),
    ];

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in commands[0] (source 'default') and custom_types.input_objects[1]"
    );
}

#[test]
fn rejects_command_result_type_colliding_with_a_role_visible_table_type() {
    let mut command = valid_command();
    command["name"] = json!("foo_bar");
    let mut metadata = metadata(vec![command]);
    metadata.sources[0].tables[0].configuration = Some(
        serde_json::from_value(json!({ "custom_name": "FooBarResult" }))
            .expect("table configuration deserializes"),
    );

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in commands[0] (source 'default') and sources[0].tables[0] (source 'default')"
    );
}

#[test]
fn rejects_pascal_cased_command_row_type_collisions_in_one_role_schema() {
    let mut snake_case = valid_command();
    snake_case["name"] = json!("foo_bar");
    snake_case["steps"][0]["insert"]["returning"] = json!(["id", "status"]);
    snake_case["result"] = json!({ "order": { "step": "order" } });
    let mut camel_case = valid_command();
    camel_case["name"] = json!("fooBar");
    camel_case["steps"][0]["insert"]["returning"] = json!(["id", "customer_id"]);
    camel_case["result"] = json!({ "order": { "step": "order" } });
    let metadata = metadata(vec![snake_case, camel_case]);

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[1].steps[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'FooBarOrderRow' is visible to role 'customer' in both commands[0].steps[0] (step 'order') and commands[1].steps[0] (step 'order')"
    );
    assert_eq!(diagnostics[1].path, "commands[1]");
    assert_eq!(
        diagnostics[1].message,
        "generated command type 'FooBarResult' is visible to role 'customer' in both commands[0] (source 'default') and commands[1] (source 'default')"
    );
}

#[test]
fn rejects_normalized_row_type_collision_between_steps_in_one_command_with_distinct_shapes() {
    let mut command = valid_command();
    let insert = command["steps"][0]["insert"].clone();
    command["steps"] = json!([
        {
            "name": "order_line",
            "insert": {
                "table": insert["table"].clone(),
                "object": insert["object"].clone(),
                "returning": ["id"]
            }
        },
        {
            "name": "orderLine",
            "insert": {
                "table": insert["table"].clone(),
                "object": insert["object"].clone(),
                "returning": ["status"]
            }
        }
    ]);
    command["result"] = json!({
        "first": { "step": "order_line" },
        "second": { "step": "orderLine" }
    });
    let metadata = metadata(vec![command]);

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0].steps[1]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'CreateOrderOrderLineRow' is visible to role 'customer' in both commands[0].steps[0] (step 'order_line') and commands[0].steps[1] (step 'orderLine')"
    );
}

#[test]
fn rejects_normalized_row_type_collision_between_steps_in_one_command_with_equal_shapes() {
    let mut command = valid_command();
    let insert = command["steps"][0]["insert"].clone();
    command["steps"] = json!([
        {
            "name": "order_line",
            "insert": {
                "table": insert["table"].clone(),
                "object": insert["object"].clone(),
                "returning": ["id"]
            }
        },
        {
            "name": "orderLine",
            "insert": {
                "table": insert["table"].clone(),
                "object": insert["object"].clone(),
                "returning": ["id"]
            }
        }
    ]);
    command["result"] = json!({
        "first": { "step": "order_line" },
        "second": { "step": "orderLine" }
    });
    let metadata = metadata(vec![command]);

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0].steps[1]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'CreateOrderOrderLineRow' is visible to role 'customer' in both commands[0].steps[0] (step 'order_line') and commands[0].steps[1] (step 'orderLine')"
    );
}

#[test]
fn rejects_step_row_type_name_that_is_not_a_legal_graphql_name() {
    let mut command = valid_command();
    command["steps"][0]["name"] = json!("order!");
    command["result"] = json!({ "order": { "step": "order!" } });
    command["steps"][0]["insert"]["returning"] = json!(["id"]);
    let metadata = metadata(vec![command]);

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0].steps[0]");
    assert_eq!(
        diagnostics[0].message,
        "generated command type 'CreateOrderOrder!Row' for role 'customer' at commands[0].steps[0] (step 'order!') is not a valid GraphQL name"
    );
}

#[test]
fn rejects_command_root_collision_with_a_query_action_visible_to_the_same_role() {
    let mut metadata = metadata(vec![valid_command()]);
    metadata.actions.push(
        serde_json::from_value(json!({
            "name": "create_order",
            "definition": {
                "type": "query",
                "handler": "https://example.invalid/action",
                "output_type": "String"
            },
            "permissions": [{ "role": "customer" }]
        }))
        .expect("query action metadata deserializes"),
    );

    let diagnostics = validate_command_catalog(
        &metadata,
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].path, "commands[0]");
    assert_eq!(
        diagnostics[0].message,
        "command root 'create_order' is visible to role 'customer' in commands[0] (source 'default') and actions[0] (action 'create_order', type 'query')"
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
fn rejects_empty_command_write_objects_before_serving() {
    let mut empty_insert = valid_command();
    empty_insert["steps"][0]["insert"]["object"] = json!({});
    assert_rejected(
        empty_insert,
        "insert object must contain at least one column assignment",
    );

    let mut empty_insert_many = valid_command();
    empty_insert_many["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({ "name": "items", "type": "[OrderInput!]!" }));
    empty_insert_many["steps"] = json!([{
        "name": "orders",
        "insert_many": {
            "table": { "schema": "public", "name": "orders" },
            "for_each": { "arg": "items" },
            "object": {},
            "returning": ["id"]
        }
    }]);
    empty_insert_many["result"] = json!({ "orders": { "step": "orders" } });
    assert_rejected(
        empty_insert_many,
        "insert_many object must contain at least one column assignment",
    );

    let mut empty_update = valid_command();
    empty_update["steps"] = json!([{
        "name": "order",
        "update": {
            "table": { "schema": "public", "name": "orders" },
            "where": { "id": { "arg": "id" } },
            "set": {},
            "returning": ["id"]
        }
    }]);
    empty_update["result"] = json!({ "order_id": { "step": "order", "column": "id" } });
    assert_rejected(
        empty_update,
        "update set must contain at least one column assignment",
    );
}

#[test]
fn command_assignability_preserves_argument_item_column_and_step_nullability() {
    let mut optional_argument = valid_command();
    optional_argument["arguments"][2]["type"] = json!("String");
    assert_rejected(
        optional_argument,
        "nullable String is not assignable to column 'status' (String)",
    );

    let mut nullable_input_item = valid_command();
    nullable_input_item["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({ "name": "items", "type": "[OrderInput!]!" }));
    nullable_input_item["steps"] = json!([{
        "name": "orders",
        "insert_many": {
            "table": { "schema": "public", "name": "orders" },
            "for_each": { "arg": "items" },
            "object": { "status": { "item": "status" } },
            "returning": ["id"]
        }
    }]);
    nullable_input_item["result"] = json!({ "orders": { "step": "orders" } });
    let mut nullable_item_metadata = metadata(vec![nullable_input_item]);
    nullable_item_metadata.custom_types.input_objects[0].fields[0].type_ = "String".to_string();
    let error = compile(&nullable_item_metadata, RelationKind::Table)
        .expect_err("a nullable item field cannot feed a required column");
    assert!(
        error
            .message
            .contains("nullable String is not assignable to column 'status' (String)"),
        "{error:?}"
    );

    let mut optional_step = valid_command();
    optional_step["steps"] = json!([
        {
            "name": "maybe_order",
            "select_one": {
                "table": { "schema": "public", "name": "orders" },
                "by": { "id": { "arg": "id" } },
                "returning": ["status"],
                "require_found": false
            }
        },
        {
            "name": "copy",
            "insert": {
                "table": { "schema": "public", "name": "orders" },
                "object": {
                    "id": { "arg": "request_id" },
                    "status": { "step": "maybe_order", "column": "status" }
                },
                "returning": ["id"]
            }
        }
    ]);
    optional_step["result"] = json!({ "order_id": { "step": "copy", "column": "id" } });
    assert_rejected(
        optional_step,
        "nullable String is not assignable to column 'status' (String)",
    );

    let mut nullable_column_catalog = catalog(RelationKind::Table);
    nullable_column_catalog
        .tables
        .get_mut("public.orders")
        .expect("orders table")
        .columns
        .push(column_with("optional_status", "text", -1, true));
    let mut nullable_column_step = valid_command();
    nullable_column_step["steps"] = json!([
        {
            "name": "source",
            "select_one": {
                "table": { "schema": "public", "name": "orders" },
                "by": { "id": { "arg": "id" } },
                "returning": ["optional_status"]
            }
        },
        {
            "name": "copy",
            "insert": {
                "table": { "schema": "public", "name": "orders" },
                "object": {
                    "id": { "arg": "request_id" },
                    "status": { "step": "source", "column": "optional_status" }
                },
                "returning": ["id"]
            }
        }
    ]);
    nullable_column_step["result"] = json!({ "order_id": { "step": "copy", "column": "id" } });
    let error = compile_with_catalog(
        &metadata(vec![nullable_column_step]),
        nullable_column_catalog,
    )
    .expect_err("a nullable returned column cannot feed a required column");
    assert!(
        error
            .message
            .contains("nullable String is not assignable to column 'status' (String)"),
        "{error:?}"
    );

    let mut nullable_rule_value = valid_command();
    nullable_rule_value["steps"][0]["insert"]["object"]["status"] =
        json!({ "rule": "maybe_status" });
    let nullable_rules = compile_catalog(
        &[RuleDefinition {
            name: "maybe_status".to_string(),
            bindings: BTreeMap::new(),
            result: RuleType::nullable(RuleType::String),
            expression: "null".to_string(),
        }],
        &[],
    )
    .expect("nullable value Rule compiles");
    let error = compile_command_catalog(
        &metadata(vec![nullable_rule_value]),
        &HashMap::from([("default".to_string(), catalog(RelationKind::Table))]),
        &nullable_rules,
        true,
    )
    .expect_err("a nullable Rule result cannot feed a required column");
    assert!(
        error
            .message
            .contains("rule 'maybe_status' must return String"),
        "{error:?}"
    );
}

#[test]
fn nullable_command_arguments_may_be_omitted_or_null_only_for_nullable_columns() {
    let mut command = valid_command();
    command["arguments"][2]["type"] = json!("String");
    let metadata = metadata(vec![command]);
    let catalogs = HashMap::from([(
        "default".to_string(),
        literal_target_catalog("text", -1, true),
    )]);
    let commands = command_catalog(&metadata, &catalogs);

    for status_argument in ["", "status: null"] {
        let plan = plan_runtime(
            &metadata,
            &catalogs,
            commands.clone(),
            &session("customer").role,
            &format!(
                r#"
                mutation {{
                  create_order(
                    id: "550e8400-e29b-41d4-a716-446655440000"
                    customer_id: "550e8400-e29b-41d4-a716-446655440001"
                    {status_argument}
                    quantity: 1
                    request_id: "550e8400-e29b-41d4-a716-446655440002"
                  ) {{ order_id }}
                }}
                "#
            ),
        )
        .expect("a nullable destination accepts omitted and explicit-null optional arguments");
        let MultiSourcePlan::Mutation { roots, .. } = plan else {
            panic!("command must plan as a mutation");
        };
        let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
            panic!("expected one command root");
        };
        let [CommandExecutionStep::Insert { object, .. }] = command.steps.as_slice() else {
            panic!("expected insert command step");
        };
        assert!(matches!(
            object
                .iter()
                .find(|assignment| assignment.column.name == "status")
                .map(|assignment| &assignment.value),
            Some(CommandExecutionValue::Scalar { value, .. }) if value.as_json().is_null()
        ));
    }
}

#[test]
fn command_result_integral_literals_are_limited_to_graphql_int_range() {
    for boundary in [json!(i32::MIN), json!(i32::MAX)] {
        let mut command = valid_command();
        command["result"]["boundary"] = json!({ "literal": boundary });
        compile(&metadata(vec![command]), RelationKind::Table)
            .expect("GraphQL Int boundaries are valid result literals");
    }

    for outside in [
        json!(i64::from(i32::MIN) - 1),
        json!(i64::from(i32::MAX) + 1),
    ] {
        let mut command = valid_command();
        command["result"]["outside"] = json!({ "literal": outside });
        assert_rejected(
            command,
            "integral command result literal is outside the GraphQL Int range",
        );
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
fn allocate_many_compiles_to_bounded_typed_ir_with_named_row_sets() {
    let command = json!({
        "name": "allocate_test",
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "arguments": [{ "name": "request_id", "type": "uuid!" }],
        "steps": [
            {
                "name": "candidates",
                "fixed_rows": {
                    "maximum_rows": 2,
                    "rows": [
                        {
                            "order_id": { "literal": "order-1" },
                            "order_line_id": { "literal": "line-1" },
                            "line_sequence": { "literal": 1 },
                            "variant_id": { "literal": "variant-1" },
                            "location_code": { "literal": "A" },
                            "inventory_level_id": { "literal": "stock-1" },
                            "requested_quantity": { "literal": 3 },
                            "available_quantity": { "literal": 2 },
                            "unit_price_minor": { "literal": 100 },
                            "currency": { "literal": "USD" },
                            "allocation_rank": { "literal": 1 }
                        },
                        {
                            "order_id": { "literal": "order-1" },
                            "order_line_id": { "literal": "line-1" },
                            "line_sequence": { "literal": 1 },
                            "variant_id": { "literal": "variant-1" },
                            "location_code": { "literal": "B" },
                            "inventory_level_id": { "literal": "stock-2" },
                            "requested_quantity": { "literal": 3 },
                            "available_quantity": { "literal": 2 },
                            "unit_price_minor": { "literal": 100 },
                            "currency": { "literal": "USD" },
                            "allocation_rank": { "literal": 2 }
                        }
                    ]
                }
            },
            {
                "name": "allocation",
                "allocate_many": {
                    "from": { "step": "candidates" },
                    "request_id": { "arg": "request_id" },
                    "group_key": ["location_code"],
                    "exact_quantity_columns": {
                        "requested": "requested_quantity",
                        "available": "available_quantity",
                        "allocated": "allocated_quantity",
                        "backordered": "backordered_quantity"
                    },
                    "allocation_id": "deterministic",
                    "returning": {
                        "groups": ["allocation_id", "order_id", "first_line_sequence", "allocation_rank", "location_code", "currency", "items"],
                        "lines": ["allocation_id", "order_id", "order_line_id", "line_sequence", "variant_id", "location_code", "inventory_level_id", "requested_quantity", "allocated_quantity", "unit_price_minor", "currency"],
                        "backorders": ["order_id", "order_line_id", "requested_quantity", "backordered_quantity"]
                    },
                    "group_order_by": ["first_line_sequence", "allocation_rank", "location_code", "allocation_id"],
                    "line_order_by": ["line_sequence", "location_code", "allocation_id"]
                }
            }
        ],
        "result": {
            "allocations": {
                "step": "allocation",
                "field": "groups",
                "as": "AllocationGroup",
                "maximum_items": 2
            }
        }
    });
    let metadata = metadata(vec![command]);
    let catalogs = HashMap::from([("default".to_owned(), catalog(RelationKind::Table))]);
    let commands = command_catalog(&metadata, &catalogs);
    let plan = plan_runtime(
        &metadata,
        &catalogs,
        commands,
        "customer",
        r#"mutation {
            allocate_test(request_id: "550e8400-e29b-41d4-a716-446655440000") {
                allocations { allocation_id order_id location_code }
            }
        }"#,
    )
    .expect("the deterministic allocation grammar compiles and plans");
    let MultiSourcePlan::Mutation { roots, .. } = plan else {
        panic!("allocation command must plan as a mutation");
    };
    let [MutationRoot::Command { command, .. }] = roots.as_slice() else {
        panic!("expected one allocation command root");
    };
    let CommandExecutionStep::AllocateMany {
        group_key,
        groups,
        lines,
        backorders,
        maximum_rows,
        ..
    } = &command.steps[1]
    else {
        panic!("the second step must be typed allocation IR");
    };
    assert_eq!(group_key[0].name, "location_code");
    assert_eq!(*maximum_rows, 256);
    assert!(groups.iter().any(|column| column.name == "items"));
    assert!(
        lines
            .iter()
            .any(|column| column.name == "allocated_quantity")
    );
    assert!(
        backorders
            .iter()
            .any(|column| column.name == "backordered_quantity")
    );
    assert!(matches!(
        &command.result[0].value,
        CommandResultValue::ProjectedRows {
            cte,
            many: true,
            maximum_items: 2,
            ..
        } if cte.ends_with("_groups")
    ));

    let introspection = introspect_runtime(
        &metadata,
        &catalogs,
        command_catalog(&metadata, &catalogs),
        "customer",
        r#"
          {
            result: __type(name: "AllocateTestResult") {
              fields {
                name
                type { kind name ofType { kind name ofType { kind name } } }
              }
            }
            group: __type(name: "AllocationGroup") {
              fields { name }
            }
          }
        "#,
    );
    assert_eq!(
        introspection["result"]["fields"][0]["type"]["ofType"]["kind"],
        "LIST"
    );
    assert_eq!(
        introspection["group"]["fields"],
        json!([
            { "name": "allocation_id" },
            { "name": "order_id" },
            { "name": "first_line_sequence" },
            { "name": "allocation_rank" },
            { "name": "location_code" },
            { "name": "currency" },
            { "name": "items" }
        ])
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
        .expect_err("command cannot collide with an action root");
    assert!(error.message.contains("actions[0]"));

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

    let mut mutable_scope = valid_command();
    mutable_scope["arguments"]
        .as_array_mut()
        .expect("arguments array")
        .push(json!({ "name": "payload", "type": "jsonb!" }));
    mutable_scope["idempotency"] = json!({
        "key": { "argument": "request_id" },
        "scope": [{ "argument": "payload" }]
    });
    assert_rejected(
        mutable_scope,
        "idempotency scope must not use json or jsonb",
    );

    let mut nullable_key = valid_command();
    nullable_key["arguments"]
        .as_array_mut()
        .expect("arguments array")[4]["type"] = json!("uuid");
    nullable_key["idempotency"] = json!({ "key": { "argument": "request_id" } });
    assert_rejected(
        nullable_key,
        "idempotency key must be a required scalar argument",
    );

    for retention in ["30", "0d", "1w", "999999999999999999999999999999999999d"] {
        let mut invalid_retention = valid_command();
        invalid_retention["idempotency"] = json!({
            "key": { "argument": "request_id" },
            "retention": retention
        });
        assert_rejected(invalid_retention, "idempotency retention");
    }

    let mut sqlite_metadata = metadata(vec![valid_command()]);
    sqlite_metadata.sources[0].kind = SourceKind::Sqlite;
    let error =
        compile(&sqlite_metadata, RelationKind::Table).expect_err("commands remain Postgres-only");
    assert!(error.message.contains("requires a Postgres source"));
}

fn descriptor_metadata(effect_session_variable: &str) -> Metadata {
    let command = json!({
        "name": "create_order",
        "source": "default",
        "permissions": [{ "role": "customer" }, { "role": "support" }],
        "arguments": [
            { "name": "id", "type": "uuid!" },
            { "name": "customer_id", "type": "uuid!" },
            { "name": "status", "type": "String!" },
            { "name": "quantity", "type": "Int!" },
            { "name": "request_id", "type": "uuid!" },
            { "name": "tree", "type": "TreeInput!" }
        ],
        "guards": [{
            "rule": "customer_is_allowed",
            "with": { "customer_id": { "arg": "customer_id" } }
        }],
        "steps": [
            {
                "name": "order",
                "insert": {
                    "table": { "schema": "public", "name": "orders" },
                    "object": {
                        "id": { "arg": "id" },
                        "customer_id": { "arg": "customer_id" },
                        "status": { "arg": "status" },
                        "quantity": { "arg": "quantity" }
                    },
                    "returning": ["id", "status"]
                }
            },
            {
                "name": "lookup",
                "select_one": {
                    "table": { "schema": "public", "name": "orders" },
                    "by": { "id": { "arg": "id" } },
                    "returning": ["status"],
                    "require_found": false
                }
            }
        ],
        "result": {
            "order_id": { "step": "order", "column": "id" },
            "maybe_status": { "step": "lookup", "column": "status" }
        },
        "idempotency": {
            "key": { "argument": "request_id" },
            "scope": [
                { "argument": "customer_id" },
                { "session_variable": "X-Donat-Request-Scope" }
            ],
            "retention": "30d"
        },
        "effects": [{
            "start_process": {
                "process": "checkout_order",
                "input": {
                    "tree": { "arg": "tree" },
                    "actor": { "session_variable": effect_session_variable }
                },
                "idempotency_key": { "argument": "request_id" }
            }
        }]
    });
    let mut metadata = metadata(vec![command]);
    metadata.custom_types.input_objects.push(
        serde_json::from_value(json!({
            "name": "TreeInput",
            "fields": [
                { "name": "label", "type": "String!" },
                { "name": "child", "type": "TreeInput" }
            ]
        }))
        .expect("recursive input metadata"),
    );

    let table = &mut metadata.sources[0].tables[0];
    table.select_permissions[0].permission.filter =
        json!({ "customer_id": { "_eq": "X-Donat-Customer-Id" } });
    table.insert_permissions[0].permission.check =
        json!({ "customer_id": { "_eq": "X-Donat-Customer-Id" } });

    let mut support_select = table.select_permissions[0].clone();
    support_select.role = "support".to_owned();
    support_select.permission.filter = json!({ "customer_id": { "_eq": "X-Donat-Support-Id" } });
    table.select_permissions.push(support_select);

    let mut support_insert = table.insert_permissions[0].clone();
    support_insert.role = "support".to_owned();
    support_insert.permission.check = json!({ "customer_id": { "_eq": "X-Donat-Support-Id" } });
    table.insert_permissions.push(support_insert);
    metadata
}

fn descriptor_catalog(metadata: &Metadata) -> donat_schema::CompiledSourceCommandCatalog {
    descriptor_catalog_with_rules(metadata, &rules())
}

fn descriptor_catalog_with_rules(
    metadata: &Metadata,
    rules: &RuleCatalog,
) -> donat_schema::CompiledSourceCommandCatalog {
    compile_command_source_catalog(
        metadata,
        "default",
        &catalog(RelationKind::Table),
        rules,
        true,
    )
    .expect("source-local command catalog compiles")
}

fn predicate_descriptor_fixture(filter: Json) -> (Metadata, Catalog) {
    let mut raw =
        serde_json::to_value(metadata(vec![valid_command()])).expect("metadata serializes");
    let orders = &mut raw["sources"][0]["tables"][0];
    orders["select_permissions"][0]["permission"]["filter"] = filter;
    orders["insert_permissions"][0]["permission"]["check"] = json!({});
    orders["object_relationships"] = json!([{
        "name": "customer",
        "using": {
            "manual_configuration": {
                "remote_table": { "schema": "public", "name": "customers" },
                "column_mapping": { "customer_id": "id" }
            }
        }
    }]);
    raw["sources"][0]["tables"]
        .as_array_mut()
        .expect("tracked tables array")
        .push(json!({
            "table": { "schema": "public", "name": "customers" }
        }));
    let metadata = serde_json::from_value(raw).expect("predicate metadata deserializes");

    let mut catalog = catalog(RelationKind::Table);
    catalog.tables.insert(
        "public.customers".to_owned(),
        TableInfo {
            schema: "public".to_owned(),
            name: "customers".to_owned(),
            relation_kind: RelationKind::Table,
            columns: vec![
                column("id", "uuid"),
                column("external_id", "int8"),
                column("active", "bool"),
            ],
            primary_key: vec!["id".to_owned()],
            foreign_keys: vec![],
        },
    );
    (metadata, catalog)
}

fn required_scalar(scalar: ValueScalar) -> TypeRef {
    TypeRef {
        nullable: false,
        value_type: ValueType::Scalar { scalar },
    }
}

fn declare_custom_scalar(metadata: &mut Metadata, name: &str) {
    metadata.custom_types.scalars.push(ScalarType {
        name: name.to_owned(),
        description: None,
    });
}

#[test]
fn command_descriptor_exposes_exact_contract() {
    let metadata = descriptor_metadata("X-Donat-Actor-Id");
    let source = descriptor_catalog(&metadata);
    let descriptor = source
        .command("create_order")
        .expect("compiled command")
        .descriptor();

    assert_eq!(descriptor.source, "default");
    assert_eq!(descriptor.name, "create_order");
    assert_eq!(
        descriptor.allowed_roles,
        BTreeSet::from(["customer".to_owned(), "support".to_owned()])
    );
    assert!(descriptor.arguments.roots["tree"].required);
    assert!(matches!(
        descriptor.arguments.roots["tree"].type_ref.value_type,
        ValueType::Ref { ref name } if name == "TreeInput"
    ));
    assert!(matches!(
        descriptor.arguments.named_objects["TreeInput"].fields["child"]
            .type_ref
            .value_type,
        ValueType::Ref { ref name } if name == "TreeInput"
    ));
    assert!(descriptor.result.roots["order_id"].required);
    assert!(!descriptor.result.roots["order_id"].type_ref.nullable);
    assert!(descriptor.result.roots["maybe_status"].type_ref.nullable);

    let uuid = TypeRef {
        nullable: false,
        value_type: ValueType::Scalar {
            scalar: ValueScalar::Uuid,
        },
    };
    let string = TypeRef {
        nullable: false,
        value_type: ValueType::Scalar {
            scalar: ValueScalar::String,
        },
    };
    assert_eq!(
        descriptor.required_session_variables["customer"],
        BTreeMap::from([
            ("x-donat-actor-id".to_owned(), string.clone()),
            ("x-donat-customer-id".to_owned(), uuid.clone()),
            ("x-donat-request-scope".to_owned(), string.clone()),
        ])
    );
    assert_eq!(
        descriptor.required_session_variables["support"],
        BTreeMap::from([
            ("x-donat-actor-id".to_owned(), string.clone()),
            ("x-donat-request-scope".to_owned(), string),
            ("x-donat-support-id".to_owned(), uuid),
        ])
    );

    let global = compile_command_catalog(
        &metadata,
        &HashMap::from([("default".to_owned(), catalog(RelationKind::Table))]),
        &rules(),
        true,
    )
    .expect("global catalog compiles");
    assert_eq!(
        descriptor,
        global
            .source("default")
            .unwrap()
            .command("create_order")
            .unwrap()
            .descriptor(),
        "global and source-local paths publish one identical descriptor"
    );
}

#[test]
fn command_descriptor_session_contracts_follow_predicate_operators_and_tables() {
    let (metadata, catalog) = predicate_descriptor_fixture(json!({
        "_and": [
            { "customer_id": { "_in": "X-Donat-Customer-Ids" } },
            { "status": { "_is_null": "X-Donat-Status-Is-Null" } },
            { "customer": {
                "external_id": { "_eq": "X-Donat-Relationship-External-Id" }
            }},
            { "_exists": {
                "_table": { "schema": "public", "name": "customers" },
                "_where": {
                    "external_id": { "_eq": "X-Donat-Exists-External-Id" }
                }
            }}
        ]
    }));
    let descriptor = compile_command_source_catalog(&metadata, "default", &catalog, &rules(), true)
        .expect("operator-aware permission sessions compile")
        .command("create_order")
        .expect("compiled command")
        .descriptor()
        .clone();
    let required = &descriptor.required_session_variables["customer"];

    assert_eq!(
        required["x-donat-customer-ids"],
        TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(required_scalar(ValueScalar::Uuid)),
            },
        },
        "_in consumes a non-null list of non-null column scalars"
    );
    assert_eq!(
        required["x-donat-status-is-null"],
        required_scalar(ValueScalar::Boolean),
        "_is_null consumes a boolean independently of the column type"
    );
    assert_eq!(
        required["x-donat-relationship-external-id"],
        required_scalar(ValueScalar::Int64),
        "relationship predicates use the remote column contract"
    );
    assert_eq!(
        required["x-donat-exists-external-id"],
        required_scalar(ValueScalar::Int64),
        "_exists._where uses its declared remote table contract"
    );
}

#[test]
fn command_descriptor_session_contracts_follow_operator_operand_types() {
    let (mut metadata, mut catalog) = predicate_descriptor_fixture(json!({
        "_and": [
            { "document": { "_has_key": "X-Donat-Document-Key" } },
            { "document": { "_has_keys_any": "X-Donat-Document-Keys" } },
            { "location": { "_st_d_within": {
                "distance": "X-Donat-Max-Distance",
                "from": "X-Donat-Origin"
            }}}
        ]
    }));
    declare_custom_scalar(&mut metadata, "geometry");
    catalog
        .tables
        .get_mut("public.orders")
        .expect("orders catalog")
        .columns
        .extend([column("document", "jsonb"), column("location", "geometry")]);
    let descriptor = compile_command_source_catalog(&metadata, "default", &catalog, &rules(), true)
        .expect("operator-specific permission sessions compile")
        .command("create_order")
        .expect("compiled command")
        .descriptor()
        .clone();
    let required = &descriptor.required_session_variables["customer"];

    assert_eq!(
        required["x-donat-document-key"],
        required_scalar(ValueScalar::String),
        "_has_key consumes a string key, not a jsonb value"
    );
    assert_eq!(
        required["x-donat-document-keys"],
        TypeRef {
            nullable: false,
            value_type: ValueType::List {
                element: Box::new(required_scalar(ValueScalar::String)),
            },
        },
        "_has_keys_any consumes a list of strings"
    );
    assert_eq!(
        required["x-donat-max-distance"],
        required_scalar(ValueScalar::Decimal),
        "_st_d_within.distance consumes a decimal"
    );
    assert_eq!(
        required["x-donat-origin"],
        required_scalar(ValueScalar::Custom {
            name: "geometry".to_owned(),
        }),
        "_st_d_within.from consumes the geo column value"
    );
}

#[test]
fn command_descriptor_rejects_undeclared_source_scalar_contract() {
    let (metadata, mut catalog) = predicate_descriptor_fixture(json!({
        "path": { "_eq": "X-Donat-Path" }
    }));
    catalog
        .tables
        .get_mut("public.orders")
        .expect("orders catalog")
        .columns
        .push(column("path", "ltree"));

    let error = compile_command_source_catalog(&metadata, "default", &catalog, &rules(), true)
        .expect_err("a GraphQL-safe PostgreSQL type is not an implicit custom scalar");

    assert_eq!(error.path, "commands[0]");
    assert_eq!(
        error.message,
        "column scalar 'ltree' has no closed session-variable contract"
    );
}

#[test]
fn command_descriptor_accepts_declared_custom_source_scalar_contract() {
    let (mut metadata, mut catalog) = predicate_descriptor_fixture(json!({
        "path": { "_eq": "X-Donat-Path" }
    }));
    declare_custom_scalar(&mut metadata, "ltree");
    catalog
        .tables
        .get_mut("public.orders")
        .expect("orders catalog")
        .columns
        .push(column("path", "ltree"));

    let descriptor = compile_command_source_catalog(&metadata, "default", &catalog, &rules(), true)
        .expect("an explicitly declared source scalar has a closed nominal contract")
        .command("create_order")
        .expect("compiled command")
        .descriptor()
        .clone();

    assert_eq!(
        descriptor.required_session_variables["customer"]["x-donat-path"],
        required_scalar(ValueScalar::Custom {
            name: "ltree".to_owned(),
        })
    );
}

#[test]
fn command_descriptor_rejects_computed_permission_session_argument() {
    let (metadata, mut catalog) = predicate_descriptor_fixture(json!({
        "session_label": { "_eq": "literal" }
    }));
    let mut raw = serde_json::to_value(metadata).expect("metadata serializes");
    raw["sources"][0]["tables"][0]["computed_fields"] = json!([{
        "name": "session_label",
        "definition": {
            "function": { "schema": "public", "name": "session_label" },
            "session_argument": "session"
        }
    }]);
    let metadata: Metadata =
        serde_json::from_value(raw).expect("computed-field metadata deserializes");
    catalog.functions.insert(
        "public.session_label".to_owned(),
        FunctionInfo {
            schema: "public".to_owned(),
            name: "session_label".to_owned(),
            args: vec![FunctionArg {
                name: Some("session".to_owned()),
                has_default: false,
                pg_type: "jsonb".to_owned(),
                composite_of: None,
            }],
            returns_table: None,
            returns_set: false,
            returns_scalar: Some("text".to_owned()),
        },
    );

    let error = compile_command_source_catalog(&metadata, "default", &catalog, &rules(), true)
        .expect_err("whole-session computed fields cannot publish a closed name set");
    assert_eq!(error.path, "commands[0]");
    assert_eq!(
        error.message,
        "computed field 'session_label' uses session_argument and cannot publish a closed session-variable contract"
    );
}

#[test]
fn command_descriptor_rejects_predicate_operator_contract_conflicts() {
    let (metadata, catalog) = predicate_descriptor_fixture(json!({
        "_and": [
            { "customer_id": { "_in": "X-Donat-Shared-Id" } },
            { "customer_id": { "_eq": "X-Donat-Shared-Id" } }
        ]
    }));
    let error = compile_command_source_catalog(&metadata, "default", &catalog, &rules(), true)
        .expect_err("one session variable cannot be both uuid and list<uuid>");

    assert_eq!(error.path, "commands[0]");
    assert_eq!(
        error.message,
        "session variable 'x-donat-shared-id' has incompatible contracts for role 'customer'"
    );
}

#[test]
fn command_descriptor_fingerprint_is_pre_process_and_deterministic() {
    let metadata = descriptor_metadata("X-Donat-Actor-Id");
    let first = descriptor_catalog(&metadata)
        .command("create_order")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    let second = descriptor_catalog(&metadata)
        .command("create_order")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    );

    let mut changed_effect = serde_json::to_value(&metadata).expect("metadata serializes");
    changed_effect["commands"][0]["effects"][0]["start_process"]["process"] =
        json!("other_checkout");
    let changed_effect: Metadata =
        serde_json::from_value(changed_effect).expect("changed metadata deserializes");
    let changed = descriptor_catalog(&changed_effect)
        .command("create_order")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    assert_ne!(first, changed, "raw effect shape is fingerprinted");

    let mut changed_guard_binding = serde_json::to_value(&metadata).expect("metadata serializes");
    changed_guard_binding["commands"][0]["guards"][0]["with"]["customer_id"] =
        json!({ "arg": "id" });
    let changed_guard_binding: Metadata =
        serde_json::from_value(changed_guard_binding).expect("changed metadata deserializes");
    let changed_guard_binding_fingerprint = descriptor_catalog(&changed_guard_binding)
        .command("create_order")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    assert_ne!(
        first, changed_guard_binding_fingerprint,
        "raw guard bindings are fingerprinted"
    );

    let mut changed_guard_message = serde_json::to_value(&metadata).expect("metadata serializes");
    changed_guard_message["commands"][0]["guards"][0]["message"] = json!("customer is not allowed");
    let changed_guard_message: Metadata =
        serde_json::from_value(changed_guard_message).expect("changed metadata deserializes");
    let changed_guard_message_fingerprint = descriptor_catalog(&changed_guard_message)
        .command("create_order")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    assert_ne!(
        first, changed_guard_message_fingerprint,
        "raw guard messages are fingerprinted"
    );

    let changed_rules = compile_catalog(
        &[
            RuleDefinition {
                name: "customer_is_allowed".to_owned(),
                bindings: BTreeMap::from([("customer_id".to_owned(), RuleType::Uuid)]),
                result: RuleType::Bool,
                expression: "false".to_owned(),
            },
            RuleDefinition {
                name: "double_quantity".to_owned(),
                bindings: BTreeMap::from([("quantity".to_owned(), RuleType::Int)]),
                result: RuleType::Int,
                expression: "quantity * 2".to_owned(),
            },
        ],
        &[],
    )
    .expect("changed rule catalog compiles");
    let changed_rule_fingerprint = descriptor_catalog_with_rules(&metadata, &changed_rules)
        .command("create_order")
        .unwrap()
        .descriptor()
        .definition_fingerprint
        .clone();
    assert_ne!(
        first, changed_rule_fingerprint,
        "referenced Rule artifact hashes are fingerprinted"
    );
}

#[test]
fn command_descriptor_rejects_incompatible_session_variable_uses() {
    let mut metadata = descriptor_metadata("X-Donat-Actor-Id");
    let table = &mut metadata.sources[0].tables[0];
    table.select_permissions[0].permission.filter =
        json!({ "customer_id": { "_eq": "X-Donat-Shared-Id" } });
    table.insert_permissions[0].permission.check =
        json!({ "quantity": { "_eq": "X-Donat-Shared-Id" } });
    let error = compile_command_source_catalog(
        &metadata,
        "default",
        &catalog(RelationKind::Table),
        &rules(),
        true,
    )
    .expect_err("one session variable cannot be both uuid and int32 for customer");
    assert!(error.message.contains("x-donat-shared-id"));
    assert!(error.message.contains("customer"));
    assert!(error.message.contains("incompatible"));
}

fn petshop_decision_rules() -> RuleCatalog {
    compile_catalog(
        &[
            RuleDefinition {
                name: "positive_quantity".to_owned(),
                bindings: BTreeMap::from([("quantity".to_owned(), RuleType::Int)]),
                result: RuleType::Bool,
                expression: "quantity > 0".to_owned(),
            },
            RuleDefinition {
                name: "double_quantity".to_owned(),
                bindings: BTreeMap::from([("quantity".to_owned(), RuleType::Int)]),
                result: RuleType::Int,
                expression: "quantity * 2".to_owned(),
            },
        ],
        &[DecisionTableDefinition {
            name: "quantity_route".to_owned(),
            revision: "fixture".to_owned(),
            inputs: BTreeMap::from([("quantity".to_owned(), RuleType::Int)]),
            output: BTreeMap::from([
                ("route".to_owned(), RuleType::String),
                ("multiplier".to_owned(), RuleType::Int),
            ]),
            hit_policy: HitPolicy::First,
            rows: vec![
                DecisionRow {
                    id: "bulk".to_owned(),
                    description: None,
                    when: BTreeMap::from([("quantity".to_owned(), "quantity >= 10".to_owned())]),
                    output: json!({ "route": "bulk", "multiplier": 2 }),
                },
                DecisionRow {
                    id: "ordinary".to_owned(),
                    description: None,
                    when: BTreeMap::from([("quantity".to_owned(), "true".to_owned())]),
                    output: json!({ "route": "ordinary", "multiplier": 1 }),
                },
            ],
            test_cases: vec![],
        }],
    )
    .expect("Petshop decision fixture compiles")
}

fn compile_petshop_command(command: Json) -> Result<(), PlanError> {
    let metadata = metadata(vec![command]);
    compile_command_catalog(
        &metadata,
        &HashMap::from([("default".to_owned(), catalog(RelationKind::Table))]),
        &petshop_decision_rules(),
        true,
    )
    .map(|_| ())
}

fn pure_petshop_command(steps: Vec<Json>, result: Json) -> Json {
    json!({
        "name": "petshop_compiler_contract",
        "source": "default",
        "permissions": [{ "role": "customer" }],
        "arguments": [
            { "name": "quantity", "type": "Int!" },
            { "name": "status", "type": "String!" },
            { "name": "enabled", "type": "Boolean!" },
            { "name": "request_id", "type": "uuid!" }
        ],
        "steps": steps,
        "result": result
    })
}

#[test]
fn petshop_project_rejects_forward_step_reference() {
    let command = pure_petshop_command(
        vec![
            json!({
                "name": "projected",
                "project": {
                    "values": {
                        "status": { "step": "later", "column": "status" }
                    }
                }
            }),
            json!({
                "name": "later",
                "project": {
                    "values": {
                        "status": { "arg": "status" }
                    }
                }
            }),
        ],
        json!({}),
    );

    let error = compile_petshop_command(command)
        .expect_err("a pure projection cannot read a later command step");
    assert_eq!(error.path, "commands[0].steps[0]");
    assert_eq!(
        error.message,
        "step reference 'later' must reference an earlier step"
    );
}

#[test]
fn petshop_project_many_rejects_scalar_input_cardinality() {
    let command = pure_petshop_command(
        vec![
            json!({
                "name": "one",
                "project": {
                    "values": {
                        "quantity": { "arg": "quantity" }
                    }
                }
            }),
            json!({
                "name": "many",
                "project_many": {
                    "from": { "step": "one" },
                    "maximum_rows": 16,
                    "values": {
                        "quantity": { "item": "quantity" }
                    }
                }
            }),
        ],
        json!({}),
    );

    let error = compile_petshop_command(command)
        .expect_err("project_many requires a prior bounded row set");
    assert_eq!(error.path, "commands[0].steps[1]");
    assert_eq!(
        error.message,
        "project_many input must be a prior row-set step"
    );
}

#[test]
fn petshop_decision_rejects_input_type_mismatch() {
    let command = pure_petshop_command(
        vec![json!({
            "name": "route",
            "decision": {
                "decision_table": "quantity_route",
                "input": {
                    "quantity": { "arg": "status" }
                },
                "returning": ["route"]
            }
        })],
        json!({}),
    );

    let error = compile_petshop_command(command)
        .expect_err("decision inputs retain the compiled table's exact types");
    assert_eq!(error.path, "commands[0].steps[0]");
    assert_eq!(
        error.message,
        "String is not assignable to decision input 'quantity' (Int)"
    );
}

#[test]
fn petshop_result_rejects_undeclared_projected_output() {
    let command = pure_petshop_command(
        vec![json!({
            "name": "projected",
            "project": {
                "values": {
                    "public_status": { "arg": "status" }
                }
            }
        })],
        json!({
            "leaked": {
                "step": "projected",
                "project": {
                    "status": "private_status"
                },
                "maximum_items": 1
            }
        }),
    );

    let error = compile_petshop_command(command)
        .expect_err("a result projection cannot expose a field absent from its producer");
    assert_eq!(error.path, "commands[0]");
    assert_eq!(
        error.message,
        "step 'projected' does not expose result field 'private_status'"
    );
}

#[test]
fn petshop_conditional_write_rejects_invalid_condition() {
    let command = pure_petshop_command(
        vec![json!({
            "name": "write",
            "update_when": {
                "when": {
                    "argument_equals": {
                        "argument": "missing",
                        "value": true
                    }
                },
                "table": { "schema": "public", "name": "orders" },
                "where": { "id": { "arg": "request_id" } },
                "set": { "status": { "arg": "status" } },
                "returning": ["id"]
            }
        })],
        json!({}),
    );

    let error =
        compile_petshop_command(command).expect_err("conditional gates name declared arguments");
    assert_eq!(error.path, "commands[0].steps[0]");
    assert_eq!(
        error.message,
        "unknown argument 'missing' in command condition"
    );
}

#[test]
fn petshop_decision_rejects_non_finite_result_literal() {
    let command = pure_petshop_command(
        vec![json!({
            "name": "projected",
            "project": {
                "values": {
                    "amount": {
                        "rule": "double_quantity",
                        "with": {
                            "quantity": { "literal": "NaN" }
                        }
                    }
                }
            }
        })],
        json!({}),
    );

    let error = compile_petshop_command(command)
        .expect_err("non-finite numeric spellings cannot enter resolved decision data");
    assert_eq!(error.path, "commands[0].steps[0]");
    assert_eq!(error.message, "invalid literal for Int");
}

#[test]
fn petshop_fixed_rows_rejects_bound_overflow() {
    let command = pure_petshop_command(
        vec![json!({
            "name": "rows",
            "fixed_rows": {
                "maximum_rows": 257,
                "rows": [{
                    "quantity": { "arg": "quantity" }
                }]
            }
        })],
        json!({}),
    );

    let error = compile_petshop_command(command)
        .expect_err("pure row-set bounds remain within the fixed deployment limit");
    assert_eq!(error.path, "commands[0].steps[0]");
    assert_eq!(
        error.message,
        "fixed_rows maximum_rows must be between 1 and 256"
    );
}
