//! `DONAT_GRAPHQL_ENABLED_APIS` gates which transports are mounted (ADR
//! `api-surfaces/decisions/003-enabled-apis-flag.md`). A disabled surface's
//! routes are not registered, so requests get a plain 404; an enabled surface
//! serves normally. These suites set the env var (via `Suite::env`) and assert
//! the gate by status code (and, for REST, the handler's JSON 404 body that
//! distinguishes an enabled-but-unknown path from a disabled-surface 404).

use donat_conformance::Suite;
use serde_json::json;

const REST: &str = "rest";

fn user_headers() -> Vec<(String, String)> {
    vec![("X-Donat-Role".to_string(), "user".to_string())]
}

/// Only `graphql` enabled: GraphQL works (200, data); REST and MCP are not
/// mounted (plain 404).
#[test]
fn enabled_apis_graphql_only() {
    let s = Suite::new("enabled_apis_graphql_only")
        .env("DONAT_GRAPHQL_ENABLED_APIS", "graphql")
        .start();
    // rest/setup.yaml tracks `pet` with a `user` select permission and a query
    // collection / rest endpoints — enough to exercise all three surfaces.
    s.setup_v1q(&format!("{REST}/setup.yaml"));

    // GraphQL is enabled: a select returns data.
    let (status, body) = s.post(
        "/v1/graphql",
        &json!({ "query": "query { pet(order_by: {id: asc}) { id name } }" }),
        &user_headers(),
    );
    assert_eq!(status, 200, "graphql should be enabled: {body}");
    assert!(
        body.get("data").and_then(|d| d.get("pet")).is_some(),
        "graphql should return data: {body}"
    );

    // REST is disabled: its routes are absent => plain 404 (not the handler's
    // JSON {"code":"not-found"} body).
    let (status, body) = s.post("/api/rest/pet/1", &json!({}), &user_headers());
    assert_eq!(status, 404, "rest should be disabled (404): {body}");
    assert!(
        body.get("code").is_none(),
        "disabled rest should NOT return the handler's JSON 404 body: {body}"
    );

    // MCP is disabled: route absent => plain 404.
    let (status, body) = s.post(
        "/mcp",
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
        &user_headers(),
    );
    assert_eq!(status, 404, "mcp should be disabled (404): {body}");
}

/// Only `rest,mcp` enabled: GraphQL is not mounted (404); MCP `tools/list`
/// works (200); a real REST endpoint serves successfully.
#[test]
fn enabled_apis_rest_mcp() {
    let s = Suite::new("enabled_apis_rest_mcp")
        .env("DONAT_GRAPHQL_ENABLED_APIS", "rest,mcp")
        .start();
    s.setup_v1q(&format!("{REST}/setup.yaml"));

    // GraphQL is disabled: route absent => plain 404.
    let (status, body) = s.post(
        "/v1/graphql",
        &json!({ "query": "query { pet { id } }" }),
        &user_headers(),
    );
    assert_eq!(status, 404, "graphql should be disabled (404): {body}");

    // MCP is enabled: tools/list returns a JSON-RPC result.
    let (status, body) = s.post(
        "/mcp",
        &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} }),
        &user_headers(),
    );
    assert_eq!(status, 200, "mcp should be enabled: {body}");
    assert!(
        body.get("result").and_then(|r| r.get("tools")).is_some(),
        "mcp tools/list should return tools: {body}"
    );

    // REST is enabled: the defined `POST /api/rest/pet` endpoint (create_pet)
    // serves the saved mutation. The harness `post` issues a POST, which this
    // endpoint accepts; it inserts and returns the new row's data.
    let (status, body) = s.post(
        "/api/rest/pet",
        &json!({ "id": 100, "name": "Spot", "status": "available" }),
        &user_headers(),
    );
    assert_eq!(status, 200, "rest endpoint should serve: {body}");
    assert_eq!(
        body.get("insert_pet")
            .and_then(|i| i.get("returning"))
            .and_then(|r| r.get(0))
            .and_then(|p| p.get("id")),
        Some(&json!(100)),
        "rest endpoint should return the inserted pet: {body}"
    );
}
