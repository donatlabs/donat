//! Donat Actions: custom GraphQL fields resolved by an HTTP webhook.
//!
//! A top-level action field maps to a webhook call. The engine POSTs
//! `{action: {name}, input: {<args>}, session_variables: {...}}` to the
//! action's handler, then shapes the JSON response to the action's output
//! object type and the field's selection set.
//!
//! Only synchronous actions are handled here (the sync core). Request/response
//! transforms, remote-join relationships from output objects, and async
//! actions are layered on later.

use axum::http::{HeaderMap, StatusCode};
use futures_util::future::BoxFuture;
use graphql_parser::query::{
    Definition, Document, Field, OperationDefinition, Selection, SelectionSet, Value as GqlValue,
};
use serde_json::{Map as JsonMap, Value as Json, json};

use donat_metadata::{ActionEntry, CustomTypeRelationship, CustomTypes, Metadata, QualifiedTable};
use donat_schema::Session;

use crate::remote::resolve_url_template;
use crate::state::SharedState;

fn is_session_header(name: &str) -> bool {
    name.starts_with("x-donat-") || name.starts_with("x-hasura-")
}

/// Cloned slice of metadata needed to resolve an action operation after the
/// engine read-lock is dropped.
pub struct ActionContext {
    actions: Vec<ActionEntry>,
    custom_types: CustomTypes,
    is_query: bool,
}

impl ActionContext {
    fn find(&self, name: &str) -> Option<&ActionEntry> {
        self.actions.iter().find(|a| a.name == name)
    }
}

/// Decide whether `doc`'s selected operation targets actions. Returns a cloned
/// [`ActionContext`] when at least one top-level field is an action, else
/// `None` (the operation falls through to normal table planning).
pub fn match_action(
    metadata: &Metadata,
    doc: &Document<'static, String>,
    operation_name: Option<&str>,
) -> Option<ActionContext> {
    if metadata.actions.is_empty() {
        return None;
    }
    let op = select_operation(doc, operation_name)?;
    let (set, is_query) = match op {
        OperationDefinition::Query(q) => (&q.selection_set, true),
        OperationDefinition::Mutation(m) => (&m.selection_set, false),
        OperationDefinition::SelectionSet(s) => (s, true),
        OperationDefinition::Subscription(_) => return None,
    };
    let any_action = set.items.iter().any(|item| {
        matches!(item, Selection::Field(f) if metadata.actions.iter().any(|a| a.name == f.name))
    });
    if !any_action {
        return None;
    }
    Some(ActionContext {
        actions: metadata.actions.clone(),
        custom_types: metadata.custom_types.clone(),
        is_query,
    })
}

/// Resolve every top-level action field by calling its webhook and shaping the
/// response. Returns a GraphQL HTTP response (`{data}` or `{errors}`).
pub async fn resolve(
    state: &SharedState,
    session: &Session,
    ctx: &ActionContext,
    doc: &Document<'static, String>,
    variables: &JsonMap<String, Json>,
    operation_name: Option<&str>,
    headers: &HeaderMap,
) -> (StatusCode, Json) {
    let Some(op) = select_operation(doc, operation_name) else {
        return err("$", "validation-failed", "no executable operation");
    };
    let set = match op {
        OperationDefinition::Query(q) => &q.selection_set,
        OperationDefinition::Mutation(m) => &m.selection_set,
        OperationDefinition::SelectionSet(s) => s,
        OperationDefinition::Subscription(_) => {
            return err("$", "validation-failed", "subscriptions are not supported");
        }
    };

    let mut data = JsonMap::new();
    for item in &set.items {
        let Selection::Field(field) = item else {
            return err(
                "$",
                "validation-failed",
                "fragments are not supported on actions",
            );
        };
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
        if field.name == "__typename" {
            data.insert(
                alias,
                Json::String(if ctx.is_query {
                    "query_root".into()
                } else {
                    "mutation_root".into()
                }),
            );
            continue;
        }
        let Some(action) = ctx.find(&field.name) else {
            return err(
                &format!("$.selectionSet.{}", field.name),
                "validation-failed",
                format!(
                    "field \"{}\" not found in type: '{}'",
                    field.name,
                    if ctx.is_query {
                        "query_root"
                    } else {
                        "mutation_root"
                    }
                ),
            );
        };

        // Permission: the role must be granted this action explicitly.
        if !action.permissions.iter().any(|p| p.role == session.role) {
            return err(
                &format!("$.selectionSet.{}", field.name),
                "validation-failed",
                format!(
                    "field \"{}\" not found in type: '{}'",
                    field.name,
                    if ctx.is_query {
                        "query_root"
                    } else {
                        "mutation_root"
                    }
                ),
            );
        }

        match call_action(
            state,
            session,
            action,
            field,
            variables,
            headers,
            &ctx.custom_types,
        )
        .await
        {
            Ok(value) => {
                data.insert(alias, value);
            }
            Err(resp) => return resp,
        }
    }

    (StatusCode::OK, json!({ "data": data }))
}

/// Build the webhook payload, POST it, and shape the response.
async fn call_action(
    state: &SharedState,
    session: &Session,
    action: &ActionEntry,
    field: &Field<'static, String>,
    variables: &JsonMap<String, Json>,
    headers: &HeaderMap,
    custom_types: &CustomTypes,
) -> Result<Json, (StatusCode, Json)> {
    let path = format!("$.selectionSet.{}", field.name);

    // Resolve the field arguments into the `input` object.
    let mut input = JsonMap::new();
    for (name, value) in &field.arguments {
        input.insert(name.clone(), value_to_json(value, variables));
    }

    // Session variables, as Donat passes them (lowercased).
    let mut session_vars = JsonMap::new();
    session_vars.insert("x-donat-role".into(), Json::String(session.role.clone()));
    session_vars.insert("x-hasura-role".into(), Json::String(session.role.clone()));
    for (k, v) in &session.vars {
        session_vars.insert(k.clone(), Json::String(v.clone()));
    }

    let payload = json!({
        "action": { "name": action.name },
        "input": input,
        "session_variables": session_vars,
    });

    let url = resolve_url_template(&action.definition.handler);
    let mut req = state.http.post(&url).json(&payload);
    if action.definition.forward_client_headers {
        for (name, value) in headers {
            let name = name.as_str();
            if is_session_header(name) || name == "authorization" {
                if let Ok(value) = value.to_str() {
                    req = req.header(name, value);
                }
            }
        }
    }

    let response = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return Err(err(
                &path,
                "unexpected",
                format!("http exception when calling webhook: {e}"),
            ));
        }
    };
    let status = response.status();
    let body: Json = response.json().await.unwrap_or(Json::Null);

    // A non-2xx handler response is an action error. Donat surfaces the
    // handler body's `message`, and for the error `extensions`:
    //   * if the body carries an `extensions` object, use it verbatim;
    //   * otherwise build `{ path, code }`, taking `code` from the body's
    //     top-level `code` field (default `unexpected`).
    if !status.is_success() {
        let message = body
            .get("message")
            .and_then(Json::as_str)
            .unwrap_or("webhook returned an error")
            .to_string();
        let extensions = match body.get("extensions") {
            Some(ext) if !ext.is_null() => ext.clone(),
            _ => {
                let code = body
                    .get("code")
                    .and_then(Json::as_str)
                    .unwrap_or("unexpected");
                json!({ "path": "$", "code": code })
            }
        };
        return Err((
            StatusCode::OK,
            json!({ "errors": [ { "extensions": extensions, "message": message } ] }),
        ));
    }

    let ty = parse_type(&action.definition.output_type);
    let mut shaped = match validate(custom_types, &ty, &body, &field.selection_set.items) {
        Ok(value) => value,
        // Output-shape errors are reported at the top level, like Donat.
        Err(message) => return Err(err("$", "unexpected", message)),
    };
    // Output objects may declare relationships to tracked tables; resolve them
    // by querying the target under the same session (so the role's permissions
    // apply), using the raw webhook row for the join values.
    fill_relationships(
        state,
        session,
        custom_types,
        &ty,
        &mut shaped,
        &body,
        &field.selection_set.items,
    )
    .await?;
    Ok(shaped)
}

/// Walk the shaped output alongside the raw webhook value, resolving any
/// selected output-object relationship into its tracked table.
fn fill_relationships<'a>(
    state: &'a SharedState,
    session: &'a Session,
    custom_types: &'a CustomTypes,
    ty: &'a TypeRef,
    shaped: &'a mut Json,
    raw: &'a Json,
    selection: &'a [Selection<'static, String>],
) -> BoxFuture<'a, Result<(), (StatusCode, Json)>> {
    Box::pin(async move {
        if shaped.is_null() {
            return Ok(());
        }
        match ty {
            TypeRef::List { inner, .. } => {
                if let (Json::Array(items), Json::Array(raws)) = (&mut *shaped, raw) {
                    for (item, raw_item) in items.iter_mut().zip(raws.iter()) {
                        fill_relationships(
                            state,
                            session,
                            custom_types,
                            inner,
                            item,
                            raw_item,
                            selection,
                        )
                        .await?;
                    }
                }
                Ok(())
            }
            TypeRef::Named { name, .. } => {
                let Some(obj) = custom_types.objects.iter().find(|o| &o.name == name) else {
                    return Ok(());
                };
                for item in selection {
                    let Selection::Field(field) = item else {
                        continue;
                    };
                    let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());

                    if let Some(rel) = obj.relationships.iter().find(|r| r.name == field.name) {
                        let resolved =
                            resolve_relationship(state, session, rel, raw, &field.selection_set)
                                .await?;
                        if let Some(map) = shaped.as_object_mut() {
                            map.insert(alias, resolved);
                        }
                        continue;
                    }

                    // A declared object/list field may itself contain
                    // relationships further down (e.g. NestedJoinObject.user_id).
                    if let Some(field_def) = obj.fields.iter().find(|f| f.name == field.name) {
                        let ftype = parse_type(&field_def.type_);
                        let raw_child = raw.get(&field.name).cloned().unwrap_or(Json::Null);
                        if let Some(child) = shaped.get_mut(&alias) {
                            fill_relationships(
                                state,
                                session,
                                custom_types,
                                &ftype,
                                child,
                                &raw_child,
                                &field.selection_set.items,
                            )
                            .await?;
                        }
                    }
                }
                Ok(())
            }
        }
    })
}

/// Resolve a single output-object relationship by querying its target table.
async fn resolve_relationship(
    state: &SharedState,
    session: &Session,
    rel: &CustomTypeRelationship,
    raw: &Json,
    selection: &SelectionSet<'static, String>,
) -> Result<Json, (StatusCode, Json)> {
    let base = table_base_name(&rel.remote_table);
    // Build `where: { <remote_col>: { _eq: <row value> }, ... }` from the
    // mapping (output-object field -> remote table column).
    let mut where_map = serde_json::Map::new();
    for (out_field, remote_col) in &rel.field_mapping {
        let value = raw.get(out_field).cloned().unwrap_or(Json::Null);
        where_map.insert(remote_col.clone(), json!({ "_eq": value }));
    }
    let is_array = rel.type_ == "array";
    let selset = render_selection(selection);
    let query = if is_array {
        format!("query($w: {base}_bool_exp){{ {base}(where: $w) {selset} }}")
    } else {
        format!("query($w: {base}_bool_exp){{ {base}(where: $w, limit: 1) {selset} }}")
    };
    let mut vars = JsonMap::new();
    vars.insert("w".into(), Json::Object(where_map));

    let data = crate::gql::execute_select_internal(state, session, &query, &vars)
        .await
        .map_err(|e| (StatusCode::OK, e))?;
    let rows = data.get(&base).cloned().unwrap_or(Json::Null);
    if is_array {
        Ok(rows)
    } else {
        Ok(rows
            .as_array()
            .and_then(|a| a.first().cloned())
            .unwrap_or(Json::Null))
    }
}

/// The GraphQL base name of a table: bare name for `public`, else
/// `<schema>_<name>` (Donat's default; custom names are not handled here).
fn table_base_name(table: &QualifiedTable) -> String {
    match table.schema() {
        "public" => table.name().to_string(),
        schema => format!("{schema}_{}", table.name()),
    }
}

/// Render a selection set back to GraphQL source for an internal query.
fn render_selection(set: &SelectionSet<'static, String>) -> String {
    format!("{set}")
}

/// A GraphQL type reference: a named type or a list, each optionally non-null.
#[derive(Debug, Clone)]
enum TypeRef {
    Named { name: String, non_null: bool },
    List { inner: Box<TypeRef>, non_null: bool },
}

impl TypeRef {
    fn non_null(&self) -> bool {
        match self {
            TypeRef::Named { non_null, .. } | TypeRef::List { non_null, .. } => *non_null,
        }
    }
}

/// Parse a GraphQL type reference such as `UserId`, `[String!]!`, `[[X]]`.
fn parse_type(s: &str) -> TypeRef {
    let t = s.trim();
    if let Some(stripped) = t.strip_suffix('!') {
        let inner = parse_type(stripped);
        return match inner {
            TypeRef::Named { name, .. } => TypeRef::Named {
                name,
                non_null: true,
            },
            TypeRef::List { inner, .. } => TypeRef::List {
                inner,
                non_null: true,
            },
        };
    }
    if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        return TypeRef::List {
            inner: Box::new(parse_type(inner)),
            non_null: false,
        };
    }
    TypeRef::Named {
        name: t.to_string(),
        non_null: false,
    }
}

/// Validate (and shape) a webhook value against an output type and selection
/// set, reproducing Donat's response-checking error messages.
fn validate(
    custom_types: &CustomTypes,
    ty: &TypeRef,
    value: &Json,
    selection: &[Selection<'static, String>],
) -> Result<Json, String> {
    if value.is_null() {
        return if ty.non_null() {
            Err("got null for the action webhook response".into())
        } else {
            Ok(Json::Null)
        };
    }

    match ty {
        TypeRef::List { inner, .. } => {
            let Json::Array(items) = value else {
                return Err("expecting array for the action webhook response".into());
            };
            let shaped = items
                .iter()
                .map(|item| validate(custom_types, inner, item, selection))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Json::Array(shaped))
        }
        TypeRef::Named { name, .. } => {
            if let Some(obj) = custom_types.objects.iter().find(|o| &o.name == name) {
                match value {
                    Json::Array(_) => Err(format!(
                        "got array for the action webhook response, expecting {name}"
                    )),
                    Json::Object(map) => project_object(custom_types, obj, map, selection),
                    other => Err(format!(
                        "got scalar {} for the action webhook response, expecting {name}",
                        scalar_kind(other)
                    )),
                }
            } else {
                // Scalar / enum / custom scalar.
                validate_scalar(name, value)
            }
        }
    }
}

/// Project an object value against its declared fields and the selection set.
fn project_object(
    custom_types: &CustomTypes,
    obj: &donat_metadata::ObjectType,
    value: &serde_json::Map<String, Json>,
    selection: &[Selection<'static, String>],
) -> Result<Json, String> {
    let mut out = JsonMap::new();
    for item in selection {
        let Selection::Field(field) = item else {
            continue;
        };
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
        if field.name == "__typename" {
            out.insert(alias, Json::String(obj.name.clone()));
            continue;
        }
        let Some(field_def) = obj.fields.iter().find(|f| f.name == field.name) else {
            // Relationships to tracked tables are resolved later (phase 3);
            // anything else passes through unshaped.
            out.insert(alias, value.get(&field.name).cloned().unwrap_or(Json::Null));
            continue;
        };
        let ftype = parse_type(&field_def.type_);
        let present = value.contains_key(&field.name);
        let raw = value.get(&field.name);
        let shaped = match raw {
            None => {
                if ftype.non_null() {
                    return Err(format!(
                        "field \"{}\" expected in webhook response, but not found",
                        field.name
                    ));
                }
                Json::Null
            }
            Some(Json::Null) => {
                let _ = present;
                if ftype.non_null() {
                    return Err(format!(
                        "expecting not null value for field \"{}\"",
                        field.name
                    ));
                }
                Json::Null
            }
            Some(v) => validate(custom_types, &ftype, v, &field.selection_set.items)?,
        };
        out.insert(alias, shaped);
    }
    Ok(Json::Object(out))
}

/// Built-in GraphQL scalars are type-checked; custom scalars (and `json`/
/// `jsonb`) accept any JSON value verbatim.
fn validate_scalar(name: &str, value: &Json) -> Result<Json, String> {
    let ok = match name {
        "String" => value.is_string(),
        "Int" => value.is_i64() || value.is_u64(),
        "Float" => value.is_number(),
        "Boolean" => value.is_boolean(),
        "ID" => value.is_string() || value.is_number(),
        // Custom scalar / json / enum: accept as-is.
        _ => return Ok(value.clone()),
    };
    if ok {
        return Ok(value.clone());
    }
    Err(match value {
        Json::Object(_) => format!("got object for the action webhook response, expecting {name}"),
        Json::Array(_) => format!("got array for the action webhook response, expecting {name}"),
        other => format!(
            "got scalar {} for the action webhook response, expecting {name}",
            scalar_kind(other)
        ),
    })
}

fn scalar_kind(value: &Json) -> &'static str {
    match value {
        Json::String(_) => "String",
        Json::Number(_) => "Number",
        Json::Bool(_) => "Boolean",
        _ => "Null",
    }
}

/// Resolve a GraphQL argument value to JSON, substituting variables.
fn value_to_json(value: &GqlValue<'static, String>, vars: &JsonMap<String, Json>) -> Json {
    match value {
        GqlValue::Variable(name) => vars.get(name).cloned().unwrap_or(Json::Null),
        GqlValue::Int(n) => Json::from(n.as_i64().unwrap_or_default()),
        GqlValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        GqlValue::String(s) => Json::String(s.clone()),
        GqlValue::Boolean(b) => Json::Bool(*b),
        GqlValue::Null => Json::Null,
        GqlValue::Enum(e) => Json::String(e.clone()),
        GqlValue::List(items) => {
            Json::Array(items.iter().map(|v| value_to_json(v, vars)).collect())
        }
        GqlValue::Object(map) => {
            let mut out = JsonMap::new();
            for (k, v) in map {
                out.insert(k.clone(), value_to_json(v, vars));
            }
            Json::Object(out)
        }
    }
}

/// Pick the operation to execute: the named one, or the sole operation.
fn select_operation<'d>(
    doc: &'d Document<'static, String>,
    operation_name: Option<&str>,
) -> Option<&'d OperationDefinition<'static, String>> {
    let ops: Vec<&OperationDefinition<'static, String>> = doc
        .definitions
        .iter()
        .filter_map(|d| match d {
            Definition::Operation(op) => Some(op),
            Definition::Fragment(_) => None,
        })
        .collect();
    match operation_name {
        Some(name) => ops.into_iter().find(|op| op_name(op) == Some(name)),
        None => {
            if ops.len() == 1 {
                Some(ops[0])
            } else {
                None
            }
        }
    }
}

fn op_name<'a>(op: &'a OperationDefinition<'static, String>) -> Option<&'a str> {
    match op {
        OperationDefinition::Query(q) => q.name.as_deref(),
        OperationDefinition::Mutation(m) => m.name.as_deref(),
        OperationDefinition::Subscription(s) => s.name.as_deref(),
        OperationDefinition::SelectionSet(_) => None,
    }
}

fn err(path: &str, code: &str, message: impl Into<String>) -> (StatusCode, Json) {
    (
        StatusCode::OK,
        json!({
            "errors": [ {
                "extensions": { "path": path, "code": code },
                "message": message.into(),
            } ]
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use donat_metadata::{CustomTypeField, ObjectType};

    fn out_object() -> CustomTypes {
        CustomTypes {
            objects: vec![ObjectType {
                name: "OutObject".into(),
                fields: vec![
                    CustomTypeField {
                        name: "id".into(),
                        type_: "ID!".into(),
                        description: None,
                    },
                    CustomTypeField {
                        name: "name".into(),
                        type_: "String".into(),
                        description: None,
                    },
                ],
                relationships: vec![],
                description: None,
            }],
            ..Default::default()
        }
    }

    /// Parse a query selection set for `{ id name }`.
    fn id_name_selection() -> Vec<Selection<'static, String>> {
        let doc = graphql_parser::parse_query::<String>("{ x { id name } }")
            .unwrap()
            .into_static();
        if let Definition::Operation(OperationDefinition::SelectionSet(s)) = &doc.definitions[0] {
            if let Selection::Field(f) = &s.items[0] {
                return f.selection_set.items.clone();
            }
        }
        unreachable!()
    }

    #[test]
    fn parses_type_wrappers() {
        assert!(matches!(
            parse_type("UserId"),
            TypeRef::Named {
                non_null: false,
                ..
            }
        ));
        assert!(matches!(
            parse_type("UserId!"),
            TypeRef::Named { non_null: true, .. }
        ));
        match parse_type("[String!]!") {
            TypeRef::List { inner, non_null } => {
                assert!(non_null);
                assert!(matches!(*inner, TypeRef::Named { non_null: true, .. }));
            }
            _ => panic!("expected list"),
        }
    }

    #[test]
    fn shapes_object_and_ignores_extra_fields() {
        let ct = out_object();
        let value = json!({ "id": "x", "name": "Alice", "extra": 1 });
        let out = validate(&ct, &parse_type("OutObject"), &value, &id_name_selection()).unwrap();
        assert_eq!(out, json!({ "id": "x", "name": "Alice" }));
    }

    #[test]
    fn null_for_non_null_output_errors() {
        let ct = out_object();
        let err = validate(&ct, &parse_type("OutObject!"), &Json::Null, &[]).unwrap_err();
        assert_eq!(err, "got null for the action webhook response");
    }

    #[test]
    fn array_for_object_output_errors() {
        let ct = out_object();
        let err = validate(
            &ct,
            &parse_type("OutObject"),
            &json!([]),
            &id_name_selection(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "got array for the action webhook response, expecting OutObject"
        );
    }

    #[test]
    fn scalar_for_object_output_errors() {
        let ct = out_object();
        let err = validate(
            &ct,
            &parse_type("OutObject"),
            &json!("s"),
            &id_name_selection(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "got scalar String for the action webhook response, expecting OutObject"
        );
    }

    #[test]
    fn missing_non_null_field_errors() {
        let ct = out_object();
        let err = validate(
            &ct,
            &parse_type("OutObject"),
            &json!({ "name": "A" }),
            &id_name_selection(),
        )
        .unwrap_err();
        assert_eq!(
            err,
            "field \"id\" expected in webhook response, but not found"
        );
    }

    #[test]
    fn null_non_null_field_errors() {
        let ct = out_object();
        let err = validate(
            &ct,
            &parse_type("OutObject"),
            &json!({ "id": null, "name": "A" }),
            &id_name_selection(),
        )
        .unwrap_err();
        assert_eq!(err, "expecting not null value for field \"id\"");
    }

    #[test]
    fn nullable_field_absent_becomes_null() {
        let ct = out_object();
        let out = validate(
            &ct,
            &parse_type("OutObject"),
            &json!({ "id": "x" }),
            &id_name_selection(),
        )
        .unwrap();
        assert_eq!(out, json!({ "id": "x", "name": null }));
    }

    #[test]
    fn non_array_for_list_output_errors() {
        let ct = out_object();
        let err = validate(
            &ct,
            &parse_type("[OutObject]"),
            &json!({}),
            &id_name_selection(),
        )
        .unwrap_err();
        assert_eq!(err, "expecting array for the action webhook response");
    }

    #[test]
    fn object_for_scalar_output_errors() {
        let ct = CustomTypes::default();
        let err = validate(&ct, &parse_type("String!"), &json!({ "a": 1 }), &[]).unwrap_err();
        assert_eq!(
            err,
            "got object for the action webhook response, expecting String"
        );
    }

    #[test]
    fn custom_scalar_passes_through_any_json() {
        let ct = CustomTypes::default();
        let out = validate(
            &ct,
            &parse_type("myCustomScalar!"),
            &json!({ "foo": "bar" }),
            &[],
        )
        .unwrap();
        assert_eq!(out, json!({ "foo": "bar" }));
    }

    #[test]
    fn list_element_null_for_non_null_errors() {
        let ct = CustomTypes::default();
        let err = validate(&ct, &parse_type("[String!]!"), &json!(["a", null]), &[]).unwrap_err();
        assert_eq!(err, "got null for the action webhook response");
    }

    #[test]
    fn table_base_name_handles_schema() {
        assert_eq!(
            table_base_name(&QualifiedTable::Name("user".into())),
            "user"
        );
        assert_eq!(
            table_base_name(&QualifiedTable::Qualified {
                schema: "app".into(),
                name: "orders".into()
            }),
            "app_orders"
        );
    }

    #[test]
    fn relationship_field_is_not_shaped_as_a_scalar() {
        // A selected relationship (absent from the object's `fields`) is left
        // as a null placeholder by the pure shaper; fill_relationships (async,
        // integration-tested) replaces it. It must not error here.
        let ct = CustomTypes {
            objects: vec![donat_metadata::ObjectType {
                name: "UserId".into(),
                fields: vec![CustomTypeField {
                    name: "id".into(),
                    type_: "Int!".into(),
                    description: None,
                }],
                relationships: vec![],
                description: None,
            }],
            ..Default::default()
        };
        let doc = graphql_parser::parse_query::<String>("{ x { id user { name } } }")
            .unwrap()
            .into_static();
        let sel = if let Definition::Operation(OperationDefinition::SelectionSet(s)) =
            &doc.definitions[0]
        {
            if let Selection::Field(f) = &s.items[0] {
                f.selection_set.items.clone()
            } else {
                unreachable!()
            }
        } else {
            unreachable!()
        };
        let out = validate(&ct, &parse_type("UserId"), &json!({ "id": 1 }), &sel).unwrap();
        assert_eq!(out, json!({ "id": 1, "user": null }));
    }
}
