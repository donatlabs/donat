//! RESTified endpoints (`/api/rest/<url>`).
//!
//! Each `rest_endpoints` entry maps an HTTP method + URL template to a saved
//! GraphQL operation stored in `query_collections`. A request is turned into
//! GraphQL variables (path params, query string, JSON body) and run through
//! the existing pipeline [`crate::gql::execute_full`] — no new permission or
//! SQL logic. The GraphQL `data` object is returned directly as the body.
//!
//! Error bodies use a small v2-style shape `{"code": ..., "error": ...}`:
//! - unknown endpoint -> 404 `{"code":"not-found","error":"endpoint not found"}`
//! - method not allowed -> 405 `{"code":"method-not-allowed","error":...}`
//! - variable coercion failure -> 400 `{"code":"bad-request","error":...}`
//! - misconfigured endpoint -> 500 `{"code":"unexpected","error":...}`
//!
//! GraphQL execution errors (permission, validation, ...) are returned as
//! `execute_full` produced them (the `{"errors":[...]}` body and its status).

use std::collections::HashMap;

use axum::{
    extract::{RawQuery, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use serde_json::{Map as JsonMap, Value as Json, json};

use donat_metadata::RestEndpoint;

use crate::{gql, state::SharedState};

/// A REST error body: `{"code": ..., "error": ...}`.
fn rest_error(code: &str, message: impl Into<String>) -> Json {
    json!({ "code": code, "error": message.into() })
}

/// Dispatch handler for every method under `/api/rest/<*path>`.
pub async fn dispatch(
    State(state): State<SharedState>,
    method: Method,
    uri: Uri,
    RawQuery(raw_query): RawQuery,
    headers: HeaderMap,
    body: Option<axum::Json<Json>>,
) -> impl IntoResponse {
    // Role is mandatory, exactly like /v1/graphql.
    let session = match gql::resolve_session(&state, &headers).await {
        Ok(s) => s,
        Err((status, errors)) => return (status, axum::Json(errors)).into_response(),
    };

    // The path after `/api/rest/`.
    let full = uri.path();
    let rest_path = full.strip_prefix("/api/rest/").unwrap_or("");
    let rest_path = rest_path.trim_matches('/');

    let engine = state.engine.read().await;

    // Resolve the matching endpoint. The most specific (most literal) URL
    // template wins, so a literal route is not shadowed by an earlier
    // parameterized one; 404 (no url match) is distinguished from 405 (url
    // matched, wrong method).
    let routing = select_endpoint(&engine.metadata.rest_endpoints, method.as_str(), rest_path);

    let (endpoint, path_params) = match routing.chosen {
        Some((ep, params)) => (ep.clone(), params),
        None => {
            if routing.url_matched {
                let allow = routing.allowed_methods.join(", ");
                return (
                    StatusCode::METHOD_NOT_ALLOWED,
                    axum::Json(rest_error(
                        "method-not-allowed",
                        format!("method {} not allowed; allowed: {allow}", method.as_str()),
                    )),
                )
                    .into_response();
            }
            return (
                StatusCode::NOT_FOUND,
                axum::Json(rest_error("not-found", "endpoint not found")),
            )
                .into_response();
        }
    };

    // Resolve the saved query text from query_collections.
    let query_text = engine
        .metadata
        .query_collections
        .iter()
        .find(|c| c.name == endpoint.definition.query.collection_name)
        .and_then(|c| {
            c.definition
                .queries
                .iter()
                .find(|q| q.name == endpoint.definition.query.query_name)
        })
        .map(|q| q.query.clone());
    let query_text = match query_text {
        Some(q) => q,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(rest_error(
                    "unexpected",
                    format!(
                        "rest endpoint '{}' references missing query '{}' in collection '{}'",
                        endpoint.name,
                        endpoint.definition.query.query_name,
                        endpoint.definition.query.collection_name
                    ),
                )),
            )
                .into_response();
        }
    };
    drop(engine);

    // Read the variable definitions from the saved query.
    let var_defs = match variable_definitions(&query_text) {
        Ok(defs) => defs,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(rest_error(
                    "unexpected",
                    format!(
                        "rest endpoint '{}' has an unparsable saved query: {e}",
                        endpoint.name
                    ),
                )),
            )
                .into_response();
        }
    };

    let query_params = parse_query_string(raw_query.as_deref().unwrap_or(""));
    let body_obj = body
        .and_then(|axum::Json(v)| v.as_object().cloned())
        .unwrap_or_default();

    let variables = match build_variables(&var_defs, &path_params, &query_params, &body_obj) {
        Ok(vars) => vars,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(rest_error("bad-request", msg)),
            )
                .into_response();
        }
    };

    let gql_body = json!({
        "query": query_text,
        "variables": variables,
        "operationName": Json::Null,
    });

    let (status, resp) = gql::execute_full(&state, &session, &gql_body, false, &headers).await;

    // Success (data, no errors) -> unwrap the data object; otherwise pass
    // through the engine's status + error body unchanged.
    if resp.get("errors").is_none() {
        if let Some(data) = resp.get("data") {
            return (StatusCode::OK, axum::Json(data.clone())).into_response();
        }
    }
    (status, axum::Json(resp)).into_response()
}

/// The declared GraphQL scalar kind a request value must be coerced to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarKind {
    Int,
    Float,
    Boolean,
    /// String, ID, or any custom scalar — kept as a JSON string.
    Other,
}

/// A declared operation variable: its name and the scalar kind it coerces to.
#[derive(Debug, Clone)]
struct VarDef {
    name: String,
    kind: ScalarKind,
}

/// Parse the saved query and read the first operation's variable definitions.
/// Returns one [`VarDef`] per declared variable (named/list/non-null types are
/// unwrapped to their innermost named type to choose the scalar kind).
fn variable_definitions(query: &str) -> Result<Vec<VarDef>, String> {
    use graphql_parser::query::{Definition, OperationDefinition, Type};

    let doc = graphql_parser::parse_query::<String>(query).map_err(|e| e.to_string())?;

    fn named<'a>(t: &'a Type<'a, String>) -> &'a str {
        match t {
            Type::NamedType(n) => n.as_str(),
            Type::ListType(inner) | Type::NonNullType(inner) => named(inner),
        }
    }
    fn kind_of(name: &str) -> ScalarKind {
        match name {
            "Int" => ScalarKind::Int,
            "Float" => ScalarKind::Float,
            "Boolean" => ScalarKind::Boolean,
            _ => ScalarKind::Other,
        }
    }

    for def in &doc.definitions {
        let defs = match def {
            Definition::Operation(OperationDefinition::Query(q)) => &q.variable_definitions,
            Definition::Operation(OperationDefinition::Mutation(m)) => &m.variable_definitions,
            Definition::Operation(OperationDefinition::Subscription(s)) => &s.variable_definitions,
            // A bare selection set has no variable definitions.
            Definition::Operation(OperationDefinition::SelectionSet(_)) => continue,
            Definition::Fragment(_) => continue,
        };
        return Ok(defs
            .iter()
            .map(|vd| VarDef {
                name: vd.name.clone(),
                kind: kind_of(named(&vd.var_type)),
            })
            .collect());
    }
    Ok(vec![])
}

/// The outcome of resolving a request against the `rest_endpoints` table.
struct Routing<'a> {
    /// The endpoint whose URL template and method both match (most specific).
    chosen: Option<(&'a RestEndpoint, HashMap<String, String>)>,
    /// At least one endpoint's URL template matched the path.
    url_matched: bool,
    /// Methods allowed across all URL-matching endpoints (for the 405 body).
    allowed_methods: Vec<String>,
}

/// Higher = more specific: the count of literal (non-`:param`) segments.
fn template_specificity(template: &str) -> usize {
    template
        .trim_matches('/')
        .split('/')
        .filter(|seg| !seg.starts_with(':'))
        .count()
}

/// Resolve the endpoint for a `(method, path)`. Among endpoints whose URL
/// template matches the path and whose methods include `method`, the most
/// specific (most literal segments) is chosen, so a literal route (e.g.
/// `pet/featured`) is never shadowed by an earlier parameterized one (e.g.
/// `pet/:id`) regardless of declaration order.
fn select_endpoint<'a>(endpoints: &'a [RestEndpoint], method: &str, path: &str) -> Routing<'a> {
    let mut url_matched = false;
    let mut allowed_methods: Vec<String> = Vec::new();
    let mut chosen: Option<(&RestEndpoint, HashMap<String, String>, usize)> = None;
    for ep in endpoints {
        if let Some(params) = match_template(&ep.url, path) {
            url_matched = true;
            allowed_methods.extend(ep.methods.iter().cloned());
            if ep.methods.iter().any(|m| m.eq_ignore_ascii_case(method)) {
                let score = template_specificity(&ep.url);
                if chosen.as_ref().is_none_or(|(_, _, best)| score > *best) {
                    chosen = Some((ep, params, score));
                }
            }
        }
    }
    Routing {
        chosen: chosen.map(|(ep, params, _)| (ep, params)),
        url_matched,
        allowed_methods,
    }
}

/// Match a URL template against a concrete path. A `:param` template segment
/// binds the corresponding path segment; literal segments must be equal; the
/// segment count must match. Returns the bound path variables, or `None` if
/// the path does not match the template.
fn match_template(template: &str, path: &str) -> Option<HashMap<String, String>> {
    let t: Vec<&str> = template.trim_matches('/').split('/').collect();
    let p: Vec<&str> = path.trim_matches('/').split('/').collect();
    if t.len() != p.len() {
        return None;
    }
    let mut params = HashMap::new();
    for (tseg, pseg) in t.iter().zip(p.iter()) {
        if let Some(name) = tseg.strip_prefix(':') {
            params.insert(name.to_string(), (*pseg).to_string());
        } else if tseg != pseg {
            return None;
        }
    }
    Some(params)
}

/// Parse a raw query string (`a=1&b=2`) into a name -> value map. Later
/// occurrences win. Keys and values are percent-decoded.
fn parse_query_string(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in raw.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

/// Minimal application/x-www-form-urlencoded decode: `+` -> space and
/// `%XX` -> byte. Invalid escapes are left as-is.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the GraphQL variables map from the declared definitions and the
/// request sources, with precedence PATH > QUERY-STRING > BODY.
///
/// Path/query values are strings and are coerced to the variable's declared
/// scalar kind (Int -> number, Float -> number, Boolean -> bool, otherwise
/// String). Body values are JSON-typed; a body *string* targeting an
/// Int/Float/Boolean variable is coerced too (see [`coerce_body`]), so the
/// result does not depend on which source supplied the value, while
/// already-typed body values pass through. A variable with no supplied value
/// is omitted (the engine applies defaults or reports a required-variable
/// error). A failed coercion is an `Err`.
fn build_variables(
    defs: &[VarDef],
    path: &HashMap<String, String>,
    query: &HashMap<String, String>,
    body: &JsonMap<String, Json>,
) -> Result<JsonMap<String, Json>, String> {
    let mut out = JsonMap::new();
    for def in defs {
        if let Some(raw) = path.get(&def.name) {
            out.insert(def.name.clone(), coerce_scalar(raw, def.kind, &def.name)?);
        } else if let Some(raw) = query.get(&def.name) {
            out.insert(def.name.clone(), coerce_scalar(raw, def.kind, &def.name)?);
        } else if let Some(v) = body.get(&def.name) {
            out.insert(def.name.clone(), coerce_body(v, def.kind, &def.name)?);
        }
        // else: omitted — let the engine decide (default / required error).
    }
    Ok(out)
}

/// Coerce a JSON body value to the variable's declared scalar kind. Body
/// values are already JSON-typed, but clients commonly send numbers/booleans
/// as JSON strings; a string destined for an Int/Float/Boolean variable is
/// coerced (matching the path/query behaviour), so the result does not depend
/// on which source supplied the value. Any non-string value (or a String/ID/
/// custom-scalar target) passes through unchanged.
fn coerce_body(v: &Json, kind: ScalarKind, var: &str) -> Result<Json, String> {
    match (v, kind) {
        (Json::String(s), ScalarKind::Int | ScalarKind::Float | ScalarKind::Boolean) => {
            coerce_scalar(s, kind, var)
        }
        _ => Ok(v.clone()),
    }
}

/// Coerce a stringly-typed path/query value to a JSON value of the variable's
/// declared scalar kind.
fn coerce_scalar(raw: &str, kind: ScalarKind, var: &str) -> Result<Json, String> {
    match kind {
        ScalarKind::Int => raw
            .parse::<i64>()
            .map(|n| json!(n))
            .map_err(|_| format!("variable '{var}' expects Int, got '{raw}'")),
        ScalarKind::Float => raw
            .parse::<f64>()
            .map(|n| json!(n))
            .map_err(|_| format!("variable '{var}' expects Float, got '{raw}'")),
        ScalarKind::Boolean => match raw {
            "true" => Ok(json!(true)),
            "false" => Ok(json!(false)),
            _ => Err(format!("variable '{var}' expects Boolean, got '{raw}'")),
        },
        ScalarKind::Other => Ok(json!(raw)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn ep(name: &str, url: &str, methods: &[&str]) -> RestEndpoint {
        serde_json::from_value(json!({
            "name": name,
            "url": url,
            "methods": methods,
            "definition": { "query": { "collection_name": "c", "query_name": name } },
        }))
        .expect("rest endpoint deserializes")
    }

    #[test]
    fn select_endpoint_prefers_literal_over_param() {
        // `pet/:id` declared BEFORE the literal `pet/featured`. A request for
        // `pet/featured` must route to the literal endpoint, not be captured
        // by the earlier parameterized route.
        let endpoints = vec![
            ep("get_pet_by_id", "pet/:id", &["GET"]),
            ep("pet_featured", "pet/featured", &["GET"]),
        ];
        let r = select_endpoint(&endpoints, "GET", "pet/featured");
        let (chosen, params) = r.chosen.expect("an endpoint matches");
        assert_eq!(chosen.name, "pet_featured");
        assert!(params.is_empty(), "literal route binds no path params");
    }

    #[test]
    fn select_endpoint_param_still_matches_non_literal() {
        let endpoints = vec![
            ep("get_pet_by_id", "pet/:id", &["GET"]),
            ep("pet_featured", "pet/featured", &["GET"]),
        ];
        let r = select_endpoint(&endpoints, "GET", "pet/7");
        let (chosen, params) = r.chosen.expect("an endpoint matches");
        assert_eq!(chosen.name, "get_pet_by_id");
        assert_eq!(params.get("id").map(String::as_str), Some("7"));
    }

    #[test]
    fn select_endpoint_405_when_url_matches_but_method_does_not() {
        let endpoints = vec![ep("get_pet_by_id", "pet/:id", &["GET"])];
        let r = select_endpoint(&endpoints, "DELETE", "pet/1");
        assert!(r.chosen.is_none());
        assert!(r.url_matched, "url matched, only the method differs");
        assert_eq!(r.allowed_methods, vec!["GET".to_string()]);
    }

    #[test]
    fn select_endpoint_404_when_no_url_matches() {
        let endpoints = vec![ep("get_pet_by_id", "pet/:id", &["GET"])];
        let r = select_endpoint(&endpoints, "GET", "owner/1");
        assert!(r.chosen.is_none());
        assert!(!r.url_matched, "no template matched -> 404, not 405");
    }

    #[test]
    fn match_template_binds_single_param() {
        let m = match_template("pet/:id", "pet/123").unwrap();
        assert_eq!(m.get("id").map(String::as_str), Some("123"));
    }

    #[test]
    fn match_template_literal_only() {
        let m = match_template("pets", "pets").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn match_template_rejects_segment_count_mismatch() {
        assert!(match_template("pet/:id", "pet").is_none());
        assert!(match_template("pet/:id", "pet/1/extra").is_none());
    }

    #[test]
    fn match_template_rejects_literal_mismatch() {
        assert!(match_template("pet/:id", "dog/1").is_none());
    }

    #[test]
    fn match_template_binds_multiple_params() {
        let m = match_template("owner/:oid/pet/:pid", "owner/7/pet/9").unwrap();
        assert_eq!(m.get("oid").map(String::as_str), Some("7"));
        assert_eq!(m.get("pid").map(String::as_str), Some("9"));
    }

    #[test]
    fn match_template_tolerates_leading_trailing_slashes() {
        let m = match_template("pet/:id", "/pet/5/").unwrap();
        assert_eq!(m.get("id").map(String::as_str), Some("5"));
    }

    #[test]
    fn parse_query_string_decodes_and_splits() {
        let q = parse_query_string("limit=2&name=a+b&q=%41");
        assert_eq!(q.get("limit").map(String::as_str), Some("2"));
        assert_eq!(q.get("name").map(String::as_str), Some("a b"));
        assert_eq!(q.get("q").map(String::as_str), Some("A"));
    }

    #[test]
    fn coerce_int_float_bool_string() {
        assert_eq!(coerce_scalar("5", ScalarKind::Int, "x").unwrap(), json!(5));
        assert_eq!(
            coerce_scalar("1.5", ScalarKind::Float, "x").unwrap(),
            json!(1.5)
        );
        assert_eq!(
            coerce_scalar("true", ScalarKind::Boolean, "x").unwrap(),
            json!(true)
        );
        assert_eq!(
            coerce_scalar("false", ScalarKind::Boolean, "x").unwrap(),
            json!(false)
        );
        assert_eq!(
            coerce_scalar("abc", ScalarKind::Other, "x").unwrap(),
            json!("abc")
        );
        // A numeric-looking string stays a string for String/ID/custom.
        assert_eq!(
            coerce_scalar("123", ScalarKind::Other, "x").unwrap(),
            json!("123")
        );
    }

    #[test]
    fn coerce_failure_for_non_numeric_int() {
        assert!(coerce_scalar("abc", ScalarKind::Int, "x").is_err());
        assert!(coerce_scalar("nope", ScalarKind::Boolean, "x").is_err());
    }

    #[test]
    fn build_variables_precedence_path_over_query_over_body() {
        let defs = vec![
            VarDef {
                name: "id".into(),
                kind: ScalarKind::Int,
            },
            VarDef {
                name: "limit".into(),
                kind: ScalarKind::Int,
            },
            VarDef {
                name: "obj".into(),
                kind: ScalarKind::Other,
            },
        ];
        let path = map(&[("id", "1")]);
        let query = map(&[("id", "2"), ("limit", "10")]);
        let mut body = JsonMap::new();
        body.insert("id".into(), json!(3));
        body.insert("limit".into(), json!(99));
        body.insert("obj".into(), json!({"k": "v"}));

        let vars = build_variables(&defs, &path, &query, &body).unwrap();
        // path wins for id
        assert_eq!(vars.get("id"), Some(&json!(1)));
        // query wins over body for limit
        assert_eq!(vars.get("limit"), Some(&json!(10)));
        // body value passes through untouched (object)
        assert_eq!(vars.get("obj"), Some(&json!({"k": "v"})));
    }

    #[test]
    fn build_variables_coerces_body_string_to_scalar_kind() {
        // A JSON body value that arrives as a string for an Int/Float/Boolean
        // variable is coerced just like a path/query value, so behavior does
        // not depend on which source supplied the value.
        let defs = vec![
            VarDef {
                name: "id".into(),
                kind: ScalarKind::Int,
            },
            VarDef {
                name: "ratio".into(),
                kind: ScalarKind::Float,
            },
            VarDef {
                name: "on".into(),
                kind: ScalarKind::Boolean,
            },
            VarDef {
                name: "note".into(),
                kind: ScalarKind::Other,
            },
        ];
        let mut body = JsonMap::new();
        body.insert("id".into(), json!("11"));
        body.insert("ratio".into(), json!("1.5"));
        body.insert("on".into(), json!("true"));
        body.insert("note".into(), json!("hi"));
        let vars = build_variables(&defs, &HashMap::new(), &HashMap::new(), &body).unwrap();
        assert_eq!(vars.get("id"), Some(&json!(11)));
        assert_eq!(vars.get("ratio"), Some(&json!(1.5)));
        assert_eq!(vars.get("on"), Some(&json!(true)));
        assert_eq!(vars.get("note"), Some(&json!("hi")));
    }

    #[test]
    fn build_variables_passes_through_already_typed_body() {
        // A correctly-typed JSON body value (number/object) is left untouched.
        let defs = vec![
            VarDef {
                name: "id".into(),
                kind: ScalarKind::Int,
            },
            VarDef {
                name: "obj".into(),
                kind: ScalarKind::Other,
            },
        ];
        let mut body = JsonMap::new();
        body.insert("id".into(), json!(7));
        body.insert("obj".into(), json!({ "k": "v" }));
        let vars = build_variables(&defs, &HashMap::new(), &HashMap::new(), &body).unwrap();
        assert_eq!(vars.get("id"), Some(&json!(7)));
        assert_eq!(vars.get("obj"), Some(&json!({ "k": "v" })));
    }

    #[test]
    fn build_variables_omits_unsupplied() {
        let defs = vec![VarDef {
            name: "id".into(),
            kind: ScalarKind::Int,
        }];
        let vars =
            build_variables(&defs, &HashMap::new(), &HashMap::new(), &JsonMap::new()).unwrap();
        assert!(vars.is_empty(), "unsupplied variable must be omitted");
    }

    #[test]
    fn build_variables_reports_coercion_failure() {
        let defs = vec![VarDef {
            name: "id".into(),
            kind: ScalarKind::Int,
        }];
        let path = map(&[("id", "notanint")]);
        let err = build_variables(&defs, &path, &HashMap::new(), &JsonMap::new()).unwrap_err();
        assert!(err.contains("Int"), "got: {err}");
    }

    #[test]
    fn variable_definitions_reads_names_and_kinds() {
        let q = "query ($id: Int!, $name: String, $on: Boolean, $r: Float) { x }";
        let defs = variable_definitions(q).unwrap();
        let by: HashMap<_, _> = defs.iter().map(|d| (d.name.as_str(), d.kind)).collect();
        assert_eq!(by.get("id"), Some(&ScalarKind::Int));
        assert_eq!(by.get("name"), Some(&ScalarKind::Other));
        assert_eq!(by.get("on"), Some(&ScalarKind::Boolean));
        assert_eq!(by.get("r"), Some(&ScalarKind::Float));
    }

    #[test]
    fn variable_definitions_unwraps_list_and_nonnull() {
        let defs = variable_definitions("query ($ids: [Int!]!) { x }").unwrap();
        assert_eq!(defs[0].kind, ScalarKind::Int);
    }
}
