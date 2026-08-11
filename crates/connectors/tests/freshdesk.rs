//! Freshdesk connector proofs (spec 023 §4), against the SDK's local provider
//! stub.

use std::time::Duration;

use base64::Engine;
use donat_connectors::providers::freshdesk;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const TICKET_ID: i64 = 20;
const CONTACT_ID: i64 = 7;

fn operation(id: &str) -> &'static Operation {
    freshdesk::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the freshdesk declaration publishes {id}"))
}

/// The exact wire form Freshdesk publishes: `-u apikey:X`, base64-encoded.
fn expected_authorization() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{SECRET_SENTINEL}:X"))
    )
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::basic_secret_username("X")
        .expect("the declared dummy password is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn ticket() -> JsonValue {
    json!({
        "id": TICKET_ID,
        "subject": "Printer offline",
        "status": 2,
        "priority": 1,
        "requester_id": 1,
        "responder_id": null,
        "created_at": "2026-08-01T11:56:51Z",
        "updated_at": "2026-08-02T11:56:51Z",
    })
}

fn contact() -> JsonValue {
    json!({
        "id": CONTACT_ID,
        "name": "Joe",
        "email": "joe@example.com",
        "phone": null,
        "active": false,
        "created_at": "2026-08-01T11:56:51Z",
        "updated_at": "2026-08-02T11:56:51Z",
    })
}

fn conversation() -> JsonValue {
    json!({
        "id": 5,
        "body_text": "Please reply as soon as possible.",
        "private": true,
        "incoming": false,
        "user_id": 1,
        "ticket_id": TICKET_ID,
        "created_at": "2026-08-02T11:56:51Z",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("ticket.get", json!({ "ticket_id": TICKET_ID })),
        (
            "ticket.list",
            json!({ "updated_since": "2026-01-01T00:00:00Z" }),
        ),
        (
            "ticket.search",
            json!({ "query": "\"priority:3\"", "page": 1 }),
        ),
        (
            "ticket.create",
            json!({
                "subject": "Printer offline", "description": "<div>help</div>",
                "email": "joe@example.com", "requester_id": null, "status": 2,
                "priority": 1, "type": null, "custom_fields": {},
            }),
        ),
        (
            "ticket.update",
            json!({ "ticket_id": TICKET_ID, "status": 3, "priority": 2,
                    "responder_id": null, "custom_fields": {} }),
        ),
        ("conversation.list", json!({ "ticket_id": TICKET_ID })),
        (
            "note.add",
            json!({ "ticket_id": TICKET_ID, "body": "<div>note</div>",
                    "private": true, "notify_emails": [] }),
        ),
        (
            "reply.add",
            json!({ "ticket_id": TICKET_ID, "body": "<div>reply</div>",
                    "cc_emails": [], "bcc_emails": [] }),
        ),
        ("contact.get", json!({ "contact_id": CONTACT_ID })),
        ("contact.list", json!({})),
        (
            "contact.create",
            json!({ "name": "Joe", "email": "joe@example.com", "phone": null,
                    "unique_external_id": null, "custom_fields": {} }),
        ),
        (
            "contact.update",
            json!({ "contact_id": CONTACT_ID, "name": "Joe", "phone": null,
                    "custom_fields": {} }),
        ),
    ]
}

/// `freshdesk_request_shape`: exact method, path, query, headers, and body for
/// every operation.
#[tokio::test]
async fn freshdesk_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}"))
            .query("")
            .header("authorization", &expected_authorization())
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, ticket()),
        Expectation::new("GET", "/api/v2/tickets")
            .query("updated_since=2026%2D01%2D01T00%3A00%3A00Z&per_page=100")
            .respond_json(200, json!([ticket()])),
        Expectation::new("GET", "/api/v2/search/tickets")
            .query("query=%22priority%3A3%22&page=1")
            .respond_json(200, json!({ "total": 1, "results": [ticket()] })),
        Expectation::new("POST", "/api/v2/tickets")
            .json_body(json!({
                "subject": "Printer offline", "description": "<div>help</div>",
                "email": "joe@example.com", "requester_id": null, "status": 2,
                "priority": 1, "type": null, "custom_fields": {},
            }))
            .respond_json(201, ticket()),
        Expectation::new("PUT", &format!("/api/v2/tickets/{TICKET_ID}"))
            .json_body(json!({ "status": 3, "priority": 2, "responder_id": null,
                               "custom_fields": {} }))
            .respond_json(200, ticket()),
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}/conversations"))
            .query("per_page=100")
            .respond_json(200, json!([conversation()])),
        Expectation::new("POST", &format!("/api/v2/tickets/{TICKET_ID}/notes"))
            .json_body(json!({ "body": "<div>note</div>", "private": true,
                               "notify_emails": [] }))
            .respond_json(201, conversation()),
        Expectation::new("POST", &format!("/api/v2/tickets/{TICKET_ID}/reply"))
            .json_body(json!({ "body": "<div>reply</div>", "cc_emails": [], "bcc_emails": [] }))
            .respond_json(201, conversation()),
        Expectation::new("GET", &format!("/api/v2/contacts/{CONTACT_ID}"))
            .query("")
            .respond_json(200, contact()),
        Expectation::new("GET", "/api/v2/contacts")
            .query("per_page=100")
            .respond_json(200, json!([contact()])),
        Expectation::new("POST", "/api/v2/contacts")
            .json_body(
                json!({ "name": "Joe", "email": "joe@example.com", "phone": null,
                               "unique_external_id": null, "custom_fields": {} }),
            )
            .respond_json(201, contact()),
        Expectation::new("PUT", &format!("/api/v2/contacts/{CONTACT_ID}"))
            .json_body(json!({ "name": "Joe", "phone": null, "custom_fields": {} }))
            .respond_json(200, contact()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v2/"),
            "{id} renders a published Freshdesk path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `freshdesk_auth_is_applied`: the API key reaches the wire as the Basic
/// *username* Freshdesk publishes, with the dummy password, and appears nowhere
/// else.
#[tokio::test]
async fn freshdesk_auth_is_applied() {
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
    // The key is the username half, which is the whole point of this plan: a
    // declaration that carried it would publish a secret.
    assert_eq!(
        applied.to_str().expect("a base64 header is ASCII"),
        expected_authorization()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(
        !format!("{:?}", freshdesk::connector().credential()).contains(SECRET_SENTINEL),
        "the declaration carries the dummy password, never the key"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", freshdesk::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `freshdesk_host_comes_only_from_deploy_time_configuration` (spec 023 §4 proof
/// 1): input, a provider body, and a continuation each fail to move the host.
#[tokio::test]
async fn freshdesk_host_comes_only_from_deploy_time_configuration() {
    let specification = freshdesk::connector().origin();
    assert_eq!(specification.host_variable(), Some(freshdesk::DOMAIN));

    let origin = freshdesk::connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            freshdesk::DOMAIN,
            "acme-support",
        )]))
        .expect("a configured domain resolves");
    assert_eq!(
        origin.as_url().as_str(),
        "https://acme-support.freshdesk.com/"
    );

    // 1. Operation input. A path value that spells another authority stays one
    //    percent-encoded segment on the configured host.
    let request = operation("ticket.search")
        .plan_request(
            &origin,
            &json!({ "query": "https://attacker.invalid/api/v2/tickets", "page": 1 }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("acme-support.freshdesk.com"));
    assert_eq!(request.url().scheme(), "https");
    assert_eq!(request.url().path(), "/api/v2/search/tickets");

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("ticket.get")
        .extract_output(&json!({ "id": TICKET_ID, "subject": "https://attacker.invalid" }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("subject"),
        Some(&json!("https://attacker.invalid"))
    );

    // 3. A `link` continuation to another origin is refused rather than
    //    followed, on a templated origin exactly as on a fixed one.
    let stub = ProviderStub::start([Expectation::new("GET", "/api/v2/tickets")
        .respond_header(
            "link",
            "<https://attacker.invalid/api/v2/tickets?page=2>; rel=\"next\"",
        )
        .respond_json(200, json!([]))])
    .await;
    let failure = freshdesk::pagination("ticket.list")
        .expect("ticket.list declares a plan")
        .collect(
            render(
                &stub,
                "ticket.list",
                json!({ "updated_since": "2026-01-01T00:00:00Z" }),
            ),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    stub.assert_satisfied();

    // And the configuration itself admits one host label and nothing else,
    // which is what Freshdesk's own "Works only via Freshdesk domains and not
    // via custom CNAMEs" describes.
    for hostile in [
        "acme.freshdesk.com",
        "acme/../evil",
        "acme:8080",
        "user@acme",
        "",
        "-acme",
        "ACME",
    ] {
        assert!(
            freshdesk::connector()
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    freshdesk::DOMAIN,
                    hostile
                )]))
                .is_err(),
            "`{hostile}` is not one host label"
        );
    }
}

/// `freshdesk_error_map`: every documented status and code reaches exactly one
/// closed class, and none of Freshdesk's prose crosses the boundary.
#[tokio::test]
async fn freshdesk_error_map() {
    let documented = [
        (400, "missing_field", ConnectorErrorClass::Validation),
        (400, "invalid_value", ConnectorErrorClass::Validation),
        (400, "datatype_mismatch", ConnectorErrorClass::Validation),
        (400, "invalid_field", ConnectorErrorClass::Validation),
        (400, "invalid_json", ConnectorErrorClass::Validation),
        (
            401,
            "invalid_credentials",
            ConnectorErrorClass::Authentication,
        ),
        (403, "access_denied", ConnectorErrorClass::Authentication),
        // A code Freshdesk publishes on a status this map answers differently:
        // the ordered map puts the code first, which is what makes a duplicate
        // contact permanent rather than a retryable validation failure.
        (409, "duplicate_value", ConnectorErrorClass::Permanent),
        (404, "not_a_published_code", ConnectorErrorClass::Permanent),
        (405, "not_a_published_code", ConnectorErrorClass::Permanent),
        (406, "not_a_published_code", ConnectorErrorClass::Permanent),
        (415, "not_a_published_code", ConnectorErrorClass::Permanent),
        (429, "not_a_published_code", ConnectorErrorClass::Http429),
        (500, "not_a_published_code", ConnectorErrorClass::Http5xx),
        (418, "not_a_published_code", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}")).respond_json(
                    status,
                    json!({
                        "description": "Validation failed",
                        "errors": [{
                            "field": "email",
                            "message": format!("acme-support shard {SECRET_SENTINEL}"),
                            "code": code,
                        }],
                    }),
                ),
            ])
            .await;
        let response = stub
            .send(render(
                &stub,
                "ticket.get",
                json!({ "ticket_id": TICKET_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = freshdesk::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-support", "Validation failed"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `freshdesk_rate_limit_is_classified`: the documented rate-limit response
/// reaches `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn freshdesk_rate_limit_is_classified() {
    let limited = json!({ "description": "Rate limit exceeded", "errors": [] });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/tickets/{TICKET_ID}"))
            .respond_header("retry-after", "34")
            .respond_header("x-ratelimit-total", "700")
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
        failures.push(freshdesk::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(34)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `freshdesk_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is Freshdesk's own `link` header, it is followed as a
/// destination on this origin, and the walk makes exactly the number of requests
/// the plan declares.
#[tokio::test]
async fn freshdesk_cursor_is_opaque_and_bounded() {
    let plan = freshdesk::pagination("ticket.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v2/tickets")
            .query("updated_since=2026%2D01%2D01T00%3A00%3A00Z&per_page=100")
            .respond_header(
                "link",
                "</api/v2/tickets?page=2&per_page=100>; rel=\"next\"",
            )
            .respond_json(200, json!([{ "id": 1 }])),
        // "If you have reached the last page of objects, then the link header
        // will not be set", which is the absence the plan ends on.
        Expectation::new("GET", "/api/v2/tickets")
            .query("page=2&per_page=100")
            .respond_json(200, json!([{ "id": 2 }])),
    ])
    .await;

    let tickets = plan
        .collect(
            render(
                &stub,
                "ticket.list",
                json!({ "updated_since": "2026-01-01T00:00:00Z" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one link and stops where the header stops");
    assert_eq!(tickets, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `freshdesk_pagination_is_bounded`: the declared plan terminates and respects
/// the call, page, item, and byte budgets, and the search — which Freshdesk caps
/// at ten pages and publishes no `link` header for — declares none.
#[tokio::test]
async fn freshdesk_pagination_is_bounded() {
    let plan = freshdesk::pagination("ticket.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/v2/tickets")
                .respond_header("link", "</api/v2/tickets?page=9>; rel=\"next\"")
                .respond_json(200, json!([{ "id": 1 }, { "id": 2 }]))
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "ticket.list",
                    json!({ "updated_since": "2026-01-01T00:00:00Z" }),
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

    for id in [
        "ticket.get",
        "ticket.search",
        "ticket.create",
        "ticket.update",
        "note.add",
        "reply.add",
        "contact.get",
        "contact.create",
        "contact.update",
    ] {
        assert!(
            freshdesk::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    // Every walked collection is a bare array at the document root.
    for id in ["ticket.list", "contact.list", "conversation.list"] {
        assert_eq!(
            freshdesk::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            ""
        );
    }
}

/// `freshdesk_effects_are_classified`: every operation carries a class, and the
/// two updates are unreachable from a Process.
#[test]
fn freshdesk_effects_are_classified() {
    let connector = freshdesk::connector();
    let expected = [
        ("ticket.get", EffectClass::ReadOnly),
        ("ticket.list", EffectClass::ReadOnly),
        ("ticket.search", EffectClass::ReadOnly),
        ("ticket.create", EffectClass::AtMostOnce),
        ("ticket.update", EffectClass::InventoryOnly),
        ("conversation.list", EffectClass::ReadOnly),
        ("note.add", EffectClass::AtMostOnce),
        ("reply.add", EffectClass::AtMostOnce),
        ("contact.get", EffectClass::ReadOnly),
        ("contact.list", EffectClass::ReadOnly),
        ("contact.create", EffectClass::AtMostOnce),
        ("contact.update", EffectClass::InventoryOnly),
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
        connector.admit_operation("ticket.update"),
        Err(OperationRejection::InventoryOnly)
    );

    // The contact create records both branches Freshdesk publishes for a
    // repeat, which is what an operator accepts when they write the opt-in.
    let evidence = operation("contact.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.repeat_produces().contains("duplicate_value"));
    assert!(evidence.searched_documentation().contains("idempot"));
}

/// `freshdesk_output_contract`: the declared pointers read Freshdesk's own
/// objects, and a numeric status stays a number.
#[test]
fn freshdesk_output_contract() {
    let get = operation("ticket.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&ticket()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": TICKET_ID, "subject": "Printer offline", "status": 2, "priority": 1,
            "requester_id": 1, "responder_id": null,
            "created_at": "2026-08-01T11:56:51Z", "updated_at": "2026-08-02T11:56:51Z",
        })
    );
    // "status | number" — the display name is not what Freshdesk publishes.
    assert_eq!(
        get.decode_response(200, br#"{"id":20,"status":"Open"}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // "Blank fields are included as null instead of being omitted", so only the
    // identity is demanded.
    assert_eq!(
        get.decode_response(200, br#"{"id":20}"#)
            .expect("only the identity is required")
            .get("id"),
        Some(&json!(20))
    );
    assert_eq!(
        get.decode_response(200, br#"{"subject":"no id"}"#)
            .expect_err("an object with no id is not a ticket")
            .class(),
        ConnectorErrorClass::Validation
    );

    // A search publishes the envelope Freshdesk answers it with, not a bare
    // array.
    assert_eq!(
        operation("ticket.search")
            .decode_response(200, br#"{"total":49,"results":[{"id":20}]}"#)
            .expect("the declared contract is satisfied"),
        json!({ "results": [{ "id": 20 }], "total": 49 })
    );
    // And a list publishes the whole document, because the collection is one.
    assert_eq!(
        operation("ticket.list")
            .decode_response(200, br#"[{"id":20}]"#)
            .expect("a bare array is the whole output"),
        json!([{ "id": 20 }])
    );
}
