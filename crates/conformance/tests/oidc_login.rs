//! The engine's own login, end to end.
//!
//! This engine issues no tokens and stores no users, so "logging in" means
//! exactly one thing: the browser comes back from the configured provider
//! holding a cookie, and that cookie is a token the engine verifies like any
//! other. These cases drive the whole path — `/auth/login` → the provider →
//! `/auth/callback` → a GraphQL request answered under the role the token
//! granted — because every part of it is only worth anything if the last step
//! works.
//!
//! It is also the only way a browser reaches this engine at all now: a role
//! comes from a verified JWT or an authentication hook, and nothing else.

use std::collections::HashMap;

use donat_conformance::{FixtureColumn, FixtureColumnType, Suite, TableFixture, idp_stub};
use serde_json::{Map, Value as Json, json};

const SIGNING_KEY: &str = "conformance-oidc-signing-key-0123456789";

const NOTE_COLUMNS: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "body",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];
const COOKIE: &str = "donat_session";

/// The JWT configuration the engine verifies the cookie with: the same HS256
/// key the stub signs with, read out of the cookie the callback sets.
fn jwt_config() -> String {
    json!({
        "type": "HS256",
        "key": SIGNING_KEY,
        "header": { "type": "Cookie", "name": COOKIE },
        "claims_map": {
            "x-donat-allowed-roles": { "path": "$.roles" },
            "x-donat-default-role": { "path": "$.roles[0]" },
            "x-donat-user-id": { "path": "$.sub", "default": "" }
        }
    })
    .to_string()
}

fn claims(roles: &[&str]) -> Map<String, Json> {
    let mut claims = Map::new();
    claims.insert("sub".to_string(), json!("operator-1"));
    claims.insert("roles".to_string(), json!(roles));
    claims
}

/// A client that never follows a redirect: every hop is asserted by hand,
/// because the point of these cases is what each hop carries.
fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("a non-redirecting client")
}

fn query_of(url: &str) -> HashMap<String, String> {
    url.split_once('?')
        .map(|(_, query)| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn cookie_named<'a>(response: &'a reqwest::blocking::Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&format!("{name}=")))
}

fn suite(idp: &idp_stub::IdpStub, base_hint: &str) -> String {
    json!({
        "authorization_endpoint": idp.authorization_endpoint(),
        "token_endpoint": idp.token_endpoint(),
        "client_id": "donat-conformance",
        "redirect_uri": format!("{base_hint}/auth/callback"),
        "scopes": ["openid", "donat"],
        "cookie": COOKIE,
        // The stand is plain HTTP; a Secure cookie would never come back.
        "cookie_secure": false
    })
    .to_string()
}

/// The whole flow, plus the two things that make it a login rather than a
/// redirect: the exchange is PKCE-proven, and the resulting cookie is a
/// session the data plane answers under.
#[test]
fn a_browser_logs_in_and_is_answered_under_the_role_its_token_granted() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["operator"]));
    // The engine's own address is not known until it boots, and the redirect
    // URI has to name it. It is only ever compared, never dereferenced by the
    // stub, so a placeholder that the engine echoes back is enough — the
    // callback below is issued against the real base URL.
    let s = Suite::new("oidc_login")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &suite(&idp, "http://127.0.0.1:0"))
        .start();
    s.install_table(&TableFixture {
        name: "note",
        columns: NOTE_COLUMNS,
        rows: vec![vec![json!(1), json!("visible to the operator")]],
        role: "operator",
        allow_aggregations: false,
        mutations: false,
    });
    let base = s.base_url();
    let http = client();

    // 1. The panel sends the browser to /auth/login.
    let login = http
        .get(format!("{base}/auth/login?redirect=/product"))
        .send()
        .expect("login responds");
    assert_eq!(login.status().as_u16(), 302, "login is a redirect");
    let flow = cookie_named(&login, "donat_auth_flow")
        .expect("the run is recorded in the browser")
        .to_string();
    assert!(flow.contains("HttpOnly"), "{flow}");
    let authorize_url = login
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("a location to the provider")
        .to_string();
    let authorize = query_of(&authorize_url);
    assert_eq!(
        authorize.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert!(authorize.contains_key("state"));

    // 2. The provider sends it back with a code for that same run.
    let provider = http
        .get(&authorize_url)
        .send()
        .expect("the provider answers");
    assert_eq!(provider.status().as_u16(), 302);
    let returned = provider
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .expect("a location back to the engine")
        .to_string();
    let returned_query = query_of(&returned);
    assert_eq!(
        returned_query.get("state"),
        authorize.get("state"),
        "the provider returns the run's own state"
    );

    // 3. The callback exchanges the code and sets the session.
    let flow_cookie = flow.split(';').next().expect("a cookie value").to_string();
    let callback = http
        .get(format!(
            "{base}/auth/callback?code={}&state={}",
            returned_query.get("code").expect("a code"),
            returned_query.get("state").expect("a state")
        ))
        .header(reqwest::header::COOKIE, &flow_cookie)
        .send()
        .expect("callback responds");
    assert_eq!(callback.status().as_u16(), 302, "the callback redirects");
    assert_eq!(
        callback
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok()),
        Some("/product"),
        "the browser returns where it started"
    );
    let session = cookie_named(&callback, COOKIE)
        .expect("the session cookie is set")
        .to_string();
    assert!(session.contains("HttpOnly"), "{session}");
    assert!(session.contains("SameSite=Lax"), "{session}");

    // The exchange proved this browser started the run.
    let exchange = idp.exchanges().pop().expect("the code was exchanged");
    assert_eq!(
        exchange.form.get("grant_type").map(String::as_str),
        Some("authorization_code")
    );
    assert!(
        exchange
            .form
            .get("code_verifier")
            .is_some_and(|v| !v.is_empty()),
        "the verifier is what a public client authenticates with"
    );
    assert!(
        !exchange.form.contains_key("client_secret"),
        "a public client sends no secret"
    );

    // 4. The cookie is a session: the data plane answers under the role the
    //    token granted, and refuses one it did not.
    let session_cookie = session
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();
    let answered = http
        .post(format!("{base}/v1/graphql"))
        .header(reqwest::header::COOKIE, &session_cookie)
        .header("X-Donat-Role", "operator")
        .json(&json!({ "query": "{ note { id body } }" }))
        .send()
        .expect("graphql responds")
        .json::<Json>()
        .expect("a JSON body");
    assert_eq!(
        answered,
        json!({ "data": { "note": [{ "id": 1, "body": "visible to the operator" }] } }),
        "the cookie the login set is a session"
    );

    let refused = http
        .post(format!("{base}/v1/graphql"))
        .header(reqwest::header::COOKIE, &session_cookie)
        .header("X-Donat-Role", "auditor")
        .json(&json!({ "query": "{ note { id } }" }))
        .send()
        .expect("graphql responds")
        .json::<Json>()
        .expect("a JSON body");
    assert_eq!(
        refused["errors"][0]["extensions"]["code"],
        json!("access-denied"),
        "a session never holds a role its token did not grant: {refused}"
    );
}

/// A callback that cannot be tied to a run this browser started mints
/// nothing. This is the whole defence against a forged login, so it is worth
/// its own case.
#[test]
fn a_callback_without_the_browsers_own_run_mints_no_session() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["operator"]));
    let s = Suite::new("oidc_forged")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &suite(&idp, "http://127.0.0.1:0"))
        .start();
    let base = s.base_url();
    let http = client();

    // No flow cookie at all: an attacker's link into the callback.
    let no_flow = http
        .get(format!(
            "{base}/auth/callback?code={}&state=whatever",
            idp_stub::CODE
        ))
        .send()
        .expect("callback responds");
    assert_eq!(no_flow.status().as_u16(), 400);
    assert!(
        cookie_named(&no_flow, COOKIE).is_none(),
        "no session issued"
    );

    // A real run, finished with somebody else's state.
    let login = http
        .get(format!("{base}/auth/login"))
        .send()
        .expect("login responds");
    let flow = cookie_named(&login, "donat_auth_flow")
        .expect("a run is recorded")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();
    let mismatched = http
        .get(format!(
            "{base}/auth/callback?code={}&state=not-this-run",
            idp_stub::CODE
        ))
        .header(reqwest::header::COOKIE, &flow)
        .send()
        .expect("callback responds");
    assert_eq!(mismatched.status().as_u16(), 400);
    assert!(
        cookie_named(&mismatched, COOKIE).is_none(),
        "no session issued"
    );
    assert!(
        idp.exchanges().is_empty(),
        "a refused callback never reaches the provider's token endpoint"
    );
}

/// Logging out is the cookie going away — there is nothing else to end,
/// because the engine kept nothing.
#[test]
fn logging_out_clears_the_session_cookie() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["operator"]));
    let s = Suite::new("oidc_logout")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &suite(&idp, "http://127.0.0.1:0"))
        .start();
    let logout = client()
        .get(format!("{}/auth/logout", s.base_url()))
        .send()
        .expect("logout responds");
    assert_eq!(logout.status().as_u16(), 302);
    let cleared = cookie_named(&logout, COOKIE).expect("the session cookie is cleared");
    assert!(cleared.contains("Max-Age=0"), "{cleared}");
}

/// A different provider, configured differently: the deployment's roles live
/// in the id token and the client authenticates with HTTP Basic. Nothing about
/// the flow changes — which is the point. The stub signs the two tokens with
/// *different* roles, so this can only pass if the engine used the one the
/// configuration named.
#[test]
fn a_provider_that_carries_roles_in_the_id_token_and_wants_basic_auth_works_the_same() {
    let idp = idp_stub::spawn_with_id_token(
        SIGNING_KEY,
        claims(&["not-the-session"]),
        claims(&["operator"]),
    );
    let oidc = json!({
        "authorization_endpoint": idp.authorization_endpoint(),
        "token_endpoint": idp.token_endpoint(),
        "client_id": "donat-conformance",
        "client_secret": "a:secret with awkward bytes",
        "redirect_uri": "http://127.0.0.1:0/auth/callback",
        "cookie": COOKIE,
        "cookie_secure": false,
        "session_token": "id_token",
        "client_auth": "client_secret_basic"
    })
    .to_string();
    let s = Suite::new("oidc_other_provider")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &oidc)
        .start();
    s.install_table(&TableFixture {
        name: "note",
        columns: NOTE_COLUMNS,
        rows: vec![vec![json!(1), json!("visible to the operator")]],
        role: "operator",
        allow_aggregations: false,
        mutations: false,
    });
    let base = s.base_url();
    let http = client();

    let login = http
        .get(format!("{base}/auth/login"))
        .send()
        .expect("login responds");
    let flow = cookie_named(&login, "donat_auth_flow")
        .expect("a run is recorded")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();
    let state = query_of(
        login
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("a location"),
    )
    .get("state")
    .expect("a state")
    .clone();

    let callback = http
        .get(format!(
            "{base}/auth/callback?code={}&state={state}",
            idp_stub::CODE
        ))
        .header(reqwest::header::COOKIE, &flow)
        .send()
        .expect("callback responds");
    assert_eq!(callback.status().as_u16(), 302);
    let session = cookie_named(&callback, COOKIE)
        .expect("the session cookie is set")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();

    // The secret went in the header, not the body — and survived encoding.
    let exchange = idp.exchanges().pop().expect("the code was exchanged");
    assert!(
        !exchange.form.contains_key("client_secret"),
        "a Basic client does not also post its secret"
    );
    let authorization = exchange
        .authorization
        .expect("a Basic client authenticates in the header");
    assert!(authorization.starts_with("Basic "));

    // And the session is the id token: the access token's role is not one the
    // browser can use.
    let answered = http
        .post(format!("{base}/v1/graphql"))
        .header(reqwest::header::COOKIE, &session)
        .header("X-Donat-Role", "operator")
        .json(&json!({ "query": "{ note { id } }" }))
        .send()
        .expect("graphql responds")
        .json::<Json>()
        .expect("a JSON body");
    assert_eq!(
        answered,
        json!({ "data": { "note": [{ "id": 1 }] } }),
        "the id token is what the configuration named: {answered}"
    );
}

/// The question a browser cannot otherwise answer.
///
/// A deployment that sets an unauthorized role answers an unauthenticated
/// request *successfully*, as that role — so "not signed in" and "this role
/// may see nothing" look identical from the outside. This is the difference,
/// and a panel that cannot see it leaves an operator on an empty screen
/// instead of sending them to log in.
#[test]
fn the_engine_reports_a_caller_back_to_itself() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["operator"]));
    let s = Suite::new("oidc_whoami")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &suite(&idp, "http://127.0.0.1:0"))
        // The case that makes this necessary: an unauthenticated request is
        // answered, not refused.
        .env("DONAT_GRAPHQL_UNAUTHORIZED_ROLE", "anonymous")
        .start();
    let base = s.base_url();
    let http = client();

    let anonymous = http
        .get(format!("{base}/auth/session"))
        .send()
        .expect("session responds")
        .json::<Json>()
        .expect("a JSON body");
    assert_eq!(
        anonymous,
        json!({ "authenticated": false, "role": "anonymous", "roles": ["anonymous"] }),
        "an unauthenticated caller is told so, and told what it would run as"
    );

    // Log in, and the same endpoint says who the browser now is.
    let login = http
        .get(format!("{base}/auth/login"))
        .send()
        .expect("login responds");
    let flow = cookie_named(&login, "donat_auth_flow")
        .expect("a run is recorded")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();
    let state = query_of(
        login
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .expect("a location"),
    )
    .get("state")
    .expect("a state")
    .clone();
    let callback = http
        .get(format!(
            "{base}/auth/callback?code={}&state={state}",
            idp_stub::CODE
        ))
        .header(reqwest::header::COOKIE, &flow)
        .send()
        .expect("callback responds");
    let session = cookie_named(&callback, COOKIE)
        .expect("the session cookie is set")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();

    let signed_in = http
        .get(format!("{base}/auth/session"))
        .header(reqwest::header::COOKIE, &session)
        .send()
        .expect("session responds")
        .json::<Json>()
        .expect("a JSON body");
    // The role it is acting as, and every role the token would allow — the
    // second is what lets a client say "your account may not do that" instead
    // of showing an error and a Retry button that can never work.
    assert_eq!(
        signed_in,
        json!({ "authenticated": true, "role": "operator", "roles": ["operator"] })
    );

    // It reports the caller and nothing else: no data, no metadata, no
    // permission list.
    assert_eq!(
        signed_in.as_object().map(|o| o.len()),
        Some(3),
        "the answer is the caller, not an administrative surface"
    );
}

/// The provider's login API, served on this engine's origin.
///
/// A first-party login page can only call the provider from the page's own
/// origin — the provider's session cookie and its `Origin` check both say so —
/// which is why a deployment may ask the engine to forward that API. What has
/// to hold is that the request arrives unchanged and the answer comes back
/// whole: a dropped `x-csrf-token` or a swallowed `Location` header each turn a
/// working login into a silent refusal.
#[test]
fn the_engine_serves_the_providers_login_api_on_its_own_origin() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["operator"]));
    let mut config: Json = serde_json::from_str(&suite(&idp, "http://127.0.0.1:0")).unwrap();
    config["login_api"] = json!(idp.base_url());
    let s = Suite::new("oidc_proxy")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &config.to_string())
        .start();
    let base = s.base_url();

    let response = client()
        .post(format!("{base}/auth/v1/oidc/authorize?client_id=panel"))
        .header("content-type", "application/json")
        .header("x-csrf-token", "csrf-from-the-page")
        .header("cookie", "RauthySession=abc")
        .header("origin", &base)
        .body(r#"{"email":"operator@example.test","pow":"1:19:solved"}"#)
        .send()
        .expect("the engine answers");

    assert_eq!(
        response.status().as_u16(),
        202,
        "the provider's own status is what the page reads"
    );
    // The code travels back in this header, and a login that loses it goes
    // nowhere at all.
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("https://provider.invalid/done?code=proxied")
    );

    let seen: Json = response.json().expect("the stub describes what it got");
    assert_eq!(
        seen["path"],
        json!("oidc/authorize"),
        "the path is unchanged"
    );
    assert_eq!(
        seen["query"]["client_id"],
        json!("panel"),
        "the query is unchanged"
    );
    assert_eq!(
        seen["body"],
        json!(r#"{"email":"operator@example.test","pow":"1:19:solved"}"#),
        "the body is unchanged"
    );
    // The three headers the provider decides on: who is asking, with which
    // token, from where.
    assert_eq!(seen["cookie"], json!("RauthySession=abc"));
    assert_eq!(seen["csrf"], json!("csrf-from-the-page"));
    assert_eq!(seen["origin"], json!(base));
    // Not the engine's own address: the next hop's host is the next hop's.
    assert_ne!(seen["host"], json!(base.trim_start_matches("http://")));
}

/// Nothing is forwarded unless a deployment asked for it.
#[test]
fn the_login_api_is_absent_until_it_is_configured() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["operator"]));
    let s = Suite::new("oidc_no_proxy")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &suite(&idp, "http://127.0.0.1:0"))
        .start();

    let response = client()
        .post(format!("{}/auth/v1/oidc/authorize", s.base_url()))
        .send()
        .expect("the engine answers");
    assert_eq!(response.status().as_u16(), 404);
}

/// The identity provider's own accounts, served by the engine itself.
///
/// A platform's first screen is "who can get in", and the answer is not rows
/// in this database. Rather than have every deployment copy the same forty
/// lines of metadata, the engine ships the declaration and a deployment names
/// three things: where the provider is, the key, and the one role allowed to
/// use it. What has to hold is that the field exists only then, that the
/// credential reaches the provider, and that no other role can see it.
#[test]
fn the_engine_serves_the_providers_accounts_to_the_role_that_was_named() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["support", "reader"]));
    let mut config: Json = serde_json::from_str(&suite(&idp, "http://127.0.0.1:0")).unwrap();
    config["login_api"] = json!(idp.base_url());
    config["admin_key"] = json!("API-Key donat$secret");
    config["admin_role"] = json!("support");
    let s = Suite::new("oidc_idp_admin")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &config.to_string())
        .start();
    let base = s.base_url();
    let token = idp.mint(&claims(&["support", "reader"]));
    let http = client();

    let ask = |role: &str| {
        http.post(format!("{base}/v1/graphql"))
            // This suite's JWT configuration reads the session cookie, which
            // is what a browser would be carrying by now.
            .header("cookie", format!("{COOKIE}={token}"))
            .header("X-Donat-Role", role)
            .json(&json!({
                "query": "{ idp_users { id email given_name authorization } }"
            }))
            .send()
            .expect("the engine answers")
            .json::<Json>()
            .expect("a GraphQL body")
    };

    let answered = ask("support");
    let users = answered["data"]["idp_users"]
        .as_array()
        .unwrap_or_else(|| panic!("expected accounts, got {answered}"));
    assert_eq!(users.len(), 2);
    assert_eq!(users[0]["email"], json!("one@example.test"));
    // The credential is the engine's to hold: it reached the provider, and it
    // was never in the metadata or in the request.
    assert_eq!(users[0]["authorization"], json!("API-Key donat$secret"));

    // Another role the same token grants sees no such field at all — not an
    // empty list, which would suggest there was something to filter.
    let refused = ask("reader");
    assert!(
        refused["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("idp_users"),
        "expected the field to be absent for another role, got {refused}"
    );
}

/// Nothing is served until a deployment asks for it.
#[test]
fn the_accounts_are_absent_until_a_key_is_configured() {
    let idp = idp_stub::spawn(SIGNING_KEY, claims(&["support"]));
    let s = Suite::new("oidc_no_idp_admin")
        .env("DONAT_GRAPHQL_JWT_SECRET", &jwt_config())
        .env("DONAT_OIDC", &suite(&idp, "http://127.0.0.1:0"))
        .start();
    let token = idp.mint(&claims(&["support"]));

    let answered: Json = client()
        .post(format!("{}/v1/graphql", s.base_url()))
        .header("cookie", format!("{COOKIE}={token}"))
        .header("X-Donat-Role", "support")
        .json(&json!({ "query": "{ idp_users { id } }" }))
        .send()
        .expect("the engine answers")
        .json()
        .expect("a GraphQL body");
    assert!(
        answered["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("idp_users"),
        "expected no such field, got {answered}"
    );
}
