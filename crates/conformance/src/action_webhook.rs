//! A minimal action-webhook stub for the conformance harness.
//!
//! Hasura's `tests-py` runs a Python `ActionsWebhookHandler`; this is the
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

// `base_url`/`admin_secret` and `EngineHandle::get` are consumed by the
// callback endpoints added in phase 2 (handlers that run a GraphQL query back
// against the engine); kept here so the wiring is in place.
#[derive(Clone)]
#[allow(dead_code)]
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

    #[allow(dead_code)]
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

/// Endpoints that run a GraphQL query back against the engine. Returns `None`
/// for unknown paths so the caller can answer 204.
fn handled_with_engine(
    _path: &str,
    _input: &Json,
    _engine: &EngineHandle,
) -> Option<(u16, Json)> {
    // Phase 2+: create-user / get-user-by-email and friends call back into the
    // engine here. Not needed for the sync-core slice.
    None
}
