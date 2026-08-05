//! Actions without a transport.
//!
//! An action is a custom GraphQL field the engine does not resolve from SQL.
//! Everything about it except *making the call* is pure: deciding that an
//! operation targets actions at all, checking the role may see one, binding the
//! field's arguments into the `input` object, and shaping whatever came back
//! against the declared output type and the caller's selection set.
//!
//! That pure half lives here so both hosts share it. `donat-server` calls a
//! webhook; an embedded host calls a function in its own process. If each
//! reimplemented the binding and shaping, the same declaration would answer
//! differently depending on where it ran, which is the one thing a portable
//! metadata format cannot afford.
//!
//! What stays with the caller: the call itself, and relationships from an
//! output object into tracked tables — those are SQL, and this crate has no
//! database.

use graphql_parser::query::{
    Definition, Document, Field, OperationDefinition, Selection, Value as GqlValue,
};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json};

use donat_metadata::{ActionEntry, CustomTypes, Metadata, action_visible_to_role};

/// A failure that has no HTTP in it. Callers map this onto their own transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionError {
    pub path: String,
    pub code: String,
    pub message: String,
}

impl ActionError {
    fn new(path: impl Into<String>, code: &str, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// Cloned slice of metadata needed to resolve an action operation after the
/// engine read-lock is dropped.
#[derive(Debug, Clone)]
pub struct ActionContext {
    actions: Vec<ActionEntry>,
    custom_types: CustomTypes,
    is_query: bool,
}

impl ActionContext {
    pub fn find(&self, name: &str) -> Option<&ActionEntry> {
        self.actions.iter().find(|a| a.name == name)
    }

    pub fn is_query(&self) -> bool {
        self.is_query
    }

    pub fn custom_types(&self) -> &CustomTypes {
        &self.custom_types
    }

    /// The root type name errors report, which differs between a query and a
    /// mutation and is quoted verbatim in the v1 error text.
    fn root_type(&self) -> &'static str {
        if self.is_query {
            "query_root"
        } else {
            "mutation_root"
        }
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

/// One resolved top-level field of an action operation.
///
/// `__typename` is answered from the schema and never reaches a handler, so it
/// is a separate variant rather than a call nobody should make.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionItem {
    Typename { alias: String, value: String },
    Call(ActionCall),
}

/// Everything a caller needs to invoke one action, and nothing about how.
///
/// `handler` absent means the action is resolved in-process by a function the
/// host registered under `name`; see the field's documentation on
/// `ActionDefinition`.
#[derive(Debug, Clone, Serialize)]
pub struct ActionCall {
    /// Response key: the field's alias, or its name when unaliased.
    pub alias: String,
    pub name: String,
    pub input: JsonMap<String, Json>,
    pub session_variables: JsonMap<String, Json>,
    pub handler: Option<String>,
    /// Seconds the caller should allow the handler, when the action says so.
    pub timeout: Option<u64>,
    pub forward_client_headers: bool,
}

/// Resolve one top-level selection of an action operation.
///
/// The role check is here rather than in the caller because a role that may not
/// see an action must be told the field does not exist, not that it is
/// forbidden — the same answer an unknown field gets, so the schema does not
/// leak through permission errors.
pub fn plan_item(
    ctx: &ActionContext,
    role: &str,
    session_vars: &std::collections::HashMap<String, String>,
    item: &Selection<'static, String>,
    variables: &JsonMap<String, Json>,
) -> Result<ActionItem, ActionError> {
    let Selection::Field(field) = item else {
        return Err(ActionError::new(
            "$",
            "validation-failed",
            "fragments are not supported on actions",
        ));
    };
    let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
    if field.name == "__typename" {
        return Ok(ActionItem::Typename {
            alias,
            value: ctx.root_type().to_string(),
        });
    }
    let Some(action) = ctx.find(&field.name) else {
        return Err(field_not_found(ctx, &field.name));
    };
    if !action_visible_to_role(action, role) {
        return Err(field_not_found(ctx, &field.name));
    }

    Ok(ActionItem::Call(ActionCall {
        alias,
        name: action.name.clone(),
        input: bind_arguments(field, variables),
        session_variables: session_variables(role, session_vars),
        handler: action.definition.handler.clone(),
        timeout: action.definition.timeout,
        forward_client_headers: action.definition.forward_client_headers,
    }))
}

/// Resolve the field arguments into the `input` object.
pub fn bind_arguments(
    field: &Field<'static, String>,
    variables: &JsonMap<String, Json>,
) -> JsonMap<String, Json> {
    let mut input = JsonMap::new();
    for (name, value) in &field.arguments {
        input.insert(name.clone(), value_to_json(value, variables));
    }
    input
}

/// Session variables, as Donat passes them (lowercased).
pub fn session_variables(
    role: &str,
    vars: &std::collections::HashMap<String, String>,
) -> JsonMap<String, Json> {
    let mut out = JsonMap::new();
    out.insert("x-donat-role".into(), Json::String(role.to_string()));
    out.insert("x-hasura-role".into(), Json::String(role.to_string()));
    for (k, v) in vars {
        out.insert(k.clone(), Json::String(v.clone()));
    }
    out
}

/// The error a field the role cannot see gets, which is the same one an
/// unknown field gets.
pub fn field_not_found(ctx: &ActionContext, field_name: &str) -> ActionError {
    ActionError::new(
        format!("$.selectionSet.{field_name}"),
        "validation-failed",
        format!(
            "field \"{field_name}\" not found in type: '{}'",
            ctx.root_type()
        ),
    )
}

pub fn is_session_header(name: &str) -> bool {
    name.starts_with("x-donat-") || name.starts_with("x-hasura-")
}

/// A GraphQL type reference: a named type or a list, each optionally non-null.
#[derive(Debug, Clone)]
pub enum TypeRef {
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
pub fn parse_type(s: &str) -> TypeRef {
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

/// Validate (and shape) a handler value against an output type and selection
/// set, reproducing Donat's response-checking error messages.
pub fn validate(
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
pub fn project_object(
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
            // Relationships to tracked tables are resolved by the caller, which
            // is the only party with a database; anything else passes through
            // unshaped.
            out.insert(alias, value.get(&field.name).cloned().unwrap_or(Json::Null));
            continue;
        };
        let ftype = parse_type(&field_def.type_);
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
pub fn value_to_json(value: &GqlValue<'static, String>, vars: &JsonMap<String, Json>) -> Json {
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
pub fn select_operation<'d>(
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

/// The top-level selection set of an action operation, for a caller that needs
/// to walk the fields itself.
pub fn selection_items<'d>(
    doc: &'d Document<'static, String>,
    operation_name: Option<&str>,
) -> Result<&'d [Selection<'static, String>], ActionError> {
    let Some(op) = select_operation(doc, operation_name) else {
        return Err(ActionError::new(
            "$",
            "validation-failed",
            "no executable operation",
        ));
    };
    match op {
        OperationDefinition::Query(q) => Ok(&q.selection_set.items),
        OperationDefinition::Mutation(m) => Ok(&m.selection_set.items),
        OperationDefinition::SelectionSet(s) => Ok(&s.items),
        OperationDefinition::Subscription(_) => Err(ActionError::new(
            "$",
            "validation-failed",
            "subscriptions are not supported",
        )),
    }
}

/// The actions a host that can only call webhooks cannot serve.
///
/// An absent handler means "resolved in-process by the embedding host". Which
/// host is serving decides whether that is satisfiable, so the check belongs to
/// the host rather than to the metadata format.
pub fn actions_without_a_handler(metadata: &Metadata) -> Vec<&str> {
    metadata
        .actions
        .iter()
        .filter(|action| action.definition.handler.is_none())
        .map(|action| action.name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata_with(actions: Json) -> Metadata {
        serde_json::from_value(json!({ "version": 3, "actions": actions }))
            .expect("metadata deserializes")
    }

    /// An action with a `handler` is what every existing exported metadata
    /// carries. Making the field optional widens what deserializes; it must not
    /// change what a v2 export means.
    #[test]
    fn an_action_with_a_handler_is_servable_by_a_webhook_host() {
        let metadata = metadata_with(json!([{
            "name": "send_email",
            "definition": { "handler": "https://example.test/hook" }
        }]));
        assert!(actions_without_a_handler(&metadata).is_empty());
    }

    /// Reported by name, because the operator's next move is a decision per
    /// action: give it a handler, or serve the metadata from an embedded host.
    #[test]
    fn an_action_without_a_handler_is_reported_by_name() {
        let metadata = metadata_with(json!([
            { "name": "render_pdf", "definition": {} },
            { "name": "send_email", "definition": { "handler": "https://example.test/hook" } }
        ]));
        assert_eq!(actions_without_a_handler(&metadata), vec!["render_pdf"]);
    }

    /// A handler-less action must still route *as an action*. If it fell
    /// through to table planning the request would fail as an unknown field,
    /// and the operator would never see the real cause.
    #[test]
    fn a_handler_less_action_still_routes_as_an_action() {
        let metadata = metadata_with(json!([{ "name": "render_pdf", "definition": {} }]));
        let doc = graphql_parser::parse_query::<String>("mutation { render_pdf }")
            .expect("query parses")
            .into_static();
        let ctx = match_action(&metadata, &doc, None).expect("the operation is an action");
        assert!(ctx.find("render_pdf").is_some());
        assert!(!ctx.is_query(), "a mutation operation is not a query");
    }

    /// The call a host has to make, with the handler carried through so the
    /// host — not this crate — decides how to reach it.
    #[test]
    fn planning_binds_arguments_and_session_variables() {
        let metadata = metadata_with(json!([{
            "name": "render_pdf",
            "definition": { "arguments": [{ "name": "invoice_id", "type": "uuid!" }] }
        }]));
        let doc =
            graphql_parser::parse_query::<String>(r#"mutation { render_pdf(invoice_id: "abc") }"#)
                .expect("query parses")
                .into_static();
        let ctx = match_action(&metadata, &doc, None).expect("an action operation");
        let items = selection_items(&doc, None).expect("a selection set");
        let session =
            std::collections::HashMap::from([("x-donat-user-id".to_string(), "7".to_string())]);

        match plan_item(&ctx, "user", &session, &items[0], &JsonMap::new())
            .expect("the action plans")
        {
            ActionItem::Call(call) => {
                assert_eq!(call.name, "render_pdf");
                assert_eq!(call.alias, "render_pdf");
                assert_eq!(call.input["invoice_id"], json!("abc"));
                assert_eq!(call.session_variables["x-donat-role"], json!("user"));
                assert_eq!(call.session_variables["x-donat-user-id"], json!("7"));
                assert!(
                    call.handler.is_none(),
                    "an in-process action has no handler"
                );
            }
            other => panic!("expected a call, got {other:?}"),
        }
    }

    /// A role that may not see an action is told the field does not exist —
    /// the same answer an unknown field gets, so permissions cannot be used to
    /// enumerate the schema.
    #[test]
    fn an_action_a_role_cannot_see_is_reported_as_an_unknown_field() {
        let metadata = metadata_with(json!([{
            "name": "render_pdf",
            "definition": {},
            "permissions": [{ "role": "admin_ops" }]
        }]));
        let doc = graphql_parser::parse_query::<String>("mutation { render_pdf }")
            .expect("query parses")
            .into_static();
        let ctx = match_action(&metadata, &doc, None).expect("an action operation");
        let items = selection_items(&doc, None).expect("a selection set");

        let err = plan_item(
            &ctx,
            "user",
            &std::collections::HashMap::new(),
            &items[0],
            &JsonMap::new(),
        )
        .expect_err("a role outside the permission list must be refused");
        assert_eq!(err.code, "validation-failed");
        assert!(
            err.message.contains("not found in type: 'mutation_root'"),
            "the refusal must not disclose that the action exists: {err:?}"
        );
    }
}
