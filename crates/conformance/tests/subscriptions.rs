//! Ported from tests-py test_subscriptions.py (`TestSubscriptionBasic`, all
//! 9 in-scope tests) and test_jwt.py (`AbstractTestSubscriptionJwtExpiry`,
//! 3 algorithms).
//!
//! The shared `ws_case` in the harness is a one-shot query-over-ws helper;
//! subscriptions need stateful connections (frames routed per query id, the
//! class-level connection replaced mid-suite, close detection), so this
//! module carries its own `WsClient` mirroring tests-py
//! `context.GQLWsClient`: frames with an `id` go to a per-query queue,
//! anything else except `ka` goes to the main queue.
//!
//! Out of scope (not part of the 9):
//! - `test_start_duplicate`: skipped upstream too
//!   (`@pytest.mark.skip`, hasura/graphql-engine#387).
//! - `TestSubscriptionBasicNoAuth`, `TestSubscriptionCtrl*`: separate
//!   classes, not in this port's scope.

use std::collections::{HashMap, VecDeque};
use std::net::TcpStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use donat_conformance::{Running, Suite, Transport, fixture_root, load_fixture, response_matches};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Map, Value as Json, json};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const DIR: &str = "queries/subscriptions/basic";
const SECRET: &str = "conformance-subs-secret";

// --------------------------------------------------------------- ws client

/// Mirror of tests-py `context.GQLWsClient` (legacy Apollo graphql-ws).
struct WsClient {
    sock: WebSocket<MaybeTlsStream<TcpStream>>,
    main: VecDeque<Json>,
    by_id: HashMap<String, VecDeque<Json>>,
    closed: bool,
}

impl WsClient {
    fn connect(s: &Running) -> WsClient {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("{}/v1/graphql", s.ws_base())
            .into_client_request()
            .expect("ws request");
        req.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "graphql-ws".parse().expect("protocol header"),
        );
        let (sock, _) = tungstenite::connect(req).expect("ws connect");
        match sock.get_ref() {
            MaybeTlsStream::Plain(tcp) => tcp
                .set_read_timeout(Some(Duration::from_millis(100)))
                .expect("set_read_timeout"),
            _ => panic!("expected a plain tcp stream"),
        }
        WsClient {
            sock,
            main: VecDeque::new(),
            by_id: HashMap::new(),
            closed: false,
        }
    }

    fn send(&mut self, frame: Json) {
        self.sock
            .send(Message::text(frame.to_string()))
            .expect("ws send");
    }

    /// One bounded read (<=100ms). Routing replicates
    /// `GQLWsClient._on_message`: frames with an `id` go to that query's
    /// queue, everything else except `ka` to the main queue.
    fn pump(&mut self) {
        if self.closed {
            std::thread::sleep(Duration::from_millis(50));
            return;
        }
        match self.sock.read() {
            Ok(Message::Text(text)) => {
                let Ok(v) = serde_json::from_str::<Json>(&text) else {
                    return;
                };
                if let Some(id) = v.get("id").and_then(Json::as_str) {
                    self.by_id.entry(id.to_string()).or_default().push_back(v);
                } else if v["type"] != "ka" {
                    self.main.push_back(v);
                }
            }
            Ok(Message::Close(_)) => self.closed = true,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => self.closed = true,
        }
    }

    /// `get_ws_event(timeout)`: next frame from the main queue.
    fn main_event(&mut self, timeout: Duration) -> Option<Json> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(ev) = self.main.pop_front() {
                return Some(ev);
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.pump();
        }
    }

    /// `get_ws_query_event(id, timeout)`.
    fn query_event(&mut self, id: &str, timeout: Duration) -> Option<Json> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(ev) = self.by_id.get_mut(id).and_then(VecDeque::pop_front) {
                return Some(ev);
            }
            if Instant::now() >= deadline {
                return None;
            }
            self.pump();
        }
    }

    /// `GQLWsClient.init`: connection_init with an optional headers
    /// payload, connection_ack expected within 3s.
    fn init(&mut self, headers: &Json) {
        let mut payload = Map::new();
        if headers.as_object().is_some_and(|h| !h.is_empty()) {
            payload.insert("headers".into(), headers.clone());
        }
        self.send(json!({"type": "connection_init", "payload": payload}));
        let ev = self
            .main_event(Duration::from_secs(3))
            .expect("no connection_ack within 3s");
        assert_eq!(ev["type"], "connection_ack", "ws init failed: {ev}");
    }

    /// True once the server closed the connection (Close frame or socket
    /// teardown), pytest's `remote_closed`.
    fn wait_closed(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while !self.closed && Instant::now() < deadline {
            self.pump();
        }
        self.closed
    }
}

// --------------------------------------------- TestSubscriptionBasic (9/9)

/// validate.py `validate_gql_ws_q` for negative_test.yaml: a FRESH
/// connection (ws_client.init re-creates it) inited with the conf headers
/// plus the admin secret (tests-py `add_auth`), then start/compare. The
/// returned client replaces the class-level connection, exactly like in
/// pytest.
fn negative_ws(s: &Running, conf: &Json, via_subscription: bool) -> WsClient {
    let mut query = conf["query"].clone();
    if via_subscription {
        // Rewrite into a subscription with the multi-root directive,
        // byte-for-byte like validate.py.
        let text = query["query"].as_str().expect("query text").to_string();
        assert!(text.starts_with("query "), "{text}");
        query["query"] = json!(format!(
            "subscription{}",
            text["query".len()..].replacen('{', " @_multiple_top_level_fields {", 1)
        ));
    }

    let mut headers = conf["headers"].clone();
    headers["X-Hasura-Admin-Secret"] = json!(SECRET);
    let mut ws = WsClient::connect(s);
    ws.init(&headers);

    ws.send(json!({"id": "hge_test", "type": "start", "payload": query}));
    let ev = ws
        .query_event("hge_test", Duration::from_secs(15))
        .expect("no response frame for negative test");
    // The expected response carries errors -> 'data' or 'error' both pass.
    assert!(
        matches!(ev["type"].as_str(), Some("data" | "error")),
        "unexpected frame type: {ev}"
    );
    let exp = &conf["response"];
    let payload = &ev["payload"];
    assert!(
        response_matches(exp, payload, query["query"].as_str()),
        "negative ws response mismatch\nexpected: {exp}\nactual: {payload}"
    );

    if via_subscription {
        ws.send(json!({"id": "hge_test", "type": "stop"}));
        // pytest: queue.Empty on the main queue right after the stop.
        let ev = ws.main_event(Duration::from_millis(300));
        assert!(ev.is_none(), "unexpected event after stop: {ev:?}");
    } else {
        let done = ws
            .query_event("hge_test", Duration::from_secs(15))
            .expect("no complete frame");
        assert_eq!(done["type"], "complete", "{done}");
    }
    ws
}

/// `test_start`: start a live subscription, expect a `data` frame with the
/// matching id within 15s (pytest checks only type and id, not payload).
fn start_subscription(ws: &mut WsClient, id: &str) {
    let query = "
    subscription {
    hge_tests_test_t1(order_by: {c1: desc}, limit: 1) {
        c1,
        c2
      }
    }
    ";
    ws.send(json!({"id": id, "payload": {"query": query}, "type": "start"}));
    let ev = ws
        .query_event(id, Duration::from_secs(15))
        .expect("no data frame for subscription start");
    assert_eq!(ev["type"], "data", "{ev}");
    assert_eq!(ev["id"], id, "{ev}");
}

/// `test_stop`: a stop with an id produces no frame at all for 3 seconds.
fn stop_subscription(ws: &mut WsClient) {
    ws.send(json!({"type": "stop", "id": "1"}));
    let ev = ws.main_event(Duration::from_secs(3));
    assert!(ev.is_none(), "unexpected event after stop: {ev:?}");
}

#[test]
fn subscription_basic() {
    let s = Suite::new("subs_basic").admin_secret(SECRET).start();

    // tests-py HGECtx applies queries/clear_db.yaml before every class:
    // the hge_tests schema the fixtures create tables in.
    let (code, resp) = s.post(
        "/v1/query",
        &json!({"type": "run_sql", "args": {
            "sql": "drop schema if exists hge_tests cascade; create schema hge_tests;"
        }}),
        &[("X-Hasura-Admin-Secret".to_string(), SECRET.to_string())],
    );
    assert!(code < 300, "clear_db failed ({code}): {resp}");

    s.setup_v1q(&format!("{DIR}/setup.yaml"));

    // ws_conn_init class fixture: connection inited with the admin secret.
    let mut ws = WsClient::connect(&s);
    ws.init(&json!({"X-Hasura-Admin-Secret": SECRET}));

    // negative_test.yaml is a single-step list file.
    let conf = load_fixture(&fixture_root().join(format!("{DIR}/negative_test.yaml")))
        .expect("loading negative_test.yaml")[0]
        .clone();

    // test_negative[http]
    s.check_query_f(&format!("{DIR}/negative_test.yaml"), Transport::Http);
    // test_negative[websocket] / [subscription]: each re-creates the class
    // connection (validate.py calls ws_client.init with the conf headers),
    // and the remaining tests run on the connection the last one left.
    negative_ws(&s, &conf, false);
    ws = negative_ws(&s, &conf, true);

    // test_connection_error: unknown frame type -> connection_error.
    ws.send(json!({"type": "test"}));
    let ev = ws
        .main_event(Duration::from_secs(15))
        .expect("no connection_error frame");
    assert_eq!(ev["type"], "connection_error", "{ev}");

    // test_start (pytest uses uuid4 ids; any unique id works).
    start_subscription(&mut ws, "9e3b1b6c-start-1");

    // test_start_duplicate: skipped upstream (@pytest.mark.skip), not ported.

    // test_stop_without_id: protocol error.
    ws.send(json!({"type": "stop"}));
    let ev = ws
        .main_event(Duration::from_secs(3))
        .expect("no connection_error for id-less stop");
    assert_eq!(ev["type"], "connection_error", "{ev}");

    // test_stop
    stop_subscription(&mut ws);

    // test_start_after_stop
    start_subscription(&mut ws, "9e3b1b6c-start-2");
    stop_subscription(&mut ws);

    // test_complete: a plain query over ws answers data + complete.
    let query = "
    query {
      hge_tests_test_t1(order_by: {c1: desc}, limit: 1) {
        c1,
        c2
      }
    }
    ";
    ws.send(json!({"id": "2", "payload": {"query": query}, "type": "start"}));
    let ev = ws
        .query_event("2", Duration::from_secs(3))
        .expect("no data frame for query over ws");
    assert!(ev["type"] == "data" && ev["id"] == "2", "{ev}");
    let ev = ws
        .query_event("2", Duration::from_secs(3))
        .expect("no complete frame for query over ws");
    assert!(ev["type"] == "complete" && ev["id"] == "2", "{ev}");

    s.teardown_v1q(&format!("{DIR}/teardown.yaml"));
}

// ----------------------------------- AbstractTestSubscriptionJwtExpiry x 3

/// test_jwt.py `AbstractTestSubscriptionJwtExpiry.test_jwt_expiry`: init the
/// ws connection with a JWT expiring in ~4s; the server must close the
/// connection by itself (pytest sleeps 6s and asserts `remote_closed`).
/// Keys are the static fixtures in fixtures/jwt_keys (see README there);
/// tests-py generates an equivalent pair per run.
fn jwt_expiry_suite(name: &str, stem: &str, key_type: &str, algorithm: Algorithm) {
    let keys = fixture_root().join("jwt_keys");
    let public = std::fs::read_to_string(keys.join(format!("{stem}_public.pem")))
        .expect("reading public key");
    let private =
        std::fs::read(keys.join(format!("{stem}_private.pem"))).expect("reading private key");
    // conftest.py: HASURA_GRAPHQL_JWT_SECRET = {'type': ..., 'key': <pem>}.
    let secret_json = json!({"type": key_type, "key": public}).to_string();

    let s = Suite::new(name)
        .admin_secret(SECRET)
        .env("HASURA_GRAPHQL_JWT_SECRET", &secret_json)
        .start();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({
        "sub": "1234567890",
        "name": "John Doe",
        "iat": now,
        "https://hasura.io/jwt/claims": {
            "x-hasura-user-id": "1",
            "x-hasura-default-role": "user",
            "x-hasura-allowed-roles": ["user"],
        },
        "exp": now + 4,
    });
    let key = match algorithm {
        Algorithm::RS512 => EncodingKey::from_rsa_pem(&private),
        Algorithm::EdDSA => EncodingKey::from_ed_pem(&private),
        Algorithm::ES256 => EncodingKey::from_ec_pem(&private),
        other => panic!("unexpected algorithm {other:?}"),
    }
    .expect("parsing private key");
    let token =
        jsonwebtoken::encode(&Header::new(algorithm), &claims, &key).expect("signing token");

    let mut ws = WsClient::connect(&s);
    ws.init(&json!({"Authorization": format!("Bearer {token}")}));
    assert!(
        ws.wait_closed(Duration::from_secs(6)),
        "server did not close the connection within 6s of token expiry"
    );
}

#[test]
fn subscription_jwt_expiry_with_rsa() {
    jwt_expiry_suite("subs_jwt_rsa", "rsa", "RS512", Algorithm::RS512);
}

#[test]
fn subscription_jwt_expiry_with_ed25519() {
    jwt_expiry_suite("subs_jwt_ed", "ed25519", "Ed25519", Algorithm::EdDSA);
}

#[test]
fn subscription_jwt_expiry_with_es() {
    jwt_expiry_suite("subs_jwt_es", "es256", "ES256", Algorithm::ES256);
}
