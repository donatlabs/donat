//! /v1/graphql execution: headers -> session, plan -> SQL -> one row.

use axum::http::HeaderMap;
use serde_json::{Map as JsonMap, Value as Json, json};

use dist_schema::{Planner, Session};

use crate::state::SharedState;

/// Maximum bracket-nesting depth accepted in a query. `graphql-parser` and
/// the planner both recurse on nesting, so an unbounded query would overflow
/// the stack (a fatal, process-aborting DoS). Real queries nest only a
/// handful of levels; this cap is far above legitimate use and far below the
/// overflow threshold.
pub const MAX_QUERY_DEPTH: usize = 100;

/// Cheap pre-parse guard: reject a query whose `{`/`(`/`[` nesting exceeds
/// [`MAX_QUERY_DEPTH`], before the recursive parser runs. Counting raw
/// brackets (including any inside string literals) is conservative, which is
/// the safe direction for a DoS guard.
pub fn query_too_deep(query: &str) -> bool {
    let mut depth: usize = 0;
    let mut max: usize = 0;
    for b in query.bytes() {
        match b {
            b'{' | b'(' | b'[' => {
                depth += 1;
                max = max.max(depth);
            }
            b'}' | b')' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max > MAX_QUERY_DEPTH
}

/// Constant-time byte-slice equality for the admin-secret check (avoids a
/// timing side-channel on the secret value; length is not secret).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A planning-level GraphQL error (shared with remote validation).
#[derive(Debug)]
pub struct GqlError {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

/// Build the request session from X-Hasura-* headers. There is no admin
/// role: the role header is mandatory and grants nothing by itself.
/// `trusted` is false when an admin secret is configured but absent from
/// the request: X-Hasura-* headers are then ignored entirely and the
/// session falls back to the unauthorized role.
pub fn session_from_headers(
    headers: &HeaderMap,
    unauthorized_role: Option<&str>,
    trusted: bool,
) -> Result<Session, Json> {
    if !trusted {
        return match unauthorized_role {
            Some(role) => Ok(Session {
                role: role.to_string(),
                vars: std::collections::HashMap::new(),
                backend_request: false,
            }),
            None => Err(json!({
                "errors": [{
                    "extensions": { "path": "$", "code": "access-denied" },
                    "message": "x-hasura-admin-secret required, but not found",
                }]
            })),
        };
    }
    let mut role = None;
    let mut vars = std::collections::HashMap::new();
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if !name.starts_with("x-hasura-") || name == "x-hasura-admin-secret" {
            continue;
        }
        let Ok(value) = value.to_str() else { continue };
        if name == "x-hasura-role" {
            role = Some(value.to_string());
        }
        vars.insert(name, value.to_string());
    }
    let backend_request = match vars.get("x-hasura-use-backend-only-permissions") {
        None => false,
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" => true,
            "false" | "f" | "no" | "n" => false,
            _ => {
                return Err(json!({
                    "errors": [{
                        "extensions": { "path": "$", "code": "bad-request" },
                        "message": "x-hasura-use-backend-only-permissions:  Not a valid boolean text. True values are [\"true\",\"t\",\"yes\",\"y\"] and  False values are [\"false\",\"f\",\"no\",\"n\"]. All values are case insensitive",
                    }]
                }));
            }
        },
    };
    // No admin role: a trusted request must name an explicit role (an
    // unauthorized-role fallback applies only to the untrusted branch above).
    match role.or_else(|| unauthorized_role.map(str::to_string)) {
        Some(role) => Ok(Session {
            role,
            vars,
            backend_request,
        }),
        None => Err(json!({
            "errors": [{
                "extensions": { "path": "$", "code": "access-denied" },
                "message": "x-hasura-role header is required (this engine has no admin role)",
            }]
        })),
    }
}

/// Full session resolution: admin secret wins (X-Hasura-* honored), then
/// JWT bearer tokens when configured, then the unauthorized role.
pub async fn resolve_session(
    state: &crate::state::AppState,
    headers: &HeaderMap,
) -> Result<Session, (axum::http::StatusCode, Json)> {
    let secret_ok = match &state.admin_secret {
        None => true,
        Some(expected) => headers
            .get("x-hasura-admin-secret")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|provided| ct_eq(provided.as_bytes(), expected.as_bytes())),
    };
    if let Some((url, mode)) = &state.auth_hook {
        if state.admin_secret.is_some() && secret_ok {
            return session_from_headers(headers, state.unauthorized_role.as_deref(), true)
                .map_err(|e| (axum::http::StatusCode::OK, e));
        }
        return webhook_session(state, url, mode, headers).await;
    }
    if let Some(jwt) = &state.jwt {
        if state.admin_secret.is_some() && secret_ok {
            return session_from_headers(headers, state.unauthorized_role.as_deref(), true)
                .map_err(|e| (axum::http::StatusCode::OK, e));
        }
        let token: Option<String> = match &jwt.header {
            crate::jwt::TokenLocation::Authorization => headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string),
            crate::jwt::TokenLocation::Cookie(name) => headers
                .get("cookie")
                .and_then(|v| v.to_str().ok())
                .and_then(|cookies| {
                    cookies.split(';').find_map(|c| {
                        let c = c.trim();
                        c.strip_prefix(&format!("{name}=")).map(str::to_string)
                    })
                }),
            crate::jwt::TokenLocation::CustomHeader(name) => headers
                .get(name.to_ascii_lowercase().as_str())
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        };
        let Some(token) = token else {
            if let Some(role) = &state.unauthorized_role {
                return Ok(Session {
                    role: role.clone(),
                    vars: std::collections::HashMap::new(),
                    backend_request: false,
                });
            }
            return Err((
                axum::http::StatusCode::OK,
                json!({
                    "errors": [{
                        "extensions": { "path": "$", "code": "invalid-headers" },
                        "message": "Missing 'Authorization' or 'Cookie' header in JWT authentication mode",
                    }]
                }),
            ));
        };
        let requested = headers
            .get("x-hasura-role")
            .and_then(|v| v.to_str().ok());
        let backend = headers
            .get("x-hasura-use-backend-only-permissions")
            .and_then(|v| v.to_str().ok())
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "true" | "t" | "yes" | "y"))
            .unwrap_or(false);
        return match jwt.session(&token, requested, backend) {
            Ok(sess) => Ok(Session {
                role: sess.role,
                vars: sess.vars,
                backend_request: backend,
            }),
            // JWT failures are HTTP 200 on /v1/graphql; the legacy
            // endpoint upgrades them to 400 itself.
            Err(e) => Err((
                axum::http::StatusCode::OK,
                json!({
                    "errors": [{
                        "extensions": { "path": "$", "code": e.code },
                        "message": e.message,
                    }]
                }),
            )),
        };
    }
    session_from_headers(headers, state.unauthorized_role.as_deref(), secret_ok)
        .map_err(|e| (axum::http::StatusCode::OK, e))
}

/// Webhook authentication: forward the client headers, expect a JSON
/// object of session variables (or 401).
async fn webhook_session(
    state: &crate::state::AppState,
    url: &str,
    mode: &str,
    headers: &HeaderMap,
) -> Result<Session, (axum::http::StatusCode, Json)> {
    let header_map: serde_json::Map<String, Json> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|v| (k.as_str().to_string(), Json::String(v.to_string())))
        })
        .collect();

    let response = if mode.eq_ignore_ascii_case("POST") {
        state
            .http
            .post(url)
            .json(&json!({ "headers": header_map }))
            .send()
            .await
    } else {
        let mut req = state.http.get(url);
        for (k, v) in &header_map {
            if let Some(v) = v.as_str() {
                req = req.header(k, v);
            }
        }
        req.send().await
    };

    let response = match response {
        Ok(r) => r,
        Err(e) => {
            return Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                json!({
                    "errors": [{
                        "extensions": { "path": "$", "code": "unexpected" },
                        "message": format!("webhook authentication request failed: {e}"),
                    }]
                }),
            ));
        }
    };

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        if let Some(role) = &state.unauthorized_role {
            return Ok(Session {
                role: role.clone(),
                vars: std::collections::HashMap::new(),
                backend_request: false,
            });
        }
        return Err((
            axum::http::StatusCode::UNAUTHORIZED,
            json!({
                "errors": [{
                    "extensions": { "path": "$", "code": "access-denied" },
                    "message": "Authentication hook unauthorized this request",
                }]
            }),
        ));
    }

    let vars_raw: Json = response.json().await.unwrap_or(Json::Null);
    let mut vars = std::collections::HashMap::new();
    if let Some(map) = vars_raw.as_object() {
        for (k, v) in map {
            let key = k.to_ascii_lowercase();
            if !key.starts_with("x-hasura-") {
                continue;
            }
            let value = match v {
                Json::String(s) => s.clone(),
                other => other.to_string(),
            };
            vars.insert(key, value);
        }
    }
    let Some(role) = vars.get("x-hasura-role").cloned() else {
        return Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            json!({
                "errors": [{
                    "extensions": { "path": "$", "code": "unexpected" },
                    "message": "webhook response did not include x-hasura-role",
                }]
            }),
        ));
    };
    Ok(Session {
        role,
        vars,
        backend_request: false,
    })
}


pub async fn execute(
    state: &SharedState,
    session: &Session,
    body: &Json,
) -> (axum::http::StatusCode, Json) {
    execute_with(state, session, body, false).await
}

pub async fn execute_with(
    state: &SharedState,
    session: &Session,
    body: &Json,
    relay: bool,
) -> (axum::http::StatusCode, Json) {
    execute_full(state, session, body, relay, &axum::http::HeaderMap::new()).await
}

pub async fn execute_full(
    state: &SharedState,
    session: &Session,
    body: &Json,
    relay: bool,
    headers: &axum::http::HeaderMap,
) -> (axum::http::StatusCode, Json) {
    let Some(query) = body.get("query").and_then(Json::as_str) else {
        return ok(error_json("validation-failed", "the key 'query' is missing"));
    };
    let variables: JsonMap<String, Json> = match body.get("variables") {
        Some(Json::Object(map)) => map.clone(),
        Some(Json::Null) | None => JsonMap::new(),
        Some(_) => return ok(error_json("validation-failed", "variables must be an object")),
    };
    let operation_name = body.get("operationName").and_then(Json::as_str);

    if query_too_deep(query) {
        return ok(error_json(
            "validation-failed",
            format!("query exceeds maximum nesting depth of {MAX_QUERY_DEPTH}"),
        ));
    }
    let doc = match graphql_parser::parse_query::<String>(query) {
        Ok(doc) => doc.into_static(),
        Err(e) => return ok(error_json("validation-failed", format!("not a valid graphql query: {e}"))),
    };

    let engine = state.engine.read().await;
    // Remote schema routing: operations aimed entirely at a permitted
    // remote schema are validated against the role's SDL and forwarded.
    let mut remote_variables = variables.clone();
    if let Some(result) =
        crate::remote::match_remote(&engine.metadata, session, &doc, &mut remote_variables)
    {
        return match result {
            Ok(target) => {
                if target.has_introspection {
                    // Answer introspection locally, forward the rest,
                    // merge in the original selection order.
                    let order: Vec<(String, bool)> = top_level_fields(&doc);
                    let mut intro_doc = doc.clone();
                    crate::remote::keep_introspection_roots(&mut intro_doc);
                    let catalog = engine.default_catalog();
                    let mut planner = Planner::new(&engine.metadata, &catalog);
                    planner.infer_function_permissions = state.infer_function_permissions;
                    let intro_data = match dist_schema::execute_introspection(
                        &planner,
                        session,
                        &intro_doc,
                        operation_name,
                        &variables,
                    ) {
                        Some(Ok(data)) => data,
                        Some(Err(e)) => return ok(e.to_graphql()),
                        None => Json::Object(JsonMap::new()),
                    };
                    drop(engine);
                    let mut remote_body = body.clone();
                    remote_body["variables"] = Json::Object(remote_variables.clone());
                    let (status, remote_resp) =
                        crate::remote::forward(state, &target, &remote_body, headers).await;
                    if remote_resp.get("errors").is_some() {
                        return (status, remote_resp);
                    }
                    let remote_data = remote_resp
                        .get("data")
                        .and_then(Json::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let intro_map = intro_data.as_object().cloned().unwrap_or_default();
                    let mut data = JsonMap::new();
                    for (alias, is_intro) in order {
                        let value = if is_intro {
                            intro_map.get(&alias).cloned()
                        } else {
                            remote_data.get(&alias).cloned()
                        };
                        data.insert(alias, value.unwrap_or(Json::Null));
                    }
                    return ok(json!({ "data": data }));
                }
                drop(engine);
                let mut remote_body = body.clone();
                remote_body["variables"] = Json::Object(remote_variables);
                let (status, mut resp) =
                    crate::remote::forward(state, &target, &remote_body, headers).await;
                if let Some(ns) = &target.namespace {
                    if resp.get("errors").is_none() {
                        let data = resp.get("data").cloned().unwrap_or(Json::Null);
                        resp["data"] = json!({ ns: data });
                    }
                }
                (status, resp)
            }
            Err(e) => ok(json!({
                "errors": [{
                    "extensions": { "path": e.path, "code": e.code },
                    "message": e.message,
                }]
            })),
        };
    }
    // Allowlist gate: the query must structurally match a listed one
    // (__typename selections are ignored, like Hasura).
    if state.allowlist_enabled {
        let normalized = normalize_for_allowlist(&doc);
        let allowed = engine.metadata.allowlist.iter().any(|entry| {
            engine
                .metadata
                .query_collections
                .iter()
                .filter(|c| c.name == entry.collection)
                .flat_map(|c| c.definition.queries.iter())
                .any(|q| {
                    graphql_parser::parse_query::<String>(&q.query)
                        .map(|d| normalize_for_allowlist(&d.into_static()) == normalized)
                        .unwrap_or(false)
                })
        });
        if !allowed {
            return ok(error_json("validation-failed", "query is not allowed"));
        }
    }
    let catalog = engine.default_catalog();
    tracing::debug!(role = %session.role, sources = engine.metadata.sources.len(),
        tables = engine.metadata.sources.first().map(|s| s.tables.len()).unwrap_or(0),
        catalog_tables = catalog.tables.len(), "graphql request");
    let mut planner = Planner::new(&engine.metadata, &catalog);
    planner.infer_function_permissions = state.infer_function_permissions;
    planner.relay = relay;
    // Introspection operations are answered from the type system directly.
    if let Some(result) = dist_schema::execute_introspection(
        &planner,
        session,
        &doc,
        operation_name,
        &variables,
    ) {
        return match result {
            Ok(data) => ok(json!({ "data": data })),
            Err(e) => ok(e.to_graphql()),
        };
    }
    let plan = match planner.plan(&doc, operation_name, &variables, session) {
        Ok(plan) => plan,
        Err(e) => return ok(e.to_graphql()),
    };

    match plan {
        dist_schema::Plan::Query(roots) => {
            let sql = dist_sqlgen::operation_to_sql_opts(&roots, state.stringify_numerics);
            drop(engine);
            tracing::debug!(%sql, "executing query");

            let Some(pool) = state.default_pool().await else {
                return ok(error_json("unexpected", "no default source"));
            };
            let client = match pool.get().await {
                Ok(c) => c,
                Err(e) => return ok(error_json("unexpected", format!("connection pool error: {e}"))),
            };
            match client.query_one(&sql, &[]).await {
                Ok(row) => match row.try_get::<_, Json>(0) {
                    Ok(mut data) => {
                        for root in &roots {
                            if let dist_ir::RootField::Select { alias, query } = root {
                                if let Some(node) = data.get_mut(alias.as_str()) {
                                    if let Err(e) = resolve_remote_joins(
                                        state,
                                        session,
                                        &query.fields,
                                        node,
                                        &format!("$.selectionSet.{alias}"),
                                    )
                                    .await
                                    {
                                        return ok(e);
                                    }
                                }
                            }
                        }
                        ok(json!({ "data": data }))
                    }
                    Err(e) => ok(error_json("unexpected", format!("cannot decode result: {e}"))),
                },
                Err(e) => ok(db_error_json(&e)),
            }
        }
        dist_schema::Plan::Mutation(roots) => {
            // Pre-compute the per-field SQL and response keys, then run
            // everything inside one transaction.
            let fields: Vec<(String, String)> = roots
                .iter()
                .map(|root| {
                    let alias = match root {
                        dist_ir::MutationRoot::FunctionCall { alias, .. }
                        | dist_ir::MutationRoot::Insert { alias, .. }
                        | dist_ir::MutationRoot::Update { alias, .. }
                        | dist_ir::MutationRoot::Delete { alias, .. }
                        | dist_ir::MutationRoot::Typename { alias, .. } => alias.clone(),
                    };
                    (alias, dist_sqlgen::mutation_to_sql_opts(root, state.stringify_numerics))
                })
                .collect();
            drop(engine);

            let Some(pool) = state.default_pool().await else {
                return ok(error_json("unexpected", "no default source"));
            };
            let mut client = match pool.get().await {
                Ok(c) => c,
                Err(e) => return ok(error_json("unexpected", format!("connection pool error: {e}"))),
            };
            let tx = match client.transaction().await {
                Ok(tx) => tx,
                Err(e) => return ok(db_error_json(&e)),
            };
            let mut data = serde_json::Map::new();
            for (alias, sql) in fields {
                tracing::debug!(%sql, "executing mutation");
                match tx.query_one(&sql, &[]).await {
                    Ok(row) => {
                        // Typename roots produce text, everything else json.
                        let value = row
                            .try_get::<_, Json>(0)
                            .or_else(|_| row.try_get::<_, String>(0).map(Json::String));
                        match value {
                            Ok(v) => {
                                data.insert(alias, v);
                            }
                            Err(e) => {
                                return ok(error_json(
                                    "unexpected",
                                    format!("cannot decode result: {e}"),
                                ));
                            }
                        }
                    }
                    Err(e) => return ok(db_error_json(&e)),
                }
            }
            if let Err(e) = tx.commit().await {
                return ok(db_error_json(&e));
            }
            ok(json!({ "data": data }))
        }
    }
}

/// Render a document with every __typename selection removed.
fn normalize_for_allowlist(doc: &graphql_parser::query::Document<'static, String>) -> String {
    use graphql_parser::query::{Definition, Selection};
    fn strip(set: &mut graphql_parser::query::SelectionSet<'static, String>) {
        set.items.retain(|item| {
            !matches!(item, Selection::Field(f) if f.name == "__typename")
        });
        for item in &mut set.items {
            match item {
                Selection::Field(f) => strip(&mut f.selection_set),
                Selection::InlineFragment(f) => strip(&mut f.selection_set),
                Selection::FragmentSpread(_) => {}
            }
        }
    }
    let mut doc = doc.clone();
    for def in &mut doc.definitions {
        match def {
            Definition::Operation(op) => {
                use graphql_parser::query::OperationDefinition::*;
                match op {
                    Query(q) => strip(&mut q.selection_set),
                    Mutation(m) => strip(&mut m.selection_set),
                    Subscription(s) => strip(&mut s.selection_set),
                    SelectionSet(s) => strip(s),
                }
            }
            Definition::Fragment(f) => strip(&mut f.selection_set),
        }
    }
    format!("{doc}")
}

/// Top-level (alias, is_introspection) pairs in selection order.
fn top_level_fields(doc: &graphql_parser::query::Document<'static, String>) -> Vec<(String, bool)> {
    use graphql_parser::query::{Definition, OperationDefinition, Selection};
    let mut out = vec![];
    for def in &doc.definitions {
        if let Definition::Operation(op) = def {
            let set = match op {
                OperationDefinition::Query(q) => &q.selection_set,
                OperationDefinition::SelectionSet(s) => s,
                _ => continue,
            };
            for item in &set.items {
                if let Selection::Field(f) = item {
                    let alias = f.alias.clone().unwrap_or_else(|| f.name.clone());
                    let is_intro = f.name == "__schema"
                        || f.name == "__type"
                        || f.name == "__typename";
                    out.push((alias, is_intro));
                }
            }
        }
    }
    out
}

/// Fill RemoteJoin placeholders by querying the remote schema per row
/// and strip the hidden "__rr__" columns.
fn resolve_remote_joins<'a>(
    state: &'a SharedState,
    session: &'a Session,
    fields: &'a [dist_ir::OutputField],
    node: &'a mut Json,
    path: &'a str,
) -> futures_util::future::BoxFuture<'a, Result<(), Json>> {
    Box::pin(async move {
        match node {
            Json::Array(items) => {
                for item in items {
                    resolve_remote_joins(state, session, fields, item, path).await?;
                }
                Ok(())
            }
            Json::Object(_) => {
                for field in fields {
                    match &field.value {
                        dist_ir::FieldValue::Object { query, .. }
                        | dist_ir::FieldValue::Array { query, .. } => {
                            if let Some(child) = node.get_mut(field.alias.as_str()) {
                                resolve_remote_joins(
                                    state,
                                    session,
                                    &query.fields,
                                    child,
                                    &format!("{path}.selectionSet.{}", field.alias),
                                )
                                .await?;
                            }
                        }
                        dist_ir::FieldValue::RemoteJoin { spec } => {
                            // Variables from the row's hidden columns.
                            let mut vars = serde_json::Map::new();
                            for (var, hidden) in &spec.variables {
                                vars.insert(
                                    var.clone(),
                                    node.get(hidden.as_str()).cloned().unwrap_or(Json::Null),
                                );
                            }
                            let doc = graphql_parser::parse_query::<String>(&spec.query)
                                .map_err(|e| {
                                    error_json("unexpected", format!("bad remote join: {e}"))
                                })?
                                .into_static();
                            let engine = state.engine.read().await;
                            let mut varmap = vars.clone();
                            let matched = crate::remote::match_remote_with(
                                &engine.metadata,
                                session,
                                &doc,
                                &mut varmap,
                                true,
                            );
                            drop(engine);
                            let value = match matched {
                                Some(Ok(target)) => {
                                    let body = json!({
                                        "query": target
                                            .rewritten_query
                                            .clone()
                                            .unwrap_or_else(|| spec.query.clone()),
                                        "variables": varmap,
                                    });
                                    let (_, resp) = crate::remote::forward(
                                        state,
                                        &target,
                                        &body,
                                        &axum::http::HeaderMap::new(),
                                    )
                                    .await;
                                    if let Some(errors) = resp.get("errors") {
                                        return Err(json!({ "errors": errors }));
                                    }
                                    resp.pointer(&format!("/data/{}", spec.root_field))
                                        .cloned()
                                        .unwrap_or(Json::Null)
                                }
                                Some(Err(e)) => {
                                    // Validation errors for the server-built
                                    // join query are reported at the client's
                                    // field path, not the join root's.
                                    let client_field =
                                        format!("{path}.selectionSet.{}", field.alias);
                                    let server_root =
                                        format!("$.selectionSet.{}", spec.root_field);
                                    let rewritten = match e.path.strip_prefix(&server_root) {
                                        Some(rest) => format!("{client_field}{rest}"),
                                        None => client_field,
                                    };
                                    return Err(json!({
                                        "errors": [{
                                            "extensions": { "path": rewritten, "code": e.code },
                                            "message": e.message,
                                        }]
                                    }));
                                }
                                None => Json::Null,
                            };
                            node[field.alias.as_str()] = value;
                        }
                        _ => {}
                    }
                }
                if let Json::Object(map) = node {
                    map.retain(|k, _| !k.starts_with("__rr__"));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    })
}

fn ok(body: Json) -> (axum::http::StatusCode, Json) {
    (axum::http::StatusCode::OK, body)
}

/// Map Postgres errors onto Hasura v2 error codes/messages.
fn db_error_json(e: &tokio_postgres::Error) -> Json {
    let Some(db) = e.as_db_error() else {
        return error_json("unexpected", e.to_string());
    };
    // Our check_violation() raises 23514 with a JSON payload carrying the
    // GraphQL error path.
    if db.code().code() == "23514" {
        if let Ok(payload) = serde_json::from_str::<Json>(db.message()) {
            if let (Some(path), Some(message)) = (
                payload.get("path").and_then(Json::as_str),
                payload.get("message").and_then(Json::as_str),
            ) {
                return json!({
                    "errors": [{
                        "extensions": { "path": path, "code": "permission-error" },
                        "message": message,
                    }]
                });
            }
        }
    }
    let (code, message) = match db.code().code() {
        "23514" => ("permission-error", db.message().to_string()),
        "23505" => ("constraint-violation", format!("Uniqueness violation. {}", db.message())),
        "23503" => ("constraint-violation", format!("Foreign key violation. {}", db.message())),
        "23502" => ("constraint-violation", format!("Not-NULL violation. {}", db.message())),
        _ => ("data-exception", db.message().to_string()),
    };
    json!({
        "errors": [{
            "extensions": { "path": "$", "code": code },
            "message": message,
        }]
    })
}

fn error_json(code: &str, message: impl Into<String>) -> Json {
    json!({
        "errors": [{
            "extensions": { "path": "$", "code": code },
            "message": message.into(),
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_depth_guard() {
        assert!(!query_too_deep("{ a { b { c } } }"));
        let deep = format!(
            "{}{}",
            "{ a ".repeat(MAX_QUERY_DEPTH + 5),
            "}".repeat(MAX_QUERY_DEPTH + 5)
        );
        assert!(query_too_deep(&deep));
        // Arg/list brackets count toward depth too.
        assert!(query_too_deep(&"(".repeat(MAX_QUERY_DEPTH + 1)));
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"secret", b"secret"));
        assert!(!ct_eq(b"secret", b"secrey"));
        assert!(!ct_eq(b"secret", b"secre"));
        assert!(ct_eq(b"", b""));
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                axum::http::HeaderName::try_from(*k).unwrap(),
                axum::http::HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn parse(q: &str) -> graphql_parser::query::Document<'static, String> {
        graphql_parser::parse_query::<String>(q).unwrap().into_static()
    }

    #[test]
    fn untrusted_request_falls_back_to_unauthorized_role() {
        let h = headers(&[("x-hasura-role", "editor"), ("x-hasura-user-id", "1")]);
        let s = session_from_headers(&h, Some("anonymous"), false).unwrap();
        assert_eq!(s.role, "anonymous");
        assert!(s.vars.is_empty(), "untrusted headers must be ignored");
    }

    #[test]
    fn untrusted_request_without_unauthorized_role_is_denied() {
        let e = session_from_headers(&HeaderMap::new(), None, false).unwrap_err();
        assert_eq!(e.pointer("/errors/0/extensions/code"), Some(&json!("access-denied")));
        assert_eq!(
            e.pointer("/errors/0/message"),
            Some(&json!("x-hasura-admin-secret required, but not found"))
        );
    }

    #[test]
    fn trusted_request_collects_x_hasura_vars() {
        let h = headers(&[
            ("x-hasura-role", "editor"),
            ("X-Hasura-User-Id", "7"),
            ("x-hasura-admin-secret", "shh"),
            ("content-type", "application/json"),
        ]);
        let s = session_from_headers(&h, None, true).unwrap();
        assert_eq!(s.role, "editor");
        assert_eq!(s.vars.get("x-hasura-user-id").map(String::as_str), Some("7"));
        assert!(!s.vars.contains_key("x-hasura-admin-secret"));
        assert!(!s.vars.contains_key("content-type"));
        assert!(!s.backend_request);
    }

    #[test]
    fn trusted_request_requires_a_role() {
        // No admin role: a trusted request with no X-Hasura-Role is denied.
        let e = session_from_headers(&headers(&[("x-hasura-user-id", "7")]), None, true)
            .unwrap_err();
        assert_eq!(
            e.pointer("/errors/0/message"),
            Some(&json!("x-hasura-role header is required (this engine has no admin role)"))
        );
    }


    #[test]
    fn backend_only_permissions_header_parsing() {
        let with = |v: &str| {
            session_from_headers(
                &headers(&[("x-hasura-role", "u"), ("x-hasura-use-backend-only-permissions", v)]),
                None,
                true,
            )
        };
        assert!(with("YES").unwrap().backend_request);
        assert!(!with("f").unwrap().backend_request);
        let e = with("maybe").unwrap_err();
        assert_eq!(e.pointer("/errors/0/extensions/code"), Some(&json!("bad-request")));
        assert_eq!(
            e.pointer("/errors/0/message"),
            Some(&json!("x-hasura-use-backend-only-permissions:  Not a valid boolean text. True values are [\"true\",\"t\",\"yes\",\"y\"] and  False values are [\"false\",\"f\",\"no\",\"n\"]. All values are case insensitive"))
        );
    }

    #[test]
    fn allowlist_comparison_ignores_typename_only() {
        let listed = parse("query getAuthors { author { id name } }");
        let with_typename =
            parse("query getAuthors { __typename author { id __typename name } }");
        let different = parse("query getAuthors { author { id } }");
        assert_eq!(
            normalize_for_allowlist(&with_typename),
            normalize_for_allowlist(&listed)
        );
        assert_ne!(
            normalize_for_allowlist(&different),
            normalize_for_allowlist(&listed)
        );
    }

    #[test]
    fn top_level_fields_keeps_order_and_flags_introspection() {
        let doc = parse("{ __schema { queryType { name } } a: user { id } __typename }");
        assert_eq!(
            top_level_fields(&doc),
            vec![
                ("__schema".to_string(), true),
                ("a".to_string(), false),
                ("__typename".to_string(), true),
            ]
        );
    }

    #[test]
    fn error_json_shape() {
        assert_eq!(
            error_json("validation-failed", "boom"),
            json!({ "errors": [{
                "extensions": { "path": "$", "code": "validation-failed" },
                "message": "boom",
            }] })
        );
    }
}
