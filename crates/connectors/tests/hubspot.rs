//! HubSpot connector proofs (spec 016 §3), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::hubspot;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const OBJECT_ID: &str = "512";

fn operation(id: &str) -> &'static Operation {
    hubspot::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the hubspot declaration publishes {id}"))
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

/// `SimplePublicObject`, whose required fields are "archived", "createdAt",
/// "id", "properties", "updatedAt".
fn object() -> JsonValue {
    json!({
        "id": OBJECT_ID,
        "properties": { "email": "bcooper@biglytics.net", "firstname": "Bryan" },
        "createdAt": "2026-08-10T10:00:00.000Z",
        "updatedAt": "2026-08-10T10:05:00.000Z",
        "archived": false,
    })
}

fn collection() -> JsonValue {
    json!({ "results": [], "paging": { "next": { "after": "NTI1Cg%3D%3D", "link": "?after=NTI1Cg%3D%3D" } } })
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    let search = json!({
        "filter_groups": [], "query": "bryan", "sorts": [],
        "properties": ["email"], "after": null,
    });
    vec![
        (
            "contact.get",
            json!({ "object_id": OBJECT_ID, "properties": "email,firstname" }),
        ),
        ("contact.list", json!({ "properties": "email" })),
        ("contact.search", search),
        (
            "contact.create",
            json!({ "properties": { "email": "b@example.test" }, "associations": [] }),
        ),
        (
            "contact.update",
            json!({ "object_id": OBJECT_ID, "properties": { "firstname": "Bryan" } }),
        ),
        (
            "company.get",
            json!({ "object_id": OBJECT_ID, "properties": "name" }),
        ),
        ("company.list", json!({ "properties": "name" })),
        (
            "deal.get",
            json!({ "object_id": OBJECT_ID, "properties": "dealname" }),
        ),
        ("deal.list", json!({ "properties": "dealname" })),
        (
            "deal.create",
            json!({ "properties": { "dealname": "New" }, "associations": [] }),
        ),
        (
            "deal.update",
            json!({ "object_id": OBJECT_ID, "properties": { "dealname": "Renamed" } }),
        ),
        (
            "ticket.get",
            json!({ "object_id": OBJECT_ID, "properties": "subject" }),
        ),
        ("ticket.list", json!({ "properties": "subject" })),
    ]
}

/// `hubspot_request_shape`: exact method, path, query, headers, and body for
/// every operation.
#[tokio::test]
async fn hubspot_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/crm/v3/objects/contacts/{OBJECT_ID}"))
            .query("properties=email%2Cfirstname")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, object()),
        Expectation::new("GET", "/crm/v3/objects/contacts")
            .query("properties=email&limit=100")
            .respond_json(200, collection()),
        Expectation::new("POST", "/crm/v3/objects/contacts/search")
            .header("content-type", "application/json")
            .json_body(json!({
                "filterGroups": [], "query": "bryan", "sorts": [],
                "properties": ["email"], "limit": 100, "after": null,
            }))
            .respond_json(200, collection()),
        Expectation::new("POST", "/crm/v3/objects/contacts")
            .json_body(json!({ "properties": { "email": "b@example.test" }, "associations": [] }))
            .respond_json(201, object()),
        Expectation::new("PATCH", &format!("/crm/v3/objects/contacts/{OBJECT_ID}"))
            .json_body(json!({ "properties": { "firstname": "Bryan" } }))
            .respond_json(200, object()),
        Expectation::new("GET", &format!("/crm/v3/objects/companies/{OBJECT_ID}"))
            .query("properties=name")
            .respond_json(200, object()),
        Expectation::new("GET", "/crm/v3/objects/companies")
            .query("properties=name&limit=100")
            .respond_json(200, collection()),
        Expectation::new("GET", &format!("/crm/v3/objects/deals/{OBJECT_ID}"))
            .query("properties=dealname")
            .respond_json(200, object()),
        Expectation::new("GET", "/crm/v3/objects/deals")
            .query("properties=dealname&limit=100")
            .respond_json(200, collection()),
        Expectation::new("POST", "/crm/v3/objects/deals")
            .json_body(json!({ "properties": { "dealname": "New" }, "associations": [] }))
            .respond_json(201, object()),
        Expectation::new("PATCH", &format!("/crm/v3/objects/deals/{OBJECT_ID}"))
            .json_body(json!({ "properties": { "dealname": "Renamed" } }))
            .respond_json(200, object()),
        Expectation::new("GET", &format!("/crm/v3/objects/tickets/{OBJECT_ID}"))
            .query("properties=subject")
            .respond_json(200, object()),
        Expectation::new("GET", "/crm/v3/objects/tickets")
            .query("properties=subject&limit=100")
            .respond_json(200, collection()),
    ])
    .await;

    for (id, input) in inputs() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// Every request this connector can render stays on `api.hubapi.com`, which is
/// the reason `form.submit` is not declared here: HubSpot serves form
/// submissions from `api.hsforms.com`, and a connector has one origin.
#[test]
fn hubspot_form_submission_is_a_different_origin() {
    let origin = donat_connectors::sdk::Origin::parse("https://api.hubapi.com")
        .expect("the published origin is valid");
    for (id, input) in inputs() {
        let request = operation(id)
            .plan_request(&origin, &input)
            .expect("the declared request renders");
        assert_eq!(
            request.url().host_str(),
            Some("api.hubapi.com"),
            "{id} renders on the compiled origin"
        );
    }
    assert!(
        hubspot::connector().operation("form.submit").is_none(),
        "a form submission is another host, so it is another connector"
    );
}

/// `hubspot_auth_is_applied`: the private-app token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else.
#[tokio::test]
async fn hubspot_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/crm/v3/objects/contacts/{OBJECT_ID}"),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, object())])
    .await;

    let request = render(
        &stub,
        "contact.get",
        json!({ "object_id": OBJECT_ID, "properties": "email" }),
    );
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
        hubspot::connector().credential(),
        hubspot::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `hubspot_error_map`: every documented status reaches exactly one closed
/// class, and none of HubSpot's prose or its `correlationId` crosses the
/// boundary.
#[tokio::test]
async fn hubspot_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (414, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (423, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (477, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (504, ConnectorErrorClass::Http5xx),
        (521, ConnectorErrorClass::Http5xx),
        (524, ConnectorErrorClass::Http5xx),
        (526, ConnectorErrorClass::Http5xx),
        // A status the table does not carry takes the declared fallback.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/crm/v3/objects/contacts/{OBJECT_ID}"),
        )
        .respond_header("retry-after", "7")
        .respond_json(
            status,
            json!({
                "status": "error",
                "message": format!("portal 62515 shard db-7 token {SECRET_SENTINEL}"),
                "correlationId": "aeb5f871-7f07-4993-9211-075dc63e7cbf",
                "category": "VALIDATION_ERROR",
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "contact.get",
                json!({ "object_id": OBJECT_ID, "properties": "email" }),
            ))
            .await
            .expect("the stub answers");

        let failure = hubspot::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [
            SECRET_SENTINEL,
            "db-7",
            "62515",
            "aeb5f871",
            "VALIDATION_ERROR",
        ] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `hubspot_rate_limit_is_classified` (spec 016 §3 proof 5): "429 Too many
/// requests — Returned when over API rate limits" reaches `http_429` with its
/// retry hint clamped.
#[tokio::test]
async fn hubspot_rate_limit_is_classified() {
    let limited = json!({ "status": "error", "message": "You have reached your secondly limit.",
                          "correlationId": "c0", "category": "RATE_LIMITS" });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/crm/v3/objects/contacts/{OBJECT_ID}"))
            .respond_header("retry-after", "1")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/crm/v3/objects/contacts/{OBJECT_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "contact.get",
                json!({ "object_id": OBJECT_ID, "properties": "email" }),
            ))
            .await
            .expect("the stub answers");
        failures.push(hubspot::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(1)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `hubspot_cursor_is_opaque_and_bounded` (spec 016 §3 proof 4): the cursor is
/// echoed verbatim, is never parsed or constructed here, and the loop stops at
/// every budget.
#[tokio::test]
async fn hubspot_cursor_is_opaque_and_bounded() {
    let plan = hubspot::pagination("contact.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    // HubSpot's own example cursor is already percent-encoded — "NTI1Cg%3D%3D" —
    // which is exactly the case a connector that "helpfully" decoded one would
    // get wrong.
    const CURSOR: &str = "NTI1Cg%3D%3D&limit=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/crm/v3/objects/contacts")
            .query("properties=email&limit=100")
            .respond_json(
                200,
                json!({ "results": [{ "id": "1" }],
                        "paging": { "next": { "after": CURSOR, "link": "?after=…" } } }),
            ),
        Expectation::new("GET", "/crm/v3/objects/contacts")
            .query(
                "properties=email&limit=100&after=NTI1Cg%253D%253D%26limit%3D9999%23%2F%2E%2E%2F",
            )
            .respond_json(
                200,
                // A response with no `paging.next` is the end of the collection.
                json!({ "results": [{ "id": "2" }] }),
            ),
    ])
    .await;
    let results = plan
        .collect(
            render(&stub, "contact.list", json!({ "properties": "email" })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one cursor and stops when paging.next is absent");
    assert_eq!(results.len(), 2);
    stub.assert_satisfied();
}

/// `hubspot_pagination_is_bounded`: the declared plan terminates and
/// respects the call, page, item, and byte budgets.
#[tokio::test]
async fn hubspot_pagination_is_bounded() {
    let plan = hubspot::pagination("contact.list").expect("the list declares a plan");
    const CURSOR: &str = "NTI1Cg%3D%3D&limit=9999#/../";

    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/crm/v3/objects/contacts").respond_json(
                200,
                json!({ "results": [{ "id": "1" }, { "id": "2" }],
                        "paging": { "next": { "after": "more" } } }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "contact.list", json!({ "properties": "email" })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // The search cursor is a body field, echoed verbatim and never a query
    // value, and the page size stays the declaration's.
    let stub = ProviderStub::start([Expectation::new("POST", "/crm/v3/objects/contacts/search")
        .query("")
        .json_body(json!({
            "filterGroups": [], "query": null, "sorts": [], "properties": [],
            "limit": 100, "after": CURSOR,
        }))
        .respond_json(200, json!({ "results": [] }))])
    .await;
    let request = render(
        &stub,
        "contact.search",
        json!({ "filter_groups": [], "query": null, "sorts": [],
                "properties": [], "after": CURSOR }),
    );
    assert_eq!(request.url().query(), None);
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    for id in [
        "contact.get",
        "contact.search",
        "contact.create",
        "contact.update",
        "company.get",
        "deal.get",
        "deal.create",
        "deal.update",
        "ticket.get",
    ] {
        assert!(
            hubspot::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `hubspot_effects_are_classified`: every operation carries a class, and every
/// write is inventory-only on a machine-checkable absence.
#[test]
fn hubspot_effects_are_classified() {
    let connector = hubspot::connector();
    let expected = [
        ("contact.get", EffectClass::ReadOnly),
        ("contact.list", EffectClass::ReadOnly),
        ("contact.search", EffectClass::ReadOnly),
        ("contact.create", EffectClass::AtMostOnce),
        ("contact.update", EffectClass::InventoryOnly),
        ("company.get", EffectClass::ReadOnly),
        ("company.list", EffectClass::ReadOnly),
        ("deal.get", EffectClass::ReadOnly),
        ("deal.list", EffectClass::ReadOnly),
        ("deal.create", EffectClass::AtMostOnce),
        ("deal.update", EffectClass::InventoryOnly),
        ("ticket.get", EffectClass::ReadOnly),
        ("ticket.list", EffectClass::ReadOnly),
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
        Err(OperationRejection::InventoryOnly),
        "a partial update whose repeat sets the same properties is not what ADR 063 admits"
    );
    let create = operation("contact.create")
        .effect()
        .cloned()
        .expect("classified");
    let evidence = create
        .no_idempotency_evidence()
        .expect("an at-most-once class carries the search that found no key");
    assert!(
        evidence
            .searched_documentation()
            .contains("does not occur anywhere"),
        "the create records the machine-checkable absence it was admitted on"
    );
    assert!(
        evidence.repeat_produces().contains("a new object id"),
        "and what a second send would leave behind"
    );
}

/// `hubspot_output_contract`: the declared pointers read `SimplePublicObject`'s
/// own required fields, and a response missing one is a validation failure.
#[test]
fn hubspot_output_contract() {
    let get = operation("contact.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&object()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": OBJECT_ID,
            "properties": { "email": "bcooper@biglytics.net", "firstname": "Bryan" },
            "archived": false,
            "created_at": "2026-08-10T10:00:00.000Z",
            "updated_at": "2026-08-10T10:05:00.000Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"1","properties":{}}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // The create's documented success is `201`, and a `200` there is not one.
    let create = operation("contact.create");
    assert!(create.is_success(201) && !create.is_success(200));
    // A collection with no `paging` is a complete collection, not a broken one.
    assert_eq!(
        operation("contact.list")
            .decode_response(200, br#"{"results":[]}"#)
            .expect("a last page carries no paging"),
        json!({ "results": [], "next_after": null })
    );
}
