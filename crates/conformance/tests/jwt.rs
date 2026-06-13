//! Ported from tests-py test_jwt.py (441 parametrized tests).
//!
//! One Rust #[test] per pytest class; inside each, the full behavior matrix
//! runs against both `/v1/graphql` (errors are HTTP 200) and
//! `/v1alpha1/graphql` (errors are HTTP 400), exactly like the
//! `@pytest.mark.parametrize('endpoint', ...)` on the abstract classes.
//!
//! AbstractTestSubscriptionJwtExpiry (3 tests: websocket token-expiry
//! connection close) is intentionally NOT ported here — it belongs to the
//! subscriptions port.
//!
//! Keys are static test-only fixtures (fixtures/jwt_keys/, see README
//! there); tests-py regenerates them per run, the matrix is otherwise
//! identical. Note tests-py 'rsa' means RS512 (fixtures/jwt.py::init_rsa).

use std::time::{SystemTime, UNIX_EPOCH};

use donat_conformance::{Running, Suite, fixture_root, load_fixture, response_matches};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Map, Value as Json, json};

const PERMS: &str = "queries/graphql_query/permissions";
const ENDPOINTS: [&str; 2] = ["/v1/graphql", "/v1alpha1/graphql"];

// ------------------------------------------------------------ configuration

#[derive(Clone, Copy)]
enum Alg {
    Rsa,     // tests-py 'rsa' -> RS512
    Ed25519, // tests-py 'ed25519' -> EdDSA
    Es,      // tests-py 'es' -> ES256
}

impl Alg {
    fn key_type(self) -> &'static str {
        match self {
            Alg::Rsa => "RS512",
            Alg::Ed25519 => "Ed25519",
            Alg::Es => "ES256",
        }
    }

    fn algorithm(self) -> Algorithm {
        match self {
            Alg::Rsa => Algorithm::RS512,
            Alg::Ed25519 => Algorithm::EdDSA,
            Alg::Es => Algorithm::ES256,
        }
    }

    fn stem(self) -> &'static str {
        match self {
            Alg::Rsa => "rsa",
            Alg::Ed25519 => "ed25519",
            Alg::Es => "es256",
        }
    }

    fn public_pem(self) -> String {
        std::fs::read_to_string(fixture_root().join(format!("jwt_keys/{}_public.pem", self.stem())))
            .expect("reading public key")
    }

    fn encoding_key(self) -> EncodingKey {
        let pem =
            std::fs::read(fixture_root().join(format!("jwt_keys/{}_private.pem", self.stem())))
                .expect("reading private key");
        match self {
            Alg::Rsa => EncodingKey::from_rsa_pem(&pem),
            Alg::Ed25519 => EncodingKey::from_ed_pem(&pem),
            Alg::Es => EncodingKey::from_ec_pem(&pem),
        }
        .expect("parsing private key")
    }
}

/// Where the engine reads the token from (the `header` key of the JWT
/// secret config), mirroring tests-py `mk_authz_header`.
#[derive(Clone, Copy)]
enum TokenIn {
    Bearer,
    Cookie(&'static str),
    CustomHeader(&'static str),
}

/// One pytest class = one of these (the `@pytest.mark.jwt(alg, {...})`
/// marker arguments).
struct JwtCfg {
    alg: Alg,
    location: TokenIn,
    stringified: bool,
    ns_path: Option<&'static str>,
    audience: Option<Json>,
    issuer: Option<&'static str>,
    allowed_skew: Option<u64>,
}

impl JwtCfg {
    fn new(alg: Alg) -> Self {
        JwtCfg {
            alg,
            location: TokenIn::Bearer,
            stringified: false,
            ns_path: None,
            audience: None,
            issuer: None,
            allowed_skew: None,
        }
    }

    fn location(mut self, location: TokenIn) -> Self {
        self.location = location;
        self
    }

    fn stringified(mut self) -> Self {
        self.stringified = true;
        self
    }

    fn ns_path(mut self, path: &'static str) -> Self {
        self.ns_path = Some(path);
        self
    }

    fn audience(mut self, aud: Json) -> Self {
        self.audience = Some(aud);
        self
    }

    fn issuer(mut self, iss: &'static str) -> Self {
        self.issuer = Some(iss);
        self
    }

    fn allowed_skew(mut self, secs: u64) -> Self {
        self.allowed_skew = Some(secs);
        self
    }

    /// The DONAT_GRAPHQL_JWT_SECRET value, exactly as conftest.py builds
    /// it: {'type': ..., 'key': <public pem>, **marker configuration}.
    fn secret_json(&self) -> String {
        let mut m = Map::new();
        m.insert("type".into(), json!(self.alg.key_type()));
        m.insert("key".into(), json!(self.alg.public_pem()));
        match self.location {
            TokenIn::Bearer => {}
            TokenIn::Cookie(name) => {
                m.insert("header".into(), json!({"type": "Cookie", "name": name}));
            }
            TokenIn::CustomHeader(name) => {
                m.insert(
                    "header".into(),
                    json!({"type": "CustomHeader", "name": name}),
                );
            }
        }
        if self.stringified {
            m.insert("claims_format".into(), json!("stringified_json"));
        }
        if let Some(path) = self.ns_path {
            m.insert("claims_namespace_path".into(), json!(path));
        }
        if let Some(aud) = &self.audience {
            m.insert("audience".into(), aud.clone());
        }
        if let Some(iss) = self.issuer {
            m.insert("issuer".into(), json!(iss));
        }
        if let Some(skew) = self.allowed_skew {
            m.insert("allowed_skew".into(), json!(skew));
        }
        Json::Object(m).to_string()
    }

    /// tests-py `format_claims`: stringified_json JSON-encodes the donat
    /// claims object into a string.
    fn format_claims(&self, donat_claims: Json) -> Json {
        if self.stringified {
            Json::String(donat_claims.to_string())
        } else {
            donat_claims
        }
    }

    /// tests-py `set_claims`: place the donat claims at the configured
    /// namespace path.
    fn set_claims(&self, claims: &mut Map<String, Json>, donat_claims: Json) {
        match self.ns_path {
            None => {
                claims.insert("https://donat.io/jwt/claims".into(), donat_claims);
            }
            Some("$") => {
                let obj = donat_claims
                    .as_object()
                    .expect("root namespace path requires object claims")
                    .clone();
                claims.extend(obj);
            }
            Some("$.donat_claims") => {
                claims.insert("donat_claims".into(), donat_claims);
            }
            Some("$.donat.claims") => {
                claims.insert("donat".into(), json!({"claims": donat_claims}));
            }
            Some("$.donat['claims%']") => {
                claims.insert("donat".into(), json!({"claims%": donat_claims}));
            }
            Some(other) => panic!("unsupported claims_namespace_path: {other}"),
        }
    }

    fn encode(&self, claims: &Map<String, Json>) -> String {
        jsonwebtoken::encode(
            &Header::new(self.alg.algorithm()),
            claims,
            &self.alg.encoding_key(),
        )
        .expect("signing token")
    }

    /// tests-py `mk_authz_header`.
    fn auth_header(&self, token: &str) -> (String, String) {
        match self.location {
            TokenIn::Bearer => ("Authorization".into(), format!("Bearer {token}")),
            TokenIn::Cookie(name) => ("Cookie".into(), format!("{name}={token}")),
            TokenIn::CustomHeader(name) => (name.into(), token.to_string()),
        }
    }
}

// ----------------------------------------------------------------- plumbing

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// The `transact` fixture's base claims (sub/name/iat/exp in the future).
fn base_claims() -> Map<String, Json> {
    let mut m = Map::new();
    m.insert("sub".into(), json!("1234567890"));
    m.insert("name".into(), json!("John Doe"));
    m.insert("iat".into(), json!(now()));
    m.insert("exp".into(), json!(now() + 3600));
    m
}

/// The base test conf every class loads:
/// permissions/user_select_query_unpublished_articles.yaml.
fn base_conf() -> Json {
    load_fixture(&fixture_root().join(format!(
        "{PERMS}/user_select_query_unpublished_articles.yaml"
    )))
    .expect("loading base conf")
}

fn error_body(code: &str, message: &str) -> Json {
    json!({
        "errors": [{
            "extensions": {"code": code, "path": "$"},
            "message": message
        }]
    })
}

/// POST the base conf's query to `endpoint` with the conf headers
/// (X-Donat-Role: user, X-Donat-User-Id: '1') plus `auth`, then assert
/// status + exact body, replicating check_query(..., add_auth=False).
fn run_request(
    s: &Running,
    endpoint: &str,
    auth: (String, String),
    expected_status: u16,
    expected_body: &Json,
    label: &str,
) {
    let conf = base_conf();
    let mut headers: Vec<(String, String)> = conf["headers"]
        .as_object()
        .expect("conf headers")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    headers.push(auth);
    let (code, resp) = s.post(endpoint, &conf["query"], &headers);
    assert_eq!(
        code,
        expected_status,
        "[{}] {label} @ {endpoint}: status mismatch (got {code}, want {expected_status})\nresponse:\n{}",
        s.name,
        serde_json::to_string_pretty(&resp).unwrap()
    );
    let query_text = conf["query"]["query"].as_str();
    assert!(
        response_matches(expected_body, &resp, query_text),
        "[{}] {label} @ {endpoint}: response mismatch\nexpected:\n{}\nactual:\n{}",
        s.name,
        serde_json::to_string_pretty(expected_body).unwrap(),
        serde_json::to_string_pretty(&resp).unwrap()
    );
}

/// Like [`run_request`] but only asserts the HTTP status, returning the
/// body for partial assertions (kept for the FIXME(engine) cases where the
/// exact error body is known to diverge from Donat; currently no live
/// caller — all ported cases assert the full body).
#[allow(dead_code)]
fn run_request_status_only(
    s: &Running,
    endpoint: &str,
    auth: (String, String),
    expected_status: u16,
    label: &str,
) -> Json {
    let conf = base_conf();
    let mut headers: Vec<(String, String)> = conf["headers"]
        .as_object()
        .expect("conf headers")
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    headers.push(auth);
    let (code, resp) = s.post(endpoint, &conf["query"], &headers);
    assert_eq!(
        code,
        expected_status,
        "[{}] {label} @ {endpoint}: status mismatch (got {code}, want {expected_status})\nresponse:\n{}",
        s.name,
        serde_json::to_string_pretty(&resp).unwrap()
    );
    resp
}

fn error_status(endpoint: &str) -> u16 {
    if endpoint == "/v1alpha1/graphql" {
        400
    } else {
        200
    }
}

/// Build a token carrying `donat_claims` (formatted + namespaced per cfg),
/// with optional extra mutation of the registered claims.
fn make_token(
    cfg: &JwtCfg,
    donat_claims: Json,
    tweak: impl FnOnce(&mut Map<String, Json>),
) -> String {
    let mut claims = base_claims();
    let formatted = cfg.format_claims(donat_claims);
    cfg.set_claims(&mut claims, formatted);
    tweak(&mut claims);
    cfg.encode(&claims)
}

fn full_donat_claims() -> Json {
    json!({
        "x-donat-user-id": "1",
        "x-donat-default-role": "user",
        "x-donat-allowed-roles": ["user"],
    })
}

// -------------------------------------------- behaviors (AbstractTestJwtBasic)

fn jwt_valid_claims_success(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(
        cfg,
        json!({
            "x-donat-user-id": "1",
            "x-donat-allowed-roles": ["user", "editor"],
            "x-donat-default-role": "user",
        }),
        |_| {},
    );
    let expected = base_conf()["response"].clone();
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        200,
        &expected,
        "valid_claims_success",
    );
}

fn jwt_invalid_role_in_request_header(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(
        cfg,
        json!({
            "x-donat-user-id": "1",
            "x-donat-allowed-roles": ["contractor", "editor"],
            "x-donat-default-role": "contractor",
        }),
        |_| {},
    );
    let expected = error_body(
        "access-denied",
        "Your requested role is not in allowed roles",
    );
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        error_status(endpoint),
        &expected,
        "invalid_role_in_request_header",
    );
}

fn jwt_no_allowed_roles_in_claim(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(
        cfg,
        json!({
            "x-donat-user-id": "1",
            "x-donat-default-role": "user",
        }),
        |_| {},
    );
    let expected = error_body(
        "jwt-missing-role-claims",
        "JWT claim does not contain x-donat-allowed-roles",
    );
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        error_status(endpoint),
        &expected,
        "no_allowed_roles_in_claim",
    );
}

fn jwt_invalid_allowed_roles_in_claim(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(
        cfg,
        json!({
            "x-donat-user-id": "1",
            "x-donat-allowed-roles": "user",
            "x-donat-default-role": "user",
        }),
        |_| {},
    );
    let expected = error_body(
        "jwt-invalid-claims",
        "invalid x-donat-allowed-roles; should be a list of roles: parsing [] failed, expected Array, but encountered String",
    );
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        error_status(endpoint),
        &expected,
        "invalid_allowed_roles_in_claim",
    );
}

fn jwt_no_default_role(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(
        cfg,
        json!({
            "x-donat-user-id": "1",
            "x-donat-allowed-roles": ["user"],
        }),
        |_| {},
    );
    let expected = error_body(
        "jwt-missing-role-claims",
        "JWT claim does not contain x-donat-default-role",
    );
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        error_status(endpoint),
        &expected,
        "no_default_role",
    );
}

fn jwt_expired(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(cfg, full_donat_claims(), |claims| {
        claims.insert("exp".into(), json!(now() - 60));
    });
    let expected = error_body("invalid-jwt", "Could not verify JWT: JWTExpired");
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        error_status(endpoint),
        &expected,
        "expired",
    );
}

fn jwt_invalid_signature(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    // tests-py signs with HS256 (HS384 would only be picked if the config
    // were HS256, never the case here) and a random key, so both the
    // algorithm and the signature are wrong.
    let mut claims = base_claims();
    let formatted = cfg.format_claims(full_donat_claims());
    cfg.set_claims(&mut claims, formatted);
    let token = jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(b"totally-wrong-key-material"),
    )
    .expect("signing wrong-key token");
    let expected = error_body(
        "invalid-jwt",
        "Could not verify JWT: JWSError JWSInvalidSignature",
    );
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        error_status(endpoint),
        &expected,
        "invalid_signature",
    );
}

fn jwt_no_audience_in_conf(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(cfg, full_donat_claims(), |claims| {
        claims.insert("aud".into(), json!("donat-test-suite"));
    });
    let expected = base_conf()["response"].clone();
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        200,
        &expected,
        "no_audience_in_conf",
    );
}

fn jwt_no_issuer_in_conf(s: &Running, cfg: &JwtCfg, endpoint: &str) {
    let token = make_token(cfg, full_donat_claims(), |claims| {
        claims.insert("iss".into(), json!("rubbish-issuer"));
    });
    let expected = base_conf()["response"].clone();
    run_request(
        s,
        endpoint,
        cfg.auth_header(&token),
        200,
        &expected,
        "no_issuer_in_conf",
    );
}

// --------------------------------------------------------------- suite loops

/// 34 suites = 34 engines, each with its own Postgres pool; running them
/// all at once exhausts max_connections (100) on the shared test Postgres.
/// Gate suite concurrency without touching the harness.
const MAX_PARALLEL_SUITES: u32 = 4;
static GATE: std::sync::Mutex<u32> = std::sync::Mutex::new(0);
static GATE_CV: std::sync::Condvar = std::sync::Condvar::new();

struct Permit;

fn acquire_permit() -> Permit {
    let mut n = GATE.lock().unwrap();
    while *n >= MAX_PARALLEL_SUITES {
        n = GATE_CV.wait(n).unwrap();
    }
    *n += 1;
    Permit
}

impl Drop for Permit {
    fn drop(&mut self) {
        *GATE.lock().unwrap() -= 1;
        GATE_CV.notify_one();
    }
}

fn start(name: &str, cfg: &JwtCfg) -> Running {
    let s = Suite::new(name)
        .admin_secret("conformance-jwt-secret")
        .env("DONAT_GRAPHQL_JWT_SECRET", &cfg.secret_json())
        .start();
    s.setup_v1q(&format!("{PERMS}/setup.yaml"));
    s
}

fn finish(s: &Running) {
    s.teardown_v1q(&format!("{PERMS}/teardown.yaml"));
}

/// AbstractTestJwtBasic: 9 behaviors x 2 endpoints.
fn run_basic_suite(name: &str, cfg: JwtCfg) {
    let _permit = acquire_permit();
    let s = start(name, &cfg);
    for endpoint in ENDPOINTS {
        jwt_valid_claims_success(&s, &cfg, endpoint);
        jwt_invalid_role_in_request_header(&s, &cfg, endpoint);
        jwt_no_allowed_roles_in_claim(&s, &cfg, endpoint);
        jwt_invalid_allowed_roles_in_claim(&s, &cfg, endpoint);
        jwt_no_default_role(&s, &cfg, endpoint);
        jwt_expired(&s, &cfg, endpoint);
        jwt_invalid_signature(&s, &cfg, endpoint);
        jwt_no_audience_in_conf(&s, &cfg, endpoint);
        jwt_no_issuer_in_conf(&s, &cfg, endpoint);
    }
    finish(&s);
}

/// AbstractTestJwtExpirySkew: exp 30s in the past, allowed_skew 60 ->
/// success on both endpoints.
fn run_expiry_skew_suite(name: &str, cfg: JwtCfg) {
    let _permit = acquire_permit();
    let s = start(name, &cfg);
    for endpoint in ENDPOINTS {
        let token = make_token(&cfg, full_donat_claims(), |claims| {
            claims.insert("exp".into(), json!(now() - 30));
        });
        let expected = base_conf()["response"].clone();
        run_request(
            &s,
            endpoint,
            cfg.auth_header(&token),
            200,
            &expected,
            "expiry_leeway",
        );
    }
    finish(&s);
}

/// AbstractTestJwtAudienceCheck: valid (first configured audience) +
/// invalid audience, x 2 endpoints.
fn run_audience_suite(name: &str, cfg: JwtCfg) {
    let _permit = acquire_permit();
    let s = start(name, &cfg);
    let valid_aud = match cfg.audience.as_ref().expect("audience cfg") {
        Json::String(aud) => aud.clone(),
        Json::Array(auds) => auds[0].as_str().unwrap().to_string(),
        other => panic!("bad audience cfg: {other}"),
    };
    for endpoint in ENDPOINTS {
        let token = make_token(&cfg, full_donat_claims(), |claims| {
            claims.insert("aud".into(), json!(valid_aud));
        });
        let expected = base_conf()["response"].clone();
        run_request(
            &s,
            endpoint,
            cfg.auth_header(&token),
            200,
            &expected,
            "valid_audience",
        );

        let token = make_token(&cfg, full_donat_claims(), |claims| {
            claims.insert("aud".into(), json!("rubbish_audience"));
        });
        let expected = error_body("invalid-jwt", "Could not verify JWT: JWTNotInAudience");
        run_request(
            &s,
            endpoint,
            cfg.auth_header(&token),
            error_status(endpoint),
            &expected,
            "invalid_audience",
        );
    }
    finish(&s);
}

/// AbstractTestJwtIssuerCheck: valid + invalid issuer, x 2 endpoints.
fn run_issuer_suite(name: &str, cfg: JwtCfg) {
    let _permit = acquire_permit();
    let s = start(name, &cfg);
    let valid_iss = cfg.issuer.expect("issuer cfg");
    for endpoint in ENDPOINTS {
        let token = make_token(&cfg, full_donat_claims(), |claims| {
            claims.insert("iss".into(), json!(valid_iss));
        });
        let expected = base_conf()["response"].clone();
        run_request(
            &s,
            endpoint,
            cfg.auth_header(&token),
            200,
            &expected,
            "valid_issuer",
        );

        let token = make_token(&cfg, full_donat_claims(), |claims| {
            claims.insert("iss".into(), json!("rubbish_issuer"));
        });
        let expected = error_body("invalid-jwt", "Could not verify JWT: JWTNotInIssuer");
        run_request(
            &s,
            endpoint,
            cfg.auth_header(&token),
            error_status(endpoint),
            &expected,
            "invalid_issuer",
        );
    }
    finish(&s);
}

// ----------------------------------------------------- tests (pytest classes)

macro_rules! suite {
    ($fn_name:ident, $runner:ident, $db:literal, $cfg:expr) => {
        #[test]
        fn $fn_name() {
            $runner($db, $cfg);
        }
    };
}

// TestJwtBasicWith{Rsa,Ed25519,Es}
suite!(
    jwt_basic_with_rsa,
    run_basic_suite,
    "jwt_rsa",
    JwtCfg::new(Alg::Rsa)
);
suite!(
    jwt_basic_with_ed25519,
    run_basic_suite,
    "jwt_ed",
    JwtCfg::new(Alg::Ed25519)
);
suite!(
    jwt_basic_with_es,
    run_basic_suite,
    "jwt_es",
    JwtCfg::new(Alg::Es)
);

// TestJwtBasicWith{Rsa,Ed25519,Es}AndCookie
suite!(
    jwt_basic_with_rsa_and_cookie,
    run_basic_suite,
    "jwt_rsa_cookie",
    JwtCfg::new(Alg::Rsa).location(TokenIn::Cookie("donat_user"))
);
suite!(
    jwt_basic_with_ed25519_and_cookie,
    run_basic_suite,
    "jwt_ed_cookie",
    JwtCfg::new(Alg::Ed25519).location(TokenIn::Cookie("donat_user"))
);
suite!(
    jwt_basic_with_es_and_cookie,
    run_basic_suite,
    "jwt_es_cookie",
    JwtCfg::new(Alg::Es).location(TokenIn::Cookie("donat_user"))
);

// TestJwtBasicWithRsaAndCustomHeader (RSA only in tests-py)
suite!(
    jwt_basic_with_rsa_and_custom_header,
    run_basic_suite,
    "jwt_rsa_hdr",
    JwtCfg::new(Alg::Rsa).location(TokenIn::CustomHeader("donat_user"))
);

// TestJwtBasicWith{...}AndStringifiedJsonClaims
suite!(
    jwt_basic_with_rsa_and_stringified_json_claims,
    run_basic_suite,
    "jwt_rsa_str",
    JwtCfg::new(Alg::Rsa).stringified()
);
suite!(
    jwt_basic_with_ed25519_and_stringified_json_claims,
    run_basic_suite,
    "jwt_ed_str",
    JwtCfg::new(Alg::Ed25519).stringified()
);
suite!(
    jwt_basic_with_es_and_stringified_json_claims,
    run_basic_suite,
    "jwt_es_str",
    JwtCfg::new(Alg::Es).stringified()
);

// TestJwtBasicWith{...}AndClaimsNamespacePathAtRoot ($)
suite!(
    jwt_basic_with_rsa_and_claims_namespace_path_at_root,
    run_basic_suite,
    "jwt_rsa_ns_root",
    JwtCfg::new(Alg::Rsa).ns_path("$")
);
suite!(
    jwt_basic_with_ed25519_and_claims_namespace_path_at_root,
    run_basic_suite,
    "jwt_ed_ns_root",
    JwtCfg::new(Alg::Ed25519).ns_path("$")
);
suite!(
    jwt_basic_with_es_and_claims_namespace_path_at_root,
    run_basic_suite,
    "jwt_es_ns_root",
    JwtCfg::new(Alg::Es).ns_path("$")
);

// ...AtOneLevelOfNesting ($.donat_claims)
suite!(
    jwt_basic_with_rsa_and_claims_namespace_path_one_level,
    run_basic_suite,
    "jwt_rsa_ns_one",
    JwtCfg::new(Alg::Rsa).ns_path("$.donat_claims")
);
suite!(
    jwt_basic_with_ed25519_and_claims_namespace_path_one_level,
    run_basic_suite,
    "jwt_ed_ns_one",
    JwtCfg::new(Alg::Ed25519).ns_path("$.donat_claims")
);
suite!(
    jwt_basic_with_es_and_claims_namespace_path_one_level,
    run_basic_suite,
    "jwt_es_ns_one",
    JwtCfg::new(Alg::Es).ns_path("$.donat_claims")
);

// ...AtTwoLevelsOfNesting ($.donat.claims)
suite!(
    jwt_basic_with_rsa_and_claims_namespace_path_two_levels,
    run_basic_suite,
    "jwt_rsa_ns_two",
    JwtCfg::new(Alg::Rsa).ns_path("$.donat.claims")
);
suite!(
    jwt_basic_with_ed25519_and_claims_namespace_path_two_levels,
    run_basic_suite,
    "jwt_ed_ns_two",
    JwtCfg::new(Alg::Ed25519).ns_path("$.donat.claims")
);
suite!(
    jwt_basic_with_es_and_claims_namespace_path_two_levels,
    run_basic_suite,
    "jwt_es_ns_two",
    JwtCfg::new(Alg::Es).ns_path("$.donat.claims")
);

// ...WithSpecialCharacters ($.donat['claims%'])
suite!(
    jwt_basic_with_rsa_and_claims_namespace_path_special_characters,
    run_basic_suite,
    "jwt_rsa_ns_spec",
    JwtCfg::new(Alg::Rsa).ns_path("$.donat['claims%']")
);
suite!(
    jwt_basic_with_ed25519_and_claims_namespace_path_special_characters,
    run_basic_suite,
    "jwt_ed_ns_spec",
    JwtCfg::new(Alg::Ed25519).ns_path("$.donat['claims%']")
);
suite!(
    jwt_basic_with_es_and_claims_namespace_path_special_characters,
    run_basic_suite,
    "jwt_es_ns_spec",
    JwtCfg::new(Alg::Es).ns_path("$.donat['claims%']")
);

// TestJwtExpirySkewWith{Rsa,Ed25519,Es} (allowed_skew: 60)
suite!(
    jwt_expiry_skew_with_rsa,
    run_expiry_skew_suite,
    "jwt_rsa_skew",
    JwtCfg::new(Alg::Rsa).allowed_skew(60)
);
suite!(
    jwt_expiry_skew_with_ed25519,
    run_expiry_skew_suite,
    "jwt_ed_skew",
    JwtCfg::new(Alg::Ed25519).allowed_skew(60)
);
suite!(
    jwt_expiry_skew_with_es,
    run_expiry_skew_suite,
    "jwt_es_skew",
    JwtCfg::new(Alg::Es).allowed_skew(60)
);

// TestSubscriptionJwtExpiryWith{Rsa,Ed25519,Es}: NOT ported here —
// websocket token-expiry close belongs to the subscriptions port.

// TestJwtAudienceCheckWith{...}AndSingleAudience
suite!(
    jwt_audience_check_with_rsa_and_single_audience,
    run_audience_suite,
    "jwt_rsa_aud",
    JwtCfg::new(Alg::Rsa).audience(json!("myapp-1234"))
);
suite!(
    jwt_audience_check_with_ed25519_and_single_audience,
    run_audience_suite,
    "jwt_ed_aud",
    JwtCfg::new(Alg::Ed25519).audience(json!("myapp-1234"))
);
suite!(
    jwt_audience_check_with_es_and_single_audience,
    run_audience_suite,
    "jwt_es_aud",
    JwtCfg::new(Alg::Es).audience(json!("myapp-1234"))
);

// TestJwtAudienceCheckWith{...}AndListOfAudiences
suite!(
    jwt_audience_check_with_rsa_and_list_of_audiences,
    run_audience_suite,
    "jwt_rsa_auds",
    JwtCfg::new(Alg::Rsa).audience(json!(["myapp-1234", "myapp-9876"]))
);
suite!(
    jwt_audience_check_with_ed25519_and_list_of_audiences,
    run_audience_suite,
    "jwt_ed_auds",
    JwtCfg::new(Alg::Ed25519).audience(json!(["myapp-1234", "myapp-9876"]))
);
suite!(
    jwt_audience_check_with_es_and_list_of_audiences,
    run_audience_suite,
    "jwt_es_auds",
    JwtCfg::new(Alg::Es).audience(json!(["myapp-1234", "myapp-9876"]))
);

// TestJwtIssuerCheckWith{Rsa,Ed25519,Es}
suite!(
    jwt_issuer_check_with_rsa,
    run_issuer_suite,
    "jwt_rsa_iss",
    JwtCfg::new(Alg::Rsa).issuer("https://donat.com")
);
suite!(
    jwt_issuer_check_with_ed25519,
    run_issuer_suite,
    "jwt_ed_iss",
    JwtCfg::new(Alg::Ed25519).issuer("https://donat.com")
);
suite!(
    jwt_issuer_check_with_es,
    run_issuer_suite,
    "jwt_es_iss",
    JwtCfg::new(Alg::Es).issuer("https://donat.com")
);
