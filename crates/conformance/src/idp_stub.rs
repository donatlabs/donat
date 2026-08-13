//! A minimal OpenID Connect provider, for exercising the engine's relying
//! party end to end.
//!
//! It does the two things the flow needs and nothing else:
//!
//! - `/authorize` — redirects straight back to the `redirect_uri` with a code
//!   and the `state` it was given. A real provider would show a login form
//!   first; what the engine's routes have to get right is everything *after*
//!   that, so the stub skips it.
//! - `/token` — exchanges that code for HS256 tokens carrying the claims the
//!   suite asked for, recording the form AND the `Authorization` header it
//!   received so a test can assert the PKCE verifier arrived and the client
//!   authenticated the way its configuration says.
//!
//! It answers with an `access_token` and an `id_token` carrying *different*
//! claims on purpose: providers disagree about which one a deployment's roles
//! live in, and a test can only prove the engine picked the configured one if
//! the two differ.
//!
//! The token is signed with the same key the suite puts in
//! `DONAT_GRAPHQL_JWT_SECRET`, so the cookie the engine sets is a token the
//! engine itself then verifies — which is the whole point of the exercise.
//!
//! Raw HTTP/1.1, one request per connection, like the rest of the harness.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value as Json, json};

/// The authorization code this stub always issues.
pub const CODE: &str = "conformance-authorization-code";

/// One recorded code exchange.
#[derive(Clone)]
pub struct Exchange {
    /// The decoded form body.
    pub form: HashMap<String, String>,
    /// The `Authorization` header, when the client sent one.
    pub authorization: Option<String>,
}

#[derive(Clone)]
pub struct IdpStub {
    base: String,
    key: String,
    exchanges: Arc<Mutex<Vec<Exchange>>>,
}

impl IdpStub {
    pub fn authorization_endpoint(&self) -> String {
        format!("{}/authorize", self.base)
    }

    pub fn token_endpoint(&self) -> String {
        format!("{}/token", self.base)
    }

    /// The stub's own origin, for a deployment that asks the engine to serve
    /// the provider's login API on its own address.
    pub fn base_url(&self) -> String {
        self.base.clone()
    }

    /// Every code exchange this stub answered.
    pub fn exchanges(&self) -> Vec<Exchange> {
        self.exchanges.lock().unwrap().clone()
    }

    /// A token signed with this stub's key, for a test that needs one without
    /// driving the whole flow.
    pub fn mint(&self, claims: &Map<String, Json>) -> String {
        sign(&self.key, claims)
    }
}

/// Start the stub. `key` signs both tokens; `claims` go in the access token
/// and `id_claims` in the id token, so a suite decides which roles a browser
/// ends up holding and through which token.
pub fn spawn_with_id_token(
    key: &str,
    claims: Map<String, Json>,
    id_claims: Map<String, Json>,
) -> IdpStub {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind idp stub");
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");
    let exchanges = Arc::new(Mutex::new(Vec::new()));
    let recorded = exchanges.clone();
    let signing_key = key.to_string();
    let key = key.to_string();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let key = key.clone();
            let claims = claims.clone();
            let id_claims = id_claims.clone();
            let recorded = recorded.clone();
            std::thread::spawn(move || {
                let Some((path, query, body, headers)) = read_request(&mut stream) else {
                    return;
                };
                if path.starts_with("/authorize") {
                    let redirect_uri = query.get("redirect_uri").cloned().unwrap_or_default();
                    let state = query.get("state").cloned().unwrap_or_default();
                    let separator = if redirect_uri.contains('?') { '&' } else { '?' };
                    let location = format!("{redirect_uri}{separator}code={CODE}&state={state}");
                    write_redirect(&mut stream, &location);
                } else if path.starts_with("/token") {
                    let form = parse_form(&body);
                    recorded.lock().unwrap().push(Exchange {
                        form: form.clone(),
                        authorization: headers
                            .iter()
                            .find(|(name, _)| name == "authorization")
                            .map(|(_, value)| value.clone()),
                    });
                    if form.get("code").map(String::as_str) != Some(CODE) {
                        write_json(&mut stream, 400, &json!({ "error": "invalid_grant" }));
                        return;
                    }
                    write_json(
                        &mut stream,
                        200,
                        &json!({
                            "access_token": sign(&key, &claims),
                            "id_token": sign(&key, &id_claims),
                            "token_type": "Bearer",
                            "expires_in": 1800
                        }),
                    );
                } else if let Some(rest) = path.strip_prefix("/auth/v1/") {
                    // The provider's own login API, which a deployment may ask
                    // the engine to serve on its own origin. What a proxy has
                    // to get right is that the request arrives unchanged and
                    // the answer comes back whole, so the stub describes what
                    // it received and answers with the two headers a login
                    // reads.
                    let header = |name: &str| {
                        headers
                            .iter()
                            .find(|(key, _)| key == name)
                            .map(|(_, value)| value.clone())
                            .unwrap_or_default()
                    };
                    if rest.starts_with("users") {
                        // The provider's admin API, enough of it to answer a
                        // list and to prove what reached it.
                        write_json(
                            &mut stream,
                            200,
                            &json!([
                                {
                                    "id": "u-1",
                                    "email": "one@example.test",
                                    "given_name": "One",
                                    "family_name": "Account",
                                    "authorization": header("authorization")
                                },
                                {
                                    "id": "u-2",
                                    "email": "two@example.test",
                                    "given_name": "Two",
                                    "family_name": "Account",
                                    "authorization": header("authorization")
                                }
                            ]),
                        );
                        return;
                    }
                    write_json_with_location(
                        &mut stream,
                        202,
                        &json!({
                            "path": rest,
                            "query": query,
                            "body": body,
                            "cookie": header("cookie"),
                            "csrf": header("x-csrf-token"),
                            "origin": header("origin"),
                            "host": header("host"),
                        }),
                        "https://provider.invalid/done?code=proxied",
                    );
                } else {
                    write_json(&mut stream, 404, &json!({ "error": "not_found" }));
                }
            });
        }
    });

    IdpStub {
        base,
        key: signing_key,
        exchanges,
    }
}

/// The common case: the deployment's claims live in the access token, and the
/// id token carries nothing a permission is written against.
pub fn spawn(key: &str, claims: Map<String, Json>) -> IdpStub {
    spawn_with_id_token(key, claims, Map::new())
}

fn sign(key: &str, claims: &Map<String, Json>) -> String {
    let mut claims = claims.clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs();
    claims
        .entry("exp".to_string())
        .or_insert_with(|| json!(now + 3600));
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &Json::Object(claims),
        &jsonwebtoken::EncodingKey::from_secret(key.as_bytes()),
    )
    .expect("signing the stub's access token")
}

/// `application/x-www-form-urlencoded`, decoded by hand: the harness's stubs
/// deliberately carry no dependencies beyond what the suites already need.
fn parse_form(body: &str) -> HashMap<String, String> {
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Returns (path, parsed query, body, headers).
type ParsedRequest = (
    String,
    HashMap<String, String>,
    String,
    Vec<(String, String)>,
);

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
    let target = request_line.split_whitespace().nth(1)?.to_string();

    let mut content_len = 0usize;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let name = k.trim().to_ascii_lowercase();
            let value = v.trim().to_string();
            if name == "content-length" {
                content_len = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
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

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_form(query)),
        None => (target, HashMap::new()),
    };
    Some((
        path,
        query,
        String::from_utf8_lossy(&body_bytes).into_owned(),
        headers,
    ))
}

fn write_redirect(stream: &mut TcpStream, location: &str) {
    let header = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.flush();
}

/// A JSON answer that also carries a `Location`, the way the provider's own
/// `POST /oidc/authorize` answers a successful login (202, not a redirect).
fn write_json_with_location(stream: &mut TcpStream, status: u16, body: &Json, location: &str) {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let header = format!(
        "HTTP/1.1 {status} Accepted\r\nContent-Type: application/json\r\nLocation: {location}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn write_json(stream: &mut TcpStream, status: u16, body: &Json) {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
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
