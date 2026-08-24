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

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};

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

/// Copy a value out of the request the engine just sent.
///
/// A provider usually echoes identifiers it was given — a quote id, an order
/// id — and a Process asserts that the answer refers to what it asked about.
/// A scripted body therefore supports one placeholder: a string of the form
/// `"$request:/json/pointer"` is replaced by the value at that pointer in the
/// request body, so a fixture does not have to know a runtime-generated UUID.
fn resolve_request_references(body: &Json, request: &Json) -> Json {
    match body {
        Json::String(value) => match value.strip_prefix("$request:") {
            Some(pointer) => request.pointer(pointer).cloned().unwrap_or(Json::Null),
            None => body.clone(),
        },
        Json::Array(values) => Json::Array(
            values
                .iter()
                .map(|value| resolve_request_references(value, request))
                .collect(),
        ),
        Json::Object(fields) => Json::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), resolve_request_references(value, request)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[derive(Default)]
struct StubState {
    scripts: HashMap<String, Vec<ScriptedResponse>>,
    defaults: HashMap<String, ScriptedResponse>,
    held: HashSet<String>,
    holding: HashMap<String, usize>,
    calls: Vec<ProviderCall>,
}

/// A handle to the running stub. Cheap to clone (shared script and recording).
#[derive(Clone)]
pub struct ProviderStub {
    base: String,
    state: Arc<Mutex<StubState>>,
    resumed: Arc<Condvar>,
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

    /// Hold every request to this path before it is answered.
    ///
    /// A module whose behaviour depends on *when* a caller acts — cancelling
    /// while a payment is still pending — needs that window to exist for real
    /// rather than by sleeping. Holding the provider keeps the Process parked
    /// in one activity for as long as the case needs, and [`Self::release`]
    /// lets it finish. Held requests are recorded only once released, so
    /// `count_for` still counts answered calls.
    pub fn hold(&self, path: &str) {
        self.state.lock().unwrap().held.insert(path.to_owned());
    }

    /// Let every held request on this path proceed.
    pub fn release(&self, path: &str) {
        self.state.lock().unwrap().held.remove(path);
        self.resumed.notify_all();
    }

    /// Block until a request for this path has arrived and is being held.
    pub fn await_held(&self, path: &str, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.state.lock().unwrap().holding.contains_key(path) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Park this request while its path is held.
    ///
    /// A hold is matched the same way a script is, wildcard included, so
    /// holding `/v1/shipments/*/labels` is not a silent no-op against the
    /// concrete path a Process actually requests.
    fn wait_while_held(&self, path: &str) {
        let mut state = self.state.lock().unwrap();
        if !holds(&state.held, path) {
            return;
        }
        *state.holding.entry(path.to_owned()).or_default() += 1;
        while holds(&state.held, path) {
            state = self.resumed.wait(state).unwrap();
        }
        if let Some(count) = state.holding.get_mut(path) {
            *count -= 1;
            if *count == 0 {
                state.holding.remove(path);
            }
        }
    }

    /// Back to a blank slate: no scripts, no defaults, no holds, no recorded
    /// calls. What a fresh stub would be, without giving up the port.
    pub fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        state.scripts.clear();
        state.defaults.clear();
        state.held.clear();
        state.calls.clear();
        drop(state);
        // Anyone parked on a hold is answered by the default now.
        self.resumed.notify_all();
    }

    /// Every provider call recorded so far, in arrival order.
    pub fn calls(&self) -> Vec<ProviderCall> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Calls recorded for one request path. A key with a `*` matches the
    /// way a registration does, so a test can count the calls to a path that
    /// carries a runtime id.
    pub fn calls_for(&self, path: &str) -> Vec<ProviderCall> {
        self.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call.path == path || prefix_matches(path, &call.path))
            .cloned()
            .collect()
    }

    pub fn count_for(&self, path: &str) -> usize {
        self.calls_for(path).len()
    }

    /// Resolve a request path to its answer.
    ///
    /// An operation template can carry a runtime value in its path -- a payment
    /// id, an order id -- so a fixture cannot always name the exact path. A
    /// registration whose key ends in `*` matches every path with that prefix;
    /// exact keys always win.
    fn answer(&self, path: &str) -> ScriptedResponse {
        let mut state = self.state.lock().unwrap();
        if let Some(queue) = state.scripts.get_mut(path)
            && !queue.is_empty()
        {
            return queue.remove(0);
        }
        let prefix_script = state
            .scripts
            .keys()
            .filter(|key| prefix_matches(key, path))
            .max_by_key(|key| key.len())
            .cloned();
        if let Some(key) = prefix_script
            && let Some(queue) = state.scripts.get_mut(&key)
            && !queue.is_empty()
        {
            return queue.remove(0);
        }
        if let Some(response) = state.defaults.get(path) {
            return response.clone();
        }
        // `defaults` is a HashMap, so iteration order is not stable. When more
        // than one wildcard key matches, take the most specific — the longest
        // literal text — rather than whichever the hash happened to yield, so
        // a case cannot pass or fail by hash order.
        state
            .defaults
            .iter()
            .filter(|(key, _)| prefix_matches(key, path))
            .max_by_key(|(key, _)| key.len())
            .map(|(_, response)| response.clone())
            .unwrap_or_else(|| ScriptedResponse::ok(json!({})))
    }

    fn record(&self, call: ProviderCall) {
        self.state.lock().unwrap().calls.push(call);
    }
}

/// Start the stub on an ephemeral localhost port.
pub fn spawn() -> ProviderStub {
    spawn_preferring(std::iter::empty())
}

/// Spawn on the first free port from `preferred`, or an ephemeral one. A
/// stable port keeps the stub's URL — and everything a deploy derives from
/// it — identical across test cases and across runs.
pub fn spawn_preferring(preferred: impl IntoIterator<Item = u16>) -> ProviderStub {
    let listener = preferred
        .into_iter()
        .find_map(|port| TcpListener::bind(("127.0.0.1", port)).ok())
        .unwrap_or_else(|| TcpListener::bind("127.0.0.1:0").expect("bind provider stub"));
    let port = listener.local_addr().unwrap().port();
    let stub = ProviderStub {
        base: format!("http://127.0.0.1:{port}"),
        state: Arc::new(Mutex::new(StubState::default())),
        resumed: Arc::new(Condvar::new()),
    };
    let accepting = stub.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let stub = accepting.clone();
            std::thread::spawn(move || {
                if let Some(call) = read_request(&mut stream) {
                    stub.wait_while_held(&call.path);
                    // Take the scripted answer before recording, so a queue of
                    // one response cannot be consumed twice by concurrent
                    // attempts and the recorded order stays the arrival order.
                    let mut response = stub.answer(&call.path);
                    response.body = resolve_request_references(&response.body, &call.body);
                    stub.record(call);
                    write_response(&mut stream, &response);
                }
            });
        }
    });

    stub
}

/// Whether any hold registration covers this request path.
fn holds(held: &HashSet<String>, path: &str) -> bool {
    held.contains(path) || held.iter().any(|key| prefix_matches(key, path))
}

/// A registration key containing one `*` matches every path with that prefix
/// and suffix. Operation templates put a runtime identifier in the middle of a
/// path (`/v1/payment-authorizations/{payment_id}/voids`), so a trailing
/// wildcard alone cannot tell one operation from its siblings.
fn prefix_matches(key: &str, path: &str) -> bool {
    key.split_once('*').is_some_and(|(prefix, suffix)| {
        path.len() >= prefix.len() + suffix.len()
            && path.starts_with(prefix)
            && path.ends_with(suffix)
    })
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
