//! GraphQL introspection (`__schema` / `__type`): builds the role's type
//! system from metadata + catalog and projects it through the client's
//! selection set. The schema reflects exactly what the planner would
//! accept: per-role roots, column masks, relationships, mutations.

use serde_json::{Map as JsonMap, Value as Json, json};

use graphql_parser::query::{Definition, Document, OperationDefinition};

use crate::plan::{Fragments, PlanError, Planner, Session, flatten, value_to_json};
use crate::naming::{root_names, table_base_name};

/// GraphQL scalar name for a Postgres type (Hasura naming).
fn scalar_name(pg_type: &str) -> &str {
    match pg_type {
        "int2" | "int4" | "serial" => "Int",
        "float4" | "float8" => "Float",
        "text" | "varchar" | "bpchar" | "name" | "citext" => "String",
        "bool" => "Boolean",
        "uuid" => "uuid",
        other => other,
    }
}

fn named(kind: &str, name: &str) -> Json {
    json!({ "__typename": "__Type", "kind": kind, "name": name, "ofType": null })
}

fn non_null(inner: Json) -> Json {
    json!({ "__typename": "__Type", "kind": "NON_NULL", "name": null, "ofType": inner })
}

fn list_of(inner: Json) -> Json {
    json!({ "__typename": "__Type", "kind": "LIST", "name": null, "ofType": inner })
}

fn field(name: &str, args: Vec<Json>, ty: Json) -> Json {
    json!({
        "__typename": "__Field",
        "name": name,
        "description": null,
        "args": args,
        "type": ty,
        "isDeprecated": false,
        "deprecationReason": null,
    })
}

fn input_value(name: &str, ty: Json) -> Json {
    json!({
        "__typename": "__InputValue",
        "name": name,
        "description": null,
        "type": ty,
        "defaultValue": null,
    })
}

fn object_type(name: &str, fields: Vec<Json>) -> Json {
    json!({
        "__typename": "__Type",
        "kind": "OBJECT",
        "name": name,
        "description": null,
        "fields": fields,
        "inputFields": null,
        "interfaces": [],
        "enumValues": null,
        "possibleTypes": null,
    })
}

fn input_object_type(name: &str, input_fields: Vec<Json>) -> Json {
    json!({
        "__typename": "__Type",
        "kind": "INPUT_OBJECT",
        "name": name,
        "description": null,
        "fields": null,
        "inputFields": input_fields,
        "interfaces": null,
        "enumValues": null,
        "possibleTypes": null,
    })
}

fn scalar_type(name: &str) -> Json {
    json!({
        "__typename": "__Type",
        "kind": "SCALAR",
        "name": name,
        "description": null,
        "fields": null,
        "inputFields": null,
        "interfaces": null,
        "enumValues": null,
        "possibleTypes": null,
    })
}

fn enum_type(name: &str, values: &[&str]) -> Json {
    let enum_values: Vec<Json> = values
        .iter()
        .map(|v| {
            json!({
                "__typename": "__EnumValue",
                "name": v,
                "description": null,
                "isDeprecated": false,
                "deprecationReason": null,
            })
        })
        .collect();
    json!({
        "__typename": "__Type",
        "kind": "ENUM",
        "name": name,
        "description": null,
        "fields": null,
        "inputFields": null,
        "interfaces": null,
        "enumValues": enum_values,
        "possibleTypes": null,
    })
}

const ORDER_BY_VALUES: &[&str] = &[
    "asc",
    "asc_nulls_first",
    "asc_nulls_last",
    "desc",
    "desc_nulls_first",
    "desc_nulls_last",
];

/// Build the full `__schema` value for the session's role.
pub(crate) fn build_schema_json(planner: &Planner, session: &Session) -> Json {
    let mut types: Vec<Json> = vec![];
    let mut scalars: std::collections::BTreeSet<String> =
        ["Int", "Float", "String", "Boolean", "ID"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    types.push(enum_type("order_by", ORDER_BY_VALUES));

    let mut query_fields: Vec<Json> = vec![];
    let mut mutation_fields: Vec<Json> = vec![];

    let select_args = |base: &str| -> Vec<Json> {
        vec![
            input_value("where", named("INPUT_OBJECT", &format!("{base}_bool_exp"))),
            input_value(
                "order_by",
                list_of(non_null(named("INPUT_OBJECT", &format!("{base}_order_by")))),
            ),
            input_value("limit", named("SCALAR", "Int")),
            input_value("offset", named("SCALAR", "Int")),
            input_value(
                "distinct_on",
                list_of(non_null(named("ENUM", &format!("{base}_select_column")))),
            ),
        ]
    };

    for (idx, entry) in planner.tables().iter().enumerate() {
        let Some(ctx) = planner.table_ctx(idx, &session.role) else {
            continue;
        };
        let base = table_base_name(entry);
        let names = root_names(entry);

        // The table object type: columns + relationships.
        let mut fields = vec![];
        let mut select_columns: Vec<String> = vec![];
        for col in &ctx.info.columns {
            if !ctx.column_allowed(&col.name) {
                continue;
            }
            let scalar = scalar_name(&col.pg_type).to_string();
            scalars.insert(scalar.clone());
            let ty = if col.nullable {
                named("SCALAR", &scalar)
            } else {
                non_null(named("SCALAR", &scalar))
            };
            fields.push(field(&col.name, vec![], ty));
            select_columns.push(col.name.clone());
        }
        for rel in &entry.object_relationships {
            if let Some((remote, _)) = planner.relationship_target(&ctx, &rel.name, "$") {
                if let Some(remote_entry) = planner.entry_for(&remote) {
                    if planner
                        .table_ctx_by_name(&remote, &session.role)
                        .is_some()
                    {
                        // Hasura makes an FK-based object relationship
                        // non-nullable when the local FK column(s) are NOT
                        // NULL (the related row always exists).
                        let base = named("OBJECT", &table_base_name(remote_entry));
                        let ty = if object_rel_is_non_null(rel, ctx.info) {
                            non_null(base)
                        } else {
                            base
                        };
                        fields.push(field(&rel.name, vec![], ty));
                    }
                }
            }
        }
        for rel in &entry.array_relationships {
            if let Some((remote, _)) = planner.relationship_target(&ctx, &rel.name, "$") {
                if let Some(remote_entry) = planner.entry_for(&remote) {
                    if planner
                        .table_ctx_by_name(&remote, &session.role)
                        .is_some()
                    {
                        let remote_base = table_base_name(remote_entry);
                        fields.push(field(
                            &rel.name,
                            select_args(&remote_base),
                            non_null(list_of(non_null(named("OBJECT", &remote_base)))),
                        ));
                    }
                }
            }
        }
        types.push(object_type(&base, fields));

        // Select-column enum, bool_exp, order_by inputs.
        let cols: Vec<&str> = select_columns.iter().map(String::as_str).collect();
        types.push(enum_type(&format!("{base}_select_column"), &cols));

        let mut bool_exp_fields = vec![
            input_value(
                "_and",
                list_of(non_null(named("INPUT_OBJECT", &format!("{base}_bool_exp")))),
            ),
            input_value(
                "_or",
                list_of(non_null(named("INPUT_OBJECT", &format!("{base}_bool_exp")))),
            ),
            input_value("_not", named("INPUT_OBJECT", &format!("{base}_bool_exp"))),
        ];
        let mut order_by_fields = vec![];
        for col in &ctx.info.columns {
            if !ctx.column_allowed(&col.name) {
                continue;
            }
            let scalar = scalar_name(&col.pg_type).to_string();
            bool_exp_fields.push(input_value(
                &col.name,
                named("INPUT_OBJECT", &format!("{scalar}_comparison_exp")),
            ));
            order_by_fields.push(input_value(&col.name, named("ENUM", "order_by")));
        }
        types.push(input_object_type(&format!("{base}_bool_exp"), bool_exp_fields));
        types.push(input_object_type(&format!("{base}_order_by"), order_by_fields));

        // Query roots.
        query_fields.push(field(
            &names.select,
            select_args(&base),
            non_null(list_of(non_null(named("OBJECT", &base)))),
        ));
        if !ctx.info.primary_key.is_empty()
            && ctx
                .info
                .primary_key
                .iter()
                .all(|pk| ctx.column_allowed(pk))
        {
            let pk_args: Vec<Json> = ctx
                .info
                .primary_key
                .iter()
                .map(|pk| {
                    let scalar =
                        scalar_name(&ctx.info.column(pk).map(|c| c.pg_type.clone()).unwrap_or_default())
                            .to_string();
                    input_value(pk, non_null(named("SCALAR", &scalar)))
                })
                .collect();
            query_fields.push(field(&names.select_by_pk, pk_args, named("OBJECT", &base)));
        }
        if ctx.allow_aggregations() {
            query_fields.push(field(
                &names.select_aggregate,
                select_args(&base),
                non_null(named("OBJECT", &format!("{base}_aggregate"))),
            ));
            types.push(object_type(
                &format!("{base}_aggregate"),
                vec![
                    field(
                        "aggregate",
                        vec![],
                        named("OBJECT", &format!("{base}_aggregate_fields")),
                    ),
                    field(
                        "nodes",
                        vec![],
                        non_null(list_of(non_null(named("OBJECT", &base)))),
                    ),
                ],
            ));
            types.push(object_type(
                &format!("{base}_aggregate_fields"),
                vec![field("count", vec![], non_null(named("SCALAR", "Int")))],
            ));
        }

        // Mutations for the role.
        let insert_perm = planner.resolve_role_perm(&entry.insert_permissions, &session.role, |p| {
            !p.backend_only || session.backend_request
        });
        if insert_perm.is_some() {
            types.push(input_object_type(
                &format!("{base}_insert_input"),
                ctx.info
                    .columns
                    .iter()
                    .map(|c| {
                        let scalar = scalar_name(&c.pg_type).to_string();
                        input_value(&c.name, named("SCALAR", &scalar))
                    })
                    .collect(),
            ));
            types.push(object_type(
                &format!("{base}_mutation_response"),
                vec![
                    field("affected_rows", vec![], non_null(named("SCALAR", "Int"))),
                    field(
                        "returning",
                        vec![],
                        non_null(list_of(non_null(named("OBJECT", &base)))),
                    ),
                ],
            ));
            mutation_fields.push(field(
                &format!("insert_{base}"),
                vec![input_value(
                    "objects",
                    non_null(list_of(non_null(named(
                        "INPUT_OBJECT",
                        &format!("{base}_insert_input"),
                    )))),
                )],
                named("OBJECT", &format!("{base}_mutation_response")),
            ));
        }
        if planner
            .resolve_role_perm(&entry.update_permissions, &session.role, |_| true)
            .is_some()
        {
            mutation_fields.push(field(
                &format!("update_{base}"),
                vec![
                    input_value(
                        "where",
                        non_null(named("INPUT_OBJECT", &format!("{base}_bool_exp"))),
                    ),
                    input_value("_set", named("INPUT_OBJECT", &format!("{base}_set_input"))),
                ],
                named("OBJECT", &format!("{base}_mutation_response")),
            ));
            types.push(input_object_type(
                &format!("{base}_set_input"),
                ctx.info
                    .columns
                    .iter()
                    .map(|c| {
                        let scalar = scalar_name(&c.pg_type).to_string();
                        input_value(&c.name, named("SCALAR", &scalar))
                    })
                    .collect(),
            ));
        }
        if planner
            .resolve_role_perm(&entry.delete_permissions, &session.role, |_| true)
            .is_some()
        {
            mutation_fields.push(field(
                &format!("delete_{base}"),
                vec![input_value(
                    "where",
                    non_null(named("INPUT_OBJECT", &format!("{base}_bool_exp"))),
                )],
                named("OBJECT", &format!("{base}_mutation_response")),
            ));
        }
    }

    // Comparison input objects for every scalar in use.
    for scalar in &scalars {
        let s = named("SCALAR", scalar);
        types.push(input_object_type(
            &format!("{scalar}_comparison_exp"),
            vec![
                input_value("_eq", s.clone()),
                input_value("_neq", s.clone()),
                input_value("_gt", s.clone()),
                input_value("_gte", s.clone()),
                input_value("_lt", s.clone()),
                input_value("_lte", s.clone()),
                input_value("_in", list_of(non_null(s.clone()))),
                input_value("_nin", list_of(non_null(s.clone()))),
                input_value("_is_null", named("SCALAR", "Boolean")),
            ],
        ));
        types.push(scalar_type(scalar));
    }

    let has_mutations = !mutation_fields.is_empty();
    let subscription_fields = query_fields.clone();
    types.push(object_type("query_root", query_fields));
    types.push(object_type("subscription_root", subscription_fields));
    if has_mutations {
        types.push(object_type("mutation_root", mutation_fields));
    }

    let if_arg = input_value("if", non_null(named("SCALAR", "Boolean")));
    json!({
        "__typename": "__Schema",
        "queryType": { "__typename": "__Type", "name": "query_root", "kind": "OBJECT" },
        "mutationType": if has_mutations {
            json!({ "__typename": "__Type", "name": "mutation_root", "kind": "OBJECT" })
        } else {
            Json::Null
        },
        "subscriptionType": { "__typename": "__Type", "name": "subscription_root", "kind": "OBJECT" },
        "types": types,
        "directives": [
            {
                "__typename": "__Directive",
                "name": "include",
                "description": null,
                "locations": ["FIELD", "FRAGMENT_SPREAD", "INLINE_FRAGMENT"],
                "args": [if_arg.clone()],
                "isRepeatable": false,
            },
            {
                "__typename": "__Directive",
                "name": "skip",
                "description": null,
                "locations": ["FIELD", "FRAGMENT_SPREAD", "INLINE_FRAGMENT"],
                "args": [if_arg],
                "isRepeatable": false,
            },
        ],
    })
}

/// If the operation is an introspection query, answer it. Returns None
/// when the operation selects regular data fields.
pub fn execute_introspection(
    planner: &Planner,
    session: &Session,
    doc: &Document<'static, String>,
    operation_name: Option<&str>,
    variables: &JsonMap<String, Json>,
) -> Option<Result<Json, PlanError>> {
    let mut fragments: Fragments = std::collections::HashMap::new();
    let mut operations = vec![];
    for def in &doc.definitions {
        match def {
            Definition::Fragment(f) => {
                fragments.insert(f.name.clone(), f);
            }
            Definition::Operation(op) => operations.push(op),
        }
    }
    let op = match operation_name {
        Some(wanted) => operations.iter().find(|op| match op {
            OperationDefinition::Query(q) => q.name.as_deref() == Some(wanted),
            _ => false,
        }),
        None if operations.len() == 1 => operations.first(),
        None => None,
    }?;
    let selection_set = match op {
        OperationDefinition::Query(q) => &q.selection_set,
        OperationDefinition::SelectionSet(s) => s,
        _ => return None,
    };

    let roots = flatten(selection_set, &fragments, variables, None).ok()?;
    let is_introspection = roots
        .iter()
        .any(|f| f.name == "__schema" || f.name == "__type");
    if !is_introspection {
        return None;
    }

    let schema = build_schema_json(planner, session);
    let mut data = JsonMap::new();
    for root in roots {
        let alias = root.alias.clone().unwrap_or_else(|| root.name.clone());
        let value = match root.name.as_str() {
            "__typename" => Json::String("query_root".to_string()),
            "__schema" => {
                match project(&schema, &root.selection_set, &fragments, variables) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                }
            }
            "__type" => {
                let name = root
                    .arguments
                    .iter()
                    .find(|(n, _)| n == "name")
                    .and_then(|(_, v)| value_to_json(v, variables, "$").ok())
                    .and_then(|v| v.as_str().map(str::to_string));
                let found = name.and_then(|n| {
                    schema["types"]
                        .as_array()
                        .and_then(|ts| ts.iter().find(|t| t["name"] == Json::String(n.clone())))
                        .cloned()
                });
                match found {
                    Some(t) => match project(&t, &root.selection_set, &fragments, variables) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    },
                    None => Json::Null,
                }
            }
            other => {
                return Some(Err(PlanError::validation(
                    "$",
                    format!("cannot mix '{other}' with introspection fields"),
                )));
            }
        };
        data.insert(alias, value);
    }
    Some(Ok(Json::Object(data)))
}

/// Project a prebuilt introspection value through a selection set.
/// An FK-based object relationship is non-nullable when every local FK
/// column is NOT NULL (the referenced row is guaranteed to exist). Manual
/// relationships are always nullable.
fn object_rel_is_non_null(
    rel: &donat_metadata::ObjectRelationship,
    info: &donat_catalog::TableInfo,
) -> bool {
    use donat_metadata::ObjRelFkColumns;
    let Some(fk) = &rel.using.foreign_key_constraint_on else {
        return false;
    };
    let cols: Vec<&str> = match fk {
        ObjRelFkColumns::Single(c) => vec![c.as_str()],
        ObjRelFkColumns::Multiple(cs) => cs.iter().map(String::as_str).collect(),
    };
    !cols.is_empty()
        && cols
            .iter()
            .all(|c| info.column(c).is_some_and(|col| !col.nullable))
}

fn project(
    node: &Json,
    selection_set: &graphql_parser::query::SelectionSet<'static, String>,
    fragments: &Fragments,
    variables: &JsonMap<String, Json>,
) -> Result<Json, PlanError> {
    match node {
        Json::Null => Ok(Json::Null),
        Json::Array(items) => items
            .iter()
            .map(|item| project(item, selection_set, fragments, variables))
            .collect::<Result<Vec<_>, _>>()
            .map(Json::Array),
        Json::Object(map) => {
            let fields = flatten(selection_set, fragments, variables, None)?;
            let mut out = JsonMap::new();
            for f in fields {
                let alias = f.alias.clone().unwrap_or_else(|| f.name.clone());
                let value = map.get(f.name.as_str()).cloned().unwrap_or(Json::Null);
                let projected = if f.selection_set.items.is_empty() {
                    value
                } else {
                    project(&value, &f.selection_set, fragments, variables)?
                };
                out.insert(alias, projected);
            }
            Ok(Json::Object(out))
        }
        scalar => Ok(scalar.clone()),
    }
}
