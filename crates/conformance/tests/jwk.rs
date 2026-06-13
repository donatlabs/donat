//! Ported from tests-py test_jwk.py: the engine's background JWKS refresher
//! (HASURA_GRAPHQL_JWT_SECRET = {"jwk_url": ...}) must re-fetch the key set
//! on a schedule driven by the stub's Cache-Control / Expires headers.
//!
//! The stub mirrors tests-py jwk_server.py: GET /jwk-cache-control builds a
//! Cache-Control header from its query string (`k=true` -> `k`, numeric ->
//! `k=<n>`) and always adds a far-future Expires (HGE must prefer
//! Cache-Control); GET /jwk-expires sends only an Expires header. Instead of
//! the python /state + /reset-state endpoints we count requests in-process
//! with atomics.
//!
//! Engine refresh intervals (crates/server/src/jwt.rs): no-store -> 1s,
//! no-cache -> 2s, max-age=N -> N, Expires-only -> 3s. Each python test
//! resets the counter at an arbitrary phase of that cycle and waits for N
//! fetches, so the expected counts below are phase-safe: with interval I the
//! k-th post-reset fetch lands at phase + (k-1)*I >= (k-1)*I seconds.
//!
//! tests-py asserts nothing beyond fetch counts/timing; on top of that each
//! scenario verifies an RS256 token (kid matching the served JWKS)
//! authorizes a simple role query, proving the fetched keys are actually
//! used.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use donat_conformance::{Suite, fixture_root};
use serde_json::{Value as Json, json};

const ADMIN_SECRET: &str = "jwk-test-secret";
const KID: &str = "test-key-1";

// ------------------------------------------------------------- JWKS stub

struct JwkStub {
    url: String,
    /// Fetch counters, keyed like the python stub's state dict.
    cache_control: Arc<AtomicUsize>,
    expires: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct StubState {
    cache_control: Arc<AtomicUsize>,
    expires: Arc<AtomicUsize>,
    jwks_body: String,
}

/// The engine only checks Expires for *presence* (it never parses the
/// value), so a fixed far-future RFC 1123 date stands in for the python
/// stub's computed timestamps.
const EXPIRES_VALUE: &str = "Thu, 01 Jan 2099 00:00:00 GMT";

/// `max-age=3&must-revalidate=true` -> `max-age=3, must-revalidate`,
/// replicating jwk_server.py's header construction.
fn cache_control_from_query(qs: &str) -> String {
    let mut vals = vec![];
    for pair in qs.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if v == "true" {
            vals.push(k.to_string());
        } else if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
            vals.push(format!("{k}={v}"));
        }
    }
    vals.join(", ")
}

async fn jwk_cache_control(
    axum::extract::State(st): axum::extract::State<StubState>,
    axum::extract::RawQuery(qs): axum::extract::RawQuery,
) -> impl axum::response::IntoResponse {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Content-Type", "application/json".parse().unwrap());
    headers.insert(
        "Cache-Control",
        cache_control_from_query(qs.as_deref().unwrap_or(""))
            .parse()
            .unwrap(),
    );
    // jwk_server.py: "HGE should always prefer Cache-Control over Expires".
    headers.insert("Expires", EXPIRES_VALUE.parse().unwrap());
    st.cache_control.fetch_add(1, Ordering::SeqCst);
    (headers, st.jwks_body.clone())
}

async fn jwk_expires(
    axum::extract::State(st): axum::extract::State<StubState>,
) -> impl axum::response::IntoResponse {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("Content-Type", "application/json".parse().unwrap());
    headers.insert("Expires", EXPIRES_VALUE.parse().unwrap());
    st.expires.fetch_add(1, Ordering::SeqCst);
    (headers, st.jwks_body.clone())
}

fn start_jwk_stub() -> JwkStub {
    let jwk: Json = serde_json::from_str(
        &std::fs::read_to_string(fixture_root().join("jwt_keys/rsa_jwk.json"))
            .expect("reading rsa_jwk.json"),
    )
    .expect("parsing rsa_jwk.json");
    let state = StubState {
        cache_control: Arc::new(AtomicUsize::new(0)),
        expires: Arc::new(AtomicUsize::new(0)),
        jwks_body: json!({ "keys": [jwk] }).to_string(),
    };
    let stub = JwkStub {
        url: String::new(),
        cache_control: state.cache_control.clone(),
        expires: state.expires.clone(),
    };

    let (tx, rx) = std::sync::mpsc::channel::<u16>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stub runtime");
        rt.block_on(async move {
            let app = axum::Router::new()
                .route("/jwk-cache-control", axum::routing::get(jwk_cache_control))
                .route("/jwk-expires", axum::routing::get(jwk_expires))
                .with_state(state);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("stub bind");
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            axum::serve(listener, app).await.expect("stub serve");
        });
    });
    let port = rx.recv().expect("stub port");
    JwkStub {
        url: format!("http://127.0.0.1:{port}"),
        ..stub
    }
}

// ------------------------------------------------------- timing harness

/// Replicates test_jwk.py wait_until_request_count_reaches: reset the
/// counter, then poll every 200ms until it reaches `num_requests` (panic
/// past `timeout_secs`). Returns seconds elapsed since the reset, measured
/// at the poll that observed the count (so it never under-reports relative
/// to the fetch that satisfied it).
fn wait_until_request_count_reaches(
    counter: &AtomicUsize,
    num_requests: usize,
    timeout_secs: u64,
) -> f64 {
    let start = Instant::now();
    counter.store(0, Ordering::SeqCst);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if counter.load(Ordering::SeqCst) >= num_requests {
            return start.elapsed().as_secs_f64();
        }
        assert!(
            start.elapsed().as_secs() <= timeout_secs,
            "waited {:.1}s for JWK requests to reach {num_requests}; only got {}",
            start.elapsed().as_secs_f64(),
            counter.load(Ordering::SeqCst)
        );
    }
}

// --------------------------------------------------------- JWT plumbing

fn sign_rs256_jwt() -> String {
    let pem = std::fs::read(fixture_root().join("jwt_keys/rsa_private.pem"))
        .expect("reading rsa_private.pem");
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(&pem).expect("rsa private pem");
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KID.to_string());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({
        "sub": "1",
        "iat": now,
        "exp": now + 3600,
        "https://hasura.io/jwt/claims": {
            "x-hasura-allowed-roles": ["user"],
            "x-hasura-default-role": "user",
            "x-hasura-user-id": "1",
        }
    });
    jsonwebtoken::encode(&header, &claims, &key).expect("signing jwt")
}

/// Accumulate the table + `user` select permission as metadata (run before
/// the engine is started, so it boots with them). The fetched JWKS is then
/// proven to verify tokens by `query_with_jwt`.
fn setup_account_metadata(s: &donat_conformance::Running) {
    let setup = json!({
        "type": "bulk",
        "args": [
            { "type": "run_sql",
              "args": { "sql": "create table account (id serial primary key, name text); insert into account (name) values ('alice');" } },
            { "type": "track_table",
              "args": { "schema": "public", "name": "account" } },
            { "type": "create_select_permission",
              "args": { "table": "account", "role": "user",
                        "permission": { "columns": ["id", "name"], "filter": {} } } }
        ]
    });
    // Applied in-harness (no admin API); the engine boots with this metadata.
    s.post("/v1/query", &setup, &[]);
}

/// Run a GraphQL query authorized only by an RS256 bearer token carrying the
/// stub's kid, proving the fetched keys are actually used.
fn query_with_jwt(s: &donat_conformance::Running) {
    let bearer = [(
        "Authorization".to_string(),
        format!("Bearer {}", sign_rs256_jwt()),
    )];
    let (code, resp) = s.post(
        "/v1/graphql",
        &json!({ "query": "query { account { id name } }" }),
        &bearer,
    );
    assert_eq!(code, 200, "jwt-authorized query failed: {resp}");
    assert_eq!(
        resp,
        json!({ "data": { "account": [{ "id": 1, "name": "alice" }] } }),
        "jwt-authorized query returned unexpected response"
    );
}

// -------------------------------------------------------------- scenarios

enum StateKey {
    CacheControl,
    Expires,
}

/// Engine startup runs `create extension postgis`, which is memory-hungry;
/// six of them at once can get a postgres backend OOM-killed in the test
/// container (taking the whole instance into crash recovery). Serialize
/// startup only — the timing phases still overlap freely.
static ENGINE_START: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn run_scenario(
    suite: &str,
    jwk_path: &str,
    state_key: StateKey,
    num_requests: usize,
    timeout_secs: u64,
    min_elapsed: Option<f64>,
) {
    let stub = start_jwk_stub();
    let s = {
        let _start = ENGINE_START
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let s = Suite::new(suite)
            .admin_secret(ADMIN_SECRET)
            .env(
                "HASURA_GRAPHQL_JWT_SECRET",
                &format!(r#"{{"jwk_url": "{}{}"}}"#, stub.url, jwk_path),
            )
            .start();
        // Accumulate metadata first, then force the engine to start (it
        // begins fetching the JWKS at boot — what this suite times). The
        // engine starts lazily, so we must trigger it before the wait loop.
        setup_account_metadata(&s);
        let _ = s.base_url();
        s
    };

    let counter = match state_key {
        StateKey::CacheControl => &stub.cache_control,
        StateKey::Expires => &stub.expires,
    };
    let time_elapsed = wait_until_request_count_reaches(counter, num_requests, timeout_secs);
    if let Some(min) = min_elapsed {
        assert!(
            time_elapsed >= min,
            "expected at least {min}s for {num_requests} JWK fetches, got {time_elapsed:.2}s"
        );
    }

    query_with_jwt(&s);
}

// test_cache_control_header_max_age: max-age=3 -> refresh interval 3s; one
// post-reset fetch arrives well within the 70s budget.
#[test]
fn jwk_cache_control_header_max_age() {
    run_scenario(
        "jwk_cc_max_age",
        "/jwk-cache-control?max-age=3",
        StateKey::CacheControl,
        1,
        70,
        None,
    );
}

// test_cache_control_header_max_age_must_revalidate: must-revalidate does
// not change the max-age-driven interval.
#[test]
fn jwk_cache_control_header_max_age_must_revalidate() {
    run_scenario(
        "jwk_cc_max_age_revalidate",
        "/jwk-cache-control?max-age=3&must-revalidate=true",
        StateKey::CacheControl,
        1,
        70,
        None,
    );
}

// test_cache_control_header_must_revalidate: no max-age/no-* directive, so
// the stub's Expires header drives a 3s interval; 3 fetches >= 2s apart.
#[test]
fn jwk_cache_control_header_must_revalidate() {
    run_scenario(
        "jwk_cc_must_revalidate",
        "/jwk-cache-control?must-revalidate=true",
        StateKey::CacheControl,
        3,
        190,
        Some(2.0),
    );
}

// test_cache_control_header_no_cache_public: no-cache -> 2s interval; the
// second post-reset fetch lands >= 2s after reset regardless of phase.
#[test]
fn jwk_cache_control_header_no_cache_public() {
    run_scenario(
        "jwk_cc_no_cache_public",
        "/jwk-cache-control?no-cache=true&public=true",
        StateKey::CacheControl,
        2,
        130,
        Some(2.0),
    );
}

// test_cache_control_header_no_store_max_age: no-store wins over max-age ->
// 1s interval; the third post-reset fetch lands >= 2s after reset.
#[test]
fn jwk_cache_control_header_no_store_max_age() {
    run_scenario(
        "jwk_cc_no_store_max_age",
        "/jwk-cache-control?no-store=true&max-age=3",
        StateKey::CacheControl,
        3,
        190,
        Some(2.0),
    );
}

// test_expires_header: Expires-only response -> 3s interval (the fixture
// uses a three second expiry, so one post-reset fetch within the budget).
#[test]
fn jwk_expires_header() {
    run_scenario(
        "jwk_expires",
        "/jwk-expires?seconds=3",
        StateKey::Expires,
        1,
        70,
        None,
    );
}
