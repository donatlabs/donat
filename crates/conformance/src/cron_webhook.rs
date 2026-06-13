//! A recording webhook stub for cron-trigger conformance.
//!
//! Unlike the action stub (which only answers), this one *records* every
//! request the engine's cron delivery loop sends, so a test can assert the
//! exact envelope and headers. Behavior by path:
//!
//! - `/ok` (and anything else)  → always 200.
//! - `/fail-then-ok`            → 500 on the first hit, 200 afterwards (to
//!   exercise retries).
//!
//! Raw HTTP/1.1, one request per connection (`Connection: close`), matching
//! the dependency-free style of the rest of the harness.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::Value as Json;

/// One recorded delivery: the request path, parsed JSON body, and headers
/// (lower-cased names).
#[derive(Clone)]
pub struct Received {
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
                if let Some((path, body, headers)) = read_request(&mut stream) {
                    // Decide the status from the count BEFORE this request, so
                    // `/fail-then-ok` fails exactly once.
                    let prior = received
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|r| r.path == path)
                        .count();
                    received.lock().unwrap().push(Received {
                        path: path.clone(),
                        body,
                        headers,
                    });
                    let status = if path.starts_with("/fail-then-ok") && prior == 0 {
                        500
                    } else {
                        200
                    };
                    write_response(&mut stream, status);
                }
            });
        }
    });

    CronWebhook { base, received }
}

/// Parse one HTTP request: returns (path, parsed-json-body, headers).
fn read_request(stream: &mut TcpStream) -> Option<(String, Json, Vec<(String, String)>)> {
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
    Some((path, body, headers))
}

fn write_response(stream: &mut TcpStream, status: u16) {
    let body = b"{}";
    let reason = if status == 200 { "OK" } else { "Error" };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
