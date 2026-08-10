//! Eventbrite connector proofs (spec 028 §4, which adopts spec 023 §4), against
//! the SDK's local provider stub.
//!
//! Eventbrite is this half of the batch's continuation-token connector: its
//! cursor is an opaque value in the response body that is spent as a *query*
//! value, and `eventbrite_cursor_is_opaque_and_bounded` proves both halves of
//! that — the walk follows it, and a body that spells a URL becomes a query
//! parameter on this origin rather than a destination.

use std::time::Duration;

use donat_connectors::providers::eventbrite;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const ORGANIZATION: &str = "123456789012";
const EVENT_ID: &str = "987654321098";

fn connector() -> Connector {
    eventbrite::connector(ORGANIZATION).expect("a configured Eventbrite organization declares")
}

fn operation(connector: &Connector, id: &str) -> Operation {
    connector
        .operation(id)
        .unwrap_or_else(|| panic!("the eventbrite declaration publishes {id}"))
        .clone()
}

fn render(stub: &ProviderStub, connector: &Connector, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(connector, id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the configured token");
    request
}

fn event() -> JsonValue {
    json!({
        "id": EVENT_ID,
        "name": { "text": "Donat Day", "html": "<p>Donat Day</p>" },
        "start": { "timezone": "Europe/Berlin", "local": "2026-09-01T18:00:00",
                   "utc": "2026-09-01T16:00:00Z" },
        "end": { "timezone": "Europe/Berlin", "local": "2026-09-01T21:00:00",
                 "utc": "2026-09-01T19:00:00Z" },
        "url": "https://www.eventbrite.com/e/donat-day-987654321098",
        "status": "live",
        "currency": "EUR",
        "organization_id": ORGANIZATION,
        "created": "2026-08-01T10:00:00Z",
        "changed": "2026-08-02T10:00:00Z",
    })
}

fn create_input() -> JsonValue {
    json!({
        "name_html": "<p>Donat Day</p>", "timezone": "Europe/Berlin",
        "start_utc": "2026-09-01T16:00:00Z", "end_utc": "2026-09-01T19:00:00Z",
        "currency": "EUR", "listed": true,
    })
}

fn create_body() -> JsonValue {
    json!({
        "event": {
            "name": { "html": "<p>Donat Day</p>" },
            "start": { "timezone": "Europe/Berlin", "utc": "2026-09-01T16:00:00Z" },
            "end": { "timezone": "Europe/Berlin", "utc": "2026-09-01T19:00:00Z" },
            "currency": "EUR",
            "listed": true,
        }
    })
}

fn organization_events() -> String {
    format!("/v3/organizations/{ORGANIZATION}/events/")
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("user.me", json!({})),
        ("event.get", json!({ "event_id": EVENT_ID })),
        (
            "event.list",
            json!({ "status": "live", "order_by": "start_asc" }),
        ),
        ("event.create", create_input()),
        (
            "event.update",
            json!({
                "event_id": EVENT_ID, "name_html": "<p>Donat Day</p>",
                "timezone": "Europe/Berlin", "start_utc": "2026-09-01T16:00:00Z",
                "end_utc": "2026-09-01T19:00:00Z", "currency": "EUR", "listed": true,
            }),
        ),
        (
            "attendee.list",
            json!({
                "event_id": EVENT_ID, "status": "attending",
                "changed_since": "2026-08-01T00:00:00Z",
            }),
        ),
        (
            "order.list",
            json!({
                "event_id": EVENT_ID, "status": "placed",
                "changed_since": "2026-08-01T00:00:00Z",
            }),
        ),
    ]
}

/// `eventbrite_request_shape`: exact method, path, query, headers, and body for
/// every operation, each organization-scoped one under this deployment's
/// organization.
#[tokio::test]
async fn eventbrite_request_shape() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v3/users/me/")
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, json!({ "id": "1", "name": "Kim Allen" })),
        Expectation::new("GET", &format!("/v3/events/{EVENT_ID}/")).respond_json(200, event()),
        Expectation::new("GET", &organization_events())
            .query("status=live&order_by=start%5Fasc")
            .respond_json(200, json!({ "events": [event()], "pagination": {} })),
        Expectation::new("POST", &organization_events())
            .json_body(create_body())
            .respond_json(200, event()),
        Expectation::new("POST", &format!("/v3/events/{EVENT_ID}/"))
            .json_body(create_body())
            .respond_json(200, event()),
        Expectation::new("GET", &format!("/v3/events/{EVENT_ID}/attendees/"))
            .query("status=attending&changed_since=2026%2D08%2D01T00%3A00%3A00Z")
            .respond_json(200, json!({ "attendees": [], "pagination": {} })),
        Expectation::new("GET", &format!("/v3/events/{EVENT_ID}/orders/"))
            .query("status=placed&changed_since=2026%2D08%2D01T00%3A00%3A00Z")
            .respond_json(200, json!({ "orders": [], "pagination": {} })),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, &connector, id, input);
        assert!(
            request.url().path().starts_with("/v3/"),
            "{id} renders under Eventbrite's own version prefix: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `eventbrite_auth_is_applied`: the configured private token reaches the wire
/// as the `Bearer` credential Eventbrite publishes, it is redacted everywhere
/// else, and a request with no token never renders.
#[tokio::test]
async fn eventbrite_auth_is_applied() {
    let connector = connector();
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/users/me/")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, json!({ "id": "1" }))])
    .await;

    let request = render(&stub, &connector, "user.me", json!({}));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    // Eventbrite also publishes `?token=`, which this connector deliberately
    // does not use: a credential on the query string reaches every proxy log
    // between here and the provider.
    assert!(!request.url_carries_credential());
    assert!(!request.url().query().unwrap_or_default().contains("token"));
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{connector:?}").contains(SECRET_SENTINEL));

    // The declared credential contract names the secret and the non-secret
    // organization, and carries neither value.
    let fields = connector.credential().fields();
    assert_eq!(fields.len(), 2, "{fields:?}");
    assert!(
        fields
            .iter()
            .any(|field| field.name() == "secret" && field.is_secret())
    );
    assert!(
        fields
            .iter()
            .any(|field| field.name() == eventbrite::ORGANIZATION_ID && !field.is_secret())
    );

    let mut unauthenticated = operation(&connector, "user.me")
        .plan_request(&stub.origin(), &json!({}))
        .expect("the declared request renders");
    let failure = AuthPlan::bearer()
        .apply(&Credential::from_fields([]), &mut unauthenticated, None)
        .expect_err("a connector with no configured token cannot send");
    assert_eq!(failure.code(), "connector_credential_missing_field");

    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `eventbrite_organization_comes_only_from_deploy_time_configuration` (spec 023
/// §4 proof 1, in the shape Eventbrite's URL grammar gives it): input, a
/// provider body, and a continuation each fail to move the organization a
/// request reaches.
#[tokio::test]
async fn eventbrite_organization_comes_only_from_deploy_time_configuration() {
    let connector = connector();

    // 1. Operation input. Both organization-scoped paths render under the
    //    configured organization whatever the input says, and no operation
    //    publishes the organization as a slot at all.
    let stub = ProviderStub::start([Expectation::new("GET", &organization_events())
        .query("status=live&order_by=start%5Fasc")
        .respond_json(200, json!({ "events": [], "pagination": {} }))])
    .await;
    stub.send(render(
        &stub,
        &connector,
        "event.list",
        json!({ "status": "live", "order_by": "start_asc" }),
    ))
    .await
    .expect("the stub answers on this deployment's organization");

    for (id, mut input) in cases() {
        if let Some(fields) = input.as_object_mut() {
            fields.insert(
                eventbrite::ORGANIZATION_ID.to_owned(),
                json!("000000000000"),
            );
        }
        let rendered = operation(&connector, id)
            .plan_request(&stub.origin(), &input)
            .expect("the declared request renders");
        assert!(
            !rendered.url().path().contains("000000000000"),
            "{id} renders under this deployment's organization: {}",
            rendered.url().path()
        );
        assert!(
            !operation(&connector, id)
                .project()
                .inputs()
                .iter()
                .any(|input| input.name() == eventbrite::ORGANIZATION_ID),
            "{id} must not publish the organization as a Process input"
        );
    }

    // 2. A provider response naming another organization is data, not a
    //    destination.
    let output = operation(&connector, "event.get")
        .extract_output(&json!({ "id": EVENT_ID, "organization_id": "000000000000" }))
        .expect("the declared contract is satisfied");
    assert_eq!(output.get("organization_id"), Some(&json!("000000000000")));

    // 3. A continuation that spells an absolute URL is spent as a *query value*
    //    on this origin, never as a destination: that is the whole difference
    //    between `TokenInBody` and the body-carried next-URI plan.
    let walk_stub = ProviderStub::start([
        Expectation::new("GET", &organization_events())
            .query("status=live&order_by=start%5Fasc")
            .respond_json(
                200,
                json!({
                    "events": [{ "id": "1" }],
                    "pagination": {
                        "continuation": "https://attacker.invalid/v3/organizations/0/events/",
                        "has_more_items": true,
                    },
                }),
            ),
        Expectation::new("GET", &organization_events())
            .query(
                "status=live&order_by=start%5Fasc\
                 &continuation=https%3A%2F%2Fattacker%2Einvalid%2Fv3%2Forganizations%2F0%2Fevents%2F",
            )
            .respond_json(200, json!({ "events": [], "pagination": {} })),
    ])
    .await;
    let events = eventbrite::pagination("event.list")
        .expect("event.list declares a plan")
        .collect(
            render(
                &walk_stub,
                &connector,
                "event.list",
                json!({ "status": "live", "order_by": "start_asc" }),
            ),
            &walk_stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| walk_stub.send(request),
        )
        .await
        .expect("a continuation is a query value, so the walk stays on this origin");
    assert_eq!(events, vec![json!({ "id": "1" })]);
    walk_stub.assert_satisfied();

    // And the configuration itself admits Eventbrite's own grammar and nothing
    // else.
    for hostile in [
        "",
        "123456789012/../000000000000",
        "12345678901a",
        "1234 56789012",
        "1234567890123456789012345",
    ] {
        assert!(
            eventbrite::validate_organization_id(hostile).is_err(),
            "`{hostile}` is not an Eventbrite organization id"
        );
        assert!(eventbrite::connector(hostile).is_err());
    }
    assert!(eventbrite::validate_organization_id(ORGANIZATION).is_ok());

    stub.assert_satisfied();
}

/// `eventbrite_error_map`: every documented status and error name reaches
/// exactly one closed class, and neither the token nor Eventbrite's prose
/// crosses the boundary.
#[tokio::test]
async fn eventbrite_error_map() {
    let connector = connector();
    let documented = [
        (400, "ARGUMENTS_ERROR", ConnectorErrorClass::Validation),
        (401, "INVALID_AUTH", ConnectorErrorClass::Authentication),
        (403, "NOT_AUTHORIZED", ConnectorErrorClass::Authentication),
        (404, "NOT_FOUND", ConnectorErrorClass::Permanent),
        (429, "HIT_RATE_LIMIT", ConnectorErrorClass::Http429),
        (500, "INTERNAL_ERROR", ConnectorErrorClass::Http5xx),
        (503, "INTERNAL_ERROR", ConnectorErrorClass::Http5xx),
        (418, "UNKNOWN", ConnectorErrorClass::Permanent),
    ];

    for (status, error, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", "/v3/users/me/").respond_json(
            status,
            json!({
                "status_code": status,
                "error": error,
                "error_description": format!("You do not have permission for {SECRET_SENTINEL}"),
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, &connector, "user.me", json!({})))
            .await
            .expect("the stub answers");

        let failure = eventbrite::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} / {error}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "do not have permission"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // The error *name* decides where the status alone would not: Eventbrite
    // publishes `HIT_RATE_LIMIT` and this map reads it.
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/users/me/").respond_json(
        403,
        json!({ "status_code": 403, "error": "HIT_RATE_LIMIT",
                "error_description": "Rate limit exceeded" }),
    )])
    .await;
    let response = stub
        .send(render(&stub, &connector, "user.me", json!({})))
        .await
        .expect("the stub answers");
    assert_eq!(
        eventbrite::error_map().classify_response(&response).class(),
        ConnectorErrorClass::Http429,
        "the published error name outranks the status it arrives with"
    );
    stub.assert_satisfied();
}

/// `eventbrite_rate_limit_is_classified`: the documented throttle reaches
/// `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn eventbrite_rate_limit_is_classified() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v3/users/me/")
            .respond_header("retry-after", "60")
            .respond_json(
                429,
                json!({ "status_code": 429, "error": "HIT_RATE_LIMIT",
                        "error_description": "Rate limit exceeded" }),
            ),
        Expectation::new("GET", "/v3/users/me/")
            .respond_header("retry-after", "604800")
            .respond_json(
                429,
                json!({ "status_code": 429, "error": "HIT_RATE_LIMIT",
                        "error_description": "Rate limit exceeded" }),
            ),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, &connector, "user.me", json!({})))
            .await
            .expect("the stub answers");
        failures.push(eventbrite::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(60)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `eventbrite_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is Eventbrite's own token, it is spent as a query value, the
/// walk ends where the key is absent, and it makes exactly the number of
/// requests the plan declares.
#[tokio::test]
async fn eventbrite_cursor_is_opaque_and_bounded() {
    let connector = connector();
    let plan = eventbrite::pagination("attendee.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    let attendees = format!("/v3/events/{EVENT_ID}/attendees/");
    let filters = "status=attending&changed_since=2026%2D08%2D01T00%3A00%3A00Z";

    let stub = ProviderStub::start([
        Expectation::new("GET", &attendees)
            .query(filters)
            .respond_json(
                200,
                json!({
                    "attendees": [{ "id": "1" }],
                    "pagination": {
                        "object_count": 2, "page_number": 1, "page_size": 50,
                        "page_count": 2, "has_more_items": true,
                        "continuation": "dGhpcyBpcyBwYWdlIDE",
                    },
                }),
            ),
        // "When all records have been retrieved, the continuation key will not
        // be present in the response."
        Expectation::new("GET", &attendees)
            .query(&format!("{filters}&continuation=dGhpcyBpcyBwYWdlIDE"))
            .respond_json(
                200,
                json!({
                    "attendees": [{ "id": "2" }],
                    "pagination": { "has_more_items": false },
                }),
            ),
    ])
    .await;

    let collected = plan
        .collect(
            render(
                &stub,
                &connector,
                "attendee.list",
                json!({
                    "event_id": EVENT_ID, "status": "attending",
                    "changed_since": "2026-08-01T00:00:00Z",
                }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk spends one continuation and stops where the key stops");
    assert_eq!(collected, vec![json!({ "id": "1" }), json!({ "id": "2" })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `eventbrite_pagination_is_bounded`: the declared plan terminates under every
/// budget, and no write or single-record read declares one.
#[tokio::test]
async fn eventbrite_pagination_is_bounded() {
    let connector = connector();
    let plan = eventbrite::pagination("event.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &organization_events()).respond_json(
                200,
                json!({
                    "events": [{ "id": "1" }, { "id": "2" }],
                    "pagination": { "continuation": "endless", "has_more_items": true },
                }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    &connector,
                    "event.list",
                    json!({ "status": "live", "order_by": "start_asc" }),
                ),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    for id in ["user.me", "event.get", "event.create", "event.update"] {
        assert!(
            eventbrite::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    for (id, items) in [
        ("event.list", "/events"),
        ("attendee.list", "/attendees"),
        ("order.list", "/orders"),
    ] {
        assert_eq!(
            eventbrite::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            items,
            "{id}"
        );
    }
}

/// `eventbrite_effects_are_classified`: the create is `AtMostOnce` on a recorded
/// absence and a named consequence, the `POST` partial update stays unreachable,
/// and every read is a read.
#[test]
fn eventbrite_effects_are_classified() {
    let connector = connector();
    let expected = [
        ("user.me", EffectClass::ReadOnly),
        ("event.get", EffectClass::ReadOnly),
        ("event.list", EffectClass::ReadOnly),
        ("event.create", EffectClass::AtMostOnce),
        ("event.update", EffectClass::InventoryOnly),
        ("attendee.list", EffectClass::ReadOnly),
        ("order.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(&connector, id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation.idempotency_binding().is_none(), "{id}");
    }

    assert_eq!(
        connector.admit_operation("event.update"),
        Err(OperationRejection::InventoryOnly)
    );
    let reason = operation(&connector, "event.update")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records its reason")
        .to_owned();
    assert!(reason.contains("PUT and DELETE only"), "{reason}");
    assert!(reason.contains("no consequence to record"), "{reason}");

    let evidence = operation(&connector, "event.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on")
        .clone();
    assert!(evidence.searched_documentation().contains("rate limit"));
    assert!(evidence.repeat_produces().contains("a second event"));
}

/// `eventbrite_output_contract`: the declared pointers read Eventbrite's own
/// objects, and its multipart strings and instants keep the shape it publishes
/// them in.
#[test]
fn eventbrite_output_contract() {
    let connector = connector();
    let get = operation(&connector, "event.get");
    let decoded = get
        .decode_response(
            200,
            &serde_json::to_vec(&event()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(decoded.get("id"), Some(&json!(EVENT_ID)));
    // ADR 071: a name is an object of `text` and `html`, and an instant is an
    // object of `timezone`, `local` and `utc`. The contract carries both whole.
    assert_eq!(
        decoded.get("name"),
        Some(&json!({ "text": "Donat Day", "html": "<p>Donat Day</p>" }))
    );
    assert_eq!(
        decoded
            .get("start")
            .and_then(|start| start.get("utc"))
            .cloned(),
        Some(json!("2026-09-01T16:00:00Z"))
    );

    assert_eq!(
        get.decode_response(200, br#"{"id":"987654321098"}"#)
            .expect("only the identity is required")
            .get("id"),
        Some(&json!(EVENT_ID))
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":987654321098}"#)
            .expect_err("an Eventbrite id is a string")
            .class(),
        ConnectorErrorClass::Validation
    );

    // A collection is the envelope Eventbrite publishes, with its continuation
    // carried as data.
    let list = operation(&connector, "event.list")
        .decode_response(
            200,
            br#"{"events":[{"id":"1"}],"pagination":{"has_more_items":false}}"#,
        )
        .expect("the declared contract is satisfied");
    assert_eq!(list.get("events"), Some(&json!([{ "id": "1" }])));
    assert_eq!(
        list.get("pagination"),
        Some(&json!({ "has_more_items": false }))
    );
}
