//! Zendesk connector proofs (spec 023 §4), against the SDK's local provider
//! stub.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use donat_connectors::providers::zendesk;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    Operation, OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const EMAIL: &str = "integrations@example.test";
const TICKET_ID: i64 = 35_436;
const USER_ID: i64 = 1;
/// The durable activity's own stable key, which is what fills the published
/// idempotency header.
const ACTIVITY_KEY: &str = "activity-00042";

fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> =
        LazyLock::new(|| zendesk::connector(EMAIL).expect("a valid account address declares"));
    &CONNECTOR
}

fn operation(id: &str) -> &'static Operation {
    connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the zendesk declaration publishes {id}"))
}

/// The exact wire form Zendesk publishes: `{email_address}/token:{api_token}`,
/// base64-encoded.
fn expected_authorization() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{EMAIL}/token:{SECRET_SENTINEL}"))
    )
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_keyed_request(&stub.origin(), &input, ACTIVITY_KEY)
        .expect("the declared request renders");
    AuthPlan::basic(&format!("{EMAIL}/token"))
        .expect("the published username form is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn ticket() -> JsonValue {
    json!({ "ticket": {
        "id": TICKET_ID,
        "subject": "Printer offline",
        "status": "open",
        "priority": "normal",
        "requester_id": USER_ID,
        "url": "https://acme.zendesk.com/api/v2/tickets/35436.json",
        "created_at": "2026-08-01T11:56:51Z",
        "updated_at": "2026-08-02T11:56:51Z",
    }})
}

fn user() -> JsonValue {
    json!({ "user": {
        "id": USER_ID,
        "name": "Joe",
        "email": "joe@example.com",
        "role": "end-user",
        "external_id": null,
        "created_at": "2026-08-01T11:56:51Z",
        "updated_at": "2026-08-02T11:56:51Z",
    }})
}

fn collection(key: &str) -> JsonValue {
    json!({ key: [], "count": 0, "next_page": null, "previous_page": null })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("ticket.get", json!({ "ticket_id": TICKET_ID })),
        ("ticket.list", json!({})),
        (
            "ticket.search",
            json!({ "query": "type:ticket", "page": 1 }),
        ),
        (
            "ticket.create",
            json!({
                "subject": "Printer offline",
                "comment": { "body": "My printer is offline." },
                "requester_id": USER_ID, "priority": "normal",
                "tags": [], "external_id": null,
            }),
        ),
        (
            "ticket.update",
            json!({ "ticket_id": TICKET_ID, "status": "solved", "priority": "normal",
                    "assignee_id": null, "tags": [] }),
        ),
        (
            "comment.add",
            json!({ "ticket_id": TICKET_ID,
                    "comment": { "body": "on it", "public": true } }),
        ),
        ("comment.list", json!({ "ticket_id": TICKET_ID })),
        ("user.get", json!({ "user_id": USER_ID })),
        ("user.list", json!({})),
        (
            "user.create",
            json!({ "name": "Joe", "email": "joe@example.com",
                    "role": "end-user", "external_id": null }),
        ),
        (
            "user.update",
            json!({ "user_id": USER_ID, "name": "Joe", "role": "end-user",
                    "external_id": null, "user_fields": {} }),
        ),
        (
            "user.create_or_update",
            json!({ "name": "Joe", "email": "joe@example.com",
                    "role": "end-user", "external_id": "ian1" }),
        ),
    ]
}

/// `zendesk_request_shape`: exact method, path, query, headers, and body for
/// every operation, including the comment append that shares the update's route.
#[tokio::test]
async fn zendesk_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}"))
            .query("")
            .header("authorization", &expected_authorization())
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, ticket()),
        Expectation::new("GET", "/api/v2/tickets")
            .query("per_page=100")
            .respond_json(200, collection("tickets")),
        Expectation::new("GET", "/api/v2/search")
            .query("query=type%3Aticket&page=1&per_page=100")
            .respond_json(200, json!({ "results": [], "count": 0, "next_page": null })),
        Expectation::new("POST", "/api/v2/tickets")
            .header("idempotency-key", ACTIVITY_KEY)
            .json_body(json!({ "ticket": {
                "subject": "Printer offline",
                "comment": { "body": "My printer is offline." },
                "requester_id": USER_ID, "priority": "normal",
                "tags": [], "external_id": null,
            }}))
            .respond_json(201, ticket()),
        Expectation::new("PUT", &format!("/api/v2/tickets/{TICKET_ID}"))
            .without_header("idempotency-key")
            .json_body(json!({ "ticket": {
                "status": "solved", "priority": "normal",
                "assignee_id": null, "tags": [],
            }}))
            .respond_json(200, ticket()),
        Expectation::new("PUT", &format!("/api/v2/tickets/{TICKET_ID}"))
            .json_body(json!({ "ticket": { "comment": { "body": "on it", "public": true } } }))
            .respond_json(200, ticket()),
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}/comments"))
            .query("per_page=100")
            .respond_json(200, collection("comments")),
        Expectation::new("GET", &format!("/api/v2/users/{USER_ID}"))
            .query("")
            .respond_json(200, user()),
        Expectation::new("GET", "/api/v2/users")
            .query("per_page=100")
            .respond_json(200, collection("users")),
        Expectation::new("POST", "/api/v2/users")
            .json_body(json!({ "user": { "name": "Joe", "email": "joe@example.com",
                                         "role": "end-user", "external_id": null } }))
            .respond_json(201, user()),
        Expectation::new("PUT", &format!("/api/v2/users/{USER_ID}"))
            .json_body(json!({ "user": { "name": "Joe", "role": "end-user",
                                         "external_id": null, "user_fields": {} } }))
            .respond_json(200, user()),
        Expectation::new("POST", "/api/v2/users/create_or_update")
            .json_body(json!({ "user": { "name": "Joe", "email": "joe@example.com",
                                         "role": "end-user", "external_id": "ian1" } }))
            .respond_json(200, user()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v2/"),
            "{id} renders a published Zendesk path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `zendesk_auth_is_applied`: the API token reaches the wire as the Basic
/// password under the `{email}/token` username Zendesk publishes, and appears
/// nowhere else.
#[tokio::test]
async fn zendesk_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}"))
                .header("authorization", &expected_authorization())
                .respond_json(200, ticket()),
        ])
        .await;

    let request = render(&stub, "ticket.get", json!({ "ticket_id": TICKET_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("a base64 header is ASCII"),
        expected_authorization()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        connector().credential(),
        zendesk::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();

    // The account address is deploy-time material with a published grammar, and
    // the `/token` suffix is this connector's to add rather than a deployment's.
    for hostile in [
        "not-an-email",
        "",
        "user@host",
        "a b@example.test",
        "u@e.test/token",
    ] {
        assert!(zendesk::connector(hostile).is_err(), "`{hostile}`");
    }
    assert!(zendesk::declaration_shape().is_ok());
}

/// `zendesk_host_comes_only_from_deploy_time_configuration` (spec 023 §4 proof
/// 1): input, a provider body, and a continuation each fail to move the host.
#[tokio::test]
async fn zendesk_host_comes_only_from_deploy_time_configuration() {
    assert_eq!(
        connector().origin().host_variable(),
        Some(zendesk::SUBDOMAIN)
    );

    let origin = connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            zendesk::SUBDOMAIN,
            "acme",
        )]))
        .expect("a configured subdomain resolves");
    assert_eq!(origin.as_url().as_str(), "https://acme.zendesk.com/");

    // 1. Operation input.
    let request = operation("ticket.search")
        .plan_request(
            &origin,
            &json!({ "query": "https://attacker.invalid/api/v2/tickets", "page": 1 }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("acme.zendesk.com"));
    assert_eq!(request.url().scheme(), "https");
    assert_eq!(request.url().path(), "/api/v2/search");

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("ticket.get")
        .extract_output(&json!({ "ticket": { "id": TICKET_ID,
                                             "url": "https://attacker.invalid/tickets/1" } }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("url"),
        Some(&json!("https://attacker.invalid/tickets/1"))
    );

    // 3. A `next_page` continuation to another origin is refused rather than
    //    followed — the body value is a destination, and it is checked.
    let stub = ProviderStub::start([Expectation::new("GET", "/api/v2/tickets").respond_json(
        200,
        json!({ "tickets": [], "next_page": "https://attacker.invalid/api/v2/tickets?page=2" }),
    )])
    .await;
    let failure = zendesk::pagination("ticket.list")
        .expect("ticket.list declares a plan")
        .collect(
            render(&stub, "ticket.list", json!({})),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    stub.assert_satisfied();

    for hostile in [
        "acme.zendesk.com",
        "acme/../evil",
        "acme:8080",
        "",
        "-acme",
        "ACME",
    ] {
        assert!(
            connector()
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    zendesk::SUBDOMAIN,
                    hostile
                )]))
                .is_err(),
            "`{hostile}` is not one host label"
        );
    }
}

/// `zendesk_error_map`: every documented status reaches exactly one closed
/// class, and none of Zendesk's prose crosses the boundary.
#[tokio::test]
async fn zendesk_error_map() {
    let documented = [
        (400, "RecordInvalid", ConnectorErrorClass::Validation),
        (
            401,
            "CouldNotAuthenticate",
            ConnectorErrorClass::Authentication,
        ),
        (403, "Forbidden", ConnectorErrorClass::Authentication),
        (404, "RecordNotFound", ConnectorErrorClass::Permanent),
        (405, "MethodNotAllowed", ConnectorErrorClass::Permanent),
        (409, "Conflict", ConnectorErrorClass::Permanent),
        (422, "RecordInvalid", ConnectorErrorClass::Validation),
        (429, "TooManyRequests", ConnectorErrorClass::Http429),
        (500, "InternalError", ConnectorErrorClass::Http5xx),
        (503, "ServiceUnavailable", ConnectorErrorClass::Http5xx),
        (418, "not_a_published_error", ConnectorErrorClass::Permanent),
        // The one code rule: a body that no longer matches its own key is a
        // request this deployment changed between attempts, not a retry.
        (
            400,
            "IdempotentRequestError",
            ConnectorErrorClass::Permanent,
        ),
    ];

    for (status, error, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v2/tickets/{TICKET_ID}"),
        )
        .respond_json(
            status,
            json!({
                "error": error,
                "description": format!("acme shard {SECRET_SENTINEL}"),
                "details": { "value": [{ "type": "blank", "description": "can't be blank" }] },
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "ticket.get",
                json!({ "ticket_id": TICKET_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = zendesk::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} error {error}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme", "can't be blank"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `zendesk_rate_limit_is_classified`: the documented rate-limit response
/// reaches `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn zendesk_rate_limit_is_classified() {
    let limited = json!({ "error": "TooManyRequests", "description": "Too many requests" });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}"))
            .respond_header("retry-after", "93")
            .respond_header("x-rate-limit", "700")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "ticket.get",
                json!({ "ticket_id": TICKET_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(zendesk::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(93)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `zendesk_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is spent exactly as Zendesk publishes it, the walk stops on the
/// `null` Zendesk documents as its end, and it makes exactly the number of
/// requests the plan declares.
#[tokio::test]
async fn zendesk_cursor_is_opaque_and_bounded() {
    let plan = zendesk::pagination("ticket.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v2/tickets")
            .query("per_page=100")
            .respond_json(
                200,
                json!({ "tickets": [{ "id": 1 }],
                        "next_page": "/api/v2/tickets.json?page=2&per_page=100" }),
            ),
        // "Stop paging when the `next_page` attribute is null."
        Expectation::new("GET", "/api/v2/tickets.json")
            .query("page=2&per_page=100")
            .respond_json(200, json!({ "tickets": [{ "id": 2 }], "next_page": null })),
    ])
    .await;

    let tickets = plan
        .collect(
            render(&stub, "ticket.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one continuation and stops on the null one");
    assert_eq!(tickets, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `zendesk_pagination_is_bounded`: the declared plans terminate and respect the
/// call, page, item, and byte budgets, and the search — which Zendesk publishes
/// as duplicate-prone under offset paging — declares none.
#[tokio::test]
async fn zendesk_pagination_is_bounded() {
    let plan = zendesk::pagination("ticket.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/v2/tickets").respond_json(
                200,
                json!({ "tickets": [{ "id": 1 }, { "id": 2 }],
                        "next_page": "/api/v2/tickets?page=9" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "ticket.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    for (id, items) in [
        ("ticket.list", "/tickets"),
        ("user.list", "/users"),
        ("comment.list", "/comments"),
    ] {
        assert_eq!(
            zendesk::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            items
        );
    }
    for id in [
        "ticket.get",
        "ticket.search",
        "ticket.create",
        "ticket.update",
        "comment.add",
        "user.get",
        "user.create",
        "user.update",
        "user.create_or_update",
    ] {
        assert!(
            zendesk::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `zendesk_effects_are_classified`: every operation carries a class, the one
/// operation Zendesk publishes a key for binds it, and the two operations
/// Zendesk documents differently are refused for their own reasons.
#[test]
fn zendesk_effects_are_classified() {
    let expected = [
        ("ticket.get", EffectClass::ReadOnly),
        ("ticket.list", EffectClass::ReadOnly),
        ("ticket.search", EffectClass::ReadOnly),
        ("ticket.create", EffectClass::ProviderIdempotentExplicitKey),
        ("ticket.update", EffectClass::InventoryOnly),
        ("comment.add", EffectClass::AtMostOnce),
        ("comment.list", EffectClass::ReadOnly),
        ("user.get", EffectClass::ReadOnly),
        ("user.list", EffectClass::ReadOnly),
        ("user.create", EffectClass::AtMostOnce),
        ("user.update", EffectClass::InventoryOnly),
        ("user.create_or_update", EffectClass::InventoryOnly),
    ];
    assert_eq!(connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
    }
    assert_eq!(
        connector().admit_operation("user.create_or_update"),
        Err(OperationRejection::InventoryOnly)
    );

    // The one explicit key in this batch carries every piece of its evidence,
    // and the margin is strictly inside Zendesk's published two hours.
    let evidence = operation("ticket.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::explicit_key_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert_eq!(
        evidence.retention().minimum(),
        Duration::from_secs(2 * 60 * 60),
        "\"Keys expire after two hours.\""
    );
    assert!(evidence.retention().clock_safety_margin() < evidence.retention().minimum());
    assert!(evidence.citation().contains("Keys expire after two hours"));
    assert!(
        evidence.retention().scope().contains("one Zendesk account"),
        "the scope Zendesk does not publish in words is recorded as a reading"
    );
    assert_eq!(
        operation("ticket.create")
            .idempotency_binding()
            .and_then(donat_connectors::sdk::IdempotencyBinding::as_header)
            .map(reqwest::header::HeaderName::as_str),
        Some("idempotency-key")
    );
    // No other operation carries a binding, because Zendesk publishes the key
    // for exactly one.
    for (id, _) in expected {
        if id != "ticket.create" {
            assert!(operation(id).idempotency_binding().is_none(), "{id}");
        }
    }

    // The upsert is refused for the opposite reason to the update: the provider
    // documents it as repeat-safe, over a method the gate does not admit.
    assert!(
        operation("user.create_or_update")
            .effect()
            .and_then(donat_connectors::sdk::Effect::inventory_reason)
            .is_some_and(
                |reason| reason.contains("repeat-safe") && reason.contains("PUT and DELETE only")
            ),
        "the upsert records both the documented semantics and the reason the gate refuses them"
    );
}

/// `zendesk_idempotency_key_is_bound_only_where_it_was_admitted`: the durable
/// activity's stable key reaches the header its class declared, on that
/// operation and on no other.
#[test]
fn zendesk_idempotency_key_is_bound_only_where_it_was_admitted() {
    let origin = connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            zendesk::SUBDOMAIN,
            "acme",
        )]))
        .expect("a configured subdomain resolves");
    let input = json!({
        "subject": "Printer offline", "comment": { "body": "help" },
        "requester_id": USER_ID, "priority": "normal", "tags": [], "external_id": null,
    });

    let keyed = operation("ticket.create")
        .plan_keyed_request(&origin, &input, ACTIVITY_KEY)
        .expect("the declared request renders");
    assert_eq!(
        keyed
            .headers()
            .get(zendesk::IDEMPOTENCY_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(ACTIVITY_KEY)
    );
    // Two renders of one activity's key produce the same header, which is what
    // makes the retry a replay rather than a second ticket.
    assert_eq!(
        format!("{:?}", keyed.headers()),
        format!(
            "{:?}",
            operation("ticket.create")
                .plan_keyed_request(&origin, &input, ACTIVITY_KEY)
                .expect("the declared request renders")
                .headers()
        )
    );

    // An operation whose class binds nothing renders exactly as it always did.
    let unkeyed = operation("comment.add")
        .plan_keyed_request(
            &origin,
            &json!({ "ticket_id": TICKET_ID, "comment": { "body": "on it" } }),
            ACTIVITY_KEY,
        )
        .expect("the declared request renders");
    assert!(unkeyed.headers().get(zendesk::IDEMPOTENCY_HEADER).is_none());

    // A key that is not a header value fails the attempt rather than being
    // trimmed into one.
    assert!(
        operation("ticket.create")
            .plan_keyed_request(&origin, &input, "activity\r\nX-Injected: 1")
            .is_err()
    );
}

/// `zendesk_output_contract`: the declared pointers read Zendesk's own
/// envelopes, and a timestamp is the ISO 8601 string Zendesk publishes rather
/// than an epoch integer.
#[test]
fn zendesk_output_contract() {
    let get = operation("ticket.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&ticket()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": TICKET_ID, "subject": "Printer offline", "status": "open",
            "priority": "normal", "requester_id": USER_ID,
            "url": "https://acme.zendesk.com/api/v2/tickets/35436.json",
            "created_at": "2026-08-01T11:56:51Z", "updated_at": "2026-08-02T11:56:51Z",
        })
    );
    // "`created_at` | string" — "Time stamps use UTC time and their format is
    // ISO 8601."
    assert_eq!(
        get.decode_response(200, br#"{"ticket":{"id":1,"created_at":1571672154}}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"ticket":{"subject":"no id"}}"#)
            .expect_err("an envelope with no id is not a ticket")
            .class(),
        ConnectorErrorClass::Validation
    );

    // The update answers with an `audit` beside the ticket, and the declaration
    // reads only what it published.
    assert_eq!(
        operation("ticket.update")
            .decode_response(
                200,
                br#"{"audit":{"id":9},"ticket":{"id":35436,"status":"solved"}}"#,
            )
            .expect("the declared contract is satisfied")
            .get("status"),
        Some(&json!("solved"))
    );
}
