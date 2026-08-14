//! The authentication hook every conformance suite runs behind.
//!
//! The engine accepts a role from exactly two places: a verified JWT, or an
//! authentication hook. Neither a header nor a shared secret can name one.
//! Conformance needs to say "this request is role X, with these session
//! variables" thousands of times without minting a token per case, so the
//! harness runs this hook and points the engine at it.
//!
//! What it does is echo: the engine forwards the client's headers, and the
//! hook answers with the `x-donat-*` / `x-hasura-*` ones as session
//! variables. That reproduces exactly what the fixtures mean by their role
//! headers, and it means the suites now exercise the hook path — which had no
//! conformance coverage at all while the header path existed.
//!
//! A request that names no role is **unauthorized** (401), not an error: the
//! engine then falls back to `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` if the suite
//! configured one, and denies otherwise. This is the one place where the
//! shape of "no role" changed with the header path's removal, and it is why
//! the hook decides it rather than leaving the engine to.
//!
//! Raw HTTP/1.1, one request per connection, matching the dependency-free
//! style of the rest of the harness.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

use serde_json::Value as Json;

/// A handle to the running hook.
#[derive(Clone)]
pub struct AuthHook {
    base: String,
}

impl AuthHook {
    /// The URL to put in `DONAT_GRAPHQL_AUTH_HOOK`.
    pub fn url(&self) -> &str {
        &self.base
    }
}

/// Start the hook on an ephemeral localhost port.
pub fn spawn() -> AuthHook {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth hook");
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}/auth");

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            std::thread::spawn(move || {
                if let Some(request) = read_request(&mut stream) {
                    match session_vars(&request) {
                        Some(vars) => write_json(&mut stream, 200, &vars),
                        None => write_json(&mut stream, 401, &Json::Object(Default::default())),
                    }
                }
            });
        }
    });

    AuthHook { base }
}

/// One parsed request: the client headers the engine forwarded.
struct Request {
    /// Headers of the request TO the hook, lower-cased. Used in GET mode,
    /// where the engine forwards the client's headers as its own.
    headers: Vec<(String, String)>,
    /// POST mode instead carries `{"headers": {...}}` as the body.
    body: Json,
}

/// Project the client's session headers into session variables, or `None`
/// when the request named no role.
fn session_vars(request: &Request) -> Option<Json> {
    let mut vars = serde_json::Map::new();
    let forwarded = request.body.get("headers").and_then(Json::as_object);
    let pairs: Vec<(String, String)> = match forwarded {
        Some(map) => map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k.to_ascii_lowercase(), v.to_string())))
            .collect(),
        None => request.headers.clone(),
    };

    let mut role = None;
    for (name, value) in pairs {
        if !(name.starts_with("x-donat-") || name.starts_with("x-hasura-")) {
            continue;
        }
        if name == "x-donat-role" || name == "x-hasura-role" {
            // A Donat-namespaced role wins, matching the engine's own
            // precedence when both are present.
            if role.is_none() || name == "x-donat-role" {
                role = Some(value.clone());
            }
        }
        vars.insert(name, Json::String(value));
    }

    let role = role?;
    vars.insert("x-donat-role".to_string(), Json::String(role.clone()));
    vars.insert("x-hasura-role".to_string(), Json::String(role));
    Some(Json::Object(vars))
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
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
    lines.next()?; // request line

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
    Some(Request { headers, body })
}

fn write_json(stream: &mut TcpStream, status: u16, body: &Json) {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        _ => "Error",
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
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn get_request(headers: &[(&str, &str)]) -> Request {
        Request {
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: Json::Null,
        }
    }

    #[test]
    fn a_request_that_names_no_role_is_unauthorized() {
        assert!(session_vars(&get_request(&[("x-donat-user-id", "7")])).is_none());
        assert!(session_vars(&get_request(&[("content-type", "application/json")])).is_none());
    }

    #[test]
    fn session_headers_become_session_variables() {
        let vars = session_vars(&get_request(&[
            ("x-donat-role", "editor"),
            ("x-donat-user-id", "7"),
            ("content-type", "application/json"),
        ]))
        .expect("a role was named");
        assert_eq!(vars.get("x-donat-role"), Some(&json!("editor")));
        assert_eq!(vars.get("x-hasura-role"), Some(&json!("editor")));
        assert_eq!(vars.get("x-donat-user-id"), Some(&json!("7")));
        assert!(vars.get("content-type").is_none(), "not a session header");
    }

    #[test]
    fn the_donat_namespace_wins_when_both_name_a_role() {
        let vars = session_vars(&get_request(&[
            ("x-hasura-role", "viewer"),
            ("x-donat-role", "editor"),
        ]))
        .expect("a role was named");
        assert_eq!(vars.get("x-donat-role"), Some(&json!("editor")));
        assert_eq!(vars.get("x-hasura-role"), Some(&json!("editor")));
    }

    #[test]
    fn a_hasura_role_alone_still_names_the_role() {
        let vars =
            session_vars(&get_request(&[("x-hasura-role", "viewer")])).expect("a role was named");
        assert_eq!(vars.get("x-donat-role"), Some(&json!("viewer")));
    }

    #[test]
    fn post_mode_reads_the_forwarded_header_object() {
        let request = Request {
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: json!({ "headers": { "X-Donat-Role": "editor", "X-Donat-User-Id": "7" } }),
        };
        let vars = session_vars(&request).expect("a role was named");
        assert_eq!(vars.get("x-donat-role"), Some(&json!("editor")));
        assert_eq!(vars.get("x-donat-user-id"), Some(&json!("7")));
    }
}
