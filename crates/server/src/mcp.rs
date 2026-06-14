//! MCP (Model Context Protocol) server at `POST /mcp`.
//!
//! A minimal hand-rolled JSON-RPC 2.0 handler in **JSON mode**: it answers a
//! POST with a single `application/json` response (no SSE streaming). It
//! exposes a small fixed set of generic, table-parameterized CRUD tools
//! (`list_tables`, `describe_table`, `query`, `insert`, `update`, `delete`)
//! for LLM clients.
//!
//! Every data operation is rendered into a parametrized GraphQL operation and
//! executed through [`crate::gql::execute_full`] — the same pipeline as
//! `/v1/graphql`. There is NO admin role and NO direct SQL: per-role
//! permissions gate every call, and the role is mandatory (resolved exactly
//! like GraphQL via [`crate::gql::resolve_session`]). Tool arguments are
//! passed as GraphQL *variables* (JSON), never rendered as GraphQL literals.
//!
//! Known limitations (v1):
//! - Only the Donat default root-field naming is used (`<t>` for the `public`
//!   schema, `<schema>_<t>` otherwise). `custom_root_fields` / `custom_name`
//!   are not honored by the tool layer.
//! - `list_tables` matches the session role directly; inherited roles are not
//!   expanded.
//! - `GET /mcp` returns 405 (SSE streaming is out of scope).

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::{Map as JsonMap, Value as Json, json};

use donat_schema::Session;

use crate::{gql, state::SharedState};

/// MCP protocol version this server implements.
const PROTOCOL_VERSION: &str = "2025-06-18";

// ---------------------------------------------------------------- JSON-RPC

/// Build a JSON-RPC 2.0 success response echoing `id`.
fn rpc_result(id: Json, result: Json) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC 2.0 error response echoing `id`.
fn rpc_error(id: Json, code: i64, message: &str) -> Json {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// `GET /mcp`: SSE streaming is out of scope, so the GET form is not allowed.
pub async fn get_not_allowed() -> impl IntoResponse {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "GET /mcp is not supported (no SSE); use POST with JSON-RPC",
    )
}

/// `POST /mcp`: a single JSON-RPC 2.0 request -> a single JSON response.
pub async fn dispatch(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Option<axum::Json<Json>>,
) -> impl IntoResponse {
    let Some(axum::Json(req)) = body else {
        return (StatusCode::OK, axum::Json(rpc_error(Json::Null, -32700, "parse error"))).into_response();
    };

    let id = req.get("id").cloned().unwrap_or(Json::Null);
    let method = req.get("method").and_then(Json::as_str).unwrap_or("");

    // A JSON-RPC *notification* has no `id` (e.g. `notifications/initialized`).
    // We acknowledge it with HTTP 200 and an empty body (no JSON-RPC response).
    if req.get("id").is_none() {
        return (StatusCode::OK, "").into_response();
    }

    match method {
        "initialize" => (StatusCode::OK, axum::Json(rpc_result(id, initialize_result()))).into_response(),
        "tools/list" => {
            (StatusCode::OK, axum::Json(rpc_result(id, json!({ "tools": tool_defs() })))).into_response()
        }
        "tools/call" => {
            // Role is mandatory, exactly like /v1/graphql. An auth failure is
            // surfaced as a JSON-RPC invalid-params error carrying the engine
            // error body.
            let session = match gql::resolve_session(&state, &headers).await {
                Ok(s) => s,
                Err((_, errors)) => {
                    let msg = auth_error_message(&errors);
                    return (StatusCode::OK, axum::Json(rpc_error(id, -32602, &msg))).into_response();
                }
            };
            let params = req.get("params").cloned().unwrap_or(Json::Null);
            let result = call_tool(&state, &session, &headers, &params).await;
            (StatusCode::OK, axum::Json(rpc_result(id, result))).into_response()
        }
        _ => (StatusCode::OK, axum::Json(rpc_error(id, -32601, "method not found"))).into_response(),
    }
}

/// Pull a human-readable message out of an engine auth-error body
/// (`{"errors":[{"message": ...}]}`), falling back to the whole body.
fn auth_error_message(errors: &Json) -> String {
    errors
        .get("errors")
        .and_then(Json::as_array)
        .and_then(|a| a.first())
        .and_then(|e| e.get("message"))
        .and_then(Json::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| errors.to_string())
}

/// The `initialize` result: protocol version, capabilities, server info.
fn initialize_result() -> Json {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "donat", "version": env!("CARGO_PKG_VERSION") },
    })
}

// ------------------------------------------------------------------ tool defs

/// The six tool definitions returned by `tools/list`, each with a JSON Schema
/// `inputSchema`.
fn tool_defs() -> Json {
    json!([
        {
            "name": "list_tables",
            "description": "List the tables the current role may access, with the operations (select/insert/update/delete) permitted on each.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "describe_table",
            "description": "Describe a table: its columns and types, relationships, and the columns the current role may select.",
            "inputSchema": {
                "type": "object",
                "properties": { "table": { "type": "string", "description": "Base table name." } },
                "required": ["table"],
                "additionalProperties": false
            }
        },
        {
            "name": "query",
            "description": "Read rows from a table with optional column selection, where-filter, order_by, limit and offset.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": { "type": "string" },
                    "columns": { "type": "array", "items": { "type": "string" } },
                    "where": { "type": "object" },
                    "order_by": { "description": "An order_by object or array of objects." },
                    "limit": { "type": "integer" },
                    "offset": { "type": "integer" }
                },
                "required": ["table"],
                "additionalProperties": false
            }
        },
        {
            "name": "insert",
            "description": "Insert one or more rows into a table. Returns affected_rows and the returning rows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": { "type": "string" },
                    "objects": { "type": "array", "items": { "type": "object" } },
                    "returning": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["table", "objects"],
                "additionalProperties": false
            }
        },
        {
            "name": "update",
            "description": "Update rows matching a where-filter by setting columns. Returns affected_rows and the returning rows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": { "type": "string" },
                    "where": { "type": "object" },
                    "set": { "type": "object" },
                    "returning": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["table", "where", "set"],
                "additionalProperties": false
            }
        },
        {
            "name": "delete",
            "description": "Delete rows matching a where-filter. Returns affected_rows and the returning rows.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "table": { "type": "string" },
                    "where": { "type": "object" },
                    "returning": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["table", "where"],
                "additionalProperties": false
            }
        }
    ])
}

// -------------------------------------------------------------- tool results

/// Build a successful tool-call result: `content` (text duplicate),
/// `structuredContent` (the data), `isError: false`.
fn tool_ok(data: Json) -> Json {
    let text = serde_json::to_string(&data).unwrap_or_else(|_| "null".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": data,
        "isError": false,
    })
}

/// Build an error tool-call result: `content` (the message), `isError: true`,
/// optionally carrying the GraphQL errors under `structuredContent`.
fn tool_err(message: impl Into<String>, errors: Option<Json>) -> Json {
    let message = message.into();
    let mut out = json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    });
    if let Some(errors) = errors {
        out["structuredContent"] = errors;
    }
    out
}

// ------------------------------------------------------------------ dispatch

/// Execute a `tools/call`: route to the named tool and return its result.
async fn call_tool(
    state: &SharedState,
    session: &Session,
    headers: &HeaderMap,
    params: &Json,
) -> Json {
    let name = params.get("name").and_then(Json::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(Json::Null);

    match name {
        "list_tables" => list_tables(state, session).await,
        "describe_table" => describe_table(state, session, &args).await,
        "query" => crud_tool(state, session, headers, &args, build_query_gql).await,
        "insert" => crud_tool(state, session, headers, &args, build_insert_gql).await,
        "update" => crud_tool(state, session, headers, &args, build_update_gql).await,
        "delete" => crud_tool(state, session, headers, &args, build_delete_gql).await,
        other => tool_err(format!("unknown tool '{other}'"), None),
    }
}

/// Resolve the table columns from the default catalog for `base` (the
/// Donat base name). Returns `None` if the table is unknown.
async fn table_columns(state: &SharedState, base: &str) -> Option<Vec<String>> {
    let engine = state.engine.read().await;
    let catalog = engine.default_catalog();
    let (schema, name) = base_to_qualified(base);
    catalog
        .table(&schema, &name)
        .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
}

/// Run a CRUD tool: build the GraphQL `(query, variables)` from the tool
/// arguments, execute through the shared pipeline, and unwrap the single root
/// field's value. The `builder` is one of the pure `build_*_gql` helpers; if
/// it needs column resolution it returns an `Err` asking the caller to pass
/// `columns`/`returning`.
async fn crud_tool<F>(
    state: &SharedState,
    session: &Session,
    headers: &HeaderMap,
    args: &Json,
    builder: F,
) -> Json
where
    F: FnOnce(&Json, Option<&[String]>) -> Result<(String, JsonMap<String, Json>), String>,
{
    let Some(base) = args.get("table").and_then(Json::as_str) else {
        return tool_err("missing required argument 'table'", None);
    };

    // Resolve the table's columns (used as the default selection / returning
    // set when the caller did not specify them).
    let columns = table_columns(state, base).await;

    let (query, variables) = match builder(args, columns.as_deref()) {
        Ok(qv) => qv,
        Err(msg) => return tool_err(msg, None),
    };

    let gql_body = json!({
        "query": query,
        "variables": Json::Object(variables),
        "operationName": Json::Null,
    });

    let (_status, resp) = gql::execute_full(state, session, &gql_body, false, headers).await;

    if let Some(errors) = resp.get("errors") {
        let msg = first_error_message(errors).unwrap_or_else(|| "graphql error".to_string());
        return tool_err(msg, Some(json!({ "errors": errors.clone() })));
    }

    // Unwrap the single root field's value from `data`.
    match resp.get("data").and_then(Json::as_object) {
        Some(data) => match data.values().next() {
            Some(value) => tool_ok(value.clone()),
            None => tool_ok(Json::Null),
        },
        None => tool_err("graphql response has no data", None),
    }
}

/// First `message` from a GraphQL `errors` array.
fn first_error_message(errors: &Json) -> Option<String> {
    errors
        .as_array()?
        .first()?
        .get("message")?
        .as_str()
        .map(str::to_string)
}

// ---------------------------------------------------------- discovery tools

/// `list_tables`: enumerate tracked tables the role may access (has at least a
/// select permission for), with the permitted operations.
async fn list_tables(state: &SharedState, session: &Session) -> Json {
    let engine = state.engine.read().await;
    let role = session.role.as_str();
    let mut tables: Vec<Json> = Vec::new();

    for source in &engine.metadata.sources {
        for entry in &source.tables {
            let has_select = entry.select_permissions.iter().any(|p| p.role == role);
            if !has_select {
                continue;
            }
            let mut ops = vec!["select".to_string()];
            if entry.insert_permissions.iter().any(|p| p.role == role) {
                ops.push("insert".to_string());
            }
            if entry.update_permissions.iter().any(|p| p.role == role) {
                ops.push("update".to_string());
            }
            if entry.delete_permissions.iter().any(|p| p.role == role) {
                ops.push("delete".to_string());
            }
            tables.push(json!({
                "name": donat_schema::table_base_name(entry),
                "schema": entry.table.schema(),
                "operations": ops,
            }));
        }
    }

    tool_ok(json!({ "tables": tables }))
}

/// `describe_table`: columns + types (from the catalog), relationships (from
/// metadata), and the columns the role may select. Errors if the table is
/// unknown to metadata.
async fn describe_table(state: &SharedState, session: &Session, args: &Json) -> Json {
    let Some(base) = args.get("table").and_then(Json::as_str) else {
        return tool_err("missing required argument 'table'", None);
    };
    let engine = state.engine.read().await;
    let role = session.role.as_str();

    // Find the tracked table entry by base name.
    let entry = engine
        .metadata
        .sources
        .iter()
        .flat_map(|s| &s.tables)
        .find(|t| donat_schema::table_base_name(t) == base);
    let Some(entry) = entry else {
        return tool_err(format!("unknown table '{base}'"), None);
    };

    // Catalog columns + types; per-column description from metadata
    // `column_config.<col>.comment` (null when absent).
    let catalog = engine.default_catalog();
    let columns: Vec<Json> = catalog
        .table(entry.table.schema(), entry.table.name())
        .map(|t| {
            t.columns
                .iter()
                .map(|c| {
                    let description = entry
                        .configuration
                        .as_ref()
                        .and_then(|cfg| cfg.column_config.get(&c.name))
                        .and_then(|cc| cc.comment.as_deref());
                    json!({
                        "name": c.name,
                        "type": c.pg_type,
                        "nullable": c.nullable,
                        "description": description,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Relationships from metadata.
    let object_relationships: Vec<&str> =
        entry.object_relationships.iter().map(|r| r.name.as_str()).collect();
    let array_relationships: Vec<&str> =
        entry.array_relationships.iter().map(|r| r.name.as_str()).collect();

    // Columns the role may select (from the select permission).
    let selectable: Json = match entry.select_permissions.iter().find(|p| p.role == role) {
        Some(p) => match &p.permission.columns {
            donat_metadata::Columns::Star => Json::String("*".to_string()),
            donat_metadata::Columns::List(cols) => json!(cols),
        },
        None => Json::Null,
    };

    tool_ok(json!({
        "name": base,
        "schema": entry.table.schema(),
        "columns": columns,
        "object_relationships": object_relationships,
        "array_relationships": array_relationships,
        "selectable_columns": selectable,
    }))
}

// ------------------------------------------------- GraphQL string builders
//
// Pure helpers: tool arguments -> (GraphQL operation text, variables). They
// pass user data as GraphQL *variables* (JSON), never as inline literals.

/// Map a Donat base name back to a (schema, name) pair for catalog lookup:
/// `s_table` -> (s, table), a bare name -> (public, name). This is a
/// best-effort inverse of the default naming (`<t>` for `public`,
/// `<schema>_<t>` otherwise); a table in `public` whose own name contains `_`
/// is mis-split, which is the documented default-naming-only limitation.
fn base_to_qualified(base: &str) -> (String, String) {
    match base.split_once('_') {
        Some((schema, name)) if !name.is_empty() => (schema.to_string(), name.to_string()),
        _ => ("public".to_string(), base.to_string()),
    }
}

/// Render a selection set body from an optional explicit column list, falling
/// back to the catalog columns. Returns `Err` if neither is available.
fn selection_columns(
    explicit: Option<&Vec<String>>,
    catalog_cols: Option<&[String]>,
) -> Result<Vec<String>, String> {
    if let Some(cols) = explicit {
        if cols.is_empty() {
            return Err("'columns' must be a non-empty list".to_string());
        }
        return Ok(cols.clone());
    }
    match catalog_cols {
        Some(cols) if !cols.is_empty() => Ok(cols.to_vec()),
        _ => Err("cannot resolve columns for this table; pass 'columns'".to_string()),
    }
}

/// Read an optional `[String]` argument (e.g. `columns`, `returning`).
fn string_list(args: &Json, key: &str) -> Option<Vec<String>> {
    args.get(key)?.as_array().map(|a| {
        a.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    })
}

/// `query` -> `query (...) { <t>(where, order_by, limit, offset) { cols } }`.
fn build_query_gql(
    args: &Json,
    catalog_cols: Option<&[String]>,
) -> Result<(String, JsonMap<String, Json>), String> {
    let base = args
        .get("table")
        .and_then(Json::as_str)
        .ok_or("missing 'table'")?;

    let explicit = string_list(args, "columns");
    let cols = selection_columns(explicit.as_ref(), catalog_cols)?;
    let selection = cols.join(" ");

    // Only declare/reference the arguments the caller actually supplied: the
    // engine requires a value for any referenced variable that has no default,
    // even nullable ones (Donat behaviour). Each `query` argument maps to its
    // GraphQL variable type.
    let mut decls: Vec<String> = Vec::new();
    let mut field_args: Vec<String> = Vec::new();
    let mut vars = JsonMap::new();
    for key in ["where", "order_by", "limit", "offset"] {
        let Some(v) = args.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let decl = match key {
            "where" => format!("$where: {base}_bool_exp"),
            "order_by" => format!("$order_by: [{base}_order_by!]"),
            "limit" => "$limit: Int".to_string(),
            "offset" => "$offset: Int".to_string(),
            _ => unreachable!(),
        };
        decls.push(decl);
        field_args.push(format!("{key}: ${key}"));
        vars.insert(key.to_string(), v.clone());
    }

    let var_decls = if decls.is_empty() {
        String::new()
    } else {
        format!("({})", decls.join(", "))
    };
    let field_args = if field_args.is_empty() {
        String::new()
    } else {
        format!("({})", field_args.join(", "))
    };
    let query =
        format!("query {var_decls} {{ {base}{field_args} {{ {selection} }} }}");
    Ok((query, vars))
}

/// `insert` -> `mutation ($objects: [<t>_insert_input!]!) { insert_<t>(objects: $objects) { affected_rows returning { cols } } }`.
fn build_insert_gql(
    args: &Json,
    catalog_cols: Option<&[String]>,
) -> Result<(String, JsonMap<String, Json>), String> {
    let base = args
        .get("table")
        .and_then(Json::as_str)
        .ok_or("missing 'table'")?;
    let objects = args
        .get("objects")
        .filter(|v| v.is_array())
        .ok_or("missing required argument 'objects' (a list of rows)")?;

    let explicit = string_list(args, "returning");
    let cols = selection_columns(explicit.as_ref(), catalog_cols)?;
    let selection = cols.join(" ");

    let query = format!(
        "mutation ($objects: [{base}_insert_input!]!) \
         {{ insert_{base}(objects: $objects) {{ affected_rows returning {{ {selection} }} }} }}"
    );

    let mut vars = JsonMap::new();
    vars.insert("objects".to_string(), objects.clone());
    Ok((query, vars))
}

/// `update` -> `mutation ($where: <t>_bool_exp!, $set: <t>_set_input) { update_<t>(where: $where, _set: $set) { affected_rows returning { cols } } }`.
fn build_update_gql(
    args: &Json,
    catalog_cols: Option<&[String]>,
) -> Result<(String, JsonMap<String, Json>), String> {
    let base = args
        .get("table")
        .and_then(Json::as_str)
        .ok_or("missing 'table'")?;
    let where_arg = args
        .get("where")
        .filter(|v| !v.is_null())
        .ok_or("missing required argument 'where'")?;
    let set_arg = args
        .get("set")
        .filter(|v| !v.is_null())
        .ok_or("missing required argument 'set'")?;

    let explicit = string_list(args, "returning");
    let cols = selection_columns(explicit.as_ref(), catalog_cols)?;
    let selection = cols.join(" ");

    let query = format!(
        "mutation ($where: {base}_bool_exp!, $set: {base}_set_input) \
         {{ update_{base}(where: $where, _set: $set) {{ affected_rows returning {{ {selection} }} }} }}"
    );

    let mut vars = JsonMap::new();
    vars.insert("where".to_string(), where_arg.clone());
    vars.insert("set".to_string(), set_arg.clone());
    Ok((query, vars))
}

/// `delete` -> `mutation ($where: <t>_bool_exp!) { delete_<t>(where: $where) { affected_rows returning { cols } } }`.
fn build_delete_gql(
    args: &Json,
    catalog_cols: Option<&[String]>,
) -> Result<(String, JsonMap<String, Json>), String> {
    let base = args
        .get("table")
        .and_then(Json::as_str)
        .ok_or("missing 'table'")?;
    let where_arg = args
        .get("where")
        .filter(|v| !v.is_null())
        .ok_or("missing required argument 'where'")?;

    let explicit = string_list(args, "returning");
    let cols = selection_columns(explicit.as_ref(), catalog_cols)?;
    let selection = cols.join(" ");

    let query = format!(
        "mutation ($where: {base}_bool_exp!) \
         {{ delete_{base}(where: $where) {{ affected_rows returning {{ {selection} }} }} }}"
    );

    let mut vars = JsonMap::new();
    vars.insert("where".to_string(), where_arg.clone());
    Ok((query, vars))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<String> {
        vec!["id".to_string(), "name".to_string(), "status".to_string()]
    }

    #[test]
    fn base_to_qualified_public_and_schema() {
        assert_eq!(base_to_qualified("pet"), ("public".to_string(), "pet".to_string()));
        assert_eq!(
            base_to_qualified("sales_order"),
            ("sales".to_string(), "order".to_string())
        );
    }

    #[test]
    fn query_uses_explicit_columns_and_variables() {
        let args = json!({
            "table": "pet",
            "columns": ["id", "name"],
            "where": { "status": { "_eq": "available" } },
            "order_by": { "id": "asc" },
            "limit": 2
        });
        let (q, vars) = build_query_gql(&args, None).unwrap();
        assert!(q.contains("$where: pet_bool_exp"), "{q}");
        assert!(q.contains("$order_by: [pet_order_by!]"), "{q}");
        assert!(q.contains("$limit: Int"), "{q}");
        // Only supplied arguments are declared/referenced (offset is absent).
        assert!(!q.contains("offset"), "{q}");
        assert!(q.contains("pet(where: $where, order_by: $order_by, limit: $limit)"), "{q}");
        assert!(q.contains("{ id name }"), "{q}");
        assert_eq!(vars.get("where"), Some(&json!({ "status": { "_eq": "available" } })));
        assert_eq!(vars.get("order_by"), Some(&json!({ "id": "asc" })));
        assert_eq!(vars.get("limit"), Some(&json!(2)));
        // offset was absent -> omitted.
        assert!(!vars.contains_key("offset"));
    }

    #[test]
    fn query_defaults_to_catalog_columns() {
        let args = json!({ "table": "pet" });
        let (q, _) = build_query_gql(&args, Some(&cols())).unwrap();
        assert!(q.contains("{ id name status }"), "{q}");
    }

    #[test]
    fn query_without_columns_or_catalog_errors() {
        let args = json!({ "table": "pet" });
        let err = build_query_gql(&args, None).unwrap_err();
        assert!(err.contains("columns"), "{err}");
    }

    #[test]
    fn insert_builds_objects_variable() {
        let args = json!({
            "table": "pet",
            "objects": [{ "id": 10, "name": "Biscuit", "status": "available" }]
        });
        let (q, vars) = build_insert_gql(&args, Some(&cols())).unwrap();
        assert!(q.contains("$objects: [pet_insert_input!]!"), "{q}");
        assert!(q.contains("insert_pet(objects: $objects)"), "{q}");
        assert!(q.contains("affected_rows returning { id name status }"), "{q}");
        assert_eq!(
            vars.get("objects"),
            Some(&json!([{ "id": 10, "name": "Biscuit", "status": "available" }]))
        );
    }

    #[test]
    fn insert_honors_explicit_returning() {
        let args = json!({
            "table": "pet",
            "objects": [{ "id": 10 }],
            "returning": ["id"]
        });
        let (q, _) = build_insert_gql(&args, Some(&cols())).unwrap();
        assert!(q.contains("returning { id }"), "{q}");
    }

    #[test]
    fn insert_without_objects_errors() {
        let args = json!({ "table": "pet" });
        let err = build_insert_gql(&args, Some(&cols())).unwrap_err();
        assert!(err.contains("objects"), "{err}");
    }

    #[test]
    fn update_builds_where_and_set() {
        let args = json!({
            "table": "pet",
            "where": { "id": { "_eq": 1 } },
            "set": { "status": "sold" }
        });
        let (q, vars) = build_update_gql(&args, Some(&cols())).unwrap();
        assert!(q.contains("$where: pet_bool_exp!"), "{q}");
        assert!(q.contains("$set: pet_set_input"), "{q}");
        assert!(q.contains("update_pet(where: $where, _set: $set)"), "{q}");
        assert!(q.contains("affected_rows returning { id name status }"), "{q}");
        assert_eq!(vars.get("where"), Some(&json!({ "id": { "_eq": 1 } })));
        assert_eq!(vars.get("set"), Some(&json!({ "status": "sold" })));
    }

    #[test]
    fn update_requires_where_and_set() {
        let no_set = json!({ "table": "pet", "where": { "id": { "_eq": 1 } } });
        assert!(build_update_gql(&no_set, Some(&cols())).unwrap_err().contains("set"));
        let no_where = json!({ "table": "pet", "set": { "status": "sold" } });
        assert!(build_update_gql(&no_where, Some(&cols())).unwrap_err().contains("where"));
    }

    #[test]
    fn delete_builds_where() {
        let args = json!({ "table": "pet", "where": { "id": { "_eq": 2 } } });
        let (q, vars) = build_delete_gql(&args, Some(&cols())).unwrap();
        assert!(q.contains("$where: pet_bool_exp!"), "{q}");
        assert!(q.contains("delete_pet(where: $where)"), "{q}");
        assert!(q.contains("affected_rows returning { id name status }"), "{q}");
        assert_eq!(vars.get("where"), Some(&json!({ "id": { "_eq": 2 } })));
    }

    #[test]
    fn delete_requires_where() {
        let args = json!({ "table": "pet" });
        assert!(build_delete_gql(&args, Some(&cols())).unwrap_err().contains("where"));
    }

    #[test]
    fn non_public_schema_in_root_names() {
        let args = json!({ "table": "sales_order", "where": { "id": { "_eq": 1 } } });
        let (q, _) = build_delete_gql(&args, Some(&cols())).unwrap();
        assert!(q.contains("delete_sales_order(where: $where)"), "{q}");
    }

    #[test]
    fn initialize_result_shape() {
        let r = initialize_result();
        assert_eq!(r["protocolVersion"], json!(PROTOCOL_VERSION));
        assert_eq!(r["serverInfo"]["name"], json!("donat"));
        assert!(r["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tool_defs_lists_six_tools() {
        let defs = tool_defs();
        let names: Vec<&str> = defs
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["list_tables", "describe_table", "query", "insert", "update", "delete"]
        );
    }

    #[test]
    fn tool_ok_carries_structured_content_and_text() {
        let r = tool_ok(json!({ "affected_rows": 1 }));
        assert_eq!(r["isError"], json!(false));
        assert_eq!(r["structuredContent"], json!({ "affected_rows": 1 }));
        assert_eq!(r["content"][0]["type"], json!("text"));
        assert_eq!(r["content"][0]["text"], json!("{\"affected_rows\":1}"));
    }

    #[test]
    fn tool_err_sets_is_error() {
        let r = tool_err("nope", Some(json!({ "errors": [] })));
        assert_eq!(r["isError"], json!(true));
        assert_eq!(r["content"][0]["text"], json!("nope"));
        assert_eq!(r["structuredContent"], json!({ "errors": [] }));
    }
}
