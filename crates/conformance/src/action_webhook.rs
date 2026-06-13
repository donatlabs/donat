//! A minimal action-webhook stub for the conformance harness.
//!
//! Donat's `tests-py` runs a Python `ActionsWebhookHandler`; this is the
//! Rust equivalent. The engine subprocess POSTs the action payload
//! (`{action, input, session_variables}`) here and we reply with the JSON the
//! handler would produce. Started once per suite on an ephemeral port; the
//! base URL is handed to the engine as `ACTION_WEBHOOK_HANDLER` so handler
//! templates (`{{ACTION_WEBHOOK_HANDLER}}/mirror-action`) resolve to it.
//!
//! Raw HTTP/1.1 (one request per connection, `Connection: close`) keeps the
//! stub dependency-free and synchronous, matching the rest of the harness.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value as Json};

/// Shared handle to the running engine, so callback endpoints (which run a
/// GraphQL query back against the engine) can reach it once it has spawned.
#[derive(Clone, Default)]
pub struct EngineHandle {
    inner: Arc<Mutex<Option<EngineInfo>>>,
}

#[derive(Clone)]
struct EngineInfo {
    base_url: String,
    admin_secret: Option<String>,
}

impl EngineHandle {
    pub fn set(&self, base_url: &str, admin_secret: Option<String>) {
        *self.inner.lock().unwrap() = Some(EngineInfo {
            base_url: base_url.to_string(),
            admin_secret,
        });
    }

    fn get(&self) -> Option<EngineInfo> {
        self.inner.lock().unwrap().clone()
    }
}

/// Spawn the webhook stub on an ephemeral localhost port. Returns its base URL
/// (e.g. `http://127.0.0.1:54321`) and a handle the harness uses to publish
/// the engine's address once it boots.
pub fn spawn() -> (String, EngineHandle) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind webhook stub");
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let engine = EngineHandle::default();
    let engine_for_thread = engine.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let engine = engine_for_thread.clone();
            std::thread::spawn(move || {
                if let Some((path, body, headers)) = read_request(&mut stream) {
                    let (status, payload) = dispatch(&path, &body, &headers, &engine);
                    write_response(&mut stream, status, &payload);
                }
            });
        }
    });

    (base, engine)
}

/// Parse one HTTP request: returns (path, parsed-json-body, raw-headers).
fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, Json, Vec<(String, String)>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    // Read until we have the full header block.
    let header_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let path = request_line.split_whitespace().nth(1)?.to_string();

    let mut headers = Vec::new();
    let mut content_len = 0usize;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim().to_string();
            if k == "content-length" {
                content_len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
    }

    // Read the body up to content-length.
    let mut body_bytes = buf[header_end + 4..].to_vec();
    while body_bytes.len() < content_len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&tmp[..n]);
    }
    let body: Json = serde_json::from_slice(&body_bytes).unwrap_or(Json::Null);
    Some((path, body, headers))
}

fn write_response(stream: &mut std::net::TcpStream, status: u16, payload: &Json) {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Status",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

/// Route a webhook request to its handler, mirroring `ActionsWebhookHandler`
/// in `tests-py/context.py`.
fn dispatch(
    path: &str,
    body: &Json,
    headers: &[(String, String)],
    engine: &EngineHandle,
) -> (u16, Json) {
    let input = body.get("input").cloned().unwrap_or(Json::Null);
    match path {
        // Echo `input.arg` straight back.
        "/mirror-action" => (200, input.get("arg").cloned().unwrap_or(Json::Null)),

        // Reflect the forwarded request headers as a list of {name, value}.
        "/mirror-headers" => {
            let list: Vec<Json> = headers
                .iter()
                .map(|(k, v)| json!({ "name": k, "value": v }))
                .collect();
            (200, json!({ "headers": list }))
        }

        // Return the request body's `blob` as a 400 error.
        "/intentional-error" => (
            400,
            input.get("blob").cloned().unwrap_or(Json::Null),
        ),

        // Whole response is null.
        "/null-response" => (200, Json::Null),

        // Object with a field omitted (the engine fills it with null).
        "/omitted-response-field" => (200, json!({ "country": "India" })),

        // A bare scalar string.
        "/scalar-response" => (200, Json::String("some-string".into())),

        // An arbitrary JSON object (for json / custom-scalar output types).
        "/json-response" => (200, json!({ "foo": "bar" })),

        // Array of custom-scalar objects.
        "/custom-scalar-array-response" => (200, json!([{ "foo": "bar" }])),

        // Scalar array including a null element.
        "/scalar-array-response" => (200, json!(["foo", "bar", null])),

        "/get-results" => (200, json!({ "result_ids": [1, 2, 3, 4] })),

        "/typed-nested-null" => (200, json!({ "id": 1, "child": null })),

        "/typed-nested-null-wrong-field" => (200, json!({ "id": null, "child": null })),

        "/recursive-output" => (
            200,
            json!({
                "direct": { "id": 1, "this": { "id": 2, "this": { "id": 3 } } },
                "list": { "id": 1, "these": [ { "id": 2, "these": [ { "id": 3 } ] }, { "id": 4 } ] },
                "mutual": { "id": 1, "that": { "id": 2, "other": { "id": 3, "that": { "id": 4 } } } }
            }),
        ),

        _ => handled_with_engine(path, &input, engine).unwrap_or((204, Json::Null)),
    }
}

/// Endpoints that run a GraphQL query back against the engine (the realistic
/// webhook pattern: the handler does work by calling the GraphQL API).
/// Returns `None` for unknown paths so the caller can answer 204.
fn handled_with_engine(path: &str, input: &Json, engine: &EngineHandle) -> Option<(u16, Json)> {
    match path {
        "/create-user" => Some(create_user(input, engine)),
        "/create-users" => Some(create_users(input, engine)),
        "/get-user-by-email" => Some(get_users_by_email(input, engine, true)),
        "/get-users-by-email" => Some(get_users_by_email(input, engine, false)),
        "/get-user-by-email-nested" => Some(get_users_by_email_nested(input, engine, true)),
        "/get-users-by-email-nested" => Some(get_users_by_email_nested(input, engine, false)),
        _ => None,
    }
}

/// POST a GraphQL query to the running engine (trusted by the admin secret;
/// with no role header it runs as the unauthorized-role fallback). Returns the
/// parsed response body.
fn engine_gql(engine: &EngineHandle, query: &str, variables: Json) -> Option<Json> {
    let info = engine.get()?;
    let client = reqwest::blocking::Client::new();
    let mut req = client
        .post(format!("{}/v1/graphql", info.base_url))
        .json(&json!({ "query": query, "variables": variables }));
    if let Some(secret) = &info.admin_secret {
        req = req.header("X-Donat-Admin-Secret", secret);
    }
    let resp = req.send().ok()?;
    resp.json::<Json>().ok()
}

/// A lax e-mail check mirroring tests-py's regex for the fixtures' purposes:
/// `local@domain.tld`.
fn valid_email(email: &str) -> bool {
    match email.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && domain.contains('.') && !email.contains(char::is_whitespace)
        }
        None => false,
    }
}

fn gql_failed() -> (u16, Json) {
    (400, json!({ "message": "GraphQL query execution failed", "code": "unexpected" }))
}

fn create_user(input: &Json, engine: &EngineHandle) -> (u16, Json) {
    let email = input.get("email").and_then(Json::as_str).unwrap_or("");
    let name = input.get("name").and_then(Json::as_str).unwrap_or("");
    if !valid_email(email) {
        return (400, json!({ "message": "Given email address is not valid", "code": "invalid-email" }));
    }
    let query = "mutation ($email: String! $name: String!) { \
        insert_user_one(object: {email: $email, name: $name}){ id } }";
    let Some(resp) = engine_gql(engine, query, json!({ "email": email, "name": name })) else {
        return gql_failed();
    };
    match resp.pointer("/data/insert_user_one") {
        Some(user) if !user.is_null() => (200, user.clone()),
        _ => gql_failed(),
    }
}

fn create_users(input: &Json, engine: &EngineHandle) -> (u16, Json) {
    let users = input.get("users").and_then(Json::as_array).cloned().unwrap_or_default();
    for u in &users {
        let email = u.get("email").and_then(Json::as_str).unwrap_or("");
        if !valid_email(email) {
            return (
                400,
                json!({ "message": format!("Email address is not valid: {email}"), "code": "invalid-email" }),
            );
        }
    }
    let query = "mutation ($insert_inputs: [user_insert_input!]!){ \
        insert_user(objects: $insert_inputs){ returning { id } } }";
    let Some(resp) = engine_gql(engine, query, json!({ "insert_inputs": users })) else {
        return gql_failed();
    };
    match resp.pointer("/data/insert_user/returning") {
        Some(returning) if returning.is_array() => (200, returning.clone()),
        _ => gql_failed(),
    }
}

fn get_users_by_email(input: &Json, engine: &EngineHandle, single: bool) -> (u16, Json) {
    let email = input.get("email").and_then(Json::as_str).unwrap_or("");
    if !valid_email(email) {
        return (400, json!({ "message": "Given email address is not valid", "code": "invalid-email" }));
    }
    let query = "query get_user($email:String!) { \
        user(where:{email:{_eq:$email}}, order_by: {id: asc}) { id } }";
    let Some(resp) = engine_gql(engine, query, json!({ "email": email })) else {
        return gql_failed();
    };
    let Some(users) = resp.pointer("/data/user").and_then(Json::as_array) else {
        return gql_failed();
    };
    if single {
        match users.first() {
            Some(u) => (200, u.clone()),
            None => gql_failed(),
        }
    } else {
        (200, Json::Array(users.clone()))
    }
}

fn get_users_by_email_nested(input: &Json, engine: &EngineHandle, single: bool) -> (u16, Json) {
    let (status, body) = get_users_by_email(input, engine, single);
    if status != 200 {
        return (status, body);
    }
    let nest = |obj: &Json| -> Json {
        let id = obj.get("id").cloned().unwrap_or(Json::Null);
        json!({
            "id": id,
            "user_id": { "id": id },
            "address": { "city": "New York", "country": "USA" },
            "addresses": [
                { "city": "Bangalore", "country": "India" },
                { "city": "Melbourne", "country": "Australia" }
            ]
        })
    };
    if single {
        (200, nest(&body))
    } else {
        let list = body.as_array().map(|a| a.iter().map(nest).collect()).unwrap_or_default();
        (200, Json::Array(list))
    }
}
