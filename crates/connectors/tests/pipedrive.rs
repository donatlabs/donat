//! Pipedrive connector proofs (spec 023 §4), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::pipedrive;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const DEAL_ID: i64 = 42;
const PERSON_ID: i64 = 7;

fn operation(id: &str) -> &'static Operation {
    pipedrive::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the pipedrive declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_header(pipedrive::TOKEN_HEADER)
        .expect("the declared header name is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn envelope(data: JsonValue) -> JsonValue {
    json!({ "success": true, "data": data, "additional_data": { "next_cursor": null } })
}

fn deal() -> JsonValue {
    json!({
        "id": DEAL_ID,
        "title": "Renewal",
        "owner_id": 9,
        "add_time": "2026-08-01T09:00:00Z",
        "update_time": "2026-08-02T09:00:00Z",
    })
}

fn person() -> JsonValue {
    json!({
        "id": PERSON_ID,
        "name": "Joe",
        "owner_id": 9,
        "add_time": "2026-08-01T09:00:00Z",
        "update_time": "2026-08-02T09:00:00Z",
    })
}

/// Every operation, with an input that satisfies it and a documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("deal.get", json!({ "record_id": DEAL_ID })),
        ("deal.list", json!({ "sort_by": "update_time" })),
        ("deal.search", json!({ "term": "renewal" })),
        (
            "deal.create",
            json!({
                "title": "Renewal", "value": 100, "currency": "EUR",
                "person_id": PERSON_ID, "org_id": null, "pipeline_id": null,
                "stage_id": null, "owner_id": null,
            }),
        ),
        (
            "deal.update",
            json!({ "record_id": DEAL_ID, "title": "Renewal", "value": 200,
                    "status": "won", "stage_id": null }),
        ),
        ("person.get", json!({ "record_id": PERSON_ID })),
        ("person.list", json!({ "sort_by": "id" })),
        ("person.search", json!({ "term": "joe" })),
        (
            "person.create",
            json!({ "name": "Joe", "emails": [], "phones": [], "org_id": null, "owner_id": null }),
        ),
        (
            "person.update",
            json!({ "record_id": PERSON_ID, "name": "Joe", "emails": [], "phones": [],
                    "owner_id": null }),
        ),
        (
            "note.add",
            json!({ "content": "<p>called</p>", "deal_id": DEAL_ID,
                    "person_id": null, "org_id": null }),
        ),
        ("note.list", json!({ "deal_id": DEAL_ID })),
    ]
}

/// `pipedrive_request_shape`: exact method, path, query, headers, and body for
/// every operation, including the v1 path the note resources are still served
/// from.
#[tokio::test]
async fn pipedrive_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/deals/{DEAL_ID}"))
            .query("")
            .header("x-api-token", SECRET_SENTINEL)
            .no_body()
            .respond_json(200, envelope(deal())),
        Expectation::new("GET", "/api/v2/deals")
            .query("sort_by=update%5Ftime&limit=100")
            .respond_json(200, envelope(json!([]))),
        Expectation::new("GET", "/api/v2/deals/search")
            .query("term=renewal&limit=100")
            .respond_json(200, envelope(json!({ "items": [] }))),
        Expectation::new("POST", "/api/v2/deals")
            .json_body(json!({
                "title": "Renewal", "value": 100, "currency": "EUR",
                "person_id": PERSON_ID, "org_id": null, "pipeline_id": null,
                "stage_id": null, "owner_id": null,
            }))
            .respond_json(200, envelope(deal())),
        Expectation::new("PATCH", &format!("/api/v2/deals/{DEAL_ID}"))
            .json_body(json!({ "title": "Renewal", "value": 200, "status": "won",
                               "stage_id": null }))
            .respond_json(200, envelope(deal())),
        Expectation::new("GET", &format!("/api/v2/persons/{PERSON_ID}"))
            .query("")
            .respond_json(200, envelope(person())),
        Expectation::new("GET", "/api/v2/persons")
            .query("sort_by=id&limit=100")
            .respond_json(200, envelope(json!([]))),
        Expectation::new("GET", "/api/v2/persons/search")
            .query("term=joe&limit=100")
            .respond_json(200, envelope(json!({ "items": [] }))),
        Expectation::new("POST", "/api/v2/persons")
            .json_body(json!({ "name": "Joe", "emails": [], "phones": [],
                               "org_id": null, "owner_id": null }))
            .respond_json(200, envelope(person())),
        Expectation::new("PATCH", &format!("/api/v2/persons/{PERSON_ID}"))
            .json_body(json!({ "name": "Joe", "emails": [], "phones": [], "owner_id": null }))
            .respond_json(200, envelope(person())),
        // Notes have no v2 form, so they are the v1 path on the same origin.
        Expectation::new("POST", "/v1/notes")
            .json_body(json!({ "content": "<p>called</p>", "deal_id": DEAL_ID,
                               "person_id": null, "org_id": null }))
            .respond_json(
                200,
                envelope(json!({ "id": 1, "add_time": "2026-08-02 09:00:00" })),
            ),
        Expectation::new("GET", "/v1/notes")
            .query(&format!("deal_id={DEAL_ID}&limit=100"))
            .respond_json(200, envelope(json!([]))),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v2/")
                || request.url().path().starts_with("/v1/"),
            "{id} renders a published Pipedrive path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `pipedrive_auth_is_applied`: the API token reaches the wire as the published
/// `x-api-token` header, never as a query value, and appears nowhere else.
#[tokio::test]
async fn pipedrive_auth_is_applied() {
    let stub = ProviderStub::start(
        [Expectation::new("GET", &format!("/api/v2/deals/{DEAL_ID}"))
            .header("x-api-token", SECRET_SENTINEL)
            .respond_json(200, envelope(deal()))],
    )
    .await;

    let request = render(&stub, "deal.get", json!({ "record_id": DEAL_ID }));
    assert!(
        request
            .headers()
            .get("x-api-token")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    // Pipedrive's legacy `api_token` query parameter is not what this connector
    // declares, and nothing here may put the credential in a URL.
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        pipedrive::connector().credential(),
        pipedrive::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `pipedrive_error_map`: every documented status reaches exactly one closed
/// class, and none of Pipedrive's prose crosses the boundary.
#[tokio::test]
async fn pipedrive_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (402, ConnectorErrorClass::Permanent),
        (403, ConnectorErrorClass::Permanent),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (410, ConnectorErrorClass::Permanent),
        (415, ConnectorErrorClass::Validation),
        (422, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (501, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v2/deals/{DEAL_ID}"),
        )
        .respond_json(
            status,
            json!({
                "success": false,
                "error": format!("acme-sandbox rejected token {SECRET_SENTINEL}"),
                "error_info": "Please check developers.pipedrive.com",
                "data": null,
                "additional_data": null,
                "code": "feature_capping_deals_limit",
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "deal.get", json!({ "record_id": DEAL_ID })))
            .await
            .expect("the stub answers");

        let failure = pipedrive::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-sandbox", "feature_capping"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // The `403` a deployment earns by ignoring a `429` carries an HTML body
    // rather than the envelope, and the status still answers.
    let stub = ProviderStub::start(
        [Expectation::new("GET", &format!("/api/v2/deals/{DEAL_ID}"))
            .respond_bytes(403, b"<html>This error is produced by Cloudflare".to_vec())],
    )
    .await;
    let response = stub
        .send(render(&stub, "deal.get", json!({ "record_id": DEAL_ID })))
        .await
        .expect("the stub answers");
    assert_eq!(
        pipedrive::error_map().classify_response(&response).class(),
        ConnectorErrorClass::Permanent
    );
    stub.assert_satisfied();
}

/// `pipedrive_rate_limit_is_classified`: the documented `429` is retryable, and
/// Pipedrive publishes no `Retry-After` — so the hint is absent unless the
/// response carried one, and a hostile one is clamped.
#[tokio::test]
async fn pipedrive_rate_limit_is_classified() {
    let limited = json!({ "success": false, "error": "rate limit exceeded",
                          "data": null, "additional_data": null });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/deals/{DEAL_ID}"))
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/api/v2/deals/{DEAL_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "deal.get", json!({ "record_id": DEAL_ID })))
            .await
            .expect("the stub answers");
        failures.push(pipedrive::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Pipedrive publishes no Retry-After, so the connector invents none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `pipedrive_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the cursor is
/// echoed back verbatim, is never parsed or constructed here, and the walk makes
/// exactly the number of requests the plan declares.
#[tokio::test]
async fn pipedrive_cursor_is_opaque_and_bounded() {
    let plan = pipedrive::pagination("deal.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const CURSOR: &str = "eyJhY3Rpdml0aWVzIjoyN30&limit=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v2/deals")
            .query("sort_by=id&limit=100")
            .respond_json(
                200,
                json!({ "success": true, "data": [{ "id": 1 }],
                        "additional_data": { "next_cursor": CURSOR } }),
            ),
        Expectation::new("GET", "/api/v2/deals")
            .query(
                "sort_by=id&limit=100&cursor=eyJhY3Rpdml0aWVzIjoyN30%26limit%3D9999%23%2F%2E%2E%2F",
            )
            .respond_json(
                200,
                // "The value of the `next_cursor` field will be `null` if you
                // have reached the end of the dataset."
                json!({ "success": true, "data": [{ "id": 2 }],
                        "additional_data": { "next_cursor": null } }),
            ),
    ])
    .await;

    let deals = plan
        .collect(
            render(&stub, "deal.list", json!({ "sort_by": "id" })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one cursor and stops on the null one");
    assert_eq!(deals, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `pipedrive_pagination_is_bounded`: every declared plan terminates and
/// respects the call, page, item, and byte budgets, and the operations that
/// declare none send one request.
#[tokio::test]
async fn pipedrive_pagination_is_bounded() {
    let plan = pipedrive::pagination("deal.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/v2/deals").respond_json(
                200,
                json!({ "success": true, "data": [{ "id": 1 }, { "id": 2 }],
                        "additional_data": { "next_cursor": "more" } }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "deal.list", json!({ "sort_by": "id" })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // A search collects the nested item list Pipedrive publishes for it, not the
    // one a list publishes.
    assert_eq!(
        pipedrive::pagination("deal.search")
            .expect("the search declares a plan")
            .items_pointer(),
        "/data/items"
    );
    assert_eq!(
        pipedrive::pagination("note.list")
            .expect("the v1 note collection declares a plan")
            .items_pointer(),
        "/data"
    );

    for id in [
        "deal.get",
        "deal.create",
        "deal.update",
        "person.get",
        "person.create",
        "person.update",
        "note.add",
    ] {
        assert!(
            pipedrive::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `pipedrive_success_envelope_cannot_carry_a_failure` (spec 023 §4 proof 4):
/// the body gate sits between the status check and the output pointers, so a
/// `200` carrying `success: false` never reads as an activity success.
#[tokio::test]
async fn pipedrive_success_envelope_cannot_carry_a_failure() {
    let get = operation("deal.get");
    let headers = reqwest::header::HeaderMap::new();

    let failed = serde_json::to_vec(&json!({
        "success": false,
        "error": "Requested service is not available",
        "error_info": "Please check developers.pipedrive.com",
        "data": null,
        "additional_data": null,
    }))
    .expect("a fixture serializes");
    let failure = pipedrive::decode(get, 200, &headers, &failed)
        .expect_err("a 200 carrying success: false is not a success");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    assert!(
        !failure.diagnostic().contains("Requested service"),
        "provider prose never crosses the boundary"
    );

    // And a body with no envelope at all is outside the published contract
    // rather than an empty success.
    assert_eq!(
        pipedrive::decode(get, 200, &headers, b"{}")
            .expect_err("an answer with no `success` is outside the contract")
            .class(),
        ConnectorErrorClass::Invariant
    );

    let ok = serde_json::to_vec(&envelope(deal())).expect("a fixture serializes");
    assert_eq!(
        pipedrive::decode(get, 200, &headers, &ok)
            .expect("the declared contract is satisfied")
            .get("id"),
        Some(&json!(DEAL_ID))
    );
    // A non-success status is classified before the body is looked at.
    assert_eq!(
        pipedrive::decode(get, 404, &headers, &ok)
            .expect_err("an undeclared status is never a success")
            .class(),
        ConnectorErrorClass::Permanent
    );
}

/// `pipedrive_effects_are_classified`: every operation carries a class, and the
/// two `PATCH` updates are refused by the method Pipedrive chose for them.
#[test]
fn pipedrive_effects_are_classified() {
    let connector = pipedrive::connector();
    let expected = [
        ("deal.get", EffectClass::ReadOnly),
        ("deal.list", EffectClass::ReadOnly),
        ("deal.search", EffectClass::ReadOnly),
        ("deal.create", EffectClass::AtMostOnce),
        ("deal.update", EffectClass::InventoryOnly),
        ("person.get", EffectClass::ReadOnly),
        ("person.list", EffectClass::ReadOnly),
        ("person.search", EffectClass::ReadOnly),
        ("person.create", EffectClass::AtMostOnce),
        ("person.update", EffectClass::InventoryOnly),
        ("note.add", EffectClass::AtMostOnce),
        ("note.list", EffectClass::ReadOnly),
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
        connector.admit_operation("deal.update"),
        Err(OperationRejection::InventoryOnly)
    );

    // The at-most-once class carries both halves of ADR 063's evidence: the
    // search that found no key, and what a second send would leave behind.
    let evidence = operation("deal.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("OpenAPI"));
    assert!(evidence.repeat_produces().contains("second deal"));
}

/// `pipedrive_output_contract`: the declared pointers read Pipedrive's own
/// envelope, and a search's payload is read from the nested object it publishes.
#[test]
fn pipedrive_output_contract() {
    let get = operation("deal.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&envelope(deal())).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": DEAL_ID, "title": "Renewal", "owner_id": 9,
            "add_time": "2026-08-01T09:00:00Z", "update_time": "2026-08-02T09:00:00Z",
        })
    );
    // "Resource IDs are returned as integers" — a string there is a contract
    // violation, not a coercion.
    assert_eq!(
        get.decode_response(200, br#"{"success":true,"data":{"id":"42"}}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"success":true,"data":{}}"#)
            .expect_err("an envelope with no id is not a deal")
            .class(),
        ConnectorErrorClass::Validation
    );

    let search = operation("deal.search");
    assert_eq!(
        search
            .decode_response(
                200,
                br#"{"success":true,"data":{"items":[{"result_score":1,"item":{"id":1}}]},
                     "additional_data":{"next_cursor":"c"}}"#,
            )
            .expect("the declared contract is satisfied"),
        json!({
            "items": [{ "result_score": 1, "item": { "id": 1 } }],
            "next_cursor": "c",
        })
    );
}
