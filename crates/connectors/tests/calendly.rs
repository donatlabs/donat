//! Calendly connector proofs (spec 013 §4), against the SDK's local provider
//! stub and against signatures this test generates itself.

mod webhook_support;

use std::time::Duration;

use donat_connectors::providers::calendly;
use donat_connectors::providers::inbound::EventIdentifier;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan, WebhookRejection,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use webhook_support as inbound;

const EVENT_UUID: &str = "AAAAAAAAAAAAAAAA";
const INVITEE_UUID: &str = "BBBBBBBBBBBBBBBB";

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    calendly::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the calendly declaration publishes {id}"))
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

fn scheduled_event() -> JsonValue {
    json!({
        "resource": {
            "uri": format!("https://api.calendly.test/scheduled_events/{EVENT_UUID}"),
            "name": "15 Minute Meeting",
            "status": "active",
            "start_time": "2019-08-24T14:15:22.000000Z",
            "end_time": "2019-08-24T14:15:22.000000Z",
            "event_type": "https://api.calendly.test/event_types/AAAA",
        }
    })
}

fn collection(next: Option<&str>) -> JsonValue {
    json!({
        "collection": [{ "uri": "https://api.calendly.test/x/1" }],
        "pagination": {
            "count": 100,
            "next_page": next.map(|token| format!("https://api.calendly.test/scheduled_events?page_token={token}")),
            "previous_page": null,
            "next_page_token": next,
            "previous_page_token": null,
        },
    })
}

/// `calendly_request_shape`: exact method, path, query, and body for every
/// operation, including the percent-encoding of a hostile path value.
#[tokio::test]
async fn calendly_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/scheduled_events/{EVENT_UUID}"))
            .query("")
            .no_body()
            .respond_json(200, scheduled_event()),
        Expectation::new("GET", "/scheduled_events")
            .query("user=https%3A%2F%2Fapi%2Ecalendly%2Etest%2Fusers%2Fme&organization=&status=active&count=100")
            .no_body()
            .respond_json(200, collection(None)),
        Expectation::new(
            "GET",
            &format!("/scheduled_events/{EVENT_UUID}/invitees/{INVITEE_UUID}"),
        )
        .query("")
        .respond_json(
            200,
            json!({
                "resource": {
                    "uri": "https://api.calendly.test/i/1",
                    "email": "a@example.test",
                    "name": "A",
                    "status": "active",
                    "event": format!("https://api.calendly.test/scheduled_events/{EVENT_UUID}"),
                }
            }),
        ),
        Expectation::new("GET", &format!("/scheduled_events/{EVENT_UUID}/invitees"))
            .query("status=active&count=100")
            .respond_json(200, collection(None)),
        Expectation::new("GET", "/users/me")
            .query("")
            .respond_json(
                200,
                json!({
                    "resource": {
                        "uri": "https://api.calendly.test/users/me",
                        "name": "John Doe",
                        "email": "john@example.test",
                        "scheduling_url": "https://calendly.test/john",
                        "current_organization": "https://api.calendly.test/organizations/A",
                    }
                }),
            ),
        // A hostile UUID stays one percent-encoded path segment.
        Expectation::new("GET", "/scheduled_events/%2E%2E%2Fusers%2Fme%3Fx%3D1")
            .respond_json(200, scheduled_event()),
    ])
    .await;

    for (id, input) in [
        ("event.get", json!({ "uuid": EVENT_UUID })),
        (
            "event.list",
            json!({
                "user": "https://api.calendly.test/users/me",
                "organization": "",
                "status": "active",
            }),
        ),
        (
            "invitee.get",
            json!({ "event_uuid": EVENT_UUID, "invitee_uuid": INVITEE_UUID }),
        ),
        (
            "invitee.list",
            json!({ "uuid": EVENT_UUID, "status": "active" }),
        ),
        ("user.me", json!({})),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(&stub, "event.get", json!({ "uuid": "../users/me?x=1" }));
    assert_eq!(hostile.url().host_str(), stub.origin().as_url().host_str());
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `calendly_auth_is_applied`: the token reaches the wire as
/// `Authorization: Bearer <token>` and appears nowhere else.
#[tokio::test]
async fn calendly_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/users/me")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(
            200,
            json!({
                "resource": {
                    "uri": "https://api.calendly.test/users/me",
                    "name": "John Doe",
                    "email": "john@example.test",
                    "scheduling_url": "https://calendly.test/john",
                    "current_organization": "https://api.calendly.test/organizations/A",
                }
            }),
        )])
    .await;

    let request = render(&stub, "user.me", json!({}));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    let response = stub.send(request).await.expect("the stub answers");
    let failure = calendly::error_map().classify_response(&response);
    assert!(
        !format!(
            "{} {} {} {:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic(),
            calendly::connector().credential(),
        )
        .contains(SECRET_SENTINEL)
    );
    stub.assert_satisfied();
}

/// `calendly_error_map`: each documented status reaches one closed class and
/// Calendly's own `title`/`message` prose never crosses the boundary.
#[tokio::test]
async fn calendly_error_map() {
    for (status, expected) in [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (424, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ] {
        let stub = ProviderStub::start([Expectation::new("GET", "/users/me").respond_json(
            status,
            json!({
                "title": "Permission Denied",
                "message": format!("You do not have permission; token {SECRET_SENTINEL} on db-7"),
                "required_scopes": ["scheduled_events:read"],
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "user.me", json!({})))
            .await
            .expect("the stub answers");
        let failure = calendly::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7", "Permission Denied"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `calendly_pagination_is_bounded`: the token walk terminates on the `null`
/// Calendly documents, never becomes a destination, and stops at its budget.
#[tokio::test]
async fn calendly_pagination_is_bounded() {
    let plan = calendly::pagination("event.list").expect("event.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/scheduled_events")
            .query("user=u&organization=&status=active&count=100")
            .respond_json(200, collection(Some("sNjq4TvMDfUHEl7z"))),
        Expectation::new("GET", "/scheduled_events")
            .query("user=u&organization=&status=active&count=100&page_token=sNjq4TvMDfUHEl7z")
            .respond_json(200, collection(None)),
    ])
    .await;
    let items = plan
        .collect(
            render(
                &stub,
                "event.list",
                json!({ "user": "u", "organization": "", "status": "active" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a null next_page_token ends the walk");
    assert_eq!(items.len(), 2);
    stub.assert_satisfied();

    // The token is data, never a destination: a body that spells another host
    // becomes a percent-encoded query value on the compiled origin. This is the
    // reason the declaration prefers the token to Calendly's own `next_page`
    // URI, whose published example carries the wrong host.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/scheduled_events")
            .respond_json(200, collection(Some("https://attacker.invalid/x"))),
        Expectation::new("GET", "/scheduled_events")
            .query("user=u&organization=&status=active&count=100&page_token=https%3A%2F%2Fattacker%2Einvalid%2Fx")
            .respond_json(200, collection(None)),
    ])
    .await;
    plan.collect(
        render(
            &stub,
            "event.list",
            json!({ "user": "u", "organization": "", "status": "active" }),
        ),
        &stub.origin(),
        &budget,
        undeclared_status_gate,
        |request| {
            assert_eq!(
                request.url().host_str(),
                stub.origin().as_url().host_str(),
                "a continuation token never becomes a destination"
            );
            stub.send(request)
        },
    )
    .await
    .expect("the hostile token is spent as a query value");
    stub.assert_satisfied();

    // An endless provider exhausts a ceiling rather than looping.
    let stub = ProviderStub::start((0..12).map(|_| {
        Expectation::new("GET", "/scheduled_events").respond_json(200, collection(Some("more")))
    }))
    .await;
    let failure = plan
        .collect(
            render(
                &stub,
                "event.list",
                json!({ "user": "u", "organization": "", "status": "active" }),
            ),
            &stub.origin(),
            &PaginationBudget::new(3, 3, 1_000, 1_024 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("an endless provider exhausts the budget");
    assert_eq!(failure.code(), "connector_pagination_budget");

    for id in ["event.get", "invitee.get", "user.me"] {
        assert!(
            calendly::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `calendly_effects_are_classified`: every operation is a `GET`, so every one
/// is read-only by its method and none is inventory-only.
#[test]
fn calendly_effects_are_classified() {
    let connector = calendly::connector();
    let expected = [
        "event.get",
        "event.list",
        "invitee.get",
        "invitee.list",
        "user.me",
    ];
    assert_eq!(connector.operations().len(), expected.len());
    for id in expected {
        assert_eq!(
            operation(id).effect_class(),
            Some(EffectClass::ReadOnly),
            "{id}"
        );
        assert!(connector.admit_operation(id).is_ok(), "{id}");
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector.admit_operation("webhook_subscription.create"),
        Err(OperationRejection::Undeclared),
        "creating a subscription is out of this batch's scope"
    );
}

/// `calendly_output_contract`: the declared pointers read Calendly's
/// `resource`/`collection` envelopes, and a missing required one fails.
#[test]
fn calendly_output_contract() {
    let get = operation("event.get");
    assert_eq!(
        get.decode_response(
            200,
            br#"{"resource":{"uri":"u","name":"n","status":"active","start_time":"s","end_time":"e","event_type":"t","location":{"type":"physical"}}}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "uri": "u", "name": "n", "status": "active",
            "start_time": "s", "end_time": "e", "event_type": "t",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"resource":{"uri":"u"}}"#)
            .expect_err("a missing required pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        operation("event.list")
            .decode_response(
                200,
                br#"{"collection":[],"pagination":{"count":0,"next_page":null,"next_page_token":null}}"#,
            )
            .expect("an exhausted collection is a success"),
        json!({ "collection": [], "next_page_token": null })
    );
}

// ---------------------------------------------------------------------------
// Inbound (spec 013 §4)
// ---------------------------------------------------------------------------

/// Calendly's published scheme, transcribed here: "Create the signed payload by
/// concatenating the timestamp (t), the character '.', and the request body's
/// JSON payload", then "computing an HMAC with the SHA256 hash function",
/// hex-encoded, in a `t=<unix>,v1=<hex>` header.
fn sign_at(timestamp: i64, body: &[u8]) -> HeaderMap {
    let mut canonical = timestamp.to_string().into_bytes();
    canonical.push(b'.');
    canonical.extend_from_slice(body);
    inbound::headers(&[(
        calendly::SIGNATURE_HEADER,
        &format!(
            "t={timestamp},v1={}",
            inbound::hex(&inbound::digest(&canonical))
        ),
    )])
}

fn sign(body: &[u8]) -> HeaderMap {
    sign_at(inbound::NOW, body)
}

#[test]
fn calendly_signature_precedes_parse() {
    inbound::signature_precedes_parse(
        calendly::connector(),
        sign,
        inbound::headers(&[(
            calendly::SIGNATURE_HEADER,
            &format!("t={},v1={}", inbound::NOW, inbound::hex(&[0u8; 32])),
        )]),
        WebhookRejection::InvalidSignature,
    );
    inbound::nothing_prints_the_secret(calendly::connector());

    // A header with no `t` element at all is unreadable rather than missing.
    assert_eq!(
        inbound::verify(
            calendly::connector(),
            &inbound::headers(&[(
                calendly::SIGNATURE_HEADER,
                &format!("v1={}", inbound::hex(&[0u8; 32]))
            )]),
            inbound::MALFORMED_BODY,
        )
        .expect_err("a signature header without its timestamp is unreadable"),
        WebhookRejection::InvalidSignature
    );
}

#[test]
fn calendly_signature_is_exact() {
    const BODY: &[u8] = br#"{"created_at":"2020-11-23T17:51:19.000000Z","event":"invitee.created","payload":{"uri":"u","email":"a@example.test"}}"#;
    let connector = calendly::connector();

    inbound::signature_is_exact(connector, BODY, sign, |headers| {
        let value = headers
            .get("calendly-webhook-signature")
            .and_then(|value| value.to_str().ok())
            .expect("the fixture is signed");
        let (timestamp, signature) = value
            .split_once(",v1=")
            .expect("the fixture carries both elements");
        let flipped = format!(
            "{}{}",
            &signature[..signature.len() - 1],
            if signature.ends_with('0') { '1' } else { '0' }
        );
        inbound::headers(&[(
            calendly::SIGNATURE_HEADER,
            &format!("{timestamp},v1={flipped}"),
        )])
    });

    // The tolerance window Calendly publishes: three minutes, and it is exact.
    // A stale timestamp fails and a fresh one passes, and the SDK's window
    // closes in both directions where Calendly's own sample only closes one.
    let trigger = inbound::trigger(connector);
    for (offset, expected) in [
        (0, Ok(())),
        (180, Ok(())),
        (-180, Ok(())),
        (181, Err(WebhookRejection::TimestampOutOfTolerance)),
        (-181, Err(WebhookRejection::TimestampOutOfTolerance)),
    ] {
        let sent = inbound::NOW - offset;
        assert_eq!(
            trigger.verify(&sign_at(sent, BODY), BODY, &inbound::secret(), inbound::NOW),
            expected,
            "a signature sent {offset} seconds from the receiving clock"
        );
    }

    inbound::triggers_share_one_scheme(connector);
    inbound::events_match_triggers(connector, calendly::events());

    // Calendly publishes no per-delivery identifier at all, and the declaration
    // records that rather than synthesizing one.
    for event in calendly::events() {
        assert_eq!(event.event_identifier(), &EventIdentifier::Unpublished);
    }
    assert_eq!(
        calendly::events()
            .iter()
            .map(donat_connectors::providers::inbound::TriggerEvent::provider_event)
            .collect::<Vec<_>>(),
        ["invitee.created", "invitee.canceled"]
    );
}

#[test]
fn calendly_comparison_is_constant_time() {
    inbound::comparison_is_constant_time("calendly.rs", &inbound::module_source("calendly"));
}

#[test]
fn calendly_body_limit_precedes_verification() {
    inbound::body_limit_precedes_verification(calendly::connector(), sign);
}
