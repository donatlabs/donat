//! A minimal upstream GraphQL server for the remote-schema conformance suite.
//!
//! tests-py runs a Node/Apollo server (`remote_schemas/nodejs/remote_schema_perms.js`)
//! as the upstream that the engine forwards to; this is the Rust equivalent.
//! It speaks just enough GraphQL to resolve the `remote_schema_perms` schema's
//! query roots (`hello`, `user`, `users`, `messages`, `message`,
//! `communications`, `profilePicture`) against the same canned data, projecting
//! the forwarded selection set. The engine validates the request against the
//! role's SDL itself, so only the *data* resolution lives here.
//!
//! Raw HTTP/1.1 (one request per connection), matching `action_webhook`.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use std::collections::HashMap;

use graphql_parser::query::{
    Definition, Field, FragmentDefinition, OperationDefinition, Selection, SelectionSet,
    Value as GqlValue,
};
use serde_json::{Map, Value as Json, json};

type Fragments<'a> = HashMap<String, &'a FragmentDefinition<'static, String>>;

/// Flatten a selection set into its concrete fields, expanding fragment
/// spreads and inline fragments (the upstream stub ignores type conditions —
/// the engine has already validated and rewritten the query).
fn fields<'a>(
    set: &'a SelectionSet<'static, String>,
    frags: &Fragments<'a>,
) -> Vec<&'a Field<'static, String>> {
    let mut out = Vec::new();
    for item in &set.items {
        match item {
            Selection::Field(f) => out.push(f),
            Selection::FragmentSpread(s) => {
                if let Some(frag) = frags.get(&s.fragment_name) {
                    out.extend(fields(&frag.selection_set, frags));
                }
            }
            Selection::InlineFragment(inf) => out.extend(fields(&inf.selection_set, frags)),
        }
    }
    out
}

/// The canned message rows, mirroring the Node server's `allMessages`.
fn all_messages() -> Vec<Json> {
    vec![
        json!({ "id": 1, "name": "alice", "msg": "You win!" }),
        json!({ "id": 2, "name": "bob", "msg": "You lose!" }),
        json!({ "id": 3, "name": "alice", "msg": "Another alice" }),
    ]
}

/// Spawn the upstream on an ephemeral localhost port; returns its base URL.
pub fn spawn() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind remote graphql stub");
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                if let Some(body) = read_request(&mut stream) {
                    let resp = execute(&body);
                    write_response(&mut stream, &resp);
                }
            });
        }
    });
    base
}

/// Execute a `{query, variables}` GraphQL request against the canned schema.
fn execute(body: &Json) -> Json {
    let query = body.get("query").and_then(Json::as_str).unwrap_or("");
    let variables = body
        .get("variables")
        .and_then(Json::as_object)
        .cloned()
        .unwrap_or_default();
    let doc = match graphql_parser::parse_query::<String>(query) {
        Ok(d) => d.into_static(),
        Err(e) => return gql_error(&format!("upstream parse error: {e}")),
    };
    let frags: Fragments = doc
        .definitions
        .iter()
        .filter_map(|d| match d {
            Definition::Fragment(f) => Some((f.name.clone(), f)),
            _ => None,
        })
        .collect();
    let Some(set) = operation_selection(&doc) else {
        return gql_error("no operation");
    };

    let mut data = Map::new();
    for field in fields(set, &frags) {
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
        let args = arg_map(&field.arguments, &variables);
        let value = resolve_root(&field.name, &args, &field.selection_set, &variables, &frags);
        data.insert(alias, value);
    }
    json!({ "data": data })
}

/// Resolve a top-level query field.
fn resolve_root(
    name: &str,
    args: &Map<String, Json>,
    sel: &SelectionSet<'static, String>,
    vars: &Map<String, Json>,
    frags: &Fragments,
) -> Json {
    match name {
        "hello" => Json::String("world".into()),
        "user" => {
            let uid = args.get("user_id").cloned().unwrap_or(Json::Null);
            project_user(&uid, sel, vars, frags)
        }
        "users" => {
            let ids = args
                .get("user_ids")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            Json::Array(
                ids.iter()
                    .map(|id| project_user(id, sel, vars, frags))
                    .collect(),
            )
        }
        "messages" => {
            let rows = filter_messages(all_messages(), args);
            Json::Array(
                rows.iter()
                    .map(|m| project_message(m, sel, frags))
                    .collect(),
            )
        }
        "communications" => {
            let mut rows = all_messages();
            if let Some(id) = args.get("id") {
                if !id.is_null() {
                    rows.retain(|m| &m["id"] == id);
                }
            }
            Json::Array(
                rows.iter()
                    .map(|m| project_message(m, sel, frags))
                    .collect(),
            )
        }
        "message" => {
            let id = args.get("id").cloned().unwrap_or(Json::Null);
            match all_messages().into_iter().find(|m| m["id"] == id) {
                Some(m) => project_message(&m, sel, frags),
                None => Json::Null,
            }
        }
        "profilePicture" => {
            // Resolver returns the `dimensions` input verbatim as a Photo.
            let dims = args.get("dimensions").cloned().unwrap_or(Json::Null);
            project_passthrough(&dims, sel, frags)
        }
        _ => Json::Null,
    }
}

/// Project the `User` type: `user_id`, `gimmeText(text)`, `userMessages(...)`.
fn project_user(
    user_id: &Json,
    sel: &SelectionSet<'static, String>,
    vars: &Map<String, Json>,
    frags: &Fragments,
) -> Json {
    if user_id.is_null() {
        return Json::Null;
    }
    let mut out = Map::new();
    for field in fields(sel, frags) {
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
        let value = match field.name.as_str() {
            "user_id" => user_id.clone(),
            "__typename" => Json::String("User".into()),
            "gimmeText" => {
                let args = arg_map(&field.arguments, vars);
                args.get("text")
                    .cloned()
                    .filter(|t| !t.is_null())
                    .unwrap_or_else(|| Json::String("no text".into()))
            }
            "userMessages" => {
                let args = arg_map(&field.arguments, vars);
                let mut rows: Vec<Json> = all_messages()
                    .into_iter()
                    .filter(|m| m["id"] == *user_id)
                    .collect();
                rows = filter_user_messages(rows, &args);
                Json::Array(
                    rows.iter()
                        .map(|m| project_message(m, &field.selection_set, frags))
                        .collect(),
                )
            }
            _ => Json::Null,
        };
        out.insert(alias, value);
    }
    Json::Object(out)
}

/// Project the `Message` type onto the selection set.
fn project_message(row: &Json, sel: &SelectionSet<'static, String>, frags: &Fragments) -> Json {
    let mut out = Map::new();
    for field in fields(sel, frags) {
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
        let value = match field.name.as_str() {
            "__typename" => Json::String("Message".into()),
            other => row.get(other).cloned().unwrap_or(Json::Null),
        };
        out.insert(alias, value);
    }
    Json::Object(out)
}

/// Project an arbitrary object (e.g. Photo) by copying selected fields.
fn project_passthrough(
    value: &Json,
    sel: &SelectionSet<'static, String>,
    frags: &Fragments,
) -> Json {
    if value.is_null() {
        return Json::Null;
    }
    let mut out = Map::new();
    for field in fields(sel, frags) {
        let alias = field.alias.clone().unwrap_or_else(|| field.name.clone());
        out.insert(alias, value.get(&field.name).cloned().unwrap_or(Json::Null));
    }
    Json::Object(out)
}

/// Apply `messages(where:, includes:)` filtering.
fn filter_messages(rows: Vec<Json>, args: &Map<String, Json>) -> Vec<Json> {
    apply_filters(rows, args.get("where"), args.get("includes"))
}

/// Apply `userMessages(whered:, includes:)` filtering (same shape, different
/// argument name for `where`).
fn filter_user_messages(rows: Vec<Json>, args: &Map<String, Json>) -> Vec<Json> {
    apply_filters(rows, args.get("whered"), args.get("includes"))
}

fn apply_filters(mut rows: Vec<Json>, where_: Option<&Json>, includes: Option<&Json>) -> Vec<Json> {
    if let Some(Json::Object(w)) = where_ {
        if let Some(Json::Object(id)) = w.get("id") {
            for (op, v) in id {
                rows.retain(|m| cmp_int(&m["id"], op, v));
            }
        }
        if let Some(Json::Object(name)) = w.get("name") {
            if let Some(eq) = name.get("eq") {
                rows.retain(|m| &m["name"] == eq);
            }
        }
    }
    if let Some(Json::Object(inc)) = includes {
        if let Some(Json::Array(ids)) = inc.get("id") {
            rows.retain(|m| ids.contains(&m["id"]));
        }
        if let Some(Json::Array(names)) = inc.get("name") {
            rows.retain(|m| names.contains(&m["name"]));
        }
    }
    rows
}

fn cmp_int(field: &Json, op: &str, v: &Json) -> bool {
    let (Some(a), Some(b)) = (field.as_i64(), v.as_i64()) else {
        return true;
    };
    match op {
        "eq" => a == b,
        "gt" => a > b,
        "lt" => a < b,
        _ => true,
    }
}

/// Resolve a field's arguments into a JSON map, substituting variables.
fn arg_map(
    args: &[(String, GqlValue<'static, String>)],
    vars: &Map<String, Json>,
) -> Map<String, Json> {
    args.iter()
        .map(|(k, v)| (k.clone(), gql_to_json(v, vars)))
        .collect()
}

fn gql_to_json(value: &GqlValue<'static, String>, vars: &Map<String, Json>) -> Json {
    match value {
        GqlValue::Variable(n) => vars.get(n).cloned().unwrap_or(Json::Null),
        GqlValue::Int(n) => Json::from(n.as_i64().unwrap_or_default()),
        GqlValue::Float(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        GqlValue::String(s) => Json::String(s.clone()),
        GqlValue::Boolean(b) => Json::Bool(*b),
        GqlValue::Null => Json::Null,
        GqlValue::Enum(e) => Json::String(e.clone()),
        GqlValue::List(items) => Json::Array(items.iter().map(|v| gql_to_json(v, vars)).collect()),
        GqlValue::Object(map) => Json::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), gql_to_json(v, vars)))
                .collect(),
        ),
    }
}

fn operation_selection<'a>(
    doc: &'a graphql_parser::query::Document<'static, String>,
) -> Option<&'a SelectionSet<'static, String>> {
    doc.definitions.iter().find_map(|d| match d {
        Definition::Operation(OperationDefinition::Query(q)) => Some(&q.selection_set),
        Definition::Operation(OperationDefinition::SelectionSet(s)) => Some(s),
        Definition::Operation(OperationDefinition::Mutation(m)) => Some(&m.selection_set),
        _ => None,
    })
}

fn gql_error(message: &str) -> Json {
    json!({ "errors": [ { "message": message } ] })
}

// ------------------------------------------------------------- raw HTTP I/O

fn read_request(stream: &mut TcpStream) -> Option<Json> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let content_len = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim().eq_ignore_ascii_case("content-length")).then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0usize);
    let mut body = buf[header_end + 4..].to_vec();
    while body.len() < content_len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some(serde_json::from_slice(&body).unwrap_or(Json::Null))
}

fn write_response(stream: &mut TcpStream, payload: &Json) {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}
