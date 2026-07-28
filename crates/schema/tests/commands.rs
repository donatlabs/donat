use std::collections::{BTreeMap, HashMap};

use donat_catalog::{Catalog, ColumnInfo, FunctionInfo, RelationKind, TableInfo};
use donat_metadata::{Metadata, SourceKind};
use donat_rules::{RuleCatalog, RuleDefinition, RuleType, compile_catalog};
use donat_schema::{PlanError, compile_command_catalog};
use serde_json::{Value as Json, json};

fn column(name: &str, pg_type: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        pg_type: pg_type.to_string(),
        native_type: None,
        nullable: false,
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
    for (column, literal, expected) in [
        ("id", json!("not-a-uuid"), "uuid"),
        ("quantity", json!("not-an-integer"), "Int"),
        ("order_date", json!("2026-99-99"), "date"),
        ("created_at", json!("not-a-timestamp"), "timestamp"),
    ] {
        let mut command = valid_command();
        command["steps"][0]["insert"]["object"][column] = json!({ "literal": literal });
        assert_rejected(command, &format!("invalid literal for {expected}"));
    }
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
