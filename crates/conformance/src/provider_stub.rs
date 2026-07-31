//! A programmable provider stub for durable connector-activity conformance.
//!
//! The cron stub answers a fixed status per path. Activity cases instead need
//! to script an exact provider conversation — a failure followed by success, a
//! decline, an ambiguous answer — and then assert what the engine actually
//! sent, including the idempotency header it must keep stable across attempts.
//!
//! Each path owns an ordered queue of scripted responses. A request consumes
//! the next one; once the queue is empty the registered default answers, and
//! without a default the stub answers `200 {}`. Every request is recorded with
//! its lower-cased headers.
//!
//! Raw HTTP/1.1, one request per connection (`Connection: close`), matching the
//! dependency-free style of the rest of the harness.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Value as Json, json};

/// One recorded provider call: request path, parsed JSON body, and headers
/// (lower-cased names).
#[derive(Clone)]
pub struct ProviderCall {
    pub path: String,
    pub body: Json,
    pub headers: Vec<(String, String)>,
}

impl ProviderCall {
    /// The first value of a lower-cased header name, if the engine sent it.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// One scripted answer.
#[derive(Clone)]
pub struct ScriptedResponse {
    pub status: u16,
    pub body: Json,
}

impl ScriptedResponse {
    pub fn ok(body: Json) -> Self {
        Self { status: 200, body }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: json!({}),
        }
    }
}

#[derive(Default)]
struct StubState {
    scripts: HashMap<String, Vec<ScriptedResponse>>,
    defaults: HashMap<String, ScriptedResponse>,
    calls: Vec<ProviderCall>,
}

/// A handle to the running stub. Cheap to clone (shared script and recording).
#[derive(Clone)]
pub struct ProviderStub {
    base: String,
    state: Arc<Mutex<StubState>>,
}

impl ProviderStub {
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// Queue the exact answers this path gives, in order. Later requests fall
    /// through to the path default.
    pub fn script(&self, path: &str, responses: Vec<ScriptedResponse>) {
        self.state
            .lock()
            .unwrap()
            .scripts
            .insert(path.to_owned(), responses);
    }

    /// The answer this path gives once its script is exhausted.
    pub fn set_default(&self, path: &str, response: ScriptedResponse) {
        self.state
            .lock()
            .unwrap()
            .defaults
            .insert(path.to_owned(), response);
    }

    /// Every provider call recorded so far, in arrival order.
    pub fn calls(&self) -> Vec<ProviderCall> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Calls recorded for one request path.
    pub fn calls_for(&self, path: &str) -> Vec<ProviderCall> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call.path == path)
            .cloned()
            .collect()
    }

    pub fn count_for(&self, path: &str) -> usize {
        self.calls_for(path).len()
    }

    fn answer(&self, path: &str) -> ScriptedResponse {
        let mut state = self.state.lock().unwrap();
        if let Some(queue) = state.scripts.get_mut(path)
            && !queue.is_empty()
        {
            return queue.remove(0);
        }
        state
            .defaults
            .get(path)
            .cloned()
            .unwrap_or_else(|| ScriptedResponse::ok(json!({})))
    }

    fn record(&self, call: ProviderCall) {
        self.state.lock().unwrap().calls.push(call);
    }
}

/// Start the stub on an ephemeral localhost port.
pub fn spawn() -> ProviderStub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind provider stub");
    let port = listener.local_addr().unwrap().port();
    let stub = ProviderStub {
        base: format!("http://127.0.0.1:{port}"),
        state: Arc::new(Mutex::new(StubState::default())),
    };
    let accepting = stub.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let stub = accepting.clone();
            std::thread::spawn(move || {
                if let Some(call) = read_request(&mut stream) {
                    // Take the scripted answer before recording, so a queue of
                    // one response cannot be consumed twice by concurrent
                    // attempts and the recorded order stays the arrival order.
                    let response = stub.answer(&call.path);
                    stub.record(call);
                    write_response(&mut stream, &response);
                }
            });
        }
    });

    stub
}

fn read_request(stream: &mut TcpStream) -> Option<ProviderCall> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
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
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if key == "content-length" {
                content_len = value.parse().unwrap_or(0);
            }
            headers.push((key, value));
        }
    }

    let mut body_bytes = buf[header_end + 4..].to_vec();
    while body_bytes.len() < content_len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&tmp[..n]);
    }
    let body: Json = serde_json::from_slice(&body_bytes).unwrap_or(Json::Null);
    Some(ProviderCall {
        path,
        body,
        headers,
    })
}

fn write_response(stream: &mut TcpStream, response: &ScriptedResponse) {
    let body = serde_json::to_vec(&response.body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        409 => "Conflict",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
