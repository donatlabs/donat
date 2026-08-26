//! A recording webhook stub for cron-trigger conformance.
//!
//! Unlike the action stub (which only answers), this one *records* every
//! request the engine's cron delivery loop sends, so a test can assert the
//! exact envelope and headers. Behavior by path:
//!
//! - `/ok` (and anything else)  → always 200, body `{}`.
//! - `/fail-then-ok`            → 500 on the first hit, 200 afterwards (to
//!   exercise retries).
//! - `/list`, `/get-list`       → 200 with a two-item list, so that a stub
//!   playing an action handler has an output an `invoke` trigger's `then`
//!   can walk.
//! - `/fail-then-list`          → 500 once, then the same list.
//! - `/echo-fail`               → 400 whose message repeats `input.token`,
//!   the way an API that rejects a credential often does.
//!
//! Raw HTTP/1.1, one request per connection (`Connection: close`), matching
//! the dependency-free style of the rest of the harness.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::Value as Json;

type ParsedRequest = (String, String, Json, Vec<(String, String)>);

/// One recorded delivery: the request path, parsed JSON body, and headers
/// (lower-cased names).
#[derive(Clone)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub body: Json,
    pub headers: Vec<(String, String)>,
}

/// A handle to the running stub. Cheap to clone (shared recording buffer).
#[derive(Clone)]
pub struct CronWebhook {
    base: String,
    received: Arc<Mutex<Vec<Received>>>,
}

impl CronWebhook {
    pub fn base_url(&self) -> &str {
        &self.base
    }

    /// All deliveries recorded so far, in arrival order.
    pub fn received(&self) -> Vec<Received> {
        self.received.lock().unwrap().clone()
    }

    /// Number of deliveries recorded for a given request path.
    pub fn count_for(&self, path: &str) -> usize {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == path)
            .count()
    }
}

/// Start the stub on an ephemeral localhost port.
pub fn spawn() -> CronWebhook {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind cron webhook stub");
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let received = Arc::new(Mutex::new(Vec::<Received>::new()));
    let received_thread = received.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let received = received_thread.clone();
            std::thread::spawn(move || {
                if let Some((method, path, body, headers)) = read_request(&mut stream) {
                    // Decide the status from the count BEFORE this request, so
                    // `/fail-then-ok` fails exactly once.
                    let prior = received
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r.path == path)
                        .count();
                    let echoed_token = body
                        .pointer("/input/token")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string();
                    received.lock().unwrap().push(Received {
                        method,
                        path: path.clone(),
                        body,
                        headers,
                    });
                    let list = || {
                        serde_json::json!([
                            { "identifier": "A-1", "title": "one" },
                            { "identifier": "A-2", "title": "two" }
                        ])
                    };
                    let (status, body) = if path.starts_with("/echo-fail") {
                        (
                            400,
                            serde_json::json!({
                                "message": format!("invalid token {echoed_token}"),
                                "code": "unauthorized"
                            }),
                        )
                    } else if path.starts_with("/fail-then-list") {
                        if prior == 0 {
                            (500, Json::Object(Default::default()))
                        } else {
                            (200, list())
                        }
                    } else if path.starts_with("/fail-then-ok") {
                        // 500 on the first hit, 200 afterwards (retry → success).
                        (
                            if prior == 0 { 500 } else { 200 },
                            Json::Object(Default::default()),
                        )
                    } else if path.starts_with("/fail") {
                        // Always fails — exercises retry exhaustion.
                        (500, Json::Object(Default::default()))
                    } else if path.starts_with("/list") || path.starts_with("/get-list") {
                        (200, list())
                    } else {
                        (200, Json::Object(Default::default()))
                    };
                    write_response(&mut stream, status, &body);
                }
            });
        }
    });

    CronWebhook { base, received }
}

/// Parse one HTTP request: returns (path, parsed-json-body, headers).
fn read_request(stream: &mut TcpStream) -> Option<ParsedRequest> {
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
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

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

    let mut body_bytes = buf[header_end + 4..].to_vec();
    while body_bytes.len() < content_len {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&tmp[..n]);
    }
    let body: Json = serde_json::from_slice(&body_bytes).unwrap_or(Json::Null);
    Some((method, path, body, headers))
}

fn write_response(stream: &mut TcpStream, status: u16, body: &Json) {
    let body = serde_json::to_vec(body).unwrap_or_default();
    let reason = if status == 200 { "OK" } else { "Error" };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
