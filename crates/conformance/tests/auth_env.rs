//! Auth- and env-dependent suites ported from tests-py:
//! - test_graphql_queries.py: TestUnauthorizedRolePermission,
//!   TestFallbackUnauthorizedRoleCookie, TestMissingUnauthorizedRoleAndCookie,
//!   TestGraphQLQueryFunctionPermissions
//! - test_allowlist_queries.py: TestAllowlistQueries
//!
//! The unauthorized-role/cookie classes are marked `@pytest.mark.admin_secret`
//! AND their tests run with `add_auth=False`: the engine must have the secret
//! configured (so plain X-Hasura-* headers are untrusted) while the checked
//! request carries no secret. `Suite` always attaches the configured secret
//! to checked requests (tests-py `add_auth=True` semantics), so these
//! suites spawn the engine directly via `EnvEngine` below, authenticating
//! setup/teardown with the secret header manually and sending the checked
//! requests with the fixture headers only (`add_auth=False` semantics).

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value as Json, json};

use dist_conformance::{
    Suite, Transport, engine_binary, fixture_root, load_fixture, pg_admin_url, response_matches,
};

/// Same role as tests-py's --hge-key: an API-level secret, never a data role.
const SECRET: &str = "conformance_admin_secret";

/// HASURA_GRAPHQL_JWT_SECRET for `@pytest.mark.jwt('rsa')`
/// (fixtures/jwt.py::init_rsa builds {"type": "RS512", "key": <public pem>}).
fn rsa_jwt_secret() -> String {
    let pem = std::fs::read_to_string(fixture_root().join("jwt_keys/rsa_public.pem"))
        .expect("jwt_keys/rsa_public.pem (see fixtures/jwt_keys/README.md)");
    json!({"type": "RS512", "key": pem}).to_string()
}

// ----------------------------------------------------------------- EnvEngine

/// A dist-api instance spawned with auth-sensitive env vars. Mirrors
/// `Suite::start()` (own database, postgis, health wait, log file) without
/// the unauthenticated bootstrap POST that a secret-protected engine rejects.
struct EnvEngine {
    name: String,
    base_url: String,
    ws_base: String,
    http: reqwest::blocking::Client,
    child: Child,
}

impl Drop for EnvEngine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl EnvEngine {
    fn start(name: &str, env: &[(&str, &str)]) -> EnvEngine {
        // Fresh database (same naming scheme as Suite) + postgis.
        let admin_url = pg_admin_url();
        let db = format!("conf_{name}");
        let mut client = postgres::Client::connect(&admin_url, postgres::NoTls)
            .expect("connecting to PG_URL (is the postgres container up?)");
        client
            .batch_execute(&format!("DROP DATABASE IF EXISTS {db} WITH (FORCE)"))
            .unwrap();
        client
            .batch_execute(&format!("CREATE DATABASE {db}"))
            .unwrap();
        let (prefix, _) = admin_url
            .rsplit_once('/')
            .expect("PG_URL must contain a db path");
        let db_url = format!("{prefix}/{db}");
        postgres::Client::connect(&db_url, postgres::NoTls)
            .unwrap()
            .batch_execute("create extension if not exists postgis")
            .unwrap();

        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let log_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/conformance-logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log = std::fs::File::create(log_dir.join(format!("{name}.log"))).unwrap();

        let mut cmd = Command::new(engine_binary());
        cmd.arg("--port")
            .arg(port.to_string())
            .env("DIST_API_DATABASE_URL", &db_url)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        for (k, v) in env {
            cmd.env(k, v);
        }
        let child = cmd.spawn().expect("spawning dist-api");

        let e = EnvEngine {
            name: name.to_string(),
            base_url: format!("http://127.0.0.1:{port}"),
            ws_base: format!("ws://127.0.0.1:{port}"),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap(),
            child,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(r) = e.http.get(format!("{}/healthz", e.base_url)).send()
                && r.status().is_success()
            {
                return e;
            }
            assert!(
                Instant::now() < deadline,
                "engine for suite {name} did not become healthy; see target/conformance-logs/{name}.log"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn post(&self, path: &str, body: &Json, headers: &[(String, String)]) -> (u16, Json) {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base_url))
            .json(body);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = req.send().expect("http request failed");
        let code = resp.status().as_u16();
        let text = resp.text().unwrap_or_default();
        (
            code,
            serde_json::from_str(&text).unwrap_or(Json::String(text)),
        )
    }

    /// Setup/teardown with the admin secret attached (tests-py hge_ctx adds
    /// the secret to its own API calls even when the test uses add_auth=False).
    fn apply_with_secret(&self, rel: &str, endpoint: &str) {
        let body = load_fixture(&fixture_root().join(rel)).expect("loading setup fixture");
        let (code, resp) = self.post(
            endpoint,
            &body,
            &[("X-Hasura-Admin-Secret".to_string(), SECRET.to_string())],
        );
        assert!(
            code < 300,
            "[{}] setup {rel} via {endpoint} failed ({code}):\n{resp:#}",
            self.name
        );
    }

    /// check_query_f with add_auth=False: only the fixture's own headers go
    /// out. Single-document fixtures only (all three ported files are).
    fn check_query_f_no_auth(&self, rel: &str, transport: Transport) {
        let conf = load_fixture(&fixture_root().join(rel)).expect("loading test fixture");
        let headers: Vec<(String, String)> = conf
            .get("headers")
            .and_then(Json::as_object)
            .map(|h| {
                h.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            v.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| v.to_string()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let query_text = conf["query"]["query"].as_str();

        if matches!(transport, Transport::Http | Transport::Both) {
            let url = conf["url"].as_str().expect("conf.url");
            let exp_status = conf.get("status").and_then(Json::as_u64).unwrap_or(200) as u16;
            let (code, resp) = self.post(url, &conf["query"], &headers);
            assert_eq!(
                code, exp_status,
                "[{}] {rel}: status mismatch\nresponse:\n{resp:#}",
                self.name
            );
            assert!(
                response_matches(&conf["response"], &resp, query_text),
                "[{}] {rel}: response mismatch\nexpected:\n{:#}\nactual:\n{resp:#}",
                self.name,
                conf["response"]
            );
        }
        if matches!(transport, Transport::Ws | Transport::Both) {
            self.ws_case(&conf, &headers, rel);
        }
    }

    /// Legacy Apollo graphql-ws flow, mirroring the harness's ws_case but
    /// with the fixture headers only in the connection_init payload.
    fn ws_case(&self, conf: &Json, headers: &[(String, String)], label: &str) {
        use tungstenite::Message;
        use tungstenite::client::IntoClientRequest;

        let url = conf["url"].as_str().unwrap();
        let mut req = format!("{}{url}", self.ws_base)
            .into_client_request()
            .expect("ws request");
        req.headers_mut()
            .insert("Sec-WebSocket-Protocol", "graphql-ws".parse().unwrap());
        let (mut sock, _) = tungstenite::connect(req).expect("ws connect");

        let mut init_payload = Map::new();
        if !headers.is_empty() {
            init_payload.insert(
                "headers".into(),
                Json::Object(headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
            );
        }
        sock.send(Message::text(
            json!({"type": "connection_init", "payload": init_payload}).to_string(),
        ))
        .unwrap();
        let frame = next_frame(&mut sock, label);
        assert_eq!(
            frame["type"], "connection_ack",
            "[{label}] ws init failed: {frame:#}"
        );

        sock.send(Message::text(
            json!({"id": "hge_test", "type": "start", "payload": conf["query"]}).to_string(),
        ))
        .unwrap();
        let frame = next_frame(&mut sock, label);
        let payload = if frame["type"] == "error" {
            json!({ "errors": [frame["payload"].clone()] })
        } else {
            frame["payload"].clone()
        };
        assert!(
            response_matches(&conf["response"], &payload, conf["query"]["query"].as_str()),
            "[{}] {label} (ws): response mismatch\nexpected:\n{:#}\nactual:\n{payload:#}",
            self.name,
            conf["response"]
        );
        if conf["response"].get("errors").is_none() {
            let done = next_frame(&mut sock, label);
            assert_eq!(done["type"], "complete", "[{label}] expected complete");
        }
        let _ = sock.close(None);
    }
}

fn next_frame<S: std::io::Read + std::io::Write>(
    sock: &mut tungstenite::WebSocket<S>,
    label: &str,
) -> Json {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "[{label}] timed out waiting for ws frame"
        );
        let msg = sock.read().expect("ws read");
        if !msg.is_text() {
            continue;
        }
        let v: Json = serde_json::from_str(msg.to_text().unwrap()).expect("ws frame json");
        if v["type"] == "ka" {
            continue;
        }
        return v;
    }
}

// -------------------------------------------------------------------- suites

const UNAUTH: &str = "queries/unauthorized_role";

/// test_graphql_queries.py::TestUnauthorizedRolePermission
/// Marks: parametrize(transport: http+websocket), per_class_tests_db_state,
/// admin_secret, hge_env(HASURA_GRAPHQL_UNAUTHORIZED_ROLE=anonymous).
/// The single test runs check_query_f(..., transport, add_auth=False): the
/// request carries X-Hasura-Role: admin but NO admin secret, so the headers
/// are untrusted and the session must fall back to the anonymous role.
#[test]
fn unauthorized_role_permission() {
    let e = EnvEngine::start(
        "unauth_role",
        &[
            ("HASURA_GRAPHQL_ADMIN_SECRET", SECRET),
            ("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous"),
        ],
    );
    e.apply_with_secret(&format!("{UNAUTH}/setup.yaml"), "/v1/query");
    // test_unauth_role
    e.check_query_f_no_auth(&format!("{UNAUTH}/unauthorized_role.yaml"), Transport::Both);
    e.apply_with_secret(&format!("{UNAUTH}/teardown.yaml"), "/v1/query");
}

/// test_graphql_queries.py::TestFallbackUnauthorizedRoleCookie
/// Marks: per_class_tests_db_state, admin_secret,
/// hge_env(HASURA_GRAPHQL_UNAUTHORIZED_ROLE=anonymous) — note: NO jwt mark
/// on this class (its JWT twin is TestFallbackUnauthorizedRoleCookieWithJwt,
/// not part of this batch). check_query_f is called without a transport
/// argument -> http only, add_auth=False.
#[test]
fn fallback_unauthorized_role_cookie() {
    let e = EnvEngine::start(
        "cookie_fallback",
        &[
            ("HASURA_GRAPHQL_ADMIN_SECRET", SECRET),
            ("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous"),
        ],
    );
    e.apply_with_secret(&format!("{UNAUTH}/setup.yaml"), "/v1/query");
    // test_fallback_unauth_role_jwt_cookie_not_set
    e.check_query_f_no_auth(
        &format!("{UNAUTH}/cookie_header_absent_unauth_role_set.yaml"),
        Transport::Http,
    );
    e.apply_with_secret(&format!("{UNAUTH}/teardown.yaml"), "/v1/query");
}

/// test_graphql_queries.py::TestMissingUnauthorizedRoleAndCookie
/// Marks: per_class_tests_db_state + jwt_configuration, admin_secret,
/// jwt('rsa') — JWT mode (RS512, public key from fixtures/jwt_keys) and NO
/// HASURA_GRAPHQL_UNAUTHORIZED_ROLE. The request sends a (non-token) Cookie
/// header and no Authorization, so JWT auth must fail with invalid-headers.
/// http only, add_auth=False.
#[test]
fn missing_unauthorized_role_and_cookie() {
    let jwt = rsa_jwt_secret();
    let e = EnvEngine::start(
        "cookie_missing",
        &[
            ("HASURA_GRAPHQL_ADMIN_SECRET", SECRET),
            ("HASURA_GRAPHQL_JWT_SECRET", &jwt),
        ],
    );
    e.apply_with_secret(&format!("{UNAUTH}/setup.yaml"), "/v1/query");
    // test_error_unauth_role_not_set_jwt_cookie_not_set
    e.check_query_f_no_auth(
        &format!("{UNAUTH}/cookie_header_absent_unauth_role_not_set.yaml"),
        Transport::Http,
    );
    e.apply_with_secret(&format!("{UNAUTH}/teardown.yaml"), "/v1/query");
}

const FUNC_PERMS: &str = "queries/graphql_query/functions/permissions";

/// test_graphql_queries.py::TestGraphQLQueryFunctionPermissions
/// Marks: parametrize(transport), per_method_tests_db_state, admin_secret,
/// hge_env(HASURA_GRAPHQL_INFER_FUNCTION_PERMISSIONS=false).
/// Every check_query_f call omits the transport argument -> http only.
/// per_method_tests_db_state -> setup/teardown wrap EACH test method.
/// NOTE: the admin_secret mark is purely environmental here — tests-py
/// sends the secret alongside the X-Hasura-Role headers, which yields the
/// same trusted-role session a secretless engine produces, and no fixture
/// asserts on the secret itself — so the suite runs without it.
#[test]
fn graphql_query_function_permissions() {
    let s = Suite::new("function_perms")
        .env("HASURA_GRAPHQL_INFER_FUNCTION_PERMISSIONS", "false")
        .start();

    // test_access_function_with_table_permissions
    s.setup_v1q(&format!("{FUNC_PERMS}/setup.yaml"));
    s.check_query_f(
        &format!("{FUNC_PERMS}/get_messages_with_table_permissions.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{FUNC_PERMS}/teardown.yaml"));

    // test_access_function_without_permission_configured
    s.setup_v1q(&format!("{FUNC_PERMS}/setup.yaml"));
    s.check_query_f(
        &format!("{FUNC_PERMS}/get_articles_without_permission_configured.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{FUNC_PERMS}/teardown.yaml"));

    // test_access_function_with_permission_configured
    // (hge_ctx.v1metadataq_f first, then the check)
    s.setup_v1q(&format!("{FUNC_PERMS}/setup.yaml"));
    s.apply(
        &format!("{FUNC_PERMS}/add_function_permission_get_articles.yaml"),
        "/v1/metadata",
    );
    s.check_query_f(
        &format!("{FUNC_PERMS}/get_articles_with_permission_configured.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{FUNC_PERMS}/teardown.yaml"));
}

const ALLOWLIST: &str = "queries/graphql_query/allowlist";

/// test_allowlist_queries.py::TestAllowlistQueries
/// Module pytestmark: hge_env(HASURA_GRAPHQL_ENABLE_ALLOWLIST=true); the
/// class itself carries no admin_secret mark. Class is parametrized over
/// http+websocket and passes the transport through -> Transport::Both,
/// except test_update_query which pytest.skips non-http transports.
#[test]
fn allowlist_queries() {
    let s = Suite::new("allowlist")
        .env("HASURA_GRAPHQL_ENABLE_ALLOWLIST", "true")
        .start();
    s.setup_v1q(&format!("{ALLOWLIST}/setup.yaml"));

    for f in [
        "query_user.yaml",
        "query_user_by_pk.yaml",
        "query_user_with_typename.yaml",
        "query_non_allowlist.yaml",
        "query_user_fragment.yaml",
    ] {
        s.check_query_f(&format!("{ALLOWLIST}/{f}"), Transport::Both);
    }
    // query_as_admin.yaml: no-role (admin) request. The allowlist is not
    // enforced for the admin role (Hasura parity), so this non-allowlisted
    // query succeeds even with the allowlist enabled.
    s.check_query_f(&format!("{ALLOWLIST}/query_as_admin.yaml"), Transport::Both);

    // test_update_query: http-only in pytest (explicit skip on websocket);
    // runs two fixture files in sequence.
    s.check_query_f(&format!("{ALLOWLIST}/update_query.yaml"), Transport::Http);
    s.check_query_f(
        &format!("{ALLOWLIST}/add_duplicate_query.yaml"),
        Transport::Http,
    );

    s.teardown_v1q(&format!("{ALLOWLIST}/teardown.yaml"));
}
