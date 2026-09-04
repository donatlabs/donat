//! Tenant isolation end to end: a token carrying a tenant claim, a schema with
//! no tenant filter written anywhere in its permissions, and two tenants that
//! must not be able to reach each other.
//!
//! The permissions installed here are deliberately the *unrestricted* ones the
//! fixture helper writes — `filter: {}`, `check: {}`. That is the ordinary
//! shape of a deployment's metadata, and it is the shape a hand-rolled tenancy
//! gets wrong: without the compiler layer these permissions mean "every row of
//! every tenant". Every assertion below would pass trivially if the filters
//! said `tenant_id: {_eq: X-Donat-Tenant-Id}`; none of them do.

use std::time::{SystemTime, UNIX_EPOCH};

use donat_conformance::{FixtureColumn, FixtureColumnType, Running, Suite, TableFixture};
use serde_json::{Value as Json, json};

const SECRET: &str = "tenancy-conformance-secret-key-32b+";
const ALPHA: &str = "tenant-alpha";
const BETA: &str = "tenant-beta";

// --------------------------------------------------------- JWT plumbing

/// A token in the shape a provider issues once it knows which tenant the
/// person signed into. `tenant` is omitted entirely when `tenant` is `None`,
/// which is what a misconfigured mapping produces — and what must be refused
/// rather than answered with an empty page.
fn token_for(role: &str, tenant: Option<&str>, subject: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut claims = json!({
        "sub": subject,
        "iat": now,
        "exp": now + 3600,
        "roles": [role],
    });
    if let Some(tenant) = tenant {
        claims["tenant"] = json!(tenant);
    }
    jsonwebtoken::encode(
        &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .expect("signing jwt")
}

fn bearer(token: &str) -> Vec<(String, String)> {
    vec![("Authorization".to_string(), format!("Bearer {token}"))]
}

// ------------------------------------------------------------- fixtures

const STORE: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "status",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

const PRODUCT: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "tenant_id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "name",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

const PLAN_REF: &[FixtureColumn] = &[
    FixtureColumn {
        name: "code",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "label",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

fn install(s: &Running) {
    s.install_table(&TableFixture {
        name: "store",
        columns: STORE,
        rows: vec![
            vec![json!(ALPHA), json!("active")],
            vec![json!(BETA), json!("active")],
        ],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
    s.install_table(&TableFixture {
        name: "product",
        columns: PRODUCT,
        rows: vec![
            vec![json!(1), json!(ALPHA), json!("alpha-one")],
            vec![json!(2), json!(ALPHA), json!("alpha-two")],
            vec![json!(3), json!(BETA), json!("beta-one")],
        ],
        role: "staff",
        allow_aggregations: true,
        mutations: true,
    });
    s.install_table(&TableFixture {
        name: "plan_ref",
        columns: PLAN_REF,
        rows: vec![vec![json!("free"), json!("Free")]],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
}

fn suite(name: &str) -> Running {
    let s = Suite::new(name)
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "type": "HS256",
                "key": SECRET,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-tenant-id": { "path": "$.tenant", "default": "" },
                    "x-donat-user-id": { "path": "$.sub", "default": "" }
                }
            })
            .to_string(),
        )
        .start();
    install(&s);
    s.set_tenancy(json!({
        "source": "default",
        "variable": "X-Donat-Tenant-Id",
        "key": "tenant_id",
        "registry": {
            "table": { "schema": "{schema}", "name": "store" },
            "key": "id",
            "status": { "column": "status", "serving": ["active"] }
        },
        "keys": [{ "table": { "schema": "{schema}", "name": "store" }, "key": "id" }],
        "exempt": [
            { "table": { "schema": "{schema}", "name": "plan_ref" }, "shared": "read_only" }
        ]
    }));
    s
}

fn names(resp: &Json) -> Vec<String> {
    let mut names: Vec<String> = resp["data"]["product"]
        .as_array()
        .unwrap_or_else(|| panic!("expected data.product in {resp}"))
        .iter()
        .map(|row| row["name"].as_str().expect("name").to_string())
        .collect();
    names.sort();
    names
}

fn query(s: &Running, token: &str, body: Json) -> (u16, Json) {
    s.post("/v1/graphql", &body, &bearer(token))
}

#[test]
fn two_tenants_never_reach_each_other() {
    let s = suite("tenancy_reads");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let beta = token_for("staff", Some(BETA), "person-b");

    // ---- read: an unrestricted permission means "my tenant's rows".
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": "query { product { id name tenant_id } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp), vec!["alpha-one", "alpha-two"], "{resp}");

    let (code, resp) = query(
        &s,
        &beta,
        json!({ "query": "query { product { id name tenant_id } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp), vec!["beta-one"], "{resp}");

    // ---- read by primary key: knowing the id is not access.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": "query { product_by_pk(id: 3) { id name } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["data"]["product_by_pk"].is_null(),
        "another tenant's row was readable by id: {resp}"
    );

    // ---- aggregate: a count must not count what a select cannot see.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": "query { product_aggregate { aggregate { count } } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["product_aggregate"]["aggregate"]["count"], 2,
        "the aggregate counted another tenant's rows: {resp}"
    );

    // ---- shared reference data is visible to both.
    for token in [&alpha, &beta] {
        let (code, resp) = query(&s, token, json!({ "query": "query { plan_ref { code } }" }));
        assert_eq!(code, 200, "{resp}");
        assert_eq!(
            resp["data"]["plan_ref"].as_array().map(Vec::len),
            Some(1),
            "shared reference data is not visible: {resp}"
        );
    }

    // ---- the registry is scoped by its own identifier.
    let (code, resp) = query(&s, &alpha, json!({ "query": "query { store { id } }" }));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["store"],
        json!([{ "id": ALPHA }]),
        "the registry leaked another tenant: {resp}"
    );
}

#[test]
fn a_write_cannot_escape_the_callers_tenant() {
    let s = suite("tenancy_writes");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let beta = token_for("staff", Some(BETA), "person-b");

    // ---- insert: the object names another tenant; the preset replaces it.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": format!(
            "mutation {{ insert_product(objects: [{{ id: 10, tenant_id: \"{BETA}\", \
             name: \"smuggled\" }}]) {{ returning {{ id tenant_id }} }} }}"
        )}),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["insert_product"]["returning"][0]["tenant_id"], ALPHA,
        "an insert landed in another tenant: {resp}"
    );

    // ---- update with the widest possible `where`.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": "mutation { update_product(where: {}, _set: { name: \"rewritten\" }) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["update_product"]["affected_rows"], 3,
        "alpha's two rows plus the one it just inserted: {resp}"
    );
    let (code, resp) = query(
        &s,
        &beta,
        json!({ "query": "query { product { id name tenant_id } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        names(&resp),
        vec!["beta-one"],
        "another tenant's row was rewritten: {resp}"
    );

    // ---- delete with the widest possible `where`.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": "mutation { delete_product(where: {}) { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["delete_product"]["affected_rows"], 3, "{resp}");
    let (code, resp) = query(
        &s,
        &beta,
        json!({ "query": "query { product { id name tenant_id } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        names(&resp),
        vec!["beta-one"],
        "a delete crossed the tenant boundary: {resp}"
    );
}

/// No tenant, no answer — and no header may supply one. A request that cannot
/// say which tenant it is in is refused rather than served an empty page,
/// because an empty page is what a misconfigured token looks like.
#[test]
fn a_request_with_no_tenant_claim_is_refused_and_no_header_supplies_one() {
    let s = suite("tenancy_no_claim");
    let tenantless = token_for("staff", None, "person-c");

    let (code, resp) = query(
        &s,
        &tenantless,
        json!({ "query": "query { product { id } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["errors"][0]["extensions"]["code"], "access-denied",
        "a tenantless request was not refused: {resp}"
    );

    // The same token, now asserting a tenant in a header. Unlike X-Donat-Role,
    // which selects among roles a token already granted, there is nothing for
    // this header to select among — so it names nothing.
    let mut headers = bearer(&tenantless);
    headers.push(("X-Donat-Tenant-Id".to_string(), ALPHA.to_string()));
    let (code, resp) = s.post(
        "/v1/graphql",
        &json!({ "query": "query { product { id } }" }),
        &headers,
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["errors"][0]["extensions"]["code"], "access-denied",
        "a header named a tenant: {resp}"
    );
}

// ------------------------------------------------- onboarding, end to end
//
// Two commands cannot be scoped by the caller's tenant, because the caller is
// not in one: the command that creates a tenant, and the command that admits
// somebody to a tenant they do not belong to yet. Both declare where their
// tenant comes from, and both are refused if they do not.

const ONBOARDING_SQL: &str = "\
CREATE TABLE public.store (id text PRIMARY KEY, status text NOT NULL);
CREATE TABLE public.member (tenant_id text NOT NULL, user_id text NOT NULL, \
    PRIMARY KEY (tenant_id, user_id));
CREATE TABLE public.invite (token text PRIMARY KEY, tenant_id text NOT NULL, \
    redeemed boolean NOT NULL DEFAULT false);
CREATE TABLE public.plan (code text PRIMARY KEY, label text NOT NULL);
INSERT INTO public.plan VALUES ('free', 'Free');
INSERT INTO public.store VALUES ('tenant-alpha', 'active'), ('tenant-beta', 'active');
INSERT INTO public.invite (token, tenant_id) VALUES \
    ('unguessable-beta-token', 'tenant-beta'), ('unguessable-alpha-token', 'tenant-alpha');
INSERT INTO public.member VALUES ('tenant-alpha', 'person-both'), ('tenant-beta', 'person-both');
";

fn onboarding_metadata() -> donat_metadata::Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": { "from_env": "DONAT_DATABASE_URL" } }
            },
            "tables": [
                {
                    "table": { "schema": "public", "name": "store" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "command_insert_permissions": [
                        { "role": "founder", "permission": { "columns": "*", "check": {} } }
                    ],
                    "command_select_permissions": [
                        { "role": "founder", "permission": { "columns": "*", "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "member" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "command_insert_permissions": [
                        { "role": "founder", "permission": { "columns": "*", "check": {} } },
                        { "role": "joiner", "permission": { "columns": "*", "check": {} } }
                    ],
                    "command_select_permissions": [
                        { "role": "founder", "permission": { "columns": "*", "filter": {} } },
                        { "role": "joiner", "permission": { "columns": "*", "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "invite" },
                    "command_select_permissions": [
                        { "role": "joiner", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "command_update_permissions": [
                        { "role": "joiner", "permission": { "columns": ["redeemed"], "filter": {}, "check": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "plan" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "command_select_permissions": [
                        { "role": "founder", "permission": { "columns": "*", "filter": {} } }
                    ]
                }
            ]
        }],
        "commands": [
            {
                "name": "register_merchant",
                "source": "default",
                "permissions": [{ "role": "founder" }],
                // The tenant does not exist when this starts. `establishes`
                // says the key for every write below comes from a step.
                "tenant": { "establishes": { "step": "store", "column": "id" } },
                "arguments": [
                    { "name": "store_id", "type": "String!" },
                    { "name": "founder_id", "type": "String!" }
                ],
                "steps": [
                    {
                        // Reads come first, before the tenant exists. Nothing
                        // here is in a tenant yet, and nothing here needs to be.
                        "name": "chosen_plan",
                        "select_one": {
                            "table": { "schema": "public", "name": "plan" },
                            "by": { "code": { "literal": "free" } },
                            "returning": ["code", "label"],
                            "require_found": true
                        }
                    },
                    {
                        "name": "store",
                        "insert": {
                            "table": { "schema": "public", "name": "store" },
                            "object": {
                                "id": { "arg": "store_id" },
                                "status": { "literal": "active" }
                            },
                            "returning": ["id", "status"]
                        }
                    },
                    {
                        "name": "member",
                        "insert": {
                            "table": { "schema": "public", "name": "member" },
                            "object": { "user_id": { "arg": "founder_id" } },
                            "returning": ["tenant_id", "user_id"]
                        }
                    }
                ],
                "result": {
                    "store_id": { "step": "store", "column": "id" },
                    "plan": { "step": "chosen_plan", "column": "label" },
                    "member_tenant": { "step": "member", "column": "tenant_id" }
                }
            },
            {
                "name": "accept_invite",
                "source": "default",
                "permissions": [{ "role": "joiner" }],
                // The caller belongs to nothing yet, so the invitation row is
                // read outside the tenant and then scopes the rest.
                "tenant": { "from": { "step": "invite", "column": "tenant_id" } },
                "arguments": [
                    { "name": "token", "type": "String!" },
                    { "name": "user_id", "type": "String!" }
                ],
                "steps": [
                    {
                        "name": "invite",
                        "tenant": "unscoped",
                        "select_one": {
                            "table": { "schema": "public", "name": "invite" },
                            "by": { "token": { "arg": "token" } },
                            "returning": ["token", "tenant_id"],
                            "require_found": true
                        }
                    },
                    {
                        "name": "member",
                        "insert": {
                            "table": { "schema": "public", "name": "member" },
                            "object": { "user_id": { "arg": "user_id" } },
                            "returning": ["tenant_id", "user_id"]
                        }
                    }
                ],
                "result": { "joined": { "step": "member", "column": "tenant_id" } }
            },
            {
                // A scoped read placed after the step this command takes its
                // tenant from. It is bounded by what that step resolved — the
                // invite's tenant — and not by the caller's, who has none.
                "name": "join_and_peek",
                "source": "default",
                "permissions": [{ "role": "joiner" }],
                "tenant": { "from": { "step": "invite", "column": "tenant_id" } },
                "arguments": [
                    { "name": "token", "type": "String!" },
                    { "name": "user_id", "type": "String!" }
                ],
                "steps": [
                    {
                        "name": "invite",
                        "tenant": "unscoped",
                        "select_one": {
                            "table": { "schema": "public", "name": "invite" },
                            "by": { "token": { "arg": "token" } },
                            "returning": ["token", "tenant_id"],
                            "require_found": true
                        }
                    },
                    {
                        "name": "peek",
                        "select_many": {
                            "table": { "schema": "public", "name": "member" },
                            "by": { "user_id": { "arg": "user_id" } },
                            "order_by": ["tenant_id"],
                            "returning": ["tenant_id", "user_id"],
                            "maximum_rows": 10
                        }
                    }
                ],
                "result": { "peeked": { "step": "peek" } }
            },
            {
                // An update after the tenant step. Its `where` names a token
                // the caller supplies, and the tenant bound decides whether
                // that row is in scope at all.
                "name": "redeem_invite",
                "source": "default",
                "permissions": [{ "role": "joiner" }],
                "tenant": { "from": { "step": "invite", "column": "tenant_id" } },
                "arguments": [
                    { "name": "token", "type": "String!" },
                    { "name": "other_token", "type": "String!" }
                ],
                "steps": [
                    {
                        "name": "invite",
                        "tenant": "unscoped",
                        "select_one": {
                            "table": { "schema": "public", "name": "invite" },
                            "by": { "token": { "arg": "token" } },
                            "returning": ["token", "tenant_id"],
                            "require_found": true
                        }
                    },
                    {
                        "name": "mark",
                        "update": {
                            "table": { "schema": "public", "name": "invite" },
                            "where": { "token": { "arg": "other_token" } },
                            "set": { "redeemed": { "literal": true } },
                            "returning": ["token", "tenant_id", "redeemed"],
                            "require_affected": true
                        }
                    }
                ],
                "result": { "redeemed": { "step": "mark", "column": "tenant_id" } }
            }
        ],
        "tenancy": {
            "source": "default",
            "variable": "X-Donat-Tenant-Id",
            "key": "tenant_id",
            "registry": {
                "table": { "schema": "public", "name": "store" },
                "key": "id",
                "status": { "column": "status", "serving": ["active"] }
            },
            "keys": [{ "table": { "schema": "public", "name": "store" }, "key": "id" }],
            "exempt": [
                { "table": { "schema": "public", "name": "plan" }, "shared": "read_only" }
            ],
            "unscoped_steps": "audited"
        }
    }))
    .expect("onboarding metadata deserializes")
}

fn onboarding_suite() -> Running {
    let s = Suite::new("tenancy_onboarding")
        .initial_metadata(onboarding_metadata())
        .with_migrations()
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "type": "HS256",
                "key": SECRET,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-tenant-id": { "path": "$.tenant", "default": "" },
                    "x-donat-user-id": { "path": "$.sub", "default": "" }
                }
            })
            .to_string(),
        )
        .start();
    let mut client =
        postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect to suite database");
    client
        .batch_execute(ONBOARDING_SQL)
        .expect("onboarding schema");
    s
}

/// A scoped read after the step the tenant came from is bounded by that
/// tenant, not by the caller's.
///
/// `from:` exists precisely because the command's tenant is not the caller's.
/// Once the step has run, the tenant it resolved is a single value in a CTE,
/// and every later read compares against it: `person-both` is a member of two
/// stores, and the read placed after the beta invitation sees only the beta
/// membership. The caller, who carries no tenant at all, contributes nothing.
#[test]
fn a_read_after_the_tenant_step_is_bounded_by_that_tenant() {
    let s = onboarding_suite();
    let joiner = token_for("joiner", None, "person-joiner");

    let (code, resp) = query(
        &s,
        &joiner,
        json!({ "query": "mutation { join_and_peek(token: \"unguessable-beta-token\", \
                          user_id: \"person-both\") { peeked { tenant_id user_id } } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["join_and_peek"]["peeked"],
        json!([{ "tenant_id": "tenant-beta", "user_id": "person-both" }]),
        "the read after the tenant step was not bounded by the invite's tenant: {resp}"
    );
}

/// An update after the tenant step carries the same bound in its predicate.
/// Naming another tenant's row in `where` finds nothing — the row is out of
/// scope, not merely unchanged — and the row is left as it was.
#[test]
fn an_update_after_the_tenant_step_cannot_reach_another_tenants_row() {
    let s = onboarding_suite();
    let joiner = token_for("joiner", None, "person-joiner");

    // The invite's own tenant: the row is in scope and the update lands.
    let (code, resp) = query(
        &s,
        &joiner,
        json!({ "query": "mutation { redeem_invite(token: \"unguessable-beta-token\", \
                          other_token: \"unguessable-beta-token\") { redeemed } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["redeem_invite"]["redeemed"], "tenant-beta",
        "{resp}"
    );

    // Another tenant's row, named by a token the caller happens to hold: the
    // tenant bound excludes it, `require_affected` reports it, and nothing
    // moved. Reading the invite out of tenant beta must not let a write reach
    // tenant alpha in the same statement.
    let (code, resp) = query(
        &s,
        &joiner,
        json!({ "query": "mutation { redeem_invite(token: \"unguessable-beta-token\", \
                          other_token: \"unguessable-alpha-token\") { redeemed } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "an update after the tenant step reached another tenant's row: {resp}"
    );
    let mut client =
        postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect to suite database");
    let redeemed: bool = client
        .query_one(
            "SELECT redeemed FROM public.invite WHERE token = 'unguessable-alpha-token'",
            &[],
        )
        .expect("read the alpha invite")
        .get(0);
    assert!(
        !redeemed,
        "the alpha invite was redeemed through tenant beta's command"
    );
}

/// The registry's serving gate applies to a command whose tenant came from a
/// step, by the value that step resolved. A valid invitation into a store the
/// registry stopped serving is refused, exactly as a member of that store is.
#[test]
fn an_invitation_into_a_store_the_registry_stopped_serving_is_refused() {
    let s = onboarding_suite();
    let joiner = token_for("joiner", None, "person-joiner");
    let mut client =
        postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect to suite database");
    client
        .execute(
            "UPDATE public.store SET status = 'suspended' WHERE id = $1",
            &[&BETA],
        )
        .expect("suspend the store");

    let (code, resp) = query(
        &s,
        &joiner,
        json!({ "query": "mutation { accept_invite(token: \"unguessable-beta-token\", \
                          user_id: \"person-joiner\") { joined } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "an invitation into a suspended store was accepted: {resp}"
    );
    let joined: i64 = client
        .query_one(
            "SELECT count(*) FROM public.member WHERE tenant_id = $1 AND user_id = 'person-joiner'",
            &[&BETA],
        )
        .expect("count memberships")
        .get(0);
    assert_eq!(joined, 0, "the membership landed in a suspended store");
}

/// Signing up a merchant runs no DDL and needs no tenant in the token: the
/// command writes the tenant row, and every later write in the same statement
/// takes its key from it.
#[test]
fn a_command_can_establish_the_tenant_it_writes_into() {
    let s = onboarding_suite();
    let founder = token_for("founder", None, "person-founder");

    let (code, resp) = query(
        &s,
        &founder,
        json!({ "query": "mutation { register_merchant(store_id: \"tenant-gamma\", \
                          founder_id: \"person-founder\") { store_id plan member_tenant } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["register_merchant"]["store_id"], "tenant-gamma",
        "{resp}"
    );
    // The membership landed in the tenant the command just created, without
    // anybody naming it in the object.
    assert_eq!(
        resp["data"]["register_merchant"]["member_tenant"], "tenant-gamma",
        "the established tenant did not reach the second write: {resp}"
    );
    // And the lookup that ran before the tenant existed answered normally.
    assert_eq!(resp["data"]["register_merchant"]["plan"], "Free", "{resp}");
}

/// A person with an invitation belongs to nothing yet. The invitation is read
/// outside the tenant — declared on that one step — and what it says scopes
/// the write that follows.
#[test]
fn an_invitation_read_outside_the_tenant_scopes_the_write_that_follows() {
    let s = onboarding_suite();
    let joiner = token_for("joiner", None, "person-joiner");

    let (code, resp) = query(
        &s,
        &joiner,
        json!({ "query": "mutation { accept_invite(token: \"unguessable-beta-token\", \
                          user_id: \"person-joiner\") { joined } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["accept_invite"]["joined"], "tenant-beta",
        "the membership did not land in the invited tenant: {resp}"
    );

    // A token nobody issued finds nothing. The unguessable key is the whole
    // authorization, which is why the escape is legal only on a unique lookup.
    let (code, resp) = query(
        &s,
        &joiner,
        json!({ "query": "mutation { accept_invite(token: \"guessed\", \
                          user_id: \"person-joiner\") { joined } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "an unissued token was accepted: {resp}"
    );
}

// ------------------------------------------------ in-tenant grants
//
// Tenancy decides which rows exist for a caller. Grants decide which of the
// operations on them that caller may perform — and they are rows the tenant
// writes for itself, so two people holding the same compiled role can differ.

const IAM_GRANT: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "tenant_id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "user_id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "action",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

fn iam_suite(name: &str) -> Running {
    let s = suite(name);
    s.install_table(&TableFixture {
        name: "iam_grant",
        columns: IAM_GRANT,
        rows: vec![
            // A reader in alpha: may read the catalogue, nothing more.
            vec![
                json!(1),
                json!(ALPHA),
                json!("person-a"),
                json!("product:read"),
            ],
            // A manager in alpha, through a wildcard.
            vec![
                json!(2),
                json!(ALPHA),
                json!("person-m"),
                json!("product:*"),
            ],
            // Somebody with the same grant in the *other* tenant.
            vec![json!(3), json!(BETA), json!("person-a"), json!("product:*")],
        ],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
    s.set_iam(json!({
        "source": "default",
        "grants": {
            "table": { "schema": "{schema}", "name": "iam_grant" },
            "subject": { "column": "user_id", "variable": "X-Donat-User-Id" },
            "tenant": { "column": "tenant_id" },
            "action": { "column": "action" }
        },
        "governed_roles": ["staff"]
    }));
    s
}

/// Same compiled role, same tenant, different grants — and the engine derives
/// the predicate rather than the deployment writing one per table.
#[test]
fn two_people_in_one_tenant_differ_by_what_they_were_granted() {
    let s = iam_suite("tenancy_iam_reads");
    let reader = token_for("staff", Some(ALPHA), "person-a");
    let ungranted = token_for("staff", Some(ALPHA), "person-nobody");

    let (code, resp) = query(
        &s,
        &reader,
        json!({ "query": "query { product { name } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp), vec!["alpha-one", "alpha-two"], "{resp}");

    let (code, resp) = query(
        &s,
        &ungranted,
        json!({ "query": "query { product { name } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        names(&resp).is_empty(),
        "a role with no grant read rows: {resp}"
    );
}

/// A grant is held in one tenant. Holding `product:*` in beta must do nothing
/// for the same person acting in alpha.
#[test]
fn a_grant_held_in_one_tenant_does_nothing_in_another() {
    let s = iam_suite("tenancy_iam_cross");
    // person-a holds only `product:read` in alpha, and `product:*` in beta.
    let in_alpha = token_for("staff", Some(ALPHA), "person-a");

    let (code, resp) = query(
        &s,
        &in_alpha,
        json!({ "query": "mutation { insert_product(objects: [{ id: 40, name: \"x\" }]) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "the beta grant authorized a write in alpha: {resp}"
    );
}

/// `product:*` satisfies every verb on that resource, and the expansion is the
/// engine's — a tenant never writes a pattern that gets executed as one.
#[test]
fn a_wildcard_grant_satisfies_every_verb_on_its_resource() {
    let s = iam_suite("tenancy_iam_wildcard");
    let manager = token_for("staff", Some(ALPHA), "person-m");
    let reader = token_for("staff", Some(ALPHA), "person-a");

    let (code, resp) = query(
        &s,
        &manager,
        json!({ "query": "mutation { insert_product(objects: [{ id: 50, name: \"new\" }]) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["insert_product"]["affected_rows"], 1, "{resp}");

    // The reader holds `product:read` only, so the same write is refused.
    let (code, resp) = query(
        &s,
        &reader,
        json!({ "query": "mutation { insert_product(objects: [{ id: 51, name: \"nope\" }]) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "a read-only grant authorized a write: {resp}"
    );

    // And an update it may not make changes nothing and says so.
    let (code, resp) = query(
        &s,
        &reader,
        json!({ "query": "mutation { update_product(where: {}, _set: { name: \"z\" }) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "a read-only grant authorized an update: {resp}"
    );

    // A delete is refused too, rather than reporting that it removed nothing.
    // "Your delete matched no rows" is not an answer to "may I delete this".
    let (code, resp) = query(
        &s,
        &reader,
        json!({ "query": "mutation { delete_product(where: {}) { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"].is_array(),
        "a read-only grant deleted quietly instead of being refused: {resp}"
    );

    // The rows are still there.
    let (code, resp) = query(
        &s,
        &reader,
        json!({ "query": "query { product { name } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp).len(), 3, "{resp}");
}

/// A command is one domain operation, so it is gated once and refuses rather
/// than running and changing nothing. `order:cancel` is deliberately not
/// `orders:update`: a merchant role may be allowed to read and edit orders and
/// still not be allowed to cancel one.
#[test]
fn a_command_needs_its_own_grant_and_says_so_when_it_is_missing() {
    let s = Suite::new("tenancy_iam_commands")
        .initial_metadata(command_iam_metadata())
        .with_migrations()
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "type": "HS256",
                "key": SECRET,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-tenant-id": { "path": "$.tenant", "default": "" },
                    "x-donat-user-id": { "path": "$.sub", "default": "" }
                }
            })
            .to_string(),
        )
        .start();
    let mut client =
        postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect to suite database");
    client
        .batch_execute(
            "CREATE TABLE public.store (id text PRIMARY KEY, status text NOT NULL);
             CREATE TABLE public.orders (id bigint PRIMARY KEY, tenant_id text NOT NULL, \
                 status text NOT NULL);
             CREATE TABLE public.iam_grant (id bigint PRIMARY KEY, tenant_id text NOT NULL, \
                 user_id text NOT NULL, action text NOT NULL);
             INSERT INTO public.store VALUES ('tenant-alpha', 'active');
             INSERT INTO public.orders VALUES (1, 'tenant-alpha', 'open');
             INSERT INTO public.iam_grant VALUES
                 (1, 'tenant-alpha', 'person-clerk', 'orders:update'),
                 (2, 'tenant-alpha', 'person-lead', 'order:cancel');",
        )
        .expect("command iam schema");

    let clerk = token_for("staff", Some(ALPHA), "person-clerk");
    let lead = token_for("staff", Some(ALPHA), "person-lead");
    let call = json!({ "query": "mutation { cancel_order(order_id: 1) { cancelled } }" });

    // Holding `orders:update` is not holding `order:cancel`.
    let (code, resp) = query(&s, &clerk, call.clone());
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["errors"][0]["extensions"]["code"], "access-denied",
        "a command ran without its own grant: {resp}"
    );
    assert!(
        resp["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("cancel_order"),
        "the refusal does not name the command: {resp}"
    );

    let (code, resp) = query(&s, &lead, call);
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["cancel_order"]["cancelled"], 1, "{resp}");
}

fn command_iam_metadata() -> donat_metadata::Metadata {
    serde_json::from_value(json!({
        "version": 3,
        "sources": [{
            "name": "default",
            "kind": "postgres",
            "configuration": {
                "connection_info": { "database_url": { "from_env": "DONAT_DATABASE_URL" } }
            },
            "tables": [
                {
                    "table": { "schema": "public", "name": "store" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "orders" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "command_select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ],
                    "command_update_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ]
                },
                {
                    "table": { "schema": "public", "name": "iam_grant" },
                    "select_permissions": [
                        { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                    ]
                }
            ]
        }],
        "commands": [{
            "name": "cancel_order",
            "source": "default",
            "permissions": [{ "role": "staff" }],
            "arguments": [{ "name": "order_id", "type": "Int!" }],
            "steps": [{
                "name": "cancelled",
                "update": {
                    "table": { "schema": "public", "name": "orders" },
                    "where": { "id": { "arg": "order_id" } },
                    "set": { "status": { "literal": "cancelled" } },
                    "returning": ["id"]
                }
            }],
            "result": { "cancelled": { "step": "cancelled", "column": "id" } }
        }],
        "tenancy": {
            "source": "default",
            "variable": "X-Donat-Tenant-Id",
            "key": "tenant_id",
            "registry": {
                "table": { "schema": "public", "name": "store" },
                "key": "id",
                "status": { "column": "status", "serving": ["active"] }
            },
            "keys": [{ "table": { "schema": "public", "name": "store" }, "key": "id" }],
            "exempt": [
                { "table": { "schema": "public", "name": "iam_grant" }, "shared": "read_only" }
            ]
        },
        "iam": {
            "source": "default",
            "grants": {
                "table": { "schema": "public", "name": "iam_grant" },
                "subject": { "column": "user_id", "variable": "X-Donat-User-Id" },
                "tenant": { "column": "tenant_id" },
                "action": { "column": "action" }
            },
            "governed_roles": ["staff"],
            "command_actions": {
                "overrides": [{ "command": "cancel_order", "action": "order:cancel" }]
            }
        }
    }))
    .expect("command iam metadata deserializes")
}

/// A role able to grant actions can grant itself anything, so the actions that
/// belong to the platform are barred by the database rather than by whichever
/// command happens to write the row.
#[test]
fn a_tenant_cannot_grant_itself_an_action_the_platform_reserved() {
    let s = suite("tenancy_iam_reserved");
    s.install_table(&TableFixture {
        name: "iam_grant",
        columns: IAM_GRANT,
        rows: vec![vec![
            json!(1),
            json!(ALPHA),
            json!("person-admin"),
            json!("iam_grant:*"),
        ]],
        role: "staff",
        allow_aggregations: false,
        mutations: true,
    });
    s.set_iam(json!({
        "source": "default",
        "grants": {
            "table": { "schema": "{schema}", "name": "iam_grant" },
            "subject": { "column": "user_id", "variable": "X-Donat-User-Id" },
            "tenant": { "column": "tenant_id" },
            "action": { "column": "action" },
            "written_via": {
                "table": { "schema": "{schema}", "name": "iam_grant" },
                "action": "action"
            }
        },
        "governed_roles": ["staff"],
        "reserved_actions": ["platform:*", "tenant:create"]
    }));
    let admin = token_for("staff", Some(ALPHA), "person-admin");

    // An ordinary in-tenant action is theirs to grant.
    let (code, resp) = query(
        &s,
        &admin,
        json!({ "query": "mutation { insert_iam_grant(objects: [{ id: 10, \
                          user_id: \"person-a\", action: \"product:read\" }]) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["insert_iam_grant"]["affected_rows"], 1,
        "{resp}"
    );

    // A reserved service prefix is not, however the row is written.
    for action in ["platform:billing", "tenant:create"] {
        let (code, resp) = query(
            &s,
            &admin,
            json!({ "query": format!(
                "mutation {{ insert_iam_grant(objects: [{{ id: 11, user_id: \"person-admin\", \
                 action: \"{action}\" }}]) {{ affected_rows }} }}"
            )}),
        );
        assert_eq!(code, 200, "{resp}");
        assert!(
            resp["errors"].is_array(),
            "a tenant granted itself the reserved action {action}: {resp}"
        );
    }
}

/// The same reservation, written by a command instead of by CRUD.
///
/// The case above says "however the row is written" and then only wrote it one
/// way. A command step skips the *table's* action deliberately — the command
/// proved its own — and for a while it skipped the reservation with it, so a
/// deployment that wrote its grants from a command could grant itself
/// everything the platform had reserved. The reservation bounds what the row
/// may say, not who may write it, so it survives the command.
#[test]
fn a_command_cannot_write_a_grant_the_platform_reserved() {
    let s = Suite::new("tenancy_iam_reserved_command")
        .initial_metadata(
            serde_json::from_value(json!({
                "version": 3,
                "sources": [{
                    "name": "default",
                    "kind": "postgres",
                    "configuration": {
                        "connection_info": {
                            "database_url": { "from_env": "DONAT_DATABASE_URL" }
                        }
                    },
                    "tables": [
                        {
                            "table": { "schema": "public", "name": "store" },
                            "select_permissions": [
                                { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                            ]
                        },
                        {
                            "table": { "schema": "public", "name": "iam_grant" },
                            "select_permissions": [
                                { "role": "staff", "permission": { "columns": "*", "filter": {} } }
                            ],
                            "command_insert_permissions": [
                                {
                                    "role": "staff",
                                    "permission": { "columns": "*", "check": {} }
                                }
                            ]
                        }
                    ]
                }],
                "commands": [
                    {
                        "name": "grant_action",
                        "source": "default",
                        "permissions": [{ "role": "staff" }],
                        "arguments": [
                            { "name": "subject", "type": "String!" },
                            { "name": "action", "type": "String!" }
                        ],
                        "steps": [
                            {
                                "name": "granted",
                                "insert": {
                                    "table": { "schema": "public", "name": "iam_grant" },
                                    "object": {
                                        "user_id": { "arg": "subject" },
                                        "action": { "arg": "action" }
                                    },
                                    "returning": ["user_id", "action"]
                                }
                            }
                        ],
                        "result": { "action": { "step": "granted", "column": "action" } }
                    }
                ]
            }))
            .expect("metadata"),
        )
        .with_migrations()
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "type": "HS256",
                "key": SECRET,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-tenant-id": { "path": "$.tenant", "default": "" },
                    "x-donat-user-id": { "path": "$.sub", "default": "" }
                }
            })
            .to_string(),
        )
        .start();
    let mut client =
        postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect to suite database");
    client
        .batch_execute(
            "CREATE TABLE public.store (id text PRIMARY KEY, status text NOT NULL);
             CREATE TABLE public.iam_grant (id bigserial PRIMARY KEY, tenant_id text NOT NULL, \
                 user_id text NOT NULL, action text NOT NULL);
             INSERT INTO public.store VALUES ('tenant-alpha', 'active');
             INSERT INTO public.iam_grant (tenant_id, user_id, action) VALUES \
                 ('tenant-alpha', 'person-admin', 'iam_grant:*'), \
                 ('tenant-alpha', 'person-admin', 'grant_action:invoke');",
        )
        .expect("schema");
    s.set_tenancy(json!({
        "source": "default",
        "variable": "X-Donat-Tenant-Id",
        "key": "tenant_id",
        "registry": {
            "table": { "schema": "public", "name": "store" },
            "key": "id",
            "status": { "column": "status", "serving": ["active"] }
        },
        "keys": [{ "table": { "schema": "public", "name": "store" }, "key": "id" }]
    }));
    s.set_iam(json!({
        "source": "default",
        "grants": {
            "table": { "schema": "public", "name": "iam_grant" },
            "subject": { "column": "user_id", "variable": "X-Donat-User-Id" },
            "tenant": { "column": "tenant_id" },
            "action": { "column": "action" },
            "written_via": {
                "table": { "schema": "public", "name": "iam_grant" },
                "action": "action"
            }
        },
        "governed_roles": ["staff"],
        "reserved_actions": ["platform:*", "tenant:create"]
    }));
    let admin = token_for("staff", Some(ALPHA), "person-admin");

    // An ordinary in-tenant action still goes through the command.
    let (code, resp) = query(
        &s,
        &admin,
        json!({ "query": "mutation { grant_action(subject: \"person-a\", \
                          action: \"product:read\") { action } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["grant_action"]["action"], "product:read",
        "{resp}"
    );

    for action in ["platform:billing", "tenant:create"] {
        let (code, resp) = query(
            &s,
            &admin,
            json!({ "query": format!(
                "mutation {{ grant_action(subject: \"person-admin\", \
                 action: \"{action}\") {{ action }} }}"
            )}),
        );
        assert_eq!(code, 200, "{resp}");
        assert!(
            resp["errors"].is_array(),
            "a command wrote the reserved action {action}: {resp}"
        );
    }
}

// ------------------------------------------------------- plan entitlements
//
// A ceiling is added as a layer, exactly as the tenant predicate is: the
// domain's own insert permission is not edited, because an overlay that could
// quietly rewrite the base would make every audit of the base meaningless.

const PLAN: &[FixtureColumn] = &[
    FixtureColumn {
        name: "code",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "max_products",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: false,
    },
];

const USAGE: &[FixtureColumn] = &[
    FixtureColumn {
        name: "tenant_id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "product_count",
        ty: FixtureColumnType::BigInt,
        nullable: false,
        primary_key: false,
    },
];

const PLANNED_STORE: &[FixtureColumn] = &[
    FixtureColumn {
        name: "id",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: true,
    },
    FixtureColumn {
        name: "status",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
    FixtureColumn {
        name: "plan_code",
        ty: FixtureColumnType::Text,
        nullable: false,
        primary_key: false,
    },
];

/// Two products already exist and the plan allows three, so exactly one more
/// fits.
fn quota_suite(name: &str) -> Running {
    let s = Suite::new(name)
        .env(
            "DONAT_GRAPHQL_JWT_SECRET",
            &json!({
                "type": "HS256",
                "key": SECRET,
                "claims_map": {
                    "x-donat-allowed-roles": { "path": "$.roles" },
                    "x-donat-default-role": { "path": "$.roles[0]" },
                    "x-donat-tenant-id": { "path": "$.tenant", "default": "" },
                    "x-donat-user-id": { "path": "$.sub", "default": "" }
                }
            })
            .to_string(),
        )
        .start();
    s.install_table(&TableFixture {
        name: "store",
        columns: PLANNED_STORE,
        rows: vec![vec![json!(ALPHA), json!("active"), json!("free")]],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
    s.install_table(&TableFixture {
        name: "plan",
        columns: PLAN,
        rows: vec![vec![json!("free"), json!(3)]],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
    s.install_table(&TableFixture {
        name: "tenant_usage",
        columns: USAGE,
        rows: vec![vec![json!(ALPHA), json!(2)]],
        role: "staff",
        allow_aggregations: false,
        mutations: false,
    });
    s.install_table(&TableFixture {
        name: "product",
        columns: PRODUCT,
        rows: vec![
            vec![json!(1), json!(ALPHA), json!("alpha-one")],
            vec![json!(2), json!(ALPHA), json!("alpha-two")],
        ],
        role: "staff",
        allow_aggregations: false,
        mutations: true,
    });
    s.set_tenancy(json!({
        "source": "default",
        "variable": "X-Donat-Tenant-Id",
        "key": "tenant_id",
        "registry": {
            "table": { "schema": "{schema}", "name": "store" },
            "key": "id",
            "status": { "column": "status", "serving": ["active"] }
        },
        "keys": [{ "table": { "schema": "{schema}", "name": "store" }, "key": "id" }],
        "exempt": [{ "table": { "schema": "{schema}", "name": "plan" }, "shared": "read_only" }]
    }));
    s.set_quotas(json!({
        "source": "default",
        "counters": {
            "table": { "schema": "{schema}", "name": "tenant_usage" },
            "tenant": { "column": "tenant_id" }
        },
        "limits": {
            "table": { "schema": "{schema}", "name": "plan" },
            "key": { "column": "code" },
            "via": { "table": { "schema": "{schema}", "name": "store" }, "column": "plan_code" }
        },
        "entitlements": [{
            "name": "products",
            "counter": "product_count",
            "maximum": "max_products",
            "consumes": [{ "table": { "schema": "{schema}", "name": "product" } }]
        }]
    }));
    s
}

fn insert_product(id: i64) -> Json {
    json!({ "query": format!(
        "mutation {{ insert_product(objects: [{{ id: {id}, name: \"p{id}\" }}]) \
         {{ affected_rows }} }}"
    )})
}

#[test]
fn a_plan_ceiling_gates_the_write_that_would_cross_it() {
    let s = quota_suite("tenancy_quota");
    let staff = token_for("staff", Some(ALPHA), "person-a");

    // The third product fits.
    let (code, resp) = query(&s, &staff, insert_product(3));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["insert_product"]["affected_rows"], 1, "{resp}");

    // The fourth does not, and says so rather than writing.
    let (code, resp) = query(&s, &staff, insert_product(4));
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no more products"),
        "the ceiling did not refuse the write: {resp}"
    );

    // The refused write left nothing behind — neither the row nor the count.
    let (code, resp) = query(&s, &staff, json!({ "query": "query { product { name } }" }));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp).len(), 3, "{resp}");

    // Deleting releases a unit, and the next write fits again.
    let (code, resp) = query(
        &s,
        &staff,
        json!({ "query": "mutation { delete_product(where: { id: { _eq: 3 } }) \
                          { affected_rows } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["delete_product"]["affected_rows"], 1, "{resp}");

    let (code, resp) = query(&s, &staff, insert_product(5));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(resp["data"]["insert_product"]["affected_rows"], 1, "{resp}");
}

/// The reason the counter moves inside the statement. Counting first and
/// writing second passes every concurrent writer, because under READ COMMITTED
/// they all read the same pre-lock state; only a lock on the usage row makes
/// the second one see what the first committed.
#[test]
fn a_ceiling_holds_when_two_writers_arrive_at_once() {
    let s = quota_suite("tenancy_quota_race");
    let staff = token_for("staff", Some(ALPHA), "person-a");
    let url = format!("{}/v1/graphql", s.base_url());
    let headers = bearer(&staff);

    // Exactly one unit is left. Both requests ask for it.
    let outcomes: Vec<Json> = std::thread::scope(|scope| {
        let handles: Vec<_> = [10_i64, 11]
            .into_iter()
            .map(|id| {
                let url = url.clone();
                let headers = headers.clone();
                scope.spawn(move || {
                    let client = reqwest::blocking::Client::new();
                    let mut request = client.post(&url).json(&insert_product(id));
                    for (name, value) in &headers {
                        request = request.header(name, value);
                    }
                    request
                        .send()
                        .expect("graphql request")
                        .json::<Json>()
                        .expect("graphql response")
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker"))
            .collect()
    });

    let accepted = outcomes
        .iter()
        .filter(|resp| resp["data"]["insert_product"]["affected_rows"] == 1)
        .count();
    assert_eq!(
        accepted, 1,
        "both writers crossed the ceiling: {outcomes:#?}"
    );

    // And the store really holds three, not four.
    let (code, resp) = query(&s, &staff, json!({ "query": "query { product { name } }" }));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp).len(), 3, "{resp}");
}

/// An upsert that only updated consumes nothing.
///
/// `ON CONFLICT DO NOTHING` was right from the start, because it returns no
/// rows. `DO UPDATE` returns the rows it overwrote as well, and counting those
/// charged a merchant a unit of their plan every time they edited a product
/// they already owned — until the ceiling locked them out of creating any.
/// The counter moves by what the statement *created*, which is what `xmax`
/// distinguishes.
#[test]
fn an_upsert_that_only_updated_consumes_no_quota() {
    let s = quota_suite("tenancy_quota_upsert");
    let staff = token_for("staff", Some(ALPHA), "person-a");

    // Two products exist and the plan allows three, so exactly one is left.
    let upsert = |name: &str| {
        json!({ "query": format!(
            "mutation {{ insert_product(objects: [{{ id: 1, name: \"{name}\" }}], \
             on_conflict: {{ constraint: product_pkey, update_columns: [name] }}) \
             {{ affected_rows }} }}"
        )})
    };

    // Rewriting the same row three times touches no new row, so it must not
    // spend the remaining unit — let alone three of them.
    for attempt in ["renamed-once", "renamed-twice", "renamed-thrice"] {
        let (code, resp) = query(&s, &staff, upsert(attempt));
        assert_eq!(code, 200, "{resp}");
        assert_eq!(
            resp["data"]["insert_product"]["affected_rows"], 1,
            "the upsert did not update: {resp}"
        );
    }

    // The third product still fits, which it would not if the updates had been
    // charged.
    let (code, resp) = query(&s, &staff, insert_product(3));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["insert_product"]["affected_rows"], 1,
        "an upsert that only updated consumed the plan: {resp}"
    );

    // And the ceiling still holds for a genuinely new row.
    let (code, resp) = query(&s, &staff, insert_product(4));
    assert_eq!(code, 200, "{resp}");
    assert!(
        resp["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no more products"),
        "the ceiling stopped holding: {resp}"
    );
}

/// An insert-or-ignore must stay an ignore.
///
/// The tenant preset is applied to an upsert's `DO UPDATE` branch, and it has
/// to be applied *only* there: a preset added unconditionally turns
/// `update_columns: []` into a `DO UPDATE` — and the tenant bound lives on the
/// same branch as the preset, so the resulting UPDATE would carry no `WHERE`
/// at all. A caller could then take another tenant's row by colliding with its
/// unique key, using nothing but an insert permission.
#[test]
fn an_insert_or_ignore_cannot_take_another_tenants_row() {
    let s = suite("tenancy_upsert");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let beta = token_for("staff", Some(BETA), "person-b");

    // Row 3 is beta's. Alpha inserts the same primary key, ignoring conflicts.
    let (code, resp) = query(
        &s,
        &alpha,
        json!({ "query": format!(
            "mutation {{ insert_product(objects: [{{ id: 3, name: \"stolen\" }}], \
             on_conflict: {{ constraint: product_pkey, update_columns: [] }}) \
             {{ affected_rows }} }}"
        )}),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        resp["data"]["insert_product"]["affected_rows"], 0,
        "the ignore was compiled into an update: {resp}"
    );

    // Beta still has its row, with its name and its tenant.
    let (code, resp) = query(
        &s,
        &beta,
        json!({ "query": "query { product { name tenant_id } }" }),
    );
    assert_eq!(code, 200, "{resp}");
    assert_eq!(
        names(&resp),
        vec!["beta-one"],
        "another tenant took the row through an insert-or-ignore: {resp}"
    );
}

/// A token stays valid for its whole lifetime, so suspending a store has to
/// take effect against tokens already issued. The registry's `serving:` list
/// is what does it.
#[test]
fn a_store_the_registry_stopped_serving_is_no_longer_answered() {
    let s = suite("tenancy_suspended");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let beta = token_for("staff", Some(BETA), "person-b");

    // Serving, so it answers.
    let (code, resp) = query(&s, &alpha, json!({ "query": "query { product { name } }" }));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp).len(), 2, "{resp}");

    let mut client =
        postgres::Client::connect(s.db_url(), postgres::NoTls).expect("connect to suite database");
    client
        .execute(
            &format!(
                "UPDATE {}.store SET status = 'suspended' WHERE id = $1",
                s.schema
            ),
            &[&ALPHA],
        )
        .expect("suspend the store");

    // The same token, unchanged, now reads nothing.
    let (code, resp) = query(&s, &alpha, json!({ "query": "query { product { name } }" }));
    assert_eq!(code, 200, "{resp}");
    assert!(
        names(&resp).is_empty(),
        "a suspended store was still served: {resp}"
    );

    // ...and every write is refused rather than quietly doing nothing. An
    // update or a delete gated by the *predicate* would report zero rows,
    // which reads as "there was nothing to do" instead of "this store is
    // suspended".
    for mutation in [
        "insert_product(objects: [{ id: 90, name: \"x\" }]) { affected_rows }",
        "update_product(where: {}, _set: { name: \"x\" }) { affected_rows }",
        "delete_product(where: {}) { affected_rows }",
    ] {
        let (code, resp) = query(
            &s,
            &alpha,
            json!({ "query": format!("mutation {{ {mutation} }}") }),
        );
        assert_eq!(code, 200, "{resp}");
        assert!(
            resp["errors"].is_array(),
            "a suspended store could still run `{mutation}`: {resp}"
        );
    }

    // The other store is untouched.
    let (code, resp) = query(&s, &beta, json!({ "query": "query { product { name } }" }));
    assert_eq!(code, 200, "{resp}");
    assert_eq!(names(&resp), vec!["beta-one"], "{resp}");
}

/// A panel showing a store's data should say which store. The engine answers
/// that on the session endpoint, so the browser never has to hold — or be able
/// to change — the value that decides what it is looking at.
#[test]
fn the_session_endpoint_names_the_tenant_the_caller_is_in() {
    let s = suite("tenancy_session");
    let alpha = token_for("staff", Some(ALPHA), "person-a");
    let tenantless = token_for("staff", None, "person-c");

    let session_of = |token: &str| {
        let http = reqwest::blocking::Client::new();
        http.get(format!("{}/auth/session", s.base_url()))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .expect("session responds")
            .json::<Json>()
            .expect("a JSON body")
    };

    let resp = session_of(&alpha);
    assert_eq!(resp["tenant"], ALPHA, "{resp}");
    assert_eq!(resp["role"], "staff", "{resp}");

    // Signed in and in no store yet is a state of its own, and it is the one a
    // store switcher exists for.
    let resp = session_of(&tenantless);
    assert!(resp["tenant"].is_null(), "{resp}");
    assert_eq!(resp["authenticated"], true, "{resp}");
}
