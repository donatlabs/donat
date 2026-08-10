//! Typeform connector proofs: Batch A's outbound half (spec 012 §3) and the
//! inbound half spec 013 adds, both against the SDK's local provider stub.  No
//! test reaches Typeform, and no test carries a real credential.

mod webhook_support;

use std::time::Duration;

use donat_connectors::providers::typeform;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const FORM_ID: &str = "abc123";
const RESPONSE_ID: &str = "01HDONATRESPONSE00000001";

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    typeform::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the typeform declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn form() -> JsonValue {
    json!({
        "id": FORM_ID,
        "title": "Order feedback",
        "fields": [],
        "settings": { "is_public": true },
    })
}

/// `typeform_request_shape`: exact method, path, query, headers, and body for
/// every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn typeform_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/forms/{FORM_ID}"))
            .query("")
            .no_body()
            .respond_json(200, form()),
        Expectation::new("GET", &format!("/forms/{FORM_ID}/responses"))
            .query("page_size=100")
            .no_body()
            .respond_json(
                200,
                json!({ "total_items": 0, "page_count": 1, "items": [] }),
            ),
        Expectation::new("DELETE", &format!("/forms/{FORM_ID}/responses"))
            .query(&format!("included_response_ids={RESPONSE_ID}"))
            .respond_bytes(200, Vec::new()),
        Expectation::new("GET", &format!("/forms/{FORM_ID}/webhooks"))
            .query("")
            .respond_json(200, json!({ "items": [] })),
        // A hostile form identifier stays one percent-encoded path segment.
        Expectation::new("GET", "/forms/%2E%2E%2Fforms%3Fpage%3D2%23y/webhooks")
            .respond_json(200, json!({ "items": [] })),
    ])
    .await;

    for (id, input) in [
        ("form.get", json!({ "form_id": FORM_ID })),
        ("response.list", json!({ "form_id": FORM_ID })),
        (
            "response.delete",
            json!({ "form_id": FORM_ID, "included_response_ids": RESPONSE_ID }),
        ),
        ("webhook.list", json!({ "form_id": FORM_ID })),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "webhook.list",
        json!({ "form_id": "../forms?page=2#y" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();

    // The forms collection carries its `page`/`page_size` pair from the
    // declared pagination plan rather than from input.
    let stub = ProviderStub::start([Expectation::new("GET", "/forms")
        .query("page=1&page_size=200")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .no_body()
        .respond_json(
            200,
            json!({ "total_items": 1, "page_count": 1, "items": [form()] }),
        )])
    .await;
    let plan = typeform::pagination("form.list").expect("form.list declares a plan");
    assert_eq!(
        plan.collect(
            render(&stub, "form.list", json!({})),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 32 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a short page is a complete walk")
        .len(),
        1
    );
    stub.assert_satisfied();
}

/// `typeform_auth_is_applied`: the personal access token reaches the wire as
/// `Authorization: Bearer <token>` and appears nowhere else.
#[tokio::test]
async fn typeform_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/forms/{FORM_ID}"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .without_header("x-api-key")
        .respond_json(200, form())])
    .await;

    let request = render(&stub, "form.get", json!({ "form_id": FORM_ID }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(
        !request.url().as_str().contains(SECRET_SENTINEL),
        "the token is not a query value"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = typeform::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        typeform::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `typeform_error_map`: the documented statuses and error codes each reach
/// exactly one of the eight closed classes, and Typeform's `description` prose
/// never crosses the boundary.
#[tokio::test]
async fn typeform_error_map() {
    let documented = [
        (400, "VALIDATION_ERROR", ConnectorErrorClass::Validation),
        (401, "UNAUTHORIZED", ConnectorErrorClass::Authentication),
        (402, "PAYMENT_REQUIRED", ConnectorErrorClass::Permanent),
        (
            403,
            "AUTHENTICATION_ERROR",
            ConnectorErrorClass::Authentication,
        ),
        (404, "NOT_EXISTING_ID", ConnectorErrorClass::Permanent),
        (405, "METHOD_NOT_ALLOWED", ConnectorErrorClass::Permanent),
        (429, "RATE_LIMIT", ConnectorErrorClass::Http429),
        (500, "SERVER_ERROR", ConnectorErrorClass::Http5xx),
        (503, "SERVICE_UNAVAILABLE", ConnectorErrorClass::Http5xx),
        // A documented code on a status the table does not name.
        (
            418,
            "INVALID_AUTHORIZATION",
            ConnectorErrorClass::Authentication,
        ),
        // Undocumented in both dimensions: the declared fallback answers.
        (418, "TEAPOT", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &format!("/forms/{FORM_ID}"))
            .respond_json(
                status,
                json!({
                    "code": code,
                    "description": format!("workspace acme on db-7.internal rejected token {SECRET_SENTINEL}"),
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "form.get", json!({ "form_id": FORM_ID })))
            .await
            .expect("the stub answers");

        let failure = typeform::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            operation("form.get")
                .decode_response(status, response.body())
                .is_err(),
            "status {status} is not a declared success"
        );

        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal", "acme", code] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // A `Retry-After` a provider sends is clamped to the SDK's ceiling.
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/forms/{FORM_ID}"))
        .respond_header("retry-after", "300000")
        .respond_json(
            429,
            json!({ "code": "RATE_LIMIT", "description": "slow down" }),
        )])
    .await;
    let response = stub
        .send(render(&stub, "form.get", json!({ "form_id": FORM_ID })))
        .await
        .expect("the stub answers");
    assert_eq!(
        typeform::error_map()
            .classify_response(&response)
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
    stub.assert_satisfied();
}

/// `typeform_pagination_is_bounded`: the declared one-based page walk
/// terminates and respects the budget, and the endpoints whose cursor the SDK
/// cannot express declare no plan at all.
#[tokio::test]
async fn typeform_pagination_is_bounded() {
    let plan = typeform::pagination("form.list").expect("form.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    fn page(count: usize, first: usize) -> JsonValue {
        json!({
            "total_items": 201,
            "page_count": 2,
            "items": (0..count).map(|index| json!({ "id": format!("form-{}", first + index) })).collect::<Vec<_>>(),
        })
    }

    // Typeform's `page` is one-based ("page (integer, default: 1)"), which is
    // exactly where the declared plan starts.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/forms")
            .query("page=1&page_size=200")
            .respond_json(200, page(200, 0)),
        Expectation::new("GET", "/forms")
            .query("page=2&page_size=200")
            .respond_json(200, page(1, 200)),
    ])
    .await;
    let forms = plan
        .collect(
            render(&stub, "form.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| {
                assert_eq!(
                    request.url().host_str(),
                    stub.origin().as_url().host_str(),
                    "a continuation never leaves the compiled origin"
                );
                stub.send(request)
            },
        )
        .await
        .expect("the declared plan walks both pages and stops");
    assert_eq!(forms.len(), 201);
    assert_eq!(forms[0], json!({ "id": "form-0" }));
    stub.assert_satisfied();

    // An endless provider exhausts each ceiling instead of looping, and the
    // page number is derived from the walk rather than from any provider value.
    for budget in [
        PaginationBudget::new(2, 8, 10_000, 1_024 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 10_000, 1_024 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 300, 1_024 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 10_000, 5_000, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start(
            (0..12).map(|_| Expectation::new("GET", "/forms").respond_json(200, page(200, 0))),
        )
        .await;
        let failure = plan
            .collect(
                render(&stub, "form.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // The responses endpoint publishes no top-level cursor, so it declares no
    // plan and its page size is part of the declaration.
    for id in [
        "response.list",
        "response.delete",
        "webhook.list",
        "form.get",
    ] {
        assert!(
            typeform::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/forms/{FORM_ID}/responses"))
                .query("page_size=100")
                .respond_json(
                    200,
                    json!({ "total_items": 5000, "page_count": 50, "items": [{ "token": "t1" }] }),
                ),
        ])
        .await;
    let request = render(&stub, "response.list", json!({ "form_id": FORM_ID }));
    assert_eq!(request.url().query(), Some("page_size=100"));
    let response = stub.send(request).await.expect("the stub answers");
    let page = operation("response.list")
        .decode_response(200, response.body())
        .expect("the declared contract is satisfied");
    assert_eq!(
        page.get("page_count").and_then(JsonValue::as_i64),
        Some(50),
        "the provider's own page count is output data, not a walk"
    );
    stub.assert_satisfied();
}

/// `typeform_effects_are_classified`: every operation carries a class, and the
/// one mutation is admitted on Typeform's own repeat-safety statement.
#[test]
fn typeform_effects_are_classified() {
    let connector = typeform::connector();
    let expected = [
        ("form.list", EffectClass::ReadOnly),
        ("form.get", EffectClass::ReadOnly),
        ("response.list", EffectClass::ReadOnly),
        (
            "response.delete",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        ("webhook.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert!(connector.admit_operation(id).is_ok(), "{id}");
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: Typeform publishes no idempotency key to bind"
        );
    }
    assert!(
        connector
            .operations()
            .iter()
            .all(|operation| operation.is_executable()),
        "this connector declares no create, so it has no inventory-only operation"
    );

    assert_eq!(
        connector.admit_operation("webhook.create"),
        Err(OperationRejection::Undeclared),
        "registering a webhook at the provider is out of spec 013's scope"
    );
}

/// Two identical deletes name the same responses and leave the same form, which
/// is the `NaturalMethod` evidence spec 010 §7 asks for.
#[tokio::test]
async fn typeform_two_identical_deletes_leave_one_result() {
    let stub = ProviderStub::start([
        Expectation::new("DELETE", &format!("/forms/{FORM_ID}/responses"))
            .query(&format!("included_response_ids={RESPONSE_ID}"))
            .respond_bytes(200, Vec::new()),
        Expectation::new("DELETE", &format!("/forms/{FORM_ID}/responses"))
            .query(&format!("included_response_ids={RESPONSE_ID}"))
            .respond_bytes(200, Vec::new()),
    ])
    .await;

    let input = json!({ "form_id": FORM_ID, "included_response_ids": RESPONSE_ID });
    let first = render(&stub, "response.delete", input.clone());
    let second = render(&stub, "response.delete", input);
    assert_eq!(first.method(), second.method());
    assert_eq!(first.url().as_str(), second.url().as_str());
    assert!(first.body().is_empty() && second.body().is_empty());

    for request in [first, second] {
        let response = stub.send(request).await.expect("the stub answers");
        assert_eq!(
            response.status.as_u16(),
            200,
            "the second delete is accepted exactly as the first: not-found IDs are ignored"
        );
    }
    stub.assert_satisfied();
}

/// `typeform_output_contract`: the declared pointers are complete and typed,
/// and a missing required pointer is a validation failure rather than a null.
#[test]
fn typeform_output_contract() {
    let get = operation("form.get");
    assert_eq!(
        get.decode_response(
            200,
            br#"{"id":"abc123","title":"Order feedback","fields":[],"settings":{"is_public":true},"_links":{"display":"x"}}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": "abc123",
            "title": "Order feedback",
            "fields": [],
            "settings": { "is_public": true },
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );
    for body in [
        br#"{"title":"Order feedback"}"#.as_slice(),
        br#"{"id":null,"title":"Order feedback"}"#.as_slice(),
        br#"{"id":7,"title":"Order feedback"}"#.as_slice(),
        br#"{"id":"abc123"}"#.as_slice(),
    ] {
        assert_eq!(
            get.decode_response(200, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    assert_eq!(
        get.decode_response(200, br#"{"id":"abc123","title":"t"}"#)
            .expect("the optional pointers are published as explicit nulls"),
        json!({ "id": "abc123", "title": "t", "fields": null, "settings": null })
    );
    assert_eq!(
        operation("webhook.list")
            .decode_response(200, br#"{"items":[{"id":"w1","enabled":true}]}"#)
            .expect("the declared webhook contract is satisfied"),
        json!({ "items": [{ "id": "w1", "enabled": true }] })
    );

    // Typeform documents no body for a successful delete, so the declaration
    // carries no pointer to satisfy and declares the success as no-content: the
    // empty body Typeform sends is the documented success.
    let delete = operation("response.delete");
    assert!(delete.is_success(200) && delete.is_no_content_success(200));
    assert_eq!(
        delete
            .decode_response(200, b"")
            .expect("the documented empty 200 is the documented success"),
        json!({})
    );
    // An operation whose contract is a body still fails without one.
    assert!(
        get.decode_response(200, b"")
            .is_err_and(|failure| failure.class() == ConnectorErrorClass::Validation),
        "a declared required pointer is not satisfied by an absent body"
    );

    // An undeclared status is never a silent success.
    assert_eq!(
        get.decode_response(204, b"")
            .expect_err("an undeclared status is a failure")
            .class(),
        ConnectorErrorClass::Permanent
    );
    for operation in typeform::connector().operations() {
        assert!(
            operation.is_success(200) && !operation.is_success(204),
            "{}: Typeform documents 200 as the success of each of these",
            operation.id()
        );
    }
}

// ---------------------------------------------------------------------------
// Inbound (spec 013 §4)
// ---------------------------------------------------------------------------

use donat_connectors::providers::inbound::EventIdentifier;
use donat_connectors::sdk::WebhookRejection;
use reqwest::header::HeaderMap;
use webhook_support as inbound;

/// Typeform's published scheme, transcribed here from its four documented
/// steps: "Using the HMAC SHA-256 algorithm, create a hash (using `secret` as a
/// key) of the entire received payload as binary", "Encode the binary hash in
/// `base64` format", "Add prefix `sha256=` to the binary hash".
fn sign(body: &[u8]) -> HeaderMap {
    inbound::headers(&[(
        typeform::SIGNATURE_HEADER,
        &format!("sha256={}", inbound::base64(&inbound::digest(body))),
    )])
}

#[test]
fn typeform_signature_precedes_parse() {
    inbound::signature_precedes_parse(
        typeform::connector(),
        sign,
        inbound::headers(&[(
            typeform::SIGNATURE_HEADER,
            &format!("sha256={}", inbound::base64(&[0u8; 32])),
        )]),
        WebhookRejection::InvalidSignature,
    );
    inbound::nothing_prints_the_secret(typeform::connector());

    // Typeform's own Python sample refuses a header whose scheme element is not
    // `sha256`, and the declared prefix is what makes that structural here: an
    // unprefixed digest offers no candidate at all.
    let body = br#"{"event_id":"LtWXD3crgy"}"#;
    assert_eq!(
        inbound::verify(
            typeform::connector(),
            &inbound::headers(&[(
                typeform::SIGNATURE_HEADER,
                &inbound::base64(&inbound::digest(body))
            )]),
            body,
        )
        .expect_err("an unprefixed digest is not a candidate"),
        WebhookRejection::InvalidSignature
    );
}

#[test]
fn typeform_signature_is_exact() {
    const BODY: &[u8] = br#"{"event_id":"LtWXD3crgy","event_type":"form_response","form_response":{"form_id":"lT4Z3j","token":"a3a12ec6","submitted_at":"2018-01-18T18:17:02Z","landed_at":"2018-01-18T18:07:02Z","answers":[]}}"#;
    inbound::signature_is_exact(typeform::connector(), BODY, sign, |headers| {
        let value = headers
            .get("typeform-signature")
            .and_then(|value| value.to_str().ok())
            .expect("the fixture is signed");
        let encoded = value
            .strip_prefix("sha256=")
            .expect("the fixture carries the declared prefix");
        let mut bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded)
            .expect("the fixture signature is base64");
        bytes[0] ^= 0x01;
        inbound::headers(&[(
            typeform::SIGNATURE_HEADER,
            &format!("sha256={}", inbound::base64(&bytes)),
        )])
    });

    // The scheme carries no timestamp, so an authentic delivery stays valid at
    // any clock. That is Typeform's contract, and the declaration records it by
    // not claiming a window it cannot enforce.
    let trigger = inbound::trigger(typeform::connector());
    for now in [0, inbound::NOW, inbound::NOW + 10_000_000] {
        assert_eq!(
            trigger.verify(&sign(BODY), BODY, &inbound::secret(), now),
            Ok(()),
            "a scheme with no timestamp does not expire"
        );
    }

    inbound::triggers_share_one_scheme(typeform::connector());
    inbound::events_match_triggers(typeform::connector(), typeform::events());
    assert_eq!(
        typeform::events()
            .iter()
            .map(donat_connectors::providers::inbound::TriggerEvent::provider_event)
            .collect::<Vec<_>>(),
        ["form_response"],
        "`form_response` is the one event type Typeform publishes a payload for"
    );
    assert_eq!(
        typeform::events()[0].event_identifier(),
        &EventIdentifier::BodyPointer("/event_id")
    );
}

#[test]
fn typeform_comparison_is_constant_time() {
    inbound::comparison_is_constant_time("typeform.rs", &inbound::module_source("typeform"));
}

#[test]
fn typeform_body_limit_precedes_verification() {
    inbound::body_limit_precedes_verification(typeform::connector(), sign);
}
