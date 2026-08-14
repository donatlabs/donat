//! End-to-end: a token fetched-key-verified through `jwk_url`, projected onto
//! session variables by `claims_map`, then enforced by a row filter.
//!
//! This is the whole chain an external identity provider drives, and nothing
//! here is specific to one: the engine's contract is "claims addressed by JSON
//! path become session variables". The payload shape below — roles as a
//! top-level array, per-user attributes nested one level down — is what the
//! petshop example's provider happens to emit, but any provider that can be
//! told to put an array and a scalar in its access token satisfies it.
//!
//! `jwk.rs` already covers the refresher's *timing*. What this adds is the
//! part a deployment actually depends on: that the identity carried by a token
//! selects rows. Two subjects with the same role must see disjoint data, a
//! subject must not be able to ask for a role the token does not carry, and a
//! token missing the attribute entirely must still authenticate — the mapping
//! declares a `default` precisely so a role that has no such attribute (staff,
//! machine users) is not locked out.

use std::time::{SystemTime, UNIX_EPOCH};

use donat_conformance::{
    FixtureColumn, FixtureColumnType, Running, Suite, TableFixture, fixture_root,
};
use serde_json::{Value as Json, json};

const KID: &str = "test-key-1";

// ------------------------------------------------------------- JWKS stub

/// Serves the RSA fixture key set. Unlike the stub in `jwk.rs` it carries no
/// counters or caching headers — refresh behaviour is that suite's subject.
fn start_jwks_stub() -> String {
    let jwk: Json = serde_json::from_str(
        &std::fs::read_to_string(fixture_root().join("jwt_keys/rsa_jwk.json"))
            .expect("reading rsa_jwk.json"),
    )
    .expect("parsing rsa_jwk.json");
    let body = json!({ "keys": [jwk] }).to_string();

    let (tx, rx) = std::sync::mpsc::channel::<u16>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stub runtime");
        rt.block_on(async move {
            let app = axum::Router::new().route(
                "/jwks",
                axum::routing::get(move || {
                    let body = body.clone();
                    async move {
                        (
                            [
                                ("Content-Type", "application/json"),
                                // Without this the engine falls back to a
                                // one-second refresh and re-fetches throughout
                                // the run. Harmless, but the key set never
                                // changes here and refresh timing is `jwk.rs`'s
                                // subject, not this suite's.
                                ("Cache-Control", "max-age=600"),
                            ],
                            body,
                        )
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("stub bind");
            tx.send(listener.local_addr().unwrap().port()).unwrap();
            axum::serve(listener, app).await.expect("stub serve");
        });
    });
    format!("http://127.0.0.1:{}/jwks", rx.recv().expect("stub port"))
}

// --------------------------------------------------------- JWT plumbing

/// Signs `claims` with the fixture RSA key under the kid the stub publishes.
fn sign(claims: Json) -> String {
    let pem = std::fs::read(fixture_root().join("jwt_keys/rsa_private.pem"))
        .expect("reading rsa_private.pem");
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(&pem).expect("rsa private pem");
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KID.to_string());
    jsonwebtoken::encode(&header, &claims, &key).expect("signing jwt")
}

/// An access token in the shape a provider issues for a shopper: the role set
/// as a top-level array, the tenant-specific identity nested under a custom
/// object. `customer_id` is omitted entirely when `customer_id` is `None`,
/// which is what a provider emits for a user who has no such attribute.
fn token_for(roles: &[&str], customer_id: Option<&str>) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut claims = json!({
        "sub": "subject-under-test",
        "iat": now,
        "exp": now + 3600,
        "scope": "openid donat",
        "roles": roles,
    });
    if let Some(id) = customer_id {
        claims["custom"] = json!({ "customer_id": id });
    }
    sign(claims)
}

fn bearer(token: &str) -> Vec<(String, String)> {
    vec![("Authorization".to_string(), format!("Bearer {token}"))]
}

/// Bearer plus an explicit role request, for a subject whose token carries
/// more than the mapping's default role.
fn bearer_as(token: &str, role: &str) -> Vec<(String, String)> {
    let mut headers = bearer(token);
    headers.push(("X-Donat-Role".to_string(), role.to_string()));
    headers
}

// ------------------------------------------------------------- fixtures

const ORDERS: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "customer_id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "order_status",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

/// `staff` gets the unrestricted permission `install_table` writes; `customer`
/// gets one filtered by the session variable the token supplies.
fn install_orders(s: &Running) {
    s.install_table(&TableFixture {
        name: "orders",
        columns: ORDERS,
        rows: vec![
            vec![json!(1), json!("customer-1"), json!("pending")],
            vec![json!(2), json!("customer-1"), json!("paid")],
            vec![json!(3), json!("customer-2"), json!("pending")],
        ],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
    s.add_select_permission_document(
        "orders",
        "customer",
        json!({
            "columns": ["id", "customer_id", "order_status"],
            "filter": { "customer_id": { "_eq": "X-Donat-User-Id" } },
            "allow_aggregations": false
        }),
    );
}

/// Row ids, sorted here rather than in the query so the assertion does not
/// depend on `order_by` support.
fn ids(resp: &Json) -> Vec<i64> {
    let mut ids: Vec<i64> = resp["data"]["orders"]
        .as_array()
        .unwrap_or_else(|| panic!("expected data.orders in {resp}"))
        .iter()
        .map(|row| row["id"].as_i64().expect("id"))
        .collect();
    ids.sort_unstable();
    ids
}

const QUERY: &str = "query { orders { id customer_id order_status } }";

// ------------------------------------------------------------- the suite

#[test]
fn claims_map_projects_token_identity_onto_row_filters() {
    let jwks = start_jwks_stub();
    let s = Suite::new("jwt_claims_map")
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "jwk_url": jwks,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-user-id": { "path": "$.custom.customer_id", "default": "" }
                }
            })
            .to_string(),
        )
        .start();
    install_orders(&s);

    // The identity in the token selects the rows. No role header, no admin
    // secret: everything the filter needs came out of the claims.
    let alice = token_for(&["customer"], Some("customer-1"));
    let (code, resp) = s.post("/v1/graphql", &json!({ "query": QUERY }), &bearer(&alice));
    assert_eq!(code, 200, "alice query failed: {resp}");
    assert_eq!(ids(&resp), vec![1, 2], "alice must see only her own orders");
    assert!(
        resp["data"]["orders"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["customer_id"] == "customer-1"),
        "alice saw another customer's row: {resp}"
    );

    // A second subject with the same role sees a disjoint set — proving the
    // filter follows the token rather than the role.
    let bob = token_for(&["customer"], Some("customer-2"));
    let (code, resp) = s.post("/v1/graphql", &json!({ "query": QUERY }), &bearer(&bob));
    assert_eq!(code, 200, "bob query failed: {resp}");
    assert_eq!(ids(&resp), vec![3], "bob must see only his own order");

    // The token bounds the role set: asking for one it does not carry is
    // denied, so a shopper cannot read past their filter by asserting `staff`.
    let (_, resp) = s.post(
        "/v1/graphql",
        &json!({ "query": QUERY }),
        &bearer_as(&alice, "staff"),
    );
    assert_eq!(
        resp["errors"][0]["extensions"]["code"], "access-denied",
        "expected access-denied for an unlisted role, got: {resp}"
    );

    // A subject whose token carries no such attribute at all still
    // authenticates — that is what the mapping's `default` is for — and the
    // unrestricted role sees everything. No role header: the default role is
    // read from the token too, so the session can only hold a role the claims
    // granted.
    let sam = token_for(&["staff"], None);
    let (code, resp) = s.post("/v1/graphql", &json!({ "query": QUERY }), &bearer(&sam));
    assert_eq!(code, 200, "staff query failed: {resp}");
    assert_eq!(ids(&resp), vec![1, 2, 3], "staff must see every order");

    // And under the filtered role the empty default is fail-closed: a shopper
    // whose attribute was never set reads nothing, rather than everything.
    let unattributed = token_for(&["customer"], None);
    let (code, resp) = s.post(
        "/v1/graphql",
        &json!({ "query": QUERY }),
        &bearer(&unattributed),
    );
    assert_eq!(code, 200, "unattributed customer query failed: {resp}");
    assert_eq!(
        ids(&resp),
        Vec::<i64>::new(),
        "a customer token without the attribute must match no rows"
    );
}
