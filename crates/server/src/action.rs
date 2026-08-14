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
use futures_util::stream::StreamExt;
use graphql_parser::query::{Document, Field, OperationDefinition, Selection, SelectionSet};
use serde_json::{Map as JsonMap, Value as Json, json};

use donat_action::{
    ActionContext, ActionError, TypeRef, is_session_header, parse_type, select_operation, validate,
    value_to_json,
};
pub use donat_action::{actions_without_a_handler, match_action};
use donat_metadata::{
    ActionEntry, CustomTypeRelationship, CustomTypes, QualifiedTable, action_visible_to_role,
};
use donat_schema::Session;

use crate::remote::resolve_url_template;
use crate::state::{Engine, EngineSnapshot, SharedState};

const MAX_CONCURRENT_ACTION_RELATIONSHIP_GROUPS: usize = 4;

pub(crate) struct ActionRequest<'a> {
    pub(crate) doc: &'a Document<'static, String>,
    pub(crate) variables: &'a JsonMap<String, Json>,
    pub(crate) operation_name: Option<&'a str>,
    pub(crate) headers: &'a HeaderMap,
}

/// Resolve every top-level action field by calling its webhook and shaping the
/// response. Returns a GraphQL HTTP response (`{data}` or `{errors}`).
pub(crate) async fn resolve(
    state: &SharedState,
    engine: EngineSnapshot,
    session: &Session,
    ctx: &ActionContext,
    request: ActionRequest<'_>,
) -> (StatusCode, Json) {
    let ActionRequest {
        doc,
        variables,
        operation_name,
        headers,
    } = request;
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

    let execute_item = |item| {
        resolve_action_item(
            state,
            engine.as_ref(),
            session,
            ctx,
            item,
            variables,
            headers,
        )
    };
    let results =
        schedule_action_items(ctx.is_query(), set.items.iter().map(execute_item).collect()).await;
    let mut data = JsonMap::new();
    for result in results {
        match result {
            Ok((alias, value)) => {
                data.insert(alias, value);
            }
            Err(response) => return response,
        }
    }

    (StatusCode::OK, json!({ "data": data }))
}

async fn schedule_action_items<F, T>(is_query: bool, futures: Vec<F>) -> Vec<T>
where
    F: std::future::Future<Output = T>,
{
    if is_query {
        futures_util::future::join_all(futures).await
    } else {
        let mut results = Vec::with_capacity(futures.len());
        for future in futures {
            results.push(future.await);
        }
        results
    }
}

async fn resolve_action_item(
    state: &SharedState,
    engine: &Engine,
    session: &Session,
    ctx: &ActionContext,
    item: &Selection<'static, String>,
    variables: &JsonMap<String, Json>,
    headers: &HeaderMap,
) -> Result<(String, Json), (StatusCode, Json)> {
    let Selection::Field(field) = item else {
        return Err(err(
            "$",
            "validation-failed",
            "fragments are not supported on actions",
        ));
    };
    let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
    if field.name == "__typename" {
        return Ok((
            alias,
            Json::String(if ctx.is_query() {
                "query_root".into()
            } else {
                "mutation_root".into()
            }),
        ));
    }
    let Some(action) = ctx.find(&field.name) else {
        return Err(action_field_not_found(ctx, field));
    };
    if !action_visible_to_role(action, &session.role) {
        return Err(action_field_not_found(ctx, field));
    }
    let value = call_action(ActionInvocation {
        state,
        engine,
        session,
        action,
        field,
        variables,
        headers,
        custom_types: ctx.custom_types(),
    })
    .await?;
    Ok((alias, value))
}

/// Build the webhook payload, POST it, and shape the response.
struct ActionInvocation<'a> {
    state: &'a SharedState,
    engine: &'a Engine,
    session: &'a Session,
    action: &'a ActionEntry,
    field: &'a Field<'static, String>,
    variables: &'a JsonMap<String, Json>,
    headers: &'a HeaderMap,
    custom_types: &'a CustomTypes,
}

/// Who a call is attributed to in this engine's journal.
///
/// The role is always known — a request without one never reaches an action.
/// The user is whatever the deployment's `claims_map` put in
/// `x-donat-user-id`, and a deployment that maps nothing there gets `unknown`
/// rather than an empty field, because a blank in a log is read as a bug in
/// the log.
struct Caller {
    role: String,
    user: String,
}

impl Caller {
    fn of(session: &Session) -> Self {
        Self {
            role: session.role.clone(),
            user: session
                .var("x-donat-user-id")
                .filter(|user| !user.is_empty())
                .unwrap_or("unknown")
                .to_string(),
        }
    }
}

async fn call_action(invocation: ActionInvocation<'_>) -> Result<Json, (StatusCode, Json)> {
    let ActionInvocation {
        state,
        engine,
        session,
        action,
        field,
        variables,
        headers,
        custom_types,
    } = invocation;
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
    let caller = Caller::of(session);

    // A handler-less action is resolved in-process by an embedded host. This
    // binary has no such registry, so there is nothing to call. `main` refuses
    // the declaration at boot; this is the guard for a snapshot that reached a
    // request some other way.
    let Some(handler) = action.definition.handler.as_deref() else {
        return Err(err(
            &path,
            "unexpected",
            format!(
                "action '{}' declares no handler, so it runs only in an embedded host \
                 that registers a function for it",
                action.name
            ),
        ));
    };
    let base_url = resolve_url_template(handler);
    // The request as it would be sent with no transform: the Donat shape.
    let mut outgoing = crate::transform::Outgoing::donat(&base_url, &payload);
    if let Some(transform) = &action.definition.request_transform {
        let context = crate::transform::context(&base_url, &payload, &session_vars);
        if let Err(message) = crate::transform::apply(&mut outgoing, transform, &context) {
            return Err(err(&path, "unexpected", message));
        }
    }
    let mut req = outgoing.into_request(&state.http);
    // Headers the action declares. `value_from_env` is read here rather than
    // held in the snapshot, so a credential lives in the process environment
    // and never in metadata — which is what lets an action stand in front of
    // an API whose key no browser may ever see.
    for (name, value) in crate::cron::resolve_headers(&action.definition.headers) {
        req = req.header(name, value);
    }
    if let Some(seconds) = action.definition.timeout {
        req = req.timeout(std::time::Duration::from_secs(seconds));
    }
    if action.definition.forward_client_headers {
        for (name, value) in headers {
            let name = name.as_str();
            if (is_session_header(name) || name == "authorization")
                && let Ok(value) = value.to_str()
            {
                req = req.header(name, value);
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
    // Who did this, in this engine's own journal.
    //
    // An action reaches something outside — an identity provider, a payment
    // API — with a credential the deployment gave it, so what that thing
    // records is "the engine", not the person who pressed the button. Nobody
    // else is in a position to say: the session is known here and nowhere
    // downstream. Written for every action rather than for the ones that
    // happen to change something, because a read of somebody's account is
    // worth the same line as a write to it.
    tracing::info!(
        target: "donat::action",
        action = %action.name,
        role = %caller.role,
        user = %caller.user,
        status = status.as_u16(),
        "action called"
    );
    // A handler that streams more than the deployment allows is refused here
    // rather than held in memory until the allocator gives up.
    let body: Json =
        match crate::upstream::read_json(response, crate::upstream::max_body_bytes()).await {
            Ok(body) => body,
            Err(error) => {
                return Err(err(
                    &path,
                    "unexpected",
                    format!("http exception when calling webhook: {error}"),
                ));
            }
        };

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

    // A response transform runs before the answer is shaped, because it is
    // what makes the answer shapeable: the handler's fields become the ones
    // `output_type` promises.
    let body = match &action.definition.response_transform {
        None => body,
        Some(transform) => {
            match crate::transform::apply_response(transform, &body, &session_vars) {
                Ok(body) => body,
                Err(message) => return Err(err(&path, "unexpected", message)),
            }
        }
    };

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
        ActionRelationshipContext {
            engine,
            session,
            custom_types,
        },
        &ty,
        &mut shaped,
        &body,
        &field.selection_set.items,
    )
    .await?;
    Ok(shaped)
}

trait ActionRelationshipExecutor: Sync {
    fn execute<'a>(
        &'a self,
        engine: &'a Engine,
        session: &'a Session,
        query: &'a str,
        variables: &'a JsonMap<String, Json>,
    ) -> BoxFuture<'a, Result<Json, Json>>;
}

struct StateActionRelationshipExecutor<'a> {
    state: &'a SharedState,
}

#[derive(Clone, Copy)]
struct ActionRelationshipContext<'a> {
    engine: &'a Engine,
    session: &'a Session,
    custom_types: &'a CustomTypes,
}

impl ActionRelationshipExecutor for StateActionRelationshipExecutor<'_> {
    fn execute<'a>(
        &'a self,
        engine: &'a Engine,
        session: &'a Session,
        query: &'a str,
        variables: &'a JsonMap<String, Json>,
    ) -> BoxFuture<'a, Result<Json, Json>> {
        Box::pin(crate::gql::execute_select_internal(
            self.state, engine, session, query, variables,
        ))
    }
}

/// Walk the shaped output alongside the raw webhook value, resolving any
/// selected output-object relationship into its tracked table.
fn fill_relationships<'a>(
    state: &'a SharedState,
    context: ActionRelationshipContext<'a>,
    ty: &'a TypeRef,
    shaped: &'a mut Json,
    raw: &'a Json,
    selection: &'a [Selection<'static, String>],
) -> BoxFuture<'a, Result<(), (StatusCode, Json)>> {
    Box::pin(async move {
        let executor = StateActionRelationshipExecutor { state };
        fill_relationships_with(&executor, context, ty, shaped, raw, selection).await
    })
}

struct ActionRelationshipEntry {
    object_pointer: String,
    filter: JsonMap<String, Json>,
}

struct ActionRelationshipGroup<'a> {
    relationship: &'a CustomTypeRelationship,
    selection: &'a SelectionSet<'static, String>,
    selection_path: String,
    field_alias: String,
    entries: Vec<ActionRelationshipEntry>,
}

#[derive(Clone, Copy)]
struct ActionRelationshipLocation<'a> {
    object_pointer: &'a str,
    selection_path: &'a str,
}

fn action_pointer_child(base: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{base}/{escaped}")
}

fn relationship_filter(rel: &CustomTypeRelationship, raw: &Json) -> JsonMap<String, Json> {
    rel.field_mapping
        .iter()
        .map(|(output_field, remote_column)| {
            (
                remote_column.clone(),
                json!({
                    "_eq": raw.get(output_field).cloned().unwrap_or(Json::Null)
                }),
            )
        })
        .collect()
}

fn collect_action_relationship_groups<'a>(
    custom_types: &'a CustomTypes,
    ty: &TypeRef,
    shaped: &Json,
    raw: &Json,
    selection: &'a [Selection<'static, String>],
    location: ActionRelationshipLocation<'_>,
    groups: &mut Vec<ActionRelationshipGroup<'a>>,
) {
    if shaped.is_null() {
        return;
    }
    match ty {
        TypeRef::List { inner, .. } => {
            if let (Json::Array(items), Json::Array(raws)) = (shaped, raw) {
                for (index, (item, raw_item)) in items.iter().zip(raws).enumerate() {
                    collect_action_relationship_groups(
                        custom_types,
                        inner,
                        item,
                        raw_item,
                        selection,
                        ActionRelationshipLocation {
                            object_pointer: &action_pointer_child(
                                location.object_pointer,
                                &index.to_string(),
                            ),
                            selection_path: location.selection_path,
                        },
                        groups,
                    );
                }
            }
        }
        TypeRef::Named { name, .. } => {
            let Some(object_type) = custom_types
                .objects
                .iter()
                .find(|object| &object.name == name)
            else {
                return;
            };
            for item in selection {
                let Selection::Field(field) = item else {
                    continue;
                };
                let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
                let field_path = format!("{}.{alias}", location.selection_path);
                if let Some(relationship) = object_type
                    .relationships
                    .iter()
                    .find(|relationship| relationship.name == field.name)
                {
                    let entry = ActionRelationshipEntry {
                        object_pointer: location.object_pointer.to_string(),
                        filter: relationship_filter(relationship, raw),
                    };
                    if let Some(group) = groups.iter_mut().find(|group| {
                        group.selection_path == field_path
                            && group.relationship.name == relationship.name
                            && group.relationship.remote_table == relationship.remote_table
                    }) {
                        group.entries.push(entry);
                    } else {
                        groups.push(ActionRelationshipGroup {
                            relationship,
                            selection: &field.selection_set,
                            selection_path: field_path,
                            field_alias: alias,
                            entries: vec![entry],
                        });
                    }
                    continue;
                }

                if let Some(field_definition) = object_type
                    .fields
                    .iter()
                    .find(|definition| definition.name == field.name)
                {
                    let field_type = parse_type(&field_definition.type_);
                    let raw_child = raw.get(&field.name).unwrap_or(&Json::Null);
                    if let Some(shaped_child) = shaped.get(&alias) {
                        collect_action_relationship_groups(
                            custom_types,
                            &field_type,
                            shaped_child,
                            raw_child,
                            &field.selection_set.items,
                            ActionRelationshipLocation {
                                object_pointer: &action_pointer_child(
                                    location.object_pointer,
                                    &alias,
                                ),
                                selection_path: &field_path,
                            },
                            groups,
                        );
                    }
                }
            }
        }
    }
}

struct ActionRelationshipBatch {
    query: String,
    variables: JsonMap<String, Json>,
    entry_to_unique: Vec<usize>,
    unique_filters: Vec<JsonMap<String, Json>>,
}

fn build_action_relationship_batch(group: &ActionRelationshipGroup<'_>) -> ActionRelationshipBatch {
    let mut unique_filters: Vec<JsonMap<String, Json>> = vec![];
    let mut unique_indexes = std::collections::HashMap::<String, usize>::new();
    let mut entry_to_unique = Vec::with_capacity(group.entries.len());
    for entry in &group.entries {
        let key = serde_json::to_string(&entry.filter)
            .expect("action relationship filters always serialize");
        if let Some(index) = unique_indexes.get(&key).copied() {
            entry_to_unique.push(index);
        } else {
            let index = unique_filters.len();
            unique_indexes.insert(key, index);
            entry_to_unique.push(index);
            unique_filters.push(entry.filter.clone());
        }
    }

    let base = table_base_name(&group.relationship.remote_table);
    let selection = render_selection(group.selection);
    let mut definitions = Vec::with_capacity(unique_filters.len());
    let mut roots = Vec::with_capacity(unique_filters.len());
    let mut variables = JsonMap::new();
    for (index, filter) in unique_filters.iter().enumerate() {
        let variable = format!("__donat_action_rel_w_{index}");
        let alias = format!("__donat_action_rel_{index}");
        definitions.push(format!("${variable}: {base}_bool_exp"));
        let limit = if group.relationship.type_ != "array" {
            ", limit: 1"
        } else {
            ""
        };
        roots.push(format!(
            "{alias}: {base}(where: ${variable}{limit}) {selection}"
        ));
        variables.insert(variable, Json::Object(filter.clone()));
    }
    ActionRelationshipBatch {
        query: format!(
            "query({}) {{ {} }}",
            definitions.join(", "),
            roots.join(" ")
        ),
        variables,
        entry_to_unique,
        unique_filters,
    }
}

async fn execute_action_relationship_group<E: ActionRelationshipExecutor + ?Sized>(
    executor: &E,
    engine: &Engine,
    session: &Session,
    group: &ActionRelationshipGroup<'_>,
) -> Result<Vec<Json>, (StatusCode, Json)> {
    let batch = build_action_relationship_batch(group);
    let is_array = group.relationship.type_ == "array";
    let shape_rows = |rows: Json| {
        if is_array {
            rows
        } else {
            rows.as_array()
                .and_then(|items| items.first().cloned())
                .unwrap_or(Json::Null)
        }
    };
    let unique_values = match executor
        .execute(engine, session, &batch.query, &batch.variables)
        .await
    {
        Ok(data) => (0..batch.unique_filters.len())
            .map(|index| {
                shape_rows(
                    data.get(format!("__donat_action_rel_{index}"))
                        .cloned()
                        .unwrap_or(Json::Null),
                )
            })
            .collect(),
        Err(_) => {
            // Recover the pre-batching error shape/path deterministically.
            let base = table_base_name(&group.relationship.remote_table);
            let selection = render_selection(group.selection);
            let limit = if is_array { "" } else { ", limit: 1" };
            let query =
                format!("query($w: {base}_bool_exp) {{ {base}(where: $w{limit}) {selection} }}");
            let mut values = Vec::with_capacity(batch.unique_filters.len());
            for filter in &batch.unique_filters {
                let variables = JsonMap::from_iter([("w".into(), Json::Object(filter.clone()))]);
                let data = executor
                    .execute(engine, session, &query, &variables)
                    .await
                    .map_err(|error| (StatusCode::OK, error))?;
                values.push(shape_rows(data.get(&base).cloned().unwrap_or(Json::Null)));
            }
            values
        }
    };
    Ok(batch
        .entry_to_unique
        .into_iter()
        .map(|index| unique_values[index].clone())
        .collect())
}

fn fill_relationships_with<'a, E: ActionRelationshipExecutor + ?Sized>(
    executor: &'a E,
    context: ActionRelationshipContext<'a>,
    ty: &'a TypeRef,
    shaped: &'a mut Json,
    raw: &'a Json,
    selection: &'a [Selection<'static, String>],
) -> BoxFuture<'a, Result<(), (StatusCode, Json)>> {
    Box::pin(async move {
        let mut groups = vec![];
        collect_action_relationship_groups(
            context.custom_types,
            ty,
            shaped,
            raw,
            selection,
            ActionRelationshipLocation {
                object_pointer: "",
                selection_path: "$",
            },
            &mut groups,
        );
        // Groups target independent relationship selections. Bound their
        // internal reads to avoid serial latency without turning a wide action
        // response into an unbounded burst. Results are reapplied in client
        // order so an error keeps the sequential implementation's observable
        // precedence.
        let work = groups
            .iter()
            .enumerate()
            .map(|(index, group)| async move {
                (
                    index,
                    execute_action_relationship_group(
                        executor,
                        context.engine,
                        context.session,
                        group,
                    )
                    .await,
                )
            })
            .collect::<Vec<_>>();
        let mut results = futures_util::stream::iter(work)
            .buffer_unordered(MAX_CONCURRENT_ACTION_RELATIONSHIP_GROUPS)
            .collect::<Vec<_>>()
            .await;
        results.sort_by_key(|(index, _)| *index);

        for (group, (_, result)) in groups.iter().zip(results) {
            let values = result?;
            for (entry, value) in group.entries.iter().zip(values) {
                let Some(Json::Object(object)) = shaped.pointer_mut(&entry.object_pointer) else {
                    continue;
                };
                object.insert(group.field_alias.clone(), value);
            }
        }
        Ok(())
    })
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

/// Map a transport-free [`ActionError`] onto this server's HTTP response.
fn action_err(e: ActionError) -> (StatusCode, Json) {
    err(&e.path, &e.code, e.message)
}

fn action_field_not_found(
    ctx: &ActionContext,
    field: &Field<'static, String>,
) -> (StatusCode, Json) {
    action_err(donat_action::field_not_found(ctx, &field.name))
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

    #[test]
    fn a_call_is_attributed_to_the_session_that_made_it() {
        let mut vars = std::collections::HashMap::new();
        vars.insert("x-donat-user-id".to_string(), "operator-7".to_string());
        let session = Session {
            role: "support".to_string(),
            vars,
            backend_request: false,
        };

        let caller = Caller::of(&session);
        assert_eq!(caller.role, "support");
        assert_eq!(caller.user, "operator-7");
    }

    #[test]
    fn a_deployment_that_maps_no_user_says_so_rather_than_leaving_a_blank() {
        let mut vars = std::collections::HashMap::new();
        // What `claims_map`'s `default: ""` produces for a token with no
        // subject to map — present, and empty.
        vars.insert("x-donat-user-id".to_string(), String::new());
        let session = Session {
            role: "support".to_string(),
            vars,
            backend_request: false,
        };

        assert_eq!(Caller::of(&session).user, "unknown");
        assert_eq!(
            Caller::of(&Session {
                role: "support".to_string(),
                vars: std::collections::HashMap::new(),
                backend_request: false,
            })
            .user,
            "unknown"
        );
    }
    use graphql_parser::query::Definition;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use donat_metadata::{CustomTypeField, ObjectType};

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

    struct RecordingRelationshipExecutor {
        calls: AtomicUsize,
    }

    impl ActionRelationshipExecutor for RecordingRelationshipExecutor {
        fn execute<'a>(
            &'a self,
            _engine: &'a Engine,
            _session: &'a Session,
            _query: &'a str,
            variables: &'a JsonMap<String, Json>,
        ) -> BoxFuture<'a, Result<Json, Json>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let mut data = JsonMap::new();
                for (name, value) in variables {
                    let Some(index) = name.strip_prefix("__donat_action_rel_w_") else {
                        continue;
                    };
                    let id = value.pointer("/id/_eq").cloned().unwrap_or(Json::Null);
                    data.insert(format!("__donat_action_rel_{index}"), json!([{ "id": id }]));
                }
                if data.is_empty() {
                    let id = variables
                        .get("w")
                        .and_then(|value| value.pointer("/id/_eq"))
                        .cloned()
                        .unwrap_or(Json::Null);
                    data.insert("user".into(), json!([{ "id": id }]));
                }
                Ok(Json::Object(data))
            })
        }
    }

    #[tokio::test]
    async fn list_action_relationships_execute_one_batched_query() {
        use std::collections::BTreeMap;

        let custom_types = CustomTypes {
            objects: vec![ObjectType {
                name: "Out".into(),
                fields: vec![CustomTypeField {
                    name: "id".into(),
                    type_: "Int!".into(),
                    description: None,
                }],
                relationships: vec![CustomTypeRelationship {
                    name: "user".into(),
                    type_: "object".into(),
                    remote_table: QualifiedTable::Name("user".into()),
                    field_mapping: BTreeMap::from([("id".into(), "id".into())]),
                }],
                description: None,
            }],
            ..Default::default()
        };
        let doc = graphql_parser::parse_query::<String>("{ lookup { id user { id } } }")
            .unwrap()
            .into_static();
        let selection = match &doc.definitions[0] {
            Definition::Operation(OperationDefinition::SelectionSet(root)) => {
                let Selection::Field(action) = &root.items[0] else {
                    unreachable!()
                };
                action.selection_set.items.as_slice()
            }
            _ => unreachable!(),
        };
        let mut shaped = json!([
            { "id": 7, "user": null },
            { "id": 7, "user": null },
            { "id": 8, "user": null }
        ]);
        let raw = shaped.clone();
        let executor = RecordingRelationshipExecutor {
            calls: AtomicUsize::new(0),
        };
        let engine = Engine::bootstrap(
            serde_json::from_value(json!({ "version": 3, "sources": [] })).unwrap(),
        );
        let session = Session {
            role: "user".into(),
            vars: Default::default(),
            backend_request: false,
        };
        let ty = parse_type("[Out]");

        fill_relationships_with(
            &executor,
            ActionRelationshipContext {
                engine: &engine,
                session: &session,
                custom_types: &custom_types,
            },
            &ty,
            &mut shaped,
            &raw,
            selection,
        )
        .await
        .unwrap();

        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            shaped,
            json!([
                { "id": 7, "user": { "id": 7 } },
                { "id": 7, "user": { "id": 7 } },
                { "id": 8, "user": { "id": 8 } }
            ])
        );
    }

    struct BarrierRelationshipExecutor {
        barrier: Arc<tokio::sync::Barrier>,
        calls: AtomicUsize,
    }

    impl ActionRelationshipExecutor for BarrierRelationshipExecutor {
        fn execute<'a>(
            &'a self,
            _engine: &'a Engine,
            _session: &'a Session,
            _query: &'a str,
            variables: &'a JsonMap<String, Json>,
        ) -> BoxFuture<'a, Result<Json, Json>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                self.barrier.wait().await;
                let mut data = JsonMap::new();
                for (name, value) in variables {
                    let Some(index) = name.strip_prefix("__donat_action_rel_w_") else {
                        continue;
                    };
                    let id = value.pointer("/id/_eq").cloned().unwrap_or(Json::Null);
                    data.insert(format!("__donat_action_rel_{index}"), json!([{ "id": id }]));
                }
                Ok(Json::Object(data))
            })
        }
    }

    #[tokio::test]
    async fn independent_action_relationship_groups_run_concurrently() {
        use std::collections::BTreeMap;

        let mapping = BTreeMap::from([("id".into(), "id".into())]);
        let custom_types = CustomTypes {
            objects: vec![ObjectType {
                name: "Out".into(),
                fields: vec![CustomTypeField {
                    name: "id".into(),
                    type_: "Int!".into(),
                    description: None,
                }],
                relationships: vec![
                    CustomTypeRelationship {
                        name: "user".into(),
                        type_: "object".into(),
                        remote_table: QualifiedTable::Name("user".into()),
                        field_mapping: mapping.clone(),
                    },
                    CustomTypeRelationship {
                        name: "account".into(),
                        type_: "object".into(),
                        remote_table: QualifiedTable::Name("account".into()),
                        field_mapping: mapping,
                    },
                ],
                description: None,
            }],
            ..Default::default()
        };
        let doc =
            graphql_parser::parse_query::<String>("{ lookup { id user { id } account { id } } }")
                .unwrap()
                .into_static();
        let selection = match &doc.definitions[0] {
            Definition::Operation(OperationDefinition::SelectionSet(root)) => {
                let Selection::Field(action) = &root.items[0] else {
                    unreachable!()
                };
                action.selection_set.items.as_slice()
            }
            _ => unreachable!(),
        };
        let mut shaped = json!({ "id": 7, "user": null, "account": null });
        let raw = shaped.clone();
        let executor = BarrierRelationshipExecutor {
            barrier: Arc::new(tokio::sync::Barrier::new(2)),
            calls: AtomicUsize::new(0),
        };
        let engine = Engine::bootstrap(
            serde_json::from_value(json!({ "version": 3, "sources": [] })).unwrap(),
        );
        let session = Session {
            role: "user".into(),
            vars: Default::default(),
            backend_request: false,
        };

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            fill_relationships_with(
                &executor,
                ActionRelationshipContext {
                    engine: &engine,
                    session: &session,
                    custom_types: &custom_types,
                },
                &parse_type("Out"),
                &mut shaped,
                &raw,
                selection,
            ),
        )
        .await
        .expect("independent groups do not serialize")
        .expect("relationship groups resolve");

        assert_eq!(executor.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            shaped,
            json!({
                "id": 7,
                "user": { "id": 7 },
                "account": { "id": 7 },
            })
        );
    }

    async fn observe_concurrency(active: Arc<AtomicUsize>, maximum: Arc<AtomicUsize>) {
        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
        maximum.fetch_max(current, Ordering::SeqCst);
        tokio::task::yield_now().await;
        active.fetch_sub(1, Ordering::SeqCst);
    }

    #[tokio::test]
    async fn query_actions_run_concurrently_but_mutations_remain_sequential() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        schedule_action_items(
            true,
            (0..3)
                .map(|_| observe_concurrency(active.clone(), maximum.clone()))
                .collect(),
        )
        .await;
        assert_eq!(maximum.load(Ordering::SeqCst), 3);

        maximum.store(0, Ordering::SeqCst);
        schedule_action_items(
            false,
            (0..3)
                .map(|_| observe_concurrency(active.clone(), maximum.clone()))
                .collect(),
        )
        .await;
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
    }
}
