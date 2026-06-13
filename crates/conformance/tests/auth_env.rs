//! Auth- and env-dependent suites ported from tests-py:
//! - test_graphql_queries.py: TestUnauthorizedRolePermission,
//!   TestFallbackUnauthorizedRoleCookie, TestMissingUnauthorizedRoleAndCookie,
//!   TestGraphQLQueryFunctionPermissions
//! - test_allowlist_queries.py: TestAllowlistQueries
//!
//! The unauthorized-role/cookie classes are marked `@pytest.mark.admin_secret`
//! AND their tests run with `add_auth=False`: the engine must have the secret
//! configured (so plain X-Hasura-* headers are untrusted) while the checked
//! request carries no secret.
//!
//! With the metadata-driven harness, setup runs in-process (no admin-API
//! POST), so we use the regular `Suite`/`Running`: `.admin_secret(SECRET)`
//! configures the engine env, `setup_v1q` accumulates metadata, and the
//! checked requests use `Running::post` directly with the fixture's own
//! headers only (`Running::post` never attaches the admin secret), giving
//! exactly the `add_auth=False` semantics.

use serde_json::{Map, Value as Json, json};

use dist_conformance::{Running, Suite, Transport, fixture_root, load_fixture, response_matches};

/// Same role as tests-py's --hge-key: an API-level secret, never a data role.
const SECRET: &str = "conformance_admin_secret";

/// HASURA_GRAPHQL_JWT_SECRET for `@pytest.mark.jwt('rsa')`
/// (fixtures/jwt.py::init_rsa builds {"type": "RS512", "key": <public pem>}).
fn rsa_jwt_secret() -> String {
    let pem = std::fs::read_to_string(fixture_root().join("jwt_keys/rsa_public.pem"))
        .expect("jwt_keys/rsa_public.pem (see fixtures/jwt_keys/README.md)");
    json!({"type": "RS512", "key": pem}).to_string()
}

/// The fixture's own headers (`add_auth=False`: no admin secret attached).
fn conf_headers(conf: &Json) -> Vec<(String, String)> {
    conf.get("headers")
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
        .unwrap_or_default()
}

/// check_query_f with add_auth=False: only the fixture's own headers go out.
/// Single-document fixtures only (all three ported files are).
fn check_query_f_no_auth(s: &Running, rel: &str, transport: Transport) {
    let conf = load_fixture(&fixture_root().join(rel)).expect("loading test fixture");
    let headers = conf_headers(&conf);
    let query_text = conf["query"]["query"].as_str();

    if matches!(transport, Transport::Http | Transport::Both) {
        let url = conf["url"].as_str().expect("conf.url");
        let exp_status = conf.get("status").and_then(Json::as_u64).unwrap_or(200) as u16;
        let (code, resp) = s.post(url, &conf["query"], &headers);
        assert_eq!(
            code, exp_status,
            "[{}] {rel}: status mismatch\nresponse:\n{resp:#}",
            s.name
        );
        assert!(
            response_matches(&conf["response"], &resp, query_text),
            "[{}] {rel}: response mismatch\nexpected:\n{:#}\nactual:\n{resp:#}",
            s.name,
            conf["response"]
        );
    }
    if matches!(transport, Transport::Ws | Transport::Both) {
        ws_case(s, &conf, &headers, rel);
    }
}

/// Legacy Apollo graphql-ws flow with the fixture headers only in the
/// connection_init payload.
fn ws_case(s: &Running, conf: &Json, headers: &[(String, String)], label: &str) {
    use tungstenite::Message;
    use tungstenite::client::IntoClientRequest;

    let url = conf["url"].as_str().unwrap();
    let mut req = format!("{}{url}", s.ws_base())
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
        s.name,
        conf["response"]
    );
    if conf["response"].get("errors").is_none() {
        let done = next_frame(&mut sock, label);
        assert_eq!(done["type"], "complete", "[{label}] expected complete");
    }
    let _ = sock.close(None);
}

fn next_frame<S: std::io::Read + std::io::Write>(
    sock: &mut tungstenite::WebSocket<S>,
    label: &str,
) -> Json {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
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
    let s = Suite::new("unauth_role")
        .admin_secret(SECRET)
        .env("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous")
        .start();
    s.setup_v1q(&format!("{UNAUTH}/setup.yaml"));
    // test_unauth_role
    check_query_f_no_auth(&s, &format!("{UNAUTH}/unauthorized_role.yaml"), Transport::Both);
    s.teardown_v1q(&format!("{UNAUTH}/teardown.yaml"));
}

/// test_graphql_queries.py::TestFallbackUnauthorizedRoleCookie
/// Marks: per_class_tests_db_state, admin_secret,
/// hge_env(HASURA_GRAPHQL_UNAUTHORIZED_ROLE=anonymous). check_query_f is
/// called without a transport argument -> http only, add_auth=False.
#[test]
fn fallback_unauthorized_role_cookie() {
    let s = Suite::new("cookie_fallback")
        .admin_secret(SECRET)
        .env("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous")
        .start();
    s.setup_v1q(&format!("{UNAUTH}/setup.yaml"));
    // test_fallback_unauth_role_jwt_cookie_not_set
    check_query_f_no_auth(
        &s,
        &format!("{UNAUTH}/cookie_header_absent_unauth_role_set.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{UNAUTH}/teardown.yaml"));
}

/// test_graphql_queries.py::TestMissingUnauthorizedRoleAndCookie
/// Marks: per_class_tests_db_state + jwt_configuration, admin_secret,
/// jwt('rsa') — JWT mode (RS512) and NO HASURA_GRAPHQL_UNAUTHORIZED_ROLE.
/// The request sends a (non-token) Cookie header and no Authorization, so
/// JWT auth must fail with invalid-headers. http only, add_auth=False.
#[test]
fn missing_unauthorized_role_and_cookie() {
    let jwt = rsa_jwt_secret();
    let s = Suite::new("cookie_missing")
        .admin_secret(SECRET)
        .env("HASURA_GRAPHQL_JWT_SECRET", &jwt)
        .start();
    s.setup_v1q(&format!("{UNAUTH}/setup.yaml"));
    // test_error_unauth_role_not_set_jwt_cookie_not_set
    check_query_f_no_auth(
        &s,
        &format!("{UNAUTH}/cookie_header_absent_unauth_role_not_set.yaml"),
        Transport::Http,
    );
    s.teardown_v1q(&format!("{UNAUTH}/teardown.yaml"));
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
///
/// Each method is its own suite (own engine + metadata): the methods differ
/// only in which function permissions exist, and the lazy engine boots from a
/// single accumulated metadata snapshot — so per-method isolation maps to one
/// suite per method (mirroring pytest's per_method_tests_db_state).
fn function_perms_suite(name: &str) -> Running {
    let s = Suite::new(name)
        .env("HASURA_GRAPHQL_INFER_FUNCTION_PERMISSIONS", "false")
        .start();
    s.setup_v1q(&format!("{FUNC_PERMS}/setup.yaml"));
    s
}

#[test]
fn function_perms_with_table_permissions() {
    // test_access_function_with_table_permissions.
    // get_messages_with_table_permissions.yaml embeds an
    // `add_function_permission get_messages` step between its two query
    // steps; that runtime mutation can't reach the already-running engine,
    // so the get_messages permission is pre-loaded into the boot metadata.
    let s = function_perms_suite("function_perms_table");
    s.post(
        "/v1/metadata",
        &json!({
            "type": "pg_create_function_permission",
            "args": { "role": "user", "function": "get_messages" }
        }),
        &[],
    );
    s.check_query_f(
        &format!("{FUNC_PERMS}/get_messages_with_table_permissions.yaml"),
        Transport::Http,
    );
}

#[test]
fn function_perms_without_permission_configured() {
    // test_access_function_without_permission_configured: no function
    // permission for get_articles -> the field is not exposed to `user`.
    let s = function_perms_suite("function_perms_none");
    s.check_query_f(
        &format!("{FUNC_PERMS}/get_articles_without_permission_configured.yaml"),
        Transport::Http,
    );
}

#[test]
fn function_perms_with_permission_configured() {
    // test_access_function_with_permission_configured: the get_articles
    // function permission is applied (boot metadata) and the query succeeds.
    let s = function_perms_suite("function_perms_with");
    s.apply(
        &format!("{FUNC_PERMS}/add_function_permission_get_articles.yaml"),
        "/v1/metadata",
    );
    s.check_query_f(
        &format!("{FUNC_PERMS}/get_articles_with_permission_configured.yaml"),
        Transport::Http,
    );
}

const ALLOWLIST: &str = "queries/graphql_query/allowlist";

/// test_allowlist_queries.py::TestAllowlistQueries
/// Module pytestmark: hge_env(HASURA_GRAPHQL_ENABLE_ALLOWLIST=true); the
/// class itself carries no admin_secret mark. Class is parametrized over
/// http+websocket and passes the transport through -> Transport::Both,
/// except test_update_query which pytest.skips non-http transports.
///
/// The allowlist (query collection + allowlist entry) is set up as metadata
/// (create_query_collection / add_collection_to_allowlist accumulate into the
/// metadata directory), so allowlist ENFORCEMENT is exercised via YAML.
///
/// test_update_query dropped: it exercised the runtime mutation of an
/// allowlisted collection (add_query_to_collection at request time) and the
/// duplicate-query error from that mutation API — both are management-API
/// behaviors, not enforcement, and the admin API is going away.
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

    s.teardown_v1q(&format!("{ALLOWLIST}/teardown.yaml"));
}
