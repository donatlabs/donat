//! /v1/graphql execution: headers -> session, plan -> SQL -> one row.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::http::HeaderMap;
use futures_util::future::join_all;
use serde_json::{Map as JsonMap, Value as Json, json};

use donat_schema::{
    MultiSourcePlan, MultiSourcePlanner, PlanError, QueryResponseSlot, Session, SourceQueryPlan,
};

use crate::state::{AppState, Engine, QueryError, SharedState, SourceRuntime};

fn trace_perf_phase(phase: &'static str, started: std::time::Instant) {
    tracing::trace!(
        target: "donat::perf",
        phase,
        elapsed_us = started.elapsed().as_micros() as u64,
        "request phase"
    );
}

/// Maximum bracket-nesting depth accepted in a query. `graphql-parser` and
/// the planner both recurse on nesting, so an unbounded query would overflow
/// the stack (a fatal, process-aborting DoS). Real queries nest only a
/// handful of levels; this cap is far above legitimate use and far below the
/// overflow threshold.
pub const MAX_QUERY_DEPTH: usize = 100;

/// Exact-source query execution boundary. Composite orchestration passes the
/// complete root slice for a source in one call and exposes no default-source
/// fallback.
pub trait SourceQueryExecutor {
    fn execute_source_query<'a>(
        &'a self,
        source: &'a str,
        roots: &'a [donat_ir::RootField],
    ) -> Pin<Box<dyn Future<Output = Result<Json, QueryError>> + Send + 'a>>;
}

impl SourceQueryExecutor for AppState {
    fn execute_source_query<'a>(
        &'a self,
        source: &'a str,
        roots: &'a [donat_ir::RootField],
    ) -> Pin<Box<dyn Future<Output = Result<Json, QueryError>> + Send + 'a>> {
        Box::pin(self.execute_source_query_json(source, roots))
    }
}

struct SnapshotSourceQueryExecutor<'a> {
    state: &'a AppState,
    runtimes: HashMap<String, SourceRuntime>,
}

impl SourceQueryExecutor for SnapshotSourceQueryExecutor<'_> {
    fn execute_source_query<'a>(
        &'a self,
        source: &'a str,
        roots: &'a [donat_ir::RootField],
    ) -> Pin<Box<dyn Future<Output = Result<Json, QueryError>> + Send + 'a>> {
        let runtime = self.runtimes.get(source).cloned();
        Box::pin(async move {
            let runtime = runtime.ok_or(QueryError::NoDefaultSource)?;
            self.state.execute_runtime_query_json(runtime, roots).await
        })
    }
}

fn planner_from_snapshot(engine: &Engine) -> Result<MultiSourcePlanner<'_>, PlanError> {
    let compiled = engine.compiled.as_deref().ok_or_else(|| {
        PlanError::new(
            "$",
            "unexpected",
            "engine schema snapshot is not initialized",
        )
    })?;
    MultiSourcePlanner::from_compiled(&engine.metadata, &engine.catalogs, compiled)
}

pub async fn execute_source_query_plans<E: SourceQueryExecutor + Sync>(
    executor: &E,
    plans: &[SourceQueryPlan],
) -> Result<Vec<Json>, QueryError> {
    join_all(
        plans
            .iter()
            .map(|plan| executor.execute_source_query(&plan.source, &plan.roots)),
    )
    .await
    .into_iter()
    .collect()
}

fn assemble_multi_source_response(
    response: &[QueryResponseSlot],
    source_data: impl IntoIterator<Item = Json>,
) -> Json {
    let mut values = std::collections::HashMap::new();
    for data in source_data {
        if let Json::Object(data) = data {
            values.extend(data);
        }
    }
    let mut ordered = JsonMap::new();
    for slot in response {
        match slot {
            QueryResponseSlot::SourceField { key } => {
                ordered.insert(key.clone(), values.remove(key).unwrap_or(Json::Null));
            }
            QueryResponseSlot::LocalTypename { key, value } => {
                ordered.insert(key.clone(), Json::String(value.clone()));
            }
        }
    }
    Json::Object(ordered)
}

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

fn is_session_header(name: &str) -> bool {
    name.starts_with("x-donat-") || name.starts_with("x-hasura-")
}

fn is_reserved_session_secret(name: &str) -> bool {
    name == "x-donat-admin-secret" || name == "x-hasura-admin-secret"
}

/// A planning-level GraphQL error (shared with remote validation).
#[derive(Debug)]
pub struct GqlError {
    pub path: String,
    pub code: &'static str,
    pub message: String,
}

/// Build the request session from X-Donat-* headers. There is no admin
/// role: the role header is mandatory and grants nothing by itself.
/// `trusted` is false when an admin secret is configured but absent from
/// the request: X-Donat-* headers are then ignored entirely and the
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
                    "message": "x-donat-admin-secret required, but not found",
                }]
            })),
        };
    }
    let mut donat_role = None;
    let mut hasura_role = None;
    let mut vars = std::collections::HashMap::new();
    for (name, value) in headers {
        let name = name.as_str().to_ascii_lowercase();
        if !is_session_header(&name) || is_reserved_session_secret(&name) {
            continue;
        }
        let Ok(value) = value.to_str() else { continue };
        if name == "x-donat-role" {
            donat_role = Some(value.to_string());
        } else if name == "x-hasura-role" {
            hasura_role = Some(value.to_string());
        }
        vars.insert(name, value.to_string());
    }
    let backend_request = match vars.get("x-donat-use-backend-only-permissions") {
        None => false,
        Some(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" => true,
            "false" | "f" | "no" | "n" => false,
            _ => {
                return Err(json!({
                    "errors": [{
                        "extensions": { "path": "$", "code": "bad-request" },
                        "message": "x-donat-use-backend-only-permissions:  Not a valid boolean text. True values are [\"true\",\"t\",\"yes\",\"y\"] and  False values are [\"false\",\"f\",\"no\",\"n\"]. All values are case insensitive",
                    }]
                }));
            }
        },
    };
    // No admin role: a trusted request must name an explicit role (an
    // unauthorized-role fallback applies only to the untrusted branch above).
    match donat_role
        .or(hasura_role)
        .or_else(|| unauthorized_role.map(str::to_string))
    {
        Some(role) => {
            vars.insert("x-donat-role".to_string(), role.clone());
            vars.insert("x-hasura-role".to_string(), role.clone());
            Ok(Session {
                role,
                vars,
                backend_request,
            })
        }
        None => Err(json!({
            "errors": [{
                "extensions": { "path": "$", "code": "access-denied" },
                "message": "x-donat-role header is required (this engine has no admin role)",
            }]
        })),
    }
}

/// Full session resolution: admin secret wins (X-Donat-* honored), then
/// JWT bearer tokens when configured, then the unauthorized role.
pub async fn resolve_session(
    state: &crate::state::AppState,
    headers: &HeaderMap,
) -> Result<Session, (axum::http::StatusCode, Json)> {
    let secret_ok = match &state.admin_secret {
        None => true,
        Some(expected) => headers
            .get("x-donat-admin-secret")
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
            .get("x-donat-role")
            .or_else(|| headers.get("x-hasura-role"))
            .and_then(|v| v.to_str().ok());
        let backend = headers
            .get("x-donat-use-backend-only-permissions")
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
            if !is_session_header(&key) || is_reserved_session_secret(&key) {
                continue;
            }
            let value = match v {
                Json::String(s) => s.clone(),
                other => other.to_string(),
            };
            vars.insert(key, value);
        }
    }
    let Some(role) = vars
        .get("x-donat-role")
        .or_else(|| vars.get("x-hasura-role"))
        .cloned()
    else {
        return Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            json!({
                "errors": [{
                    "extensions": { "path": "$", "code": "unexpected" },
                    "message": "webhook response did not include x-donat-role",
                }]
            }),
        ));
    };
    vars.insert("x-donat-role".to_string(), role.clone());
    vars.insert("x-hasura-role".to_string(), role.clone());
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
        return ok(error_json(
            "validation-failed",
            "the key 'query' is missing",
        ));
    };
    let variables: JsonMap<String, Json> = match body.get("variables") {
        Some(Json::Object(map)) => map.clone(),
        Some(Json::Null) | None => JsonMap::new(),
        Some(_) => {
            return ok(error_json(
                "validation-failed",
                "variables must be an object",
            ));
        }
    };
    let operation_name = body.get("operationName").and_then(Json::as_str);

    if query_too_deep(query) {
        return ok(error_json(
            "validation-failed",
            format!("query exceeds maximum nesting depth of {MAX_QUERY_DEPTH}"),
        ));
    }
    let parse_started = std::time::Instant::now();
    let doc = match graphql_parser::parse_query::<String>(query) {
        Ok(doc) => doc.into_static(),
        Err(e) => {
            return ok(error_json(
                "validation-failed",
                format!("not a valid graphql query: {e}"),
            ));
        }
    };
    trace_perf_phase("graphql.parse", parse_started);

    execute_parsed_full(
        state,
        session,
        body,
        relay,
        headers,
        doc,
        variables,
        operation_name,
    )
    .await
}

/// Execute a document compiled into the current immutable engine snapshot.
/// REST endpoints use this path so their saved query is not reparsed for every
/// request. The caller is responsible for applying textual limits while
/// compiling the document.
pub(crate) async fn execute_preparsed_full(
    state: &SharedState,
    session: &Session,
    body: &Json,
    relay: bool,
    headers: &axum::http::HeaderMap,
    doc: &graphql_parser::query::Document<'static, String>,
) -> (axum::http::StatusCode, Json) {
    let variables: JsonMap<String, Json> = match body.get("variables") {
        Some(Json::Object(map)) => map.clone(),
        Some(Json::Null) | None => JsonMap::new(),
        Some(_) => {
            return ok(error_json(
                "validation-failed",
                "variables must be an object",
            ));
        }
    };
    let operation_name = body.get("operationName").and_then(Json::as_str);
    execute_parsed_full(
        state,
        session,
        body,
        relay,
        headers,
        doc.clone(),
        variables,
        operation_name,
    )
    .await
}

async fn execute_parsed_full(
    state: &SharedState,
    session: &Session,
    body: &Json,
    relay: bool,
    headers: &axum::http::HeaderMap,
    doc: graphql_parser::query::Document<'static, String>,
    variables: JsonMap<String, Json>,
    operation_name: Option<&str>,
) -> (axum::http::StatusCode, Json) {
    let routing_started = std::time::Instant::now();
    let engine = state.engine_snapshot().await;
    // Remote schema routing: operations aimed entirely at a permitted
    // remote schema are validated against the role's SDL and forwarded.
    let mut remote_variables = variables.clone();
    if let Some(result) = crate::remote::match_remote(&engine, session, &doc, &mut remote_variables)
    {
        trace_perf_phase("graphql.route", routing_started);
        return match result {
            Ok(target) => {
                if target.has_introspection {
                    // Answer introspection locally, forward the rest,
                    // merge in the original selection order.
                    let order: Vec<(String, bool)> = top_level_fields(&doc);
                    let mut intro_doc = doc.clone();
                    crate::remote::keep_introspection_roots(&mut intro_doc);
                    let planner = match planner_from_snapshot(&engine) {
                        Ok(planner) => planner,
                        Err(error) => return ok(error.to_graphql()),
                    };
                    let intro_data = match donat_schema::execute_multi_source_introspection(
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
    // Action routing: an operation whose top-level fields are actions is
    // resolved by calling the action's HTTP handler, not by SQL planning.
    if let Some(ctx) = crate::action::match_action(&engine.metadata, &doc, operation_name) {
        trace_perf_phase("graphql.route", routing_started);
        return crate::action::resolve(
            state,
            engine,
            session,
            &ctx,
            &doc,
            &variables,
            operation_name,
            headers,
        )
        .await;
    }
    trace_perf_phase("graphql.route", routing_started);
    // Allowlist gate: the query must structurally match a listed one
    // (__typename selections are ignored, like Donat).
    if state.allowlist_enabled {
        let allowlist_started = std::time::Instant::now();
        let normalized = normalize_for_allowlist(&doc);
        if !engine.allowed_queries.contains(&normalized) {
            return ok(error_json("validation-failed", "query is not allowed"));
        }
        trace_perf_phase("graphql.allowlist", allowlist_started);
    }
    tracing::trace!(role = %session.role, sources = engine.metadata.sources.len(),
        tables = engine.metadata.sources.iter().map(|source| source.tables.len()).sum::<usize>(),
        catalog_tables = engine.catalogs.values().map(|catalog| catalog.tables.len()).sum::<usize>(),
        "graphql request");
    let planning_started = std::time::Instant::now();
    let mut planner = match planner_from_snapshot(&engine) {
        Ok(planner) => planner,
        Err(error) => return ok(error.to_graphql()),
    };
    if let Err(error) = planner.set_relay(relay) {
        return ok(error.to_graphql());
    }
    // Introspection operations are answered from the type system directly.
    if let Some(result) = donat_schema::execute_multi_source_introspection(
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
    trace_perf_phase("graphql.plan", planning_started);

    match plan {
        MultiSourcePlan::Query { sources, response } => {
            let mut runtimes = HashMap::with_capacity(sources.len());
            for source in &sources {
                let Some(runtime) = engine.runtimes.get(&source.source).cloned() else {
                    return ok(error_json(
                        "unexpected",
                        format!("runtime for source '{}' not found", source.source),
                    ));
                };
                runtimes.insert(source.source.clone(), runtime);
            }
            let executor = SnapshotSourceQueryExecutor {
                state: state.as_ref(),
                runtimes,
            };
            match execute_source_query_plans(&executor, &sources).await {
                Ok(mut source_data) => {
                    for (source_plan, data) in sources.iter().zip(&mut source_data) {
                        for root in &source_plan.roots {
                            let donat_ir::RootField::Select { alias, query } = root else {
                                continue;
                            };
                            if let Some(node) = data.get_mut(alias.as_str()) {
                                if let Err(e) = resolve_remote_joins(
                                    state,
                                    engine.as_ref(),
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
                    let data = assemble_multi_source_response(&response, source_data);
                    ok(json!({ "data": data }))
                }
                Err(e) => ok(query_error_json(e)),
            }
        }
        MultiSourcePlan::Mutation {
            source,
            roots,
            response,
        } => {
            let Some(source) = source else {
                drop(engine);
                let data = assemble_multi_source_response(&response, std::iter::empty());
                return ok(json!({ "data": data }));
            };
            let Some(runtime) = engine.runtimes.get(&source).cloned() else {
                return ok(error_json(
                    "unexpected",
                    format!("runtime for source '{source}' not found"),
                ));
            };
            let pool = match runtime {
                SourceRuntime::Sqlite { pool, settings, .. } => {
                    drop(engine);
                    return match state
                        .execute_sqlite_mutations_at(pool, settings, &roots)
                        .await
                    {
                        Ok(data) => ok(json!({
                            "data": assemble_multi_source_response(&response, [data])
                        })),
                        Err(e) => ok(sqlite_mutation_error_json(e)),
                    };
                }
                SourceRuntime::Mysql { pool, settings, .. } => {
                    let Some(catalog) = engine.catalogs.get(&source) else {
                        return ok(error_json(
                            "unexpected",
                            format!("catalog for source '{source}' not found"),
                        ));
                    };
                    let primary_keys: std::collections::HashMap<String, Vec<String>> = roots
                        .iter()
                        .filter_map(|root| {
                            let table = match root {
                                donat_ir::MutationRoot::Insert { insert, .. } => &insert.table,
                                donat_ir::MutationRoot::Update { update, .. } => &update.table,
                                donat_ir::MutationRoot::Delete { delete, .. } => &delete.table,
                                donat_ir::MutationRoot::FunctionCall { .. }
                                | donat_ir::MutationRoot::Typename { .. } => return None,
                            };
                            let key = format!("{}.{}", table.schema, table.name);
                            let primary_key = catalog
                                .tables
                                .get(&key)
                                .map(|info| info.primary_key.clone())
                                .unwrap_or_default();
                            Some((key, primary_key))
                        })
                        .collect();
                    drop(engine);
                    return match state
                        .execute_mysql_mutations_at(primary_keys, pool, settings, &roots)
                        .await
                    {
                        Ok(data) => ok(json!({
                            "data": assemble_multi_source_response(&response, [data])
                        })),
                        Err(e) => ok(mysql_mutation_error_json(e)),
                    };
                }
                SourceRuntime::Clickhouse { .. } => {
                    return ok(error_json(
                        "unexpected",
                        format!("mutations are not supported for source '{source}'"),
                    ));
                }
                SourceRuntime::Postgres { pool, .. } => pool,
            };
            // Pre-compute the per-field SQL and response keys, then run
            // everything inside one transaction.
            let fields: Vec<(String, String)> = roots
                .iter()
                .map(|root| {
                    let alias = match root {
                        donat_ir::MutationRoot::FunctionCall { alias, .. }
                        | donat_ir::MutationRoot::Insert { alias, .. }
                        | donat_ir::MutationRoot::Update { alias, .. }
                        | donat_ir::MutationRoot::Delete { alias, .. }
                        | donat_ir::MutationRoot::Typename { alias, .. } => alias.clone(),
                    };
                    (
                        alias,
                        donat_sqlgen::mutation_to_sql_opts(root, state.stringify_numerics),
                    )
                })
                .collect();
            drop(engine);
            let mut client = match pool.get().await {
                Ok(c) => c,
                Err(e) => {
                    return ok(error_json(
                        "unexpected",
                        format!("connection pool error: {e}"),
                    ));
                }
            };
            let tx = match client.transaction().await {
                Ok(tx) => tx,
                Err(e) => return ok(db_error_json(&e)),
            };
            let mut data = serde_json::Map::new();
            for (alias, sql) in fields {
                tracing::trace!(target: "donat::sql", %sql, "executing mutation");
                match tx.query_one(&sql, &[]).await {
                    Ok(row) => {
                        // Typename roots produce text, everything else json.
                        // A by-pk mutation that matches no row (e.g. blocked by
                        // the update/delete permission filter) yields a SQL
                        // NULL in column 0 — decode as Option so it becomes a
                        // JSON null, not a decode error.
                        let value = row
                            .try_get::<_, Option<Json>>(0)
                            .map(|o| o.unwrap_or(Json::Null))
                            .or_else(|_| {
                                row.try_get::<_, Option<String>>(0)
                                    .map(|o| o.map(Json::String).unwrap_or(Json::Null))
                            });
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
            let data = assemble_multi_source_response(&response, [Json::Object(data)]);
            ok(json!({ "data": data }))
        }
    }
}

/// Plan and run a self-contained SELECT for the given role, returning the
/// `data` object on success or a GraphQL error body on failure. Used to
/// resolve action output-object relationships into tracked tables (the target
/// is queried under the same session, so the role's permissions apply).
fn plan_internal_select_from_snapshot(
    engine: &Engine,
    session: &Session,
    doc: &graphql_parser::query::Document<'static, String>,
    variables: &JsonMap<String, Json>,
) -> Result<(String, Vec<donat_ir::RootField>, SourceRuntime), Json> {
    let compiled = engine
        .compiled
        .as_deref()
        .ok_or_else(|| error_json("unexpected", "engine schema snapshot is not initialized"))?;
    let source = engine
        .metadata
        .sources
        .iter()
        .find(|source| source.name == "default")
        .or_else(|| engine.metadata.sources.first())
        .ok_or_else(|| error_json("unexpected", "no default source"))?;
    let planner = compiled
        .source_planner(&engine.metadata, &engine.catalogs, &source.name)
        .map_err(|error| error.to_graphql())?;
    let plan = planner
        .plan(doc, None, variables, session)
        .map_err(|error| error.to_graphql())?;
    let roots = match plan {
        donat_schema::Plan::Query(roots) => roots,
        _ => return Err(error_json("unexpected", "internal query must be a select")),
    };
    let runtime = engine.runtimes.get(&source.name).cloned().ok_or_else(|| {
        error_json(
            "unexpected",
            format!("runtime for source '{}' not found", source.name),
        )
    })?;
    Ok((source.name.clone(), roots, runtime))
}

pub(crate) async fn execute_select_internal(
    state: &SharedState,
    engine: &Engine,
    session: &Session,
    query: &str,
    variables: &JsonMap<String, Json>,
) -> Result<Json, Json> {
    let doc = graphql_parser::parse_query::<String>(query)
        .map_err(|e| error_json("unexpected", format!("internal query parse error: {e}")))?
        .into_static();

    let (_, roots, runtime) = plan_internal_select_from_snapshot(engine, session, &doc, variables)?;
    let SourceRuntime::Postgres { pool, .. } = runtime else {
        return Err(error_json("unexpected", "no default source"));
    };
    let sql = donat_sqlgen::operation_to_sql_opts(&roots, state.stringify_numerics);
    let client = pool
        .get()
        .await
        .map_err(|e| error_json("unexpected", format!("connection pool error: {e}")))?;
    let row = client
        .query_one(&sql, &[])
        .await
        .map_err(|e| db_error_json(&e))?;
    let mut data: Json = row
        .try_get::<_, Json>(0)
        .map_err(|e| error_json("unexpected", format!("cannot decode result: {e}")))?;
    for root in &roots {
        if let donat_ir::RootField::Select { alias, query } = root {
            if let Some(node) = data.get_mut(alias.as_str()) {
                resolve_remote_joins(
                    state,
                    engine,
                    session,
                    &query.fields,
                    node,
                    &format!("$.selectionSet.{alias}"),
                )
                .await?;
            }
        }
    }
    Ok(data)
}

/// Render a document with every __typename selection removed.
pub(crate) fn normalize_for_allowlist(
    doc: &graphql_parser::query::Document<'static, String>,
) -> String {
    use graphql_parser::query::{Definition, Selection};
    fn strip(set: &mut graphql_parser::query::SelectionSet<'static, String>) {
        set.items
            .retain(|item| !matches!(item, Selection::Field(f) if f.name == "__typename"));
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
                    let is_intro =
                        f.name == "__schema" || f.name == "__type" || f.name == "__typename";
                    out.push((alias, is_intro));
                }
            }
        }
    }
    out
}

struct RemoteJoinEntry {
    object_pointer: String,
    variables: JsonMap<String, Json>,
}

struct RemoteJoinGroup<'a> {
    spec: &'a donat_ir::RemoteJoinSpec,
    client_field_path: String,
    field_alias: String,
    entries: Vec<RemoteJoinEntry>,
}

struct PreparedRemoteJoin {
    target: crate::remote::RemoteTarget,
    query: String,
    variables: JsonMap<String, Json>,
}

const REMOTE_JOIN_BATCH_SIZE: usize = 100;
const REMOTE_JOIN_MAX_IN_FLIGHT_BATCHES: usize = 4;

fn pointer_child(base: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{base}/{escaped}")
}

fn collect_remote_join_groups<'a>(
    fields: &'a [donat_ir::OutputField],
    node: &Json,
    object_pointer: &str,
    path: &str,
    groups: &mut Vec<RemoteJoinGroup<'a>>,
) {
    match node {
        Json::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_remote_join_groups(
                    fields,
                    item,
                    &pointer_child(object_pointer, &index.to_string()),
                    path,
                    groups,
                );
            }
        }
        Json::Object(map) => {
            for field in fields {
                match &field.value {
                    donat_ir::FieldValue::Object { query, .. }
                    | donat_ir::FieldValue::Array { query, .. } => {
                        if let Some(child) = map.get(field.alias.as_str()) {
                            collect_remote_join_groups(
                                &query.fields,
                                child,
                                &pointer_child(object_pointer, &field.alias),
                                &format!("{path}.selectionSet.{}", field.alias),
                                groups,
                            );
                        }
                    }
                    donat_ir::FieldValue::RemoteJoin { spec } => {
                        let client_field_path = format!("{path}.selectionSet.{}", field.alias);
                        let variables = spec
                            .variables
                            .iter()
                            .map(|(variable, hidden)| {
                                (
                                    variable.clone(),
                                    map.get(hidden.as_str()).cloned().unwrap_or(Json::Null),
                                )
                            })
                            .collect();
                        if let Some(group) = groups.iter_mut().find(|group| {
                            group.client_field_path == client_field_path
                                && group.spec.query == spec.query
                                && group.spec.schema == spec.schema
                        }) {
                            group.entries.push(RemoteJoinEntry {
                                object_pointer: object_pointer.to_string(),
                                variables,
                            });
                        } else {
                            groups.push(RemoteJoinGroup {
                                spec,
                                client_field_path,
                                field_alias: field.alias.clone(),
                                entries: vec![RemoteJoinEntry {
                                    object_pointer: object_pointer.to_string(),
                                    variables,
                                }],
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

fn rename_query_value_variables(
    value: &mut graphql_parser::query::Value<'static, String>,
    names: &std::collections::HashMap<String, String>,
) {
    use graphql_parser::query::Value;
    match value {
        Value::Variable(name) => {
            if let Some(replacement) = names.get(name) {
                *name = replacement.clone();
            }
        }
        Value::List(items) => {
            for item in items {
                rename_query_value_variables(item, names);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                rename_query_value_variables(value, names);
            }
        }
        _ => {}
    }
}

fn rename_query_directive_variables(
    directives: &mut [graphql_parser::query::Directive<'static, String>],
    names: &std::collections::HashMap<String, String>,
) {
    for directive in directives {
        for (_, value) in &mut directive.arguments {
            rename_query_value_variables(value, names);
        }
    }
}

fn rename_selection_variables(
    selection_set: &mut graphql_parser::query::SelectionSet<'static, String>,
    names: &std::collections::HashMap<String, String>,
) {
    use graphql_parser::query::Selection;
    for selection in &mut selection_set.items {
        match selection {
            Selection::Field(field) => {
                for (_, value) in &mut field.arguments {
                    rename_query_value_variables(value, names);
                }
                rename_query_directive_variables(&mut field.directives, names);
                rename_selection_variables(&mut field.selection_set, names);
            }
            Selection::InlineFragment(fragment) => {
                rename_query_directive_variables(&mut fragment.directives, names);
                rename_selection_variables(&mut fragment.selection_set, names);
            }
            Selection::FragmentSpread(fragment) => {
                rename_query_directive_variables(&mut fragment.directives, names);
            }
        }
    }
}

fn build_remote_join_batch(
    prepared: &[PreparedRemoteJoin],
) -> Option<(String, JsonMap<String, Json>)> {
    use graphql_parser::query::{Definition, OperationDefinition, Selection};

    let mut combined_doc = None;
    let mut combined_variables = JsonMap::new();
    let mut variable_definitions = vec![];
    let mut selections = vec![];

    for (index, request) in prepared.iter().enumerate() {
        let mut doc = graphql_parser::parse_query::<String>(&request.query)
            .ok()?
            .into_static();
        if doc
            .definitions
            .iter()
            .any(|definition| matches!(definition, Definition::Fragment(_)))
        {
            return None;
        }
        let query = doc.definitions.iter_mut().find_map(|definition| {
            let Definition::Operation(OperationDefinition::Query(query)) = definition else {
                return None;
            };
            Some(query)
        })?;
        if query.selection_set.items.len() != 1 {
            return None;
        }

        let names: std::collections::HashMap<_, _> = query
            .variable_definitions
            .iter()
            .map(|definition| {
                (
                    definition.name.clone(),
                    format!("__donat_rr_{index}_{}", definition.name),
                )
            })
            .collect();
        for definition in &mut query.variable_definitions {
            definition.name = names.get(&definition.name)?.clone();
        }
        rename_selection_variables(&mut query.selection_set, &names);
        let Selection::Field(field) = query.selection_set.items.first_mut()? else {
            return None;
        };
        field.alias = Some(format!("__donat_rr_{index}"));

        for (name, value) in &request.variables {
            combined_variables.insert(names.get(name)?.clone(), value.clone());
        }
        variable_definitions.extend(query.variable_definitions.clone());
        selections.extend(query.selection_set.items.clone());
        if combined_doc.is_none() {
            combined_doc = Some(doc);
        }
    }

    let mut combined_doc = combined_doc?;
    let combined_query = combined_doc.definitions.iter_mut().find_map(|definition| {
        let Definition::Operation(OperationDefinition::Query(query)) = definition else {
            return None;
        };
        Some(query)
    })?;
    combined_query.variable_definitions = variable_definitions;
    combined_query.selection_set.items = selections;
    Some((format!("{combined_doc}"), combined_variables))
}

async fn execute_remote_join_sequential(
    state: &SharedState,
    prepared: &[PreparedRemoteJoin],
    root_field: &str,
    batch_permits: &Arc<tokio::sync::Semaphore>,
) -> Result<Vec<Json>, Json> {
    let mut values = Vec::with_capacity(prepared.len());
    for request in prepared {
        let body = json!({
            "query": request.query,
            "variables": request.variables,
        });
        let permit = batch_permits
            .clone()
            .acquire_owned()
            .await
            .expect("remote batch semaphore is never closed");
        let (_, response) =
            crate::remote::forward(state, &request.target, &body, &HeaderMap::new()).await;
        drop(permit);
        if let Some(errors) = response.get("errors") {
            return Err(json!({ "errors": errors }));
        }
        values.push(
            response
                .pointer(&format!("/data/{root_field}"))
                .cloned()
                .unwrap_or(Json::Null),
        );
    }
    Ok(values)
}

async fn execute_remote_join_chunk(
    state: &SharedState,
    prepared: &[PreparedRemoteJoin],
    root_field: &str,
    batch_permits: Arc<tokio::sync::Semaphore>,
) -> Result<Vec<Json>, Json> {
    if prepared.len() == 1 {
        return execute_remote_join_sequential(state, prepared, root_field, &batch_permits).await;
    }
    let Some((query, variables)) = build_remote_join_batch(prepared) else {
        return execute_remote_join_sequential(state, prepared, root_field, &batch_permits).await;
    };
    let first = prepared.first().expect("non-empty remote join batch");
    let target = crate::remote::RemoteTarget {
        url: first.target.url.clone(),
        forward_client_headers: first.target.forward_client_headers,
        rewritten_query: Some(query.clone()),
        has_introspection: false,
        namespace: None,
        timeout_seconds: first.target.timeout_seconds,
    };
    let body = json!({ "query": query, "variables": variables });
    let permit = batch_permits
        .clone()
        .acquire_owned()
        .await
        .expect("remote batch semaphore is never closed");
    let (_, response) = crate::remote::forward(state, &target, &body, &HeaderMap::new()).await;
    drop(permit);
    if let Some(errors) = response.get("errors") {
        if remote_batch_is_validation_error(errors) {
            return execute_remote_join_sequential(state, prepared, root_field, &batch_permits)
                .await;
        }
        // Transport failures, timeouts, and resolver errors must not fan out
        // into N retries. Only a recognized GraphQL validation rejection says
        // that the upstream cannot execute our aliased batch shape.
        return Err(json!({
            "errors": restore_remote_join_error_paths(errors, root_field)
        }));
    }
    Ok((0..prepared.len())
        .map(|index| {
            response
                .pointer(&format!("/data/__donat_rr_{index}"))
                .cloned()
                .unwrap_or(Json::Null)
        })
        .collect())
}

fn restore_remote_join_error_paths(errors: &Json, root_field: &str) -> Json {
    let mut restored = errors.clone();
    let Some(items) = restored.as_array_mut() else {
        return restored;
    };
    for error in items {
        let Some(error) = error.as_object_mut() else {
            continue;
        };
        if let Some(path) = error.get_mut("path").and_then(Json::as_array_mut)
            && path
                .first()
                .and_then(Json::as_str)
                .is_some_and(|segment| segment.starts_with("__donat_rr_"))
        {
            path[0] = Json::String(root_field.to_string());
        }
        let extension_path = error
            .get("extensions")
            .and_then(Json::as_object)
            .and_then(|extensions| extensions.get("path"))
            .and_then(Json::as_str)
            .map(str::to_string);
        if let Some(path) = extension_path
            && let Some(start) = path.find("__donat_rr_")
        {
            let end = path[start..]
                .find(|character: char| character == '.' || character == '[')
                .map(|offset| start + offset)
                .unwrap_or(path.len());
            if let Some(extensions) = error.get_mut("extensions").and_then(Json::as_object_mut) {
                extensions.insert(
                    "path".to_string(),
                    Json::String(format!("{}{}{}", &path[..start], root_field, &path[end..])),
                );
            }
        }
    }
    restored
}

fn remote_batch_is_validation_error(errors: &Json) -> bool {
    let Some(errors) = errors.as_array() else {
        return false;
    };
    !errors.is_empty()
        && errors.iter().all(|error| {
            let Some(code) = error.pointer("/extensions/code").and_then(Json::as_str) else {
                return false;
            };
            matches!(
                code.to_ascii_lowercase().as_str(),
                "validation-failed"
                    | "validation_failed"
                    | "graphql_validation_failed"
                    | "graphql_validation_error"
            )
        })
}

async fn resolve_remote_join_group(
    state: &SharedState,
    engine: &Engine,
    session: &Session,
    group: &RemoteJoinGroup<'_>,
    batch_permits: Arc<tokio::sync::Semaphore>,
) -> Result<Vec<Json>, Json> {
    let doc = graphql_parser::parse_query::<String>(&group.spec.query)
        .map_err(|error| error_json("unexpected", format!("bad remote join: {error}")))?
        .into_static();

    let mut unique_variables: Vec<JsonMap<String, Json>> = vec![];
    let mut unique_indexes: HashMap<String, usize> = HashMap::new();
    let mut entry_to_unique = Vec::with_capacity(group.entries.len());
    for entry in &group.entries {
        let key = serde_json::to_string(&entry.variables)
            .expect("remote relationship variables always serialize");
        if let Some(index) = unique_indexes.get(&key).copied() {
            entry_to_unique.push(index);
        } else {
            let index = unique_variables.len();
            unique_indexes.insert(key, index);
            entry_to_unique.push(index);
            unique_variables.push(entry.variables.clone());
        }
    }

    let mut prepared = Vec::with_capacity(unique_variables.len());
    for mut variables in unique_variables {
        let matched = crate::remote::match_remote_with(engine, session, &doc, &mut variables, true);
        let target = match matched {
            Some(Ok(target)) => target,
            Some(Err(error)) => {
                let server_root = format!("$.selectionSet.{}", group.spec.root_field);
                let rewritten = match error.path.strip_prefix(&server_root) {
                    Some(rest) => format!("{}{rest}", group.client_field_path),
                    None => group.client_field_path.clone(),
                };
                return Err(json!({
                    "errors": [{
                        "extensions": { "path": rewritten, "code": error.code },
                        "message": error.message,
                    }]
                }));
            }
            None => return Ok(vec![Json::Null; group.entries.len()]),
        };
        let query = target
            .rewritten_query
            .clone()
            .unwrap_or_else(|| group.spec.query.clone());
        prepared.push(PreparedRemoteJoin {
            target,
            query,
            variables,
        });
    }

    let mut unique_values = Vec::with_capacity(prepared.len());
    let window_size = REMOTE_JOIN_BATCH_SIZE * REMOTE_JOIN_MAX_IN_FLIGHT_BATCHES;
    for window in prepared.chunks(window_size) {
        let results = join_all(window.chunks(REMOTE_JOIN_BATCH_SIZE).map(|chunk| {
            execute_remote_join_chunk(state, chunk, &group.spec.root_field, batch_permits.clone())
        }))
        .await;
        for result in results {
            unique_values.extend(result?);
        }
    }

    Ok(entry_to_unique
        .into_iter()
        .map(|index| unique_values[index].clone())
        .collect())
}

fn strip_remote_join_hidden_fields(fields: &[donat_ir::OutputField], node: &mut Json) {
    match node {
        Json::Array(items) => {
            for item in items {
                strip_remote_join_hidden_fields(fields, item);
            }
        }
        Json::Object(map) => {
            for field in fields {
                match &field.value {
                    donat_ir::FieldValue::Object { query, .. }
                    | donat_ir::FieldValue::Array { query, .. } => {
                        if let Some(child) = map.get_mut(field.alias.as_str()) {
                            strip_remote_join_hidden_fields(&query.fields, child);
                        }
                    }
                    _ => {}
                }
            }
            map.retain(|key, _| !key.starts_with("__rr__"));
        }
        _ => {}
    }
}

/// Fill RemoteJoin placeholders with one upstream GraphQL operation per
/// relationship selection, deduplicating repeated join keys, then strip the
/// hidden "__rr__" columns.
fn resolve_remote_joins<'a>(
    state: &'a SharedState,
    engine: &'a Engine,
    session: &'a Session,
    fields: &'a [donat_ir::OutputField],
    node: &'a mut Json,
    path: &'a str,
) -> futures_util::future::BoxFuture<'a, Result<(), Json>> {
    Box::pin(async move {
        let mut groups = vec![];
        collect_remote_join_groups(fields, node, "", path, &mut groups);
        let batch_permits = Arc::new(tokio::sync::Semaphore::new(
            REMOTE_JOIN_MAX_IN_FLIGHT_BATCHES,
        ));
        for window in groups.chunks(REMOTE_JOIN_MAX_IN_FLIGHT_BATCHES) {
            let resolved = join_all(window.iter().map(|group| {
                resolve_remote_join_group(state, engine, session, group, batch_permits.clone())
            }))
            .await;
            for (group, values) in window.iter().zip(resolved) {
                for (entry, value) in group.entries.iter().zip(values?) {
                    let Some(Json::Object(object)) = node.pointer_mut(&entry.object_pointer) else {
                        continue;
                    };
                    object.insert(group.field_alias.clone(), value);
                }
            }
        }
        strip_remote_join_hidden_fields(fields, node);
        Ok(())
    })
}

fn ok(body: Json) -> (axum::http::StatusCode, Json) {
    (axum::http::StatusCode::OK, body)
}

/// Map Postgres errors onto Donat v2 error codes/messages.
/// Map a backend read failure to the GraphQL error body. The Postgres
/// variants reproduce the exact bodies the inline query path produced before
/// the multi-backend dispatch was introduced.
fn query_error_json(e: QueryError) -> Json {
    match e {
        QueryError::NoDefaultSource => error_json("unexpected", "no default source"),
        QueryError::Pool(msg) => error_json("unexpected", format!("connection pool error: {msg}")),
        QueryError::Decode(msg) => error_json("unexpected", format!("cannot decode result: {msg}")),
        QueryError::Postgres(err) => db_error_json(&err),
        QueryError::Sqlite(msg) => error_json("data-exception", msg),
        QueryError::Mysql(msg) => error_json("data-exception", msg),
        QueryError::Clickhouse(msg) => error_json("data-exception", msg),
    }
}
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
        "23505" => (
            "constraint-violation",
            format!("Uniqueness violation. {}", db.message()),
        ),
        "23503" => (
            "constraint-violation",
            format!("Foreign key violation. {}", db.message()),
        ),
        "23502" => (
            "constraint-violation",
            format!("Not-NULL violation. {}", db.message()),
        ),
        _ => ("data-exception", db.message().to_string()),
    };
    json!({
        "errors": [{
            "extensions": { "path": "$", "code": code },
            "message": message,
        }]
    })
}

/// Map a SQLite mutation failure to the GraphQL error body. `CheckViolation`
/// reproduces the exact `permission-error` shape the Postgres path produces
/// from `donat.check_violation()` (SQLSTATE 23514), so a violated check looks
/// identical regardless of backend.
fn sqlite_mutation_error_json(e: crate::state::SqliteMutationError) -> Json {
    use crate::state::SqliteMutationError as E;
    match e {
        E::CheckViolation { path } => json!({
            "errors": [{
                "extensions": { "path": path, "code": "permission-error" },
                "message": "check constraint of an insert/update permission has failed",
            }]
        }),
        E::Sqlite(msg) => error_json("data-exception", msg),
        E::Other(msg) => error_json("unexpected", msg),
    }
}

/// Map a MySQL mutation failure to the GraphQL error body. `CheckViolation`
/// reproduces the exact `permission-error` shape the Postgres/SQLite paths
/// produce, so a violated check looks identical regardless of backend.
fn mysql_mutation_error_json(e: crate::state::MysqlMutationError) -> Json {
    use crate::state::MysqlMutationError as E;
    match e {
        E::CheckViolation { path } => json!({
            "errors": [{
                "extensions": { "path": path, "code": "permission-error" },
                "message": "check constraint of an insert/update permission has failed"
            }]
        }),
        E::Mysql(msg) => error_json("data-exception", msg),
        E::Other(msg) => error_json("unexpected", msg),
    }
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        graphql_parser::parse_query::<String>(q)
            .unwrap()
            .into_static()
    }

    #[test]
    fn untrusted_request_falls_back_to_unauthorized_role() {
        let h = headers(&[("x-donat-role", "editor"), ("x-donat-user-id", "1")]);
        let s = session_from_headers(&h, Some("anonymous"), false).unwrap();
        assert_eq!(s.role, "anonymous");
        assert!(s.vars.is_empty(), "untrusted headers must be ignored");
    }

    #[test]
    fn untrusted_request_without_unauthorized_role_is_denied() {
        let e = session_from_headers(&HeaderMap::new(), None, false).unwrap_err();
        assert_eq!(
            e.pointer("/errors/0/extensions/code"),
            Some(&json!("access-denied"))
        );
        assert_eq!(
            e.pointer("/errors/0/message"),
            Some(&json!("x-donat-admin-secret required, but not found"))
        );
    }

    #[test]
    fn trusted_request_collects_x_donat_vars() {
        let h = headers(&[
            ("x-donat-role", "editor"),
            ("X-Donat-User-Id", "7"),
            ("x-donat-admin-secret", "shh"),
            ("content-type", "application/json"),
        ]);
        let s = session_from_headers(&h, None, true).unwrap();
        assert_eq!(s.role, "editor");
        assert_eq!(s.vars.get("x-donat-user-id").map(String::as_str), Some("7"));
        assert!(!s.vars.contains_key("x-donat-admin-secret"));
        assert!(!s.vars.contains_key("content-type"));
        assert!(!s.backend_request);
    }

    #[test]
    fn trusted_request_collects_x_hasura_vars() {
        let h = headers(&[
            ("X-Hasura-Role", "editor"),
            ("X-Hasura-User-Id", "7"),
            ("X-Hasura-Admin-Secret", "ignored"),
        ]);
        let s = session_from_headers(&h, None, true).unwrap();
        assert_eq!(s.role, "editor");
        assert_eq!(
            s.vars.get("x-hasura-user-id").map(String::as_str),
            Some("7")
        );
        assert_eq!(
            s.vars.get("x-hasura-role").map(String::as_str),
            Some("editor")
        );
        assert_eq!(
            s.vars.get("x-donat-role").map(String::as_str),
            Some("editor")
        );
        assert!(!s.vars.contains_key("x-donat-admin-secret"));
        assert!(!s.vars.contains_key("x-hasura-admin-secret"));
    }

    #[test]
    fn x_donat_role_wins_over_x_hasura_role() {
        let h = headers(&[
            ("X-Hasura-Role", "hasura_user"),
            ("X-Donat-Role", "donat_user"),
        ]);
        let s = session_from_headers(&h, None, true).unwrap();
        assert_eq!(s.role, "donat_user");
        assert_eq!(
            s.vars.get("x-hasura-role").map(String::as_str),
            Some("donat_user")
        );
    }

    #[test]
    fn trusted_request_requires_a_role() {
        // No admin role: a trusted request with no X-Donat-Role is denied.
        let e =
            session_from_headers(&headers(&[("x-donat-user-id", "7")]), None, true).unwrap_err();
        assert_eq!(
            e.pointer("/errors/0/message"),
            Some(&json!(
                "x-donat-role header is required (this engine has no admin role)"
            ))
        );
    }

    #[test]
    fn backend_only_permissions_header_parsing() {
        let with = |v: &str| {
            session_from_headers(
                &headers(&[
                    ("x-donat-role", "u"),
                    ("x-donat-use-backend-only-permissions", v),
                ]),
                None,
                true,
            )
        };
        assert!(with("YES").unwrap().backend_request);
        assert!(!with("f").unwrap().backend_request);
        let e = with("maybe").unwrap_err();
        assert_eq!(
            e.pointer("/errors/0/extensions/code"),
            Some(&json!("bad-request"))
        );
        assert_eq!(
            e.pointer("/errors/0/message"),
            Some(&json!(
                "x-donat-use-backend-only-permissions:  Not a valid boolean text. True values are [\"true\",\"t\",\"yes\",\"y\"] and  False values are [\"false\",\"f\",\"no\",\"n\"]. All values are case insensitive"
            ))
        );
    }

    #[test]
    fn allowlist_comparison_ignores_typename_only() {
        let listed = parse("query getAuthors { author { id name } }");
        let with_typename = parse("query getAuthors { __typename author { id __typename name } }");
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
    fn remote_join_cleanup_keeps_json_keys_outside_planner_rows() {
        use donat_ir::{FieldValue, OutputField};

        let fields = vec![OutputField {
            alias: "payload".to_string(),
            value: FieldValue::Column {
                column: "payload".to_string(),
                pg_type: "jsonb".to_string(),
            },
        }];
        let mut node = json!({
            "__rr__id": 7,
            "payload": { "__rr__user_key": "preserve" }
        });

        strip_remote_join_hidden_fields(&fields, &mut node);

        assert_eq!(
            node,
            json!({
                "payload": { "__rr__user_key": "preserve" }
            })
        );
    }

    #[test]
    fn remote_batch_errors_restore_original_root_paths() {
        let restored = restore_remote_join_error_paths(
            &json!([{
                "path": ["__donat_rr_1", "name"],
                "extensions": {
                    "path": "$.selectionSet.__donat_rr_1.selectionSet.name",
                    "code": "unexpected"
                },
                "message": "remote resolver failed"
            }]),
            "message",
        );

        assert_eq!(
            restored,
            json!([{
                "path": ["message", "name"],
                "extensions": {
                    "path": "$.selectionSet.message.selectionSet.name",
                    "code": "unexpected"
                },
                "message": "remote resolver failed"
            }])
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

    #[test]
    fn internal_action_select_reuses_the_compiled_default_source() {
        use std::collections::{BTreeMap, HashMap};

        use crate::state::{Engine, SourceRuntime};
        use donat_catalog::{Catalog, ColumnInfo, TableInfo};
        use donat_metadata::Metadata;

        let metadata: Metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [{
                "name": "default",
                "kind": "sqlite",
                "configuration": {
                    "connection_info": { "database_url": "/tmp/action.sqlite" }
                },
                "tables": [{
                    "table": { "schema": "public", "name": "item" },
                    "configuration": { "custom_name": "public_item" },
                    "select_permissions": [{
                        "role": "user",
                        "permission": { "columns": ["id"], "filter": {} }
                    }]
                }]
            }]
        }))
        .expect("metadata deserializes");
        let catalog = Catalog {
            tables: BTreeMap::from([(
                "public.item".to_string(),
                TableInfo {
                    schema: "public".to_string(),
                    name: "item".to_string(),
                    columns: vec![ColumnInfo {
                        name: "id".to_string(),
                        pg_type: "int8".to_string(),
                        native_type: None,
                        nullable: false,
                        has_default: false,
                    }],
                    primary_key: vec!["id".to_string()],
                    foreign_keys: vec![],
                },
            )]),
            functions: BTreeMap::new(),
        };
        let engine = Engine::compiled(
            metadata,
            HashMap::from([("default".to_string(), catalog)]),
            HashMap::from([(
                "default".to_string(),
                SourceRuntime::Sqlite {
                    path: "/tmp/action.sqlite".to_string(),
                    pool: Arc::new(crate::state::SqlitePool::new(
                        "/tmp/action.sqlite".to_string(),
                        8,
                    )),
                    settings: crate::state::RuntimePoolSettings::default(),
                },
            )]),
            true,
        )
        .expect("engine compiles");
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };

        for _ in 0..2 {
            let (source, roots, runtime) = plan_internal_select_from_snapshot(
                &engine,
                &session,
                &parse("{ public_item { id } }"),
                &JsonMap::new(),
            )
            .expect("internal action select plans from the snapshot");
            assert_eq!(source, "default");
            assert_eq!(roots.len(), 1);
            assert!(matches!(runtime, SourceRuntime::Sqlite { .. }));
        }
    }

    fn shared_state(engine: Arc<Engine>) -> SharedState {
        Arc::new(AppState {
            engine: tokio::sync::RwLock::new(engine),
            default_url: "postgres://unused".to_string(),
            admin_secret: None,
            unauthorized_role: None,
            stringify_numerics: false,
            infer_function_permissions: true,
            jwt: None,
            auth_hook: None,
            http: reqwest::Client::new(),
            allowlist_enabled: false,
            subscription_permits: Arc::new(tokio::sync::Semaphore::new(1_000)),
        })
    }

    fn empty_metadata() -> donat_metadata::Metadata {
        serde_json::from_value(json!({ "version": 3, "sources": [] }))
            .expect("empty metadata deserializes")
    }

    #[tokio::test]
    async fn remote_join_uses_request_snapshot_after_publication() {
        use donat_ir::{FieldValue, OutputField, RemoteJoinSpec};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote server");
        let address = listener.local_addr().expect("remote server address");
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(|| async {
                axum::Json(json!({ "data": { "message": { "name": "old" } } }))
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("remote server");
        });

        let old_metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [],
            "remote_schemas": [{
                "name": "messages",
                "definition": { "url": format!("http://{address}/") },
                "permissions": [{
                    "role": "user",
                    "definition": {
                        "schema": "type Query { message(id: Int!): Message } type Message { name: String }"
                    }
                }]
            }]
        }))
        .expect("remote metadata deserializes");
        let request_snapshot = Arc::new(Engine::bootstrap(old_metadata));
        let state = shared_state(Arc::new(Engine::bootstrap(empty_metadata())));
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };
        let fields = vec![OutputField {
            alias: "joined".to_string(),
            value: FieldValue::RemoteJoin {
                spec: RemoteJoinSpec {
                    schema: "messages".to_string(),
                    query: "query($v0: Int!) { message(id: $v0) { name } }".to_string(),
                    variables: vec![("v0".to_string(), "__rr__id".to_string())],
                    root_field: "message".to_string(),
                },
            },
        }];
        let mut node = json!({ "__rr__id": 7, "joined": null });

        resolve_remote_joins(
            &state,
            request_snapshot.as_ref(),
            &session,
            &fields,
            &mut node,
            "$.selectionSet.item",
        )
        .await
        .expect("remote join resolves from request snapshot");

        assert_eq!(node, json!({ "joined": { "name": "old" } }));
        server.abort();
    }

    #[tokio::test]
    async fn remote_join_batches_rows_and_deduplicates_join_keys() {
        use axum::extract::State;
        use donat_ir::{FieldValue, OutputField, RemoteJoinSpec};

        let requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote server");
        let address = listener.local_addr().expect("remote server address");
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(
                    |State(requests): State<Arc<AtomicUsize>>,
                     axum::Json(body): axum::Json<Json>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        let query = body.get("query").and_then(Json::as_str).unwrap_or_default();
                        if query.contains("__donat_rr_0") {
                            axum::Json(json!({
                                "data": {
                                    "__donat_rr_0": { "name": "seven" },
                                    "__donat_rr_1": { "name": "eight" }
                                }
                            }))
                        } else {
                            let id = body.pointer("/variables/v0").and_then(Json::as_i64);
                            let name = if id == Some(7) { "seven" } else { "eight" };
                            axum::Json(json!({ "data": { "message": { "name": name } } }))
                        }
                    },
                ),
            )
            .with_state(requests.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("remote server");
        });

        let metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [],
            "remote_schemas": [{
                "name": "messages",
                "definition": { "url": format!("http://{address}/") },
                "permissions": [{
                    "role": "user",
                    "definition": {
                        "schema": "type Query { message(id: Int!): Message } type Message { name: String }"
                    }
                }]
            }]
        }))
        .expect("remote metadata deserializes");
        let engine = Engine::bootstrap(metadata);
        let state = shared_state(Arc::new(Engine::bootstrap(empty_metadata())));
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };
        let fields = vec![OutputField {
            alias: "joined".to_string(),
            value: FieldValue::RemoteJoin {
                spec: RemoteJoinSpec {
                    schema: "messages".to_string(),
                    query: "query($v0: Int!) { message(id: $v0) { name } }".to_string(),
                    variables: vec![("v0".to_string(), "__rr__id".to_string())],
                    root_field: "message".to_string(),
                },
            },
        }];
        let mut node = json!([
            { "__rr__id": 7, "joined": null },
            { "__rr__id": 7, "joined": null },
            { "__rr__id": 8, "joined": null }
        ]);

        resolve_remote_joins(
            &state,
            &engine,
            &session,
            &fields,
            &mut node,
            "$.selectionSet.items",
        )
        .await
        .expect("remote joins resolve");

        assert_eq!(
            node,
            json!([
                { "joined": { "name": "seven" } },
                { "joined": { "name": "seven" } },
                { "joined": { "name": "eight" } }
            ])
        );
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn remote_join_groups_share_a_global_batch_limit() {
        use axum::extract::State;
        use donat_ir::{FieldValue, OutputField, RemoteJoinSpec};

        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote server");
        let address = listener.local_addr().expect("remote server address");
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(
                    |State((active, maximum, requests)): State<(
                        Arc<AtomicUsize>,
                        Arc<AtomicUsize>,
                        Arc<AtomicUsize>,
                    )>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(current, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        axum::Json(json!({ "data": {} }))
                    },
                ),
            )
            .with_state((active, maximum.clone(), requests.clone()));
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("remote server");
        });

        let metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [],
            "remote_schemas": [{
                "name": "messages",
                "definition": { "url": format!("http://{address}/") },
                "permissions": [{
                    "role": "user",
                    "definition": {
                        "schema": "type Query { message(id: Int!): Message } type Message { name: String }"
                    }
                }]
            }]
        }))
        .expect("remote metadata deserializes");
        let engine = Engine::bootstrap(metadata);
        let state = shared_state(Arc::new(Engine::bootstrap(empty_metadata())));
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };
        let fields: Vec<OutputField> = ["one", "two", "three", "four"]
            .into_iter()
            .map(|alias| OutputField {
                alias: alias.to_string(),
                value: FieldValue::RemoteJoin {
                    spec: RemoteJoinSpec {
                        schema: "messages".to_string(),
                        query: "query($v0: Int!) { message(id: $v0) { name } }".to_string(),
                        variables: vec![("v0".to_string(), format!("__rr__{alias}"))],
                        root_field: "message".to_string(),
                    },
                },
            })
            .collect();
        let mut node = Json::Array(
            (0..=REMOTE_JOIN_BATCH_SIZE)
                .map(|id| {
                    json!({
                        "__rr__one": id,
                        "__rr__two": id,
                        "__rr__three": id,
                        "__rr__four": id,
                    })
                })
                .collect(),
        );

        resolve_remote_joins(
            &state,
            &engine,
            &session,
            &fields,
            &mut node,
            "$.selectionSet.items",
        )
        .await
        .expect("remote joins resolve");

        assert_eq!(requests.load(Ordering::SeqCst), 8);
        assert!(
            maximum.load(Ordering::SeqCst) <= REMOTE_JOIN_MAX_IN_FLIGHT_BATCHES,
            "all relationship groups share the same remote batch limit"
        );
        server.abort();
    }

    #[tokio::test]
    async fn remote_join_batch_timeout_does_not_retry_each_key() {
        use axum::extract::State;
        use donat_ir::{FieldValue, OutputField, RemoteJoinSpec};

        let requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote server");
        let address = listener.local_addr().expect("remote server address");
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(|State(requests): State<Arc<AtomicUsize>>| async move {
                    requests.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    axum::Json(json!({ "data": { "message": null } }))
                }),
            )
            .with_state(requests.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("remote server");
        });

        let metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [],
            "remote_schemas": [{
                "name": "messages",
                "definition": {
                    "url": format!("http://{address}/"),
                    "timeout_seconds": 1
                },
                "permissions": [{
                    "role": "user",
                    "definition": {
                        "schema": "type Query { message(id: Int!): Message } type Message { name: String }"
                    }
                }]
            }]
        }))
        .expect("remote metadata deserializes");
        let engine = Engine::bootstrap(metadata);
        let state = shared_state(Arc::new(Engine::bootstrap(empty_metadata())));
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };
        let fields = vec![OutputField {
            alias: "joined".to_string(),
            value: FieldValue::RemoteJoin {
                spec: RemoteJoinSpec {
                    schema: "messages".to_string(),
                    query: "query($v0: Int!) { message(id: $v0) { name } }".to_string(),
                    variables: vec![("v0".to_string(), "__rr__id".to_string())],
                    root_field: "message".to_string(),
                },
            },
        }];
        let mut node = json!([
            { "__rr__id": 7, "joined": null },
            { "__rr__id": 8, "joined": null }
        ]);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            resolve_remote_joins(
                &state,
                &engine,
                &session,
                &fields,
                &mut node,
                "$.selectionSet.items",
            ),
        )
        .await
        .expect("timeout response must not trigger sequential retries");

        assert!(result.is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn remote_join_batch_validation_error_falls_back_sequentially() {
        use axum::extract::State;
        use donat_ir::{FieldValue, OutputField, RemoteJoinSpec};

        let requests = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind remote server");
        let address = listener.local_addr().expect("remote server address");
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::post(
                    |State(requests): State<Arc<AtomicUsize>>,
                     axum::Json(body): axum::Json<Json>| async move {
                        requests.fetch_add(1, Ordering::SeqCst);
                        let query = body.get("query").and_then(Json::as_str).unwrap_or_default();
                        if query.contains("__donat_rr_0") {
                            return axum::Json(json!({
                                "errors": [{
                                    "extensions": { "code": "GRAPHQL_VALIDATION_FAILED" },
                                    "message": "aliased batches are not supported"
                                }]
                            }));
                        }
                        let id = body.pointer("/variables/v0").and_then(Json::as_i64);
                        let name = if id == Some(7) { "seven" } else { "eight" };
                        axum::Json(json!({ "data": { "message": { "name": name } } }))
                    },
                ),
            )
            .with_state(requests.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("remote server");
        });

        let metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [],
            "remote_schemas": [{
                "name": "messages",
                "definition": { "url": format!("http://{address}/") },
                "permissions": [{
                    "role": "user",
                    "definition": {
                        "schema": "type Query { message(id: Int!): Message } type Message { name: String }"
                    }
                }]
            }]
        }))
        .expect("remote metadata deserializes");
        let engine = Engine::bootstrap(metadata);
        let state = shared_state(Arc::new(Engine::bootstrap(empty_metadata())));
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };
        let fields = vec![OutputField {
            alias: "joined".to_string(),
            value: FieldValue::RemoteJoin {
                spec: RemoteJoinSpec {
                    schema: "messages".to_string(),
                    query: "query($v0: Int!) { message(id: $v0) { name } }".to_string(),
                    variables: vec![("v0".to_string(), "__rr__id".to_string())],
                    root_field: "message".to_string(),
                },
            },
        }];
        let mut node = json!([
            { "__rr__id": 7, "joined": null },
            { "__rr__id": 8, "joined": null }
        ]);

        resolve_remote_joins(
            &state,
            &engine,
            &session,
            &fields,
            &mut node,
            "$.selectionSet.items",
        )
        .await
        .expect("recognized validation error falls back");

        assert_eq!(
            node,
            json!([
                { "joined": { "name": "seven" } },
                { "joined": { "name": "eight" } }
            ])
        );
        assert_eq!(requests.load(Ordering::SeqCst), 3);
        server.abort();
    }

    #[tokio::test]
    async fn action_relationship_uses_request_snapshot_after_publication() {
        use std::collections::BTreeMap;

        use donat_catalog::{Catalog, ColumnInfo, TableInfo};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind action server");
        let address = listener.local_addr().expect("action server address");
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(|| async { axum::Json(json!({ "id": 7 })) }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("action server");
        });

        let metadata = serde_json::from_value(json!({
            "version": 3,
            "sources": [{
                "name": "default",
                "kind": "postgres",
                "configuration": {
                    "connection_info": {
                        "database_url": "postgres://postgres:postgres@127.0.0.1:1/unused"
                    }
                },
                "tables": [{
                    "table": { "schema": "public", "name": "user" },
                    "select_permissions": [{
                        "role": "user",
                        "permission": { "columns": ["id"], "filter": {} }
                    }]
                }]
            }],
            "actions": [{
                "name": "lookup",
                "definition": {
                    "kind": "synchronous",
                    "type": "query",
                    "handler": format!("http://{address}/"),
                    "output_type": "Lookup"
                },
                "permissions": [{ "role": "user" }]
            }],
            "custom_types": {
                "objects": [{
                    "name": "Lookup",
                    "fields": [{ "name": "id", "type": "Int!" }],
                    "relationships": [{
                        "name": "user",
                        "type": "object",
                        "remote_table": { "schema": "public", "name": "user" },
                        "field_mapping": { "id": "id" }
                    }]
                }]
            }
        }))
        .expect("action metadata deserializes");
        let catalog = Catalog {
            tables: BTreeMap::from([(
                "public.user".to_string(),
                TableInfo {
                    schema: "public".to_string(),
                    name: "user".to_string(),
                    columns: vec![ColumnInfo {
                        name: "id".to_string(),
                        pg_type: "int8".to_string(),
                        native_type: None,
                        nullable: false,
                        has_default: false,
                    }],
                    primary_key: vec!["id".to_string()],
                    foreign_keys: vec![],
                },
            )]),
            functions: BTreeMap::new(),
        };
        let pool = crate::state::make_pool(
            "postgres://postgres:postgres@127.0.0.1:1/unused?connect_timeout=1",
        )
        .expect("pool constructs");
        let request_snapshot = Arc::new(
            Engine::compiled(
                metadata,
                HashMap::from([("default".to_string(), catalog)]),
                HashMap::from([(
                    "default".to_string(),
                    SourceRuntime::Postgres {
                        url: "postgres://postgres:postgres@127.0.0.1:1/unused".to_string(),
                        pool,
                        settings: crate::state::RuntimePoolSettings::default(),
                    },
                )]),
                true,
            )
            .expect("action snapshot compiles"),
        );
        let state = shared_state(Arc::new(Engine::bootstrap(empty_metadata())));
        let doc = parse("{ lookup { id user { id } } }");
        let ctx = crate::action::match_action(&request_snapshot.metadata, &doc, None)
            .expect("action matches old snapshot");
        let session = Session {
            role: "user".to_string(),
            vars: HashMap::new(),
            backend_request: false,
        };

        let (_, body) = crate::action::resolve(
            &state,
            request_snapshot,
            &session,
            &ctx,
            &doc,
            &JsonMap::new(),
            None,
            &HeaderMap::new(),
        )
        .await;

        assert!(
            body.pointer("/errors/0/message")
                .and_then(Json::as_str)
                .is_some_and(|message| message.starts_with("connection pool error:")),
            "unexpected body: {body}"
        );
        server.abort();
    }
}
