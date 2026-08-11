//! Notion connector proofs (spec 016 §3), against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::notion;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const PAGE_ID: &str = "59833787-2cf9-4fdf-8782-e53db20768a5";
const BLOCK_ID: &str = "b55c9c91-384d-452b-81db-d1ef79372b75";
const DATA_SOURCE_ID: &str = "897e5a76-ae52-4b48-9fdf-e71f5945d1af";
const DATABASE_ID: &str = "668d797c-76fa-4934-9b05-ad288df2d136";
const USER_ID: &str = "d40e767c-d7af-4b18-a86d-55c61f1e39a4";

/// A path value as the SDK renders it.
///
/// The SDK percent-encodes every non-alphanumeric byte of a path segment, so a
/// Notion UUID arrives with its hyphens as `%2D`. That is equivalent under
/// RFC 3986 §2.3 — "URIs that differ in the replacement of an unreserved
/// character with its corresponding percent-encoded US-ASCII octet are
/// equivalent" — and it is the same encoding `github.file.get` and
/// `aws_s3.object.get` send an identifier in.
fn segment(value: &str) -> String {
    value.replace('-', "%2D")
}

fn operation(id: &str) -> &'static Operation {
    notion::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the notion declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn page_object() -> JsonValue {
    json!({
        "object": "page",
        "id": PAGE_ID,
        "created_time": "2022-03-01T19:05:00.000Z",
        "last_edited_time": "2022-07-06T20:25:00.000Z",
        "parent": { "type": "data_source_id", "data_source_id": DATA_SOURCE_ID },
        "properties": { "Name": { "id": "title", "type": "title", "title": [] } },
        "url": "https://www.notion.so/Avocado-59833787"
    })
}

fn list_object(results: JsonValue) -> JsonValue {
    json!({ "object": "list", "results": results, "next_cursor": null, "has_more": false })
}

/// Every operation, with an input that satisfies it and a documented success.
fn cases() -> Vec<(&'static str, JsonValue, JsonValue)> {
    vec![
        ("page.get", json!({ "page_id": PAGE_ID }), page_object()),
        (
            "page.create",
            json!({
                "parent": { "data_source_id": DATA_SOURCE_ID },
                "properties": { "Name": { "title": [] } },
            }),
            page_object(),
        ),
        (
            "page.update",
            json!({ "page_id": PAGE_ID, "properties": {}, "archived": false }),
            page_object(),
        ),
        (
            "data_source.query",
            json!({
                "data_source_id": DATA_SOURCE_ID,
                "filter": { "property": "Done", "checkbox": { "equals": true } },
                "sorts": [],
                "start_cursor": null,
            }),
            list_object(json!([])),
        ),
        (
            "database.get",
            json!({ "database_id": DATABASE_ID }),
            json!({
                "object": "database",
                "id": DATABASE_ID,
                "title": [],
                "data_sources": [{ "id": DATA_SOURCE_ID, "name": "Tasks" }],
                "url": "https://www.notion.so/668d797c",
            }),
        ),
        (
            "block.children_list",
            json!({ "block_id": BLOCK_ID }),
            list_object(json!([])),
        ),
        (
            "block.children_append",
            json!({ "block_id": BLOCK_ID, "children": [] }),
            list_object(json!([])),
        ),
        (
            "user.get",
            json!({ "user_id": USER_ID }),
            json!({ "object": "user", "id": USER_ID, "type": "person",
                    "name": "Avocado Lovelace", "avatar_url": null }),
        ),
        ("user.list", json!({}), list_object(json!([]))),
        (
            "search",
            json!({ "query": "External", "filter": { "value": "page", "property": "object" },
                    "sort": { "direction": "ascending", "timestamp": "last_edited_time" },
                    "start_cursor": null }),
            list_object(json!([])),
        ),
    ]
}

/// `notion_request_shape`: exact method, path, query, headers, and body for
/// every operation, including the pinned `Notion-Version` on all of them.
#[tokio::test]
async fn notion_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v1/pages/{}", segment(PAGE_ID)))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("notion-version", notion::API_VERSION)
            .no_body()
            .respond_json(200, page_object()),
        Expectation::new("POST", "/v1/pages")
            .header("notion-version", notion::API_VERSION)
            .header("content-type", "application/json")
            .json_body(json!({
                "parent": { "data_source_id": DATA_SOURCE_ID },
                "properties": { "Name": { "title": [] } },
            }))
            .respond_json(200, page_object()),
        Expectation::new("PATCH", &format!("/v1/pages/{}", segment(PAGE_ID)))
            .json_body(json!({ "properties": {}, "archived": false }))
            .respond_json(200, page_object()),
        Expectation::new(
            "POST",
            &format!("/v1/data_sources/{}/query", segment(DATA_SOURCE_ID)),
        )
        .json_body(json!({
            "filter": { "property": "Done", "checkbox": { "equals": true } },
            "sorts": [],
            "start_cursor": null,
            "page_size": 100,
        }))
        .respond_json(200, list_object(json!([]))),
        Expectation::new("GET", &format!("/v1/databases/{}", segment(DATABASE_ID)))
            .query("")
            .respond_json(
                200,
                json!({ "id": DATABASE_ID, "data_sources": [], "title": [], "url": "u" }),
            ),
        Expectation::new("GET", &format!("/v1/blocks/{}/children", segment(BLOCK_ID)))
            .query("page_size=100")
            .no_body()
            .respond_json(200, list_object(json!([]))),
        Expectation::new(
            "PATCH",
            &format!("/v1/blocks/{}/children", segment(BLOCK_ID)),
        )
        .json_body(json!({ "children": [] }))
        .respond_json(200, list_object(json!([]))),
        Expectation::new("GET", &format!("/v1/users/{}", segment(USER_ID)))
            .query("")
            .respond_json(
                200,
                json!({ "id": USER_ID, "type": "person", "name": "n", "avatar_url": null }),
            ),
        Expectation::new("GET", "/v1/users")
            .query("page_size=100")
            .respond_json(200, list_object(json!([]))),
        Expectation::new("POST", "/v1/search")
            .json_body(json!({
                "query": "External",
                "filter": { "value": "page", "property": "object" },
                "sort": { "direction": "ascending", "timestamp": "last_edited_time" },
                "start_cursor": null,
                "page_size": 100,
            }))
            .respond_json(200, list_object(json!([]))),
    ])
    .await;

    for (id, input, _) in cases() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();

    // Every operation pins the contract version; a deployment cannot drift onto
    // whatever Notion serves by default, because there is no default.
    for (id, input, _) in cases() {
        let request = render(&stub, id, input);
        assert_eq!(
            request
                .headers()
                .get("notion-version")
                .and_then(|value| value.to_str().ok()),
            Some(notion::API_VERSION),
            "{id}"
        );
    }
}

/// `notion_auth_is_applied`: the integration token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else.
#[tokio::test]
async fn notion_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/v1/pages/{}", segment(PAGE_ID)))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, page_object()),
        ])
        .await;

    let request = render(&stub, "page.get", json!({ "page_id": PAGE_ID }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        notion::connector().credential(),
        notion::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `notion_error_map`: every documented status and code reaches exactly one
/// closed class, and none of Notion's prose crosses the boundary.
#[tokio::test]
async fn notion_error_map() {
    let documented = [
        (400, "invalid_json", ConnectorErrorClass::Validation),
        (400, "invalid_request_url", ConnectorErrorClass::Validation),
        (400, "validation_error", ConnectorErrorClass::Validation),
        (400, "missing_version", ConnectorErrorClass::Validation),
        (401, "unauthorized", ConnectorErrorClass::Authentication),
        (
            403,
            "restricted_resource",
            ConnectorErrorClass::Authentication,
        ),
        (404, "object_not_found", ConnectorErrorClass::Permanent),
        (409, "conflict_error", ConnectorErrorClass::Permanent),
        (429, "rate_limited", ConnectorErrorClass::Http429),
        (500, "internal_server_error", ConnectorErrorClass::Http5xx),
        (502, "bad_gateway", ConnectorErrorClass::Http5xx),
        (503, "service_unavailable", ConnectorErrorClass::Http5xx),
        (
            503,
            "database_connection_unavailable",
            ConnectorErrorClass::Http5xx,
        ),
        (504, "gateway_timeout", ConnectorErrorClass::Http5xx),
        (529, "service_overload", ConnectorErrorClass::Http5xx),
        // A status the table does not carry, with a code no rule names.
        (418, "teapot", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/v1/pages/{}", segment(PAGE_ID)),
        )
        .respond_header("retry-after", "3")
        .respond_json(
            status,
            json!({
                "object": "error",
                "status": status,
                "code": code,
                "message": format!("workspace acme on shard db-7 with token {SECRET_SENTINEL}"),
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "page.get", json!({ "page_id": PAGE_ID })))
            .await
            .expect("the stub answers");

        let failure = notion::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            operation("page.get")
                .decode_response(status, response.body())
                .is_err(),
            "status {status} is not a declared success"
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme", "db-7", code] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `notion_rate_limit_is_classified` (spec 016 §3 proof 5): the documented
/// rate-limit response reaches `http_429` with its retry hint clamped.
#[tokio::test]
async fn notion_rate_limit_is_classified() {
    let limited = json!({ "object": "error", "status": 429, "code": "rate_limited",
                          "message": "You have been rate limited." });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v1/pages/{}", segment(PAGE_ID)))
            .respond_header("retry-after", "3")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/v1/pages/{}", segment(PAGE_ID)))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "page.get", json!({ "page_id": PAGE_ID })))
            .await
            .expect("the stub answers");
        failures.push(notion::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    // "respect the `Retry-After` response header. The header value is an
    // integer number of seconds."
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(3)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `notion_cursor_is_opaque_and_bounded` (spec 016 §3 proof 4): the `GET`
/// cursor is echoed verbatim and the loop stops at every budget; the two `POST`
/// reads carry their cursor in the body, where the caller echoes it and one call
/// is one page.
#[tokio::test]
async fn notion_cursor_is_opaque_and_bounded() {
    let plan = notion::pagination("user.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const CURSOR: &str = "0430f0b9-b3d4-4dcb-9a1b-1c8e2f3a4b5c&page_size=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/v1/users")
            .query("page_size=100")
            .respond_json(
                200,
                json!({ "object": "list", "results": [{ "id": "u1" }],
                        "next_cursor": CURSOR, "has_more": true }),
            ),
        Expectation::new("GET", "/v1/users")
            .query(
                "page_size=100&start_cursor=0430f0b9%2Db3d4%2D4dcb%2D9a1b%2D1c8e2f3a4b5c%26page%5Fsize%3D9999%23%2F%2E%2E%2F",
            )
            .respond_json(
                200,
                // "`next_cursor` — A string that can be used to retrieve the
                // next page of results", null when there is none.
                json!({ "object": "list", "results": [{ "id": "u2" }],
                        "next_cursor": null, "has_more": false }),
            ),
    ])
    .await;
    let members = plan
        .collect(
            render(&stub, "user.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one cursor and stops on the null one");
    assert_eq!(members.len(), 2);
    stub.assert_satisfied();
}

/// `notion_pagination_is_bounded`: the declared plan terminates and
/// respects the call, page, item, and byte budgets.
#[tokio::test]
async fn notion_pagination_is_bounded() {
    let plan = notion::pagination("user.list").expect("the list declares a plan");
    const CURSOR: &str = "0430f0b9-b3d4-4dcb-9a1b-1c8e2f3a4b5c&page_size=9999#/../";

    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v1/users").respond_json(
                200,
                json!({ "results": [{ "id": "u1" }, { "id": "u2" }],
                        "next_cursor": "more", "has_more": true }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "user.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // The body-carried cursor: echoed verbatim into the request, never parsed,
    // and the page size stays the declaration's.
    let stub = ProviderStub::start([Expectation::new("POST", "/v1/search")
        .json_body(json!({
            "query": "q", "filter": null, "sort": null,
            "start_cursor": CURSOR, "page_size": 100,
        }))
        .respond_json(
            200,
            json!({ "results": [], "next_cursor": null, "has_more": false }),
        )])
    .await;
    let request = render(
        &stub,
        "search",
        json!({ "query": "q", "filter": null, "sort": null, "start_cursor": CURSOR }),
    );
    let sent: JsonValue = serde_json::from_slice(request.body()).expect("the body is JSON");
    assert_eq!(sent["start_cursor"], json!(CURSOR));
    assert_eq!(sent["page_size"], json!(100));
    assert_eq!(
        request.url().query(),
        None,
        "a body cursor never becomes a query"
    );
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    for id in [
        "page.get",
        "page.create",
        "page.update",
        "database.get",
        "user.get",
        "block.children_append",
        "search",
        "data_source.query",
    ] {
        assert!(
            notion::pagination(id).is_none(),
            "{id} declares no query-parameter continuation plan"
        );
    }
}

/// `notion_effects_are_classified`: every operation carries a class, and every
/// write is inventory-only on Notion's own retry guidance.
#[test]
fn notion_effects_are_classified() {
    let connector = notion::connector();
    let expected = [
        ("page.get", EffectClass::ReadOnly),
        ("page.create", EffectClass::AtMostOnce),
        ("page.update", EffectClass::InventoryOnly),
        ("data_source.query", EffectClass::ReadOnly),
        ("database.get", EffectClass::ReadOnly),
        ("block.children_list", EffectClass::ReadOnly),
        ("block.children_append", EffectClass::AtMostOnce),
        ("user.get", EffectClass::ReadOnly),
        ("user.list", EffectClass::ReadOnly),
        ("search", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector.admit_operation("page.update"),
        Err(OperationRejection::InventoryOnly),
        "a partial update whose repeat sets the same properties is not what ADR 063 admits"
    );
    assert!(
        operation("block.children_append")
            .effect()
            .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
            .is_some_and(|evidence| evidence
                .searched_documentation()
                .contains("its own idempotency protection")
                && evidence.repeat_produces().contains("a second copy")),
        "the write records Notion's own retry guidance, and the duplicate it would leave"
    );
}

/// `notion_output_contract`: the declared pointers read Notion's own objects,
/// and a body that does not satisfy them is a validation failure, not a null.
#[test]
fn notion_output_contract() {
    let get = operation("page.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&page_object()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": PAGE_ID,
            "url": "https://www.notion.so/Avocado-59833787",
            "properties": { "Name": { "id": "title", "type": "title", "title": [] } },
            "parent": { "type": "data_source_id", "data_source_id": DATA_SOURCE_ID },
            "created_time": "2022-03-01T19:05:00.000Z",
            "last_edited_time": "2022-07-06T20:25:00.000Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"object":"page","id":"x"}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );

    // "`name` (string | null)" — an absent optional pointer is published as
    // null rather than refused.
    assert_eq!(
        operation("user.get")
            .decode_response(200, br#"{"object":"user","id":"u1","type":"bot"}"#)
            .expect("an optional pointer may be absent"),
        json!({ "id": "u1", "type": "bot", "name": null, "avatar_url": null })
    );

    // The list envelope: `has_more` is required, because a walk that could not
    // read it would not know whether it had the whole collection.
    assert_eq!(
        operation("user.list")
            .decode_response(200, br#"{"object":"list","results":[]}"#)
            .expect_err("a list with no has_more is not a page")
            .class(),
        ConnectorErrorClass::Validation
    );
}
