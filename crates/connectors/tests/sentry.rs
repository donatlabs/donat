//! Sentry connector proofs (spec 013 §4), against the SDK's local provider stub
//! and against signatures this test generates itself.

mod webhook_support;

use donat_connectors::providers::inbound::EventIdentifier;
use donat_connectors::providers::sentry;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan, WebhookRejection,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use webhook_support as inbound;

const ORG: &str = "acme";
const PROJECT: &str = "engine";
const ISSUE_ID: &str = "1234567890";

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    sentry::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the sentry declaration publishes {id}"))
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

fn issue() -> JsonValue {
    json!({
        "id": ISSUE_ID,
        "shortId": "ENGINE-Y",
        "title": "TypeError",
        "status": "unresolved",
        "level": "error",
        "permalink": "https://acme.sentry.test/issues/1234567890/",
    })
}

/// `sentry_request_shape`: exact method, path, query, and body for every
/// operation, on the organization-scoped routes Sentry publishes today.
#[tokio::test]
async fn sentry_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new(
            "GET",
            &format!("/api/0/organizations/{ORG}/issues/{ISSUE_ID}/"),
        )
        .query("")
        .no_body()
        .respond_json(200, issue()),
        Expectation::new("GET", &format!("/api/0/organizations/{ORG}/issues/"))
            .query("query=is%3Aunresolved&limit=100")
            .no_body()
            .respond_json(200, json!([issue()])),
        Expectation::new(
            "PUT",
            &format!("/api/0/organizations/{ORG}/issues/{ISSUE_ID}/"),
        )
        .json_body(json!({ "status": "resolved", "assignedTo": "user:1" }))
        .respond_bytes(200, Vec::new()),
        Expectation::new(
            "GET",
            &format!("/api/0/projects/{ORG}/{PROJECT}/events/deadbeef/"),
        )
        .query("")
        .respond_json(
            200,
            json!({
                "id": "1", "eventID": "deadbeef", "groupID": "9",
                "title": "TypeError", "dateCreated": "2026-01-01T00:00:00Z",
            }),
        ),
        Expectation::new("GET", &format!("/api/0/projects/{ORG}/{PROJECT}/events/"))
            .query("statsPeriod=24h")
            .respond_json(200, json!([])),
        Expectation::new("GET", &format!("/api/0/projects/{ORG}/{PROJECT}/"))
            .query("")
            .respond_json(
                200,
                json!({
                    "id": "5", "slug": PROJECT, "name": "Engine",
                    "platform": "rust", "dateCreated": "2026-01-01T00:00:00Z",
                }),
            ),
        Expectation::new("GET", &format!("/api/0/organizations/{ORG}/projects/"))
            .query("per_page=100")
            .respond_json(200, json!([])),
        Expectation::new(
            "GET",
            &format!("/api/0/organizations/{ORG}/releases/v1%2E0%2E0/"),
        )
        .query("")
        .respond_json(
            200,
            json!({
                "id": 7, "version": "v1.0.0", "shortVersion": "v1.0.0",
                "dateCreated": "2026-01-01T00:00:00Z", "dateReleased": null,
            }),
        ),
        Expectation::new("GET", &format!("/api/0/organizations/{ORG}/releases/"))
            .query("query=v1&per_page=100")
            .respond_json(200, json!([])),
    ])
    .await;

    for (id, input) in [
        (
            "issue.get",
            json!({ "organization": ORG, "issue_id": ISSUE_ID }),
        ),
        (
            "issue.list",
            json!({ "organization": ORG, "query": "is:unresolved" }),
        ),
        (
            "issue.update",
            json!({
                "organization": ORG, "issue_id": ISSUE_ID,
                "status": "resolved", "assignedTo": "user:1",
            }),
        ),
        (
            "event.get",
            json!({ "organization": ORG, "project": PROJECT, "event_id": "deadbeef" }),
        ),
        (
            "event.list",
            json!({ "organization": ORG, "project": PROJECT, "statsPeriod": "24h" }),
        ),
        (
            "project.get",
            json!({ "organization": ORG, "project": PROJECT }),
        ),
        ("project.list", json!({ "organization": ORG })),
        (
            "release.get",
            json!({ "organization": ORG, "version": "v1.0.0" }),
        ),
        (
            "release.list",
            json!({ "organization": ORG, "query": "v1" }),
        ),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();

    // A release version is free-form, and it stays one percent-encoded segment.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/0/organizations/{ORG}/releases/%2E%2E%2Fprojects%3Fx%3D1/"),
    )
    .respond_json(
        200,
        json!({ "id": 1, "version": "v", "dateCreated": "2026-01-01T00:00:00Z" }),
    )])
    .await;
    let hostile = render(
        &stub,
        "release.get",
        json!({ "organization": ORG, "version": "../projects?x=1" }),
    );
    assert_eq!(hostile.url().host_str(), stub.origin().as_url().host_str());
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `sentry_auth_is_applied`: the auth token reaches the wire as
/// `Authorization: Bearer <token>` and appears nowhere else.
#[tokio::test]
async fn sentry_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/0/organizations/{ORG}/issues/{ISSUE_ID}/"),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, issue())])
    .await;

    let request = render(
        &stub,
        "issue.get",
        json!({ "organization": ORG, "issue_id": ISSUE_ID }),
    );
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    let response = stub.send(request).await.expect("the stub answers");
    let failure = sentry::error_map().classify_response(&response);
    assert!(
        !format!(
            "{} {} {} {:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic(),
            sentry::connector().credential(),
        )
        .contains(SECRET_SENTINEL)
    );
    stub.assert_satisfied();
}

/// `sentry_error_map`: each documented status reaches one closed class, and the
/// `detail` a client sees in practice — which Sentry publishes no schema for —
/// never crosses the boundary.
#[tokio::test]
async fn sentry_error_map() {
    for (status, expected) in [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ] {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/0/organizations/{ORG}/issues/{ISSUE_ID}/"),
        )
        .respond_json(
            status,
            json!({
                "detail": format!("You do not have permission; token {SECRET_SENTINEL} on db-7"),
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "issue.get",
                json!({ "organization": ORG, "issue_id": ISSUE_ID }),
            ))
            .await
            .expect("the stub answers");
        let failure = sentry::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7", "do not have permission"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `sentry_pagination_is_bounded`: no collection declares a continuation plan,
/// because Sentry publishes a `rel="next"` link even when the walk is over and
/// marks exhaustion with a link parameter no plan in the closed set reads.
/// Each collection therefore asks for one page whose size is in the declaration.
#[test]
fn sentry_pagination_is_bounded() {
    let sizes = [
        ("issue.list", "limit=100"),
        ("project.list", "per_page=100"),
        ("release.list", "per_page=100"),
    ];
    for (id, expected) in sizes {
        let projected = operation(id).project();
        assert!(
            projected.query().iter().any(|entry| {
                matches!(
                    entry.value(),
                    donat_connectors::sdk::ValueSource::Static(value)
                        if format!("{}={value}", entry.key()) == expected
                )
            }),
            "{id} declares the page-size parameter its own endpoint publishes ({expected})"
        );
    }
    // The project event list documents neither `per_page` nor `limit`, so the
    // declaration adds neither.
    assert!(
        operation("event.list")
            .project()
            .query()
            .iter()
            .all(|entry| entry.key() != "per_page" && entry.key() != "limit"),
        "a declaration adds no parameter the provider does not publish"
    );
}

/// `sentry_effects_are_classified`: the one `PUT` is inventory-only on Sentry's
/// own "Only the attributes submitted are modified", and everything else is a
/// `GET`.
#[test]
fn sentry_effects_are_classified() {
    let connector = sentry::connector();
    let expected = [
        ("issue.get", EffectClass::ReadOnly),
        ("issue.list", EffectClass::ReadOnly),
        ("issue.update", EffectClass::InventoryOnly),
        ("event.get", EffectClass::ReadOnly),
        ("event.list", EffectClass::ReadOnly),
        ("project.get", EffectClass::ReadOnly),
        ("project.list", EffectClass::ReadOnly),
        ("release.get", EffectClass::ReadOnly),
        ("release.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());
    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
    }
    assert_eq!(
        connector.admit_operation("issue.update"),
        Err(OperationRejection::InventoryOnly)
    );
    assert!(
        operation("issue.update")
            .effect()
            .and_then(donat_connectors::sdk::Effect::inventory_reason)
            .is_some_and(|reason| reason.contains("Only the attributes submitted are modified")),
        "the recorded reason is Sentry's own sentence"
    );
    assert_eq!(
        connector.admit_operation("issue.delete"),
        Err(OperationRejection::Undeclared)
    );
}

/// `sentry_output_contract`: the declared pointers are typed as Sentry publishes
/// them — an issue id is a JSON string — and its own empty `200` on the update
/// is the documented success.
#[test]
fn sentry_output_contract() {
    let get = operation("issue.get");
    assert_eq!(
        get.decode_response(
            200,
            br#"{"id":"1234567890","shortId":"ENGINE-Y","title":"TypeError","status":"unresolved","level":"error","permalink":"p","count":"3","userCount":3}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": "1234567890", "shortId": "ENGINE-Y", "title": "TypeError",
            "status": "unresolved", "level": "error", "permalink": "p",
        })
    );
    assert_eq!(
        get.decode_response(
            200,
            br#"{"id":1234567890,"shortId":"E","title":"T","status":"unresolved"}"#,
        )
        .expect_err("a numeric id is not the string Sentry publishes")
        .class(),
        ConnectorErrorClass::Validation
    );

    let update = operation("issue.update");
    assert!(update.is_success(200) && update.is_no_content_success(200));
    assert_eq!(
        update
            .decode_response(200, b"")
            .expect("Sentry's own published 200 example carries no body"),
        json!({})
    );
}

// ---------------------------------------------------------------------------
// Inbound (spec 013 §4)
// ---------------------------------------------------------------------------

/// Sentry's published scheme, transcribed here: `hmac.new(key=client_secret,
/// msg=body, digestmod=hashlib.sha256).hexdigest()`, compared against
/// `sentry-hook-signature`.
fn sign(body: &[u8]) -> HeaderMap {
    inbound::headers(&[
        (
            sentry::SIGNATURE_HEADER,
            &inbound::hex(&inbound::digest(body)),
        ),
        (sentry::RESOURCE_HEADER, "issue"),
        (
            sentry::REQUEST_ID_HEADER,
            "0d0b0b3a1a9b4b0f8f0a1b2c3d4e5f60",
        ),
    ])
}

#[test]
fn sentry_signature_precedes_parse() {
    inbound::signature_precedes_parse(
        sentry::connector(),
        sign,
        inbound::headers(&[(sentry::SIGNATURE_HEADER, &inbound::hex(&[0u8; 32]))]),
        WebhookRejection::InvalidSignature,
    );
    inbound::nothing_prints_the_secret(sentry::connector());

    // Sentry's scheme carries no prefix at all, so a `sha256=`-prefixed value —
    // the shape three other connectors in this batch use — is not a candidate.
    let body = br#"{"action":"created"}"#;
    assert_eq!(
        inbound::verify(
            sentry::connector(),
            &inbound::headers(&[(
                sentry::SIGNATURE_HEADER,
                &format!("sha256={}", inbound::hex(&inbound::digest(body)))
            )]),
            body,
        )
        .expect_err("a prefixed digest is not this scheme's candidate"),
        WebhookRejection::InvalidSignature
    );
}

#[test]
fn sentry_signature_is_exact() {
    const BODY: &[u8] = br#"{"action":"created","installation":{"uuid":"24b397fc"},"data":{"issue":{"id":"1","shortId":"E-1","title":"T","status":"unresolved","project":{"slug":"engine"},"web_url":"w"}},"actor":{"type":"application","id":"sentry","name":"Sentry"}}"#;
    inbound::signature_is_exact(sentry::connector(), BODY, sign, |headers| {
        let value = headers
            .get("sentry-hook-signature")
            .and_then(|value| value.to_str().ok())
            .expect("the fixture is signed");
        let flipped = format!(
            "{}{}",
            &value[..value.len() - 1],
            if value.ends_with('0') { '1' } else { '0' }
        );
        inbound::headers(&[(sentry::SIGNATURE_HEADER, &flipped)])
    });
    inbound::triggers_share_one_scheme(sentry::connector());
    inbound::events_match_triggers(sentry::connector(), sentry::events());

    // Sentry's one published per-delivery identifier is a header, and the
    // declaration names it rather than inventing a body field Sentry's envelope
    // does not carry.
    for event in sentry::events() {
        assert_eq!(
            event.event_identifier(),
            &EventIdentifier::Header("Request-ID")
        );
        assert!(
            event.fields().iter().any(|field| field.name() == "action"),
            "`{}` exposes the action Sentry publishes in the body",
            event.provider_event()
        );
    }
    assert_eq!(
        sentry::events()
            .iter()
            .map(donat_connectors::providers::inbound::TriggerEvent::provider_event)
            .collect::<Vec<_>>(),
        ["issue.created", "issue.resolved"]
    );
}

#[test]
fn sentry_comparison_is_constant_time() {
    inbound::comparison_is_constant_time("sentry.rs", &inbound::module_source("sentry"));
}

#[test]
fn sentry_body_limit_precedes_verification() {
    inbound::body_limit_precedes_verification(sentry::connector(), sign);
}
