//! Microsoft Outlook connector proofs (spec 012 §3 plus spec 015 §3), against
//! the SDK's local provider stub.
//!
//! No test here reaches Microsoft, and no test carries a real credential. The
//! two spec 015 proofs that need a database — `microsoft_outlook_rotation_survives_crash`
//! and the startup half of the permission check — are credential-lifecycle and
//! deployment properties rather than request-shape ones, and live in
//! `crates/server/tests/microsoft_365.rs`.

mod microsoft_graph_support;

use donat_connectors::providers::{microsoft_graph, microsoft_outlook};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use microsoft_graph_support::{
    assert_effects, assert_headers_are_static, assert_next_link_stays_on_origin,
    assert_odata_error_is_typed_by_code, assert_throttling_is_classified, documented_failures,
    graph_error,
};
use serde_json::{Value as JsonValue, json};

/// A Graph message id: base64url-ish with `=`, `-`, and `_`, which is why a
/// path value is percent-encoded rather than interpolated.
const MESSAGE: &str = "AAMkAGI2THVSAAA=";
const EVENT: &str = "AAMkAGI2TG93AAA=";

/// The `Prefer` value every Outlook operation that names an item id declares.
const PREFER_ITEM: &str = "IdType=\"ImmutableId\", outlook.body-content-type=\"text\"";
const PREFER_EVENT: &str =
    "IdType=\"ImmutableId\", outlook.body-content-type=\"text\", outlook.timezone=\"UTC\"";

/// `$select` renders percent-encoded, so the expectations name the encoded form.
const MESSAGE_SELECT: &str = "$select=id%2Csubject%2CbodyPreview%2Cbody%2CreceivedDateTime%2C\
                              sentDateTime%2CisRead%2CisDraft%2ChasAttachments%2Cimportance%2C\
                              webLink%2CparentFolderId%2CconversationId%2Cfrom";

fn operation(id: &str) -> &'static Operation {
    microsoft_outlook::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the microsoft_outlook declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    microsoft_graph_support::render(stub, operation(id), input)
}

/// One message body as Graph returns it under this connector's `$select` mask
/// and `Prefer` header.
fn message_body() -> JsonValue {
    json!({
        "id": MESSAGE,
        "subject": "Meet for lunch?",
        "bodyPreview": "The new cafeteria is open.",
        "body": { "contentType": "text", "content": "The new cafeteria is open." },
        "receivedDateTime": "2026-08-10T12:00:00Z",
        "isRead": false,
        "isDraft": false,
        "hasAttachments": false,
        "from": { "emailAddress": { "name": "Adele Vance", "address": "adelev@contoso.com" } },
        "parentFolderId": "AQMkADYAAAIBDAAAAA==",
        "conversationId": "AAQkAGI2THVS",
        "webLink": "https://outlook.office365.com/owa/?ItemID=AAMkAGI2THVSAAA%3D",
    })
}

/// `microsoft_outlook_request_shape`: exact method, path, query, headers, and
/// body for every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn microsoft_outlook_request_shape() {
    let message_path = "/v1.0/me/messages/AAMkAGI2THVSAAA%3D";
    let event_path = "/v1.0/me/events/AAMkAGI2TG93AAA%3D";
    let stub = ProviderStub::start([
        Expectation::new("GET", message_path)
            .query(MESSAGE_SELECT)
            .header("prefer", PREFER_ITEM)
            .no_body()
            .respond_json(200, message_body()),
        Expectation::new("GET", "/v1.0/me/messages")
            .query(&format!("{MESSAGE_SELECT}&$top=50"))
            .header("prefer", PREFER_ITEM)
            .no_body()
            .respond_json(200, json!({ "value": [message_body()] })),
        Expectation::new("POST", "/v1.0/me/sendMail")
            .header("content-type", "application/json")
            .json_body(json!({
                "message": {
                    "subject": "Meet for lunch?",
                    "body": { "contentType": "Text", "content": "The new cafeteria is open." },
                    "toRecipients": [{ "emailAddress": { "address": "frannis@contoso.com" } }],
                },
                "saveToSentItems": true,
            }))
            .respond_bytes(202, ""),
        Expectation::new("POST", &format!("{message_path}/move"))
            .query(MESSAGE_SELECT)
            .json_body(json!({ "destinationId": "deleteditems" }))
            .respond_json(201, message_body()),
        Expectation::new("PATCH", message_path)
            .query(MESSAGE_SELECT)
            .json_body(json!({ "isRead": true }))
            .respond_json(200, message_body()),
        Expectation::new("DELETE", message_path)
            .header("prefer", PREFER_ITEM)
            .respond_bytes(204, ""),
        Expectation::new("POST", "/v1.0/me/messages")
            .query(MESSAGE_SELECT)
            .json_body(json!({
                "subject": "Draft",
                "body": { "contentType": "Text", "content": "text" },
                "toRecipients": [{ "emailAddress": { "address": "frannis@contoso.com" } }],
            }))
            .respond_json(201, message_body()),
        Expectation::new("POST", &format!("{message_path}/send")).respond_bytes(202, ""),
        Expectation::new("GET", event_path)
            .header("prefer", PREFER_EVENT)
            .respond_json(200, json!({ "id": EVENT })),
        Expectation::new("GET", "/v1.0/me/events").respond_json(200, json!({ "value": [] })),
        Expectation::new("POST", "/v1.0/me/events")
            .json_body(json!({
                "subject": "Let's go for lunch",
                "start": { "dateTime": "2026-08-15T12:00:00", "timeZone": "UTC" },
                "end": { "dateTime": "2026-08-15T14:00:00", "timeZone": "UTC" },
                "attendees": [],
                "transactionId": "7E163156-7762-4BEB-A1C6-729EA81755A7",
            }))
            .respond_json(201, json!({ "id": EVENT })),
        Expectation::new("PATCH", event_path)
            .json_body(json!({ "subject": "Renamed" }))
            .respond_json(200, json!({ "id": EVENT })),
        Expectation::new("DELETE", event_path).respond_bytes(204, ""),
        Expectation::new("GET", "/v1.0/me/contacts/AAMkAGI2THVSAAA%3D")
            .respond_json(200, json!({ "id": MESSAGE })),
        Expectation::new("GET", "/v1.0/me/contacts").respond_json(200, json!({ "value": [] })),
        Expectation::new("GET", "/v1.0/me/mailFolders")
            .without_header("prefer")
            .respond_json(200, json!({ "value": [] })),
        // A hostile message identifier stays one percent-encoded path segment.
        Expectation::new(
            "GET",
            "/v1.0/me/messages/%2E%2E%2F%2E%2E%2Fv1%2E0%2Fme%2FsendMail%3Fx%3D1%23y",
        )
        .respond_json(200, message_body()),
    ])
    .await;

    let recipients = json!([{ "emailAddress": { "address": "frannis@contoso.com" } }]);
    for (id, input) in [
        ("message.get", json!({ "message_id": MESSAGE })),
        ("message.list", json!({})),
        (
            "message.send",
            json!({
                "subject": "Meet for lunch?",
                "body_content_type": "Text",
                "body_content": "The new cafeteria is open.",
                "to_recipients": recipients,
            }),
        ),
        (
            "message.move",
            json!({ "message_id": MESSAGE, "destination_id": "deleteditems" }),
        ),
        (
            "message.update",
            json!({ "message_id": MESSAGE, "is_read": true }),
        ),
        ("message.delete", json!({ "message_id": MESSAGE })),
        (
            "draft.create",
            json!({
                "subject": "Draft",
                "body_content_type": "Text",
                "body_content": "text",
                "to_recipients": recipients,
            }),
        ),
        ("draft.send", json!({ "message_id": MESSAGE })),
        ("event.get", json!({ "event_id": EVENT })),
        ("event.list", json!({})),
        (
            "event.create",
            json!({
                "subject": "Let's go for lunch",
                "start_at": "2026-08-15T12:00:00",
                "end_at": "2026-08-15T14:00:00",
                "time_zone": "UTC",
                "attendees": [],
                "transaction_id": "7E163156-7762-4BEB-A1C6-729EA81755A7",
            }),
        ),
        (
            "event.update",
            json!({ "event_id": EVENT, "subject": "Renamed" }),
        ),
        ("event.delete", json!({ "event_id": EVENT })),
        ("contact.get", json!({ "contact_id": MESSAGE })),
        ("contact.list", json!({})),
        ("folder.list", json!({})),
    ] {
        let request = render(&stub, id, input);
        stub.send(request).await.expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "message.get",
        json!({ "message_id": "../../v1.0/me/sendMail?x=1#y" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(
        hostile.url().query(),
        Some(MESSAGE_SELECT),
        "and cannot add a query of its own"
    );
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();

    // A required declared input with no value is a failure, not an omitted
    // field: `destinationId` is one Microsoft documents as the request body.
    assert!(
        operation("message.move")
            .plan_request(&stub.origin(), &json!({ "message_id": MESSAGE }))
            .is_err(),
        "a missing required input is refused before the request leaves"
    );
}

/// `microsoft_outlook_auth_is_applied`: the stored access token reaches the wire
/// as `Authorization: Bearer <token>`, and appears in no error, log, or
/// diagnostic.
#[tokio::test]
async fn microsoft_outlook_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", "/v1.0/me/messages/AAMkAGI2THVSAAA%3D")
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, message_body()),
        ])
        .await;

    let request = render(&stub, "message.get", json!({ "message_id": MESSAGE }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive(),
        "an applied credential is marked sensitive so a header dump redacts it"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(
        !request.url().as_str().contains(SECRET_SENTINEL),
        "the token is a header and never a query value"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = microsoft_outlook::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        microsoft_outlook::connector().credential(),
    );
    assert!(
        !surface.contains(SECRET_SENTINEL),
        "a stored credential never reaches a log line, an error, or a diagnostic: {surface}"
    );
    // The declaration configures no credential field at all: the token is the
    // credential store's, per attempt.
    assert!(
        microsoft_outlook::connector()
            .credential()
            .fields()
            .is_empty()
    );
    stub.assert_satisfied();

    // ...and with no stored credential the request is refused rather than sent
    // without one.
    let mut bare = operation("message.get")
        .plan_request(&stub.origin(), &json!({ "message_id": MESSAGE }))
        .expect("the declared request renders");
    let refused = AuthPlan::oauth2_authorization_code()
        .apply(&Credential::from_fields([]), &mut bare, None)
        .expect_err("an unauthorized Graph request is never sent");
    assert_eq!(refused.class(), ConnectorErrorClass::Invariant);
}

/// `microsoft_outlook_error_map`: every documented failure reaches exactly one
/// of the eight classes, with a Donat-owned message and no provider text.
#[tokio::test]
async fn microsoft_outlook_error_map() {
    for (status, code, expected) in documented_failures() {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", "/v1.0/me/messages/AAMkAGI2THVSAAA%3D")
                    .respond_json(status, graph_error(code)),
            ])
            .await;
        let response = stub
            .send(render(
                &stub,
                "message.get",
                json!({ "message_id": MESSAGE }),
            ))
            .await
            .expect("the stub answers");

        let failure = microsoft_outlook::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            microsoft_outlook::decode(
                operation("message.get"),
                status,
                response.headers(),
                response.body(),
            )
            .is_err(),
            "status {status} is not a declared success"
        );

        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal"] {
            assert!(
                !surface.contains(leaked),
                "status {status} leaked {leaked} in {surface}"
            );
        }
        stub.assert_satisfied();
    }
}

/// `microsoft_outlook_odata_error_is_typed`: the documented envelope maps by its
/// machine-readable `code`, and the human `message` is never forwarded.
#[test]
fn microsoft_outlook_odata_error_is_typed() {
    assert_odata_error_is_typed_by_code();

    // The same envelope arriving under a *success* status is refused rather
    // than decoded into an activity output.
    let failure = microsoft_outlook::decode(
        operation("message.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"error":{"code":"itemNotFound","message":"The resource could not be found."}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    assert_eq!(
        failure.code(),
        microsoft_graph::SUCCESS_CARRIES_ERROR.code()
    );
}

/// `microsoft_outlook_throttling_is_classified`: the documented throttling
/// response and its retry hint reach `http_429`, with the hint clamped.
#[tokio::test]
async fn microsoft_outlook_throttling_is_classified() {
    assert_throttling_is_classified(
        "GET",
        "/v1.0/me/messages/AAMkAGI2THVSAAA%3D",
        operation("message.get"),
        json!({ "message_id": MESSAGE }),
    )
    .await;
}

/// `microsoft_outlook_next_link_stays_on_origin` (and
/// `microsoft_outlook_pagination_is_bounded`): the walk follows a same-origin
/// `@odata.nextLink` and refuses one that points anywhere else, with no request
/// made.
#[tokio::test]
async fn microsoft_outlook_next_link_stays_on_origin() {
    for (id, path) in [
        ("message.list", "/v1.0/me/messages"),
        ("event.list", "/v1.0/me/events"),
        ("contact.list", "/v1.0/me/contacts"),
        ("folder.list", "/v1.0/me/mailFolders"),
    ] {
        let plan = microsoft_outlook::pagination(id)
            .unwrap_or_else(|| panic!("{id} declares a continuation plan"));
        assert_next_link_stays_on_origin(plan, operation(id), json!({}), "GET", path, "/value")
            .await;
    }

    // An operation that is not a collection publishes no plan at all, so there
    // is no provider-chosen destination to follow.
    for id in ["message.get", "message.send", "event.get", "contact.get"] {
        assert!(microsoft_outlook::pagination(id).is_none(), "{id}");
    }
}

/// `microsoft_outlook_consistency_headers_are_declared`: the headers Microsoft
/// documents as changing what an answer *means* are declaration material, and
/// no operation derives one from input.
#[test]
fn microsoft_outlook_consistency_headers_are_declared() {
    assert_headers_are_static(
        microsoft_outlook::connector(),
        &[
            ("message.get", &[("prefer", PREFER_ITEM)]),
            ("message.list", &[("prefer", PREFER_ITEM)]),
            ("message.move", &[("prefer", PREFER_ITEM)]),
            ("message.update", &[("prefer", PREFER_ITEM)]),
            ("message.delete", &[("prefer", PREFER_ITEM)]),
            ("draft.create", &[("prefer", PREFER_ITEM)]),
            ("draft.send", &[("prefer", PREFER_ITEM)]),
            ("contact.get", &[("prefer", PREFER_ITEM)]),
            ("contact.list", &[("prefer", PREFER_ITEM)]),
            ("event.get", &[("prefer", PREFER_EVENT)]),
            ("event.list", &[("prefer", PREFER_EVENT)]),
            ("event.create", &[("prefer", PREFER_EVENT)]),
            ("event.update", &[("prefer", PREFER_EVENT)]),
            ("event.delete", &[("prefer", PREFER_EVENT)]),
            // "Container types (mailFolder, calendar, etc.) don't support
            // immutable ID", and `sendMail` neither takes nor returns an item
            // id, so neither declares one.
            ("folder.list", &[]),
            ("message.send", &[]),
        ],
    );
}

/// `microsoft_outlook_effects_are_classified`: every operation carries a class,
/// and an inventory-only one cannot be enabled by a deployment.
#[test]
fn microsoft_outlook_effects_are_classified() {
    assert_effects(
        microsoft_outlook::connector(),
        &[
            ("message.get", EffectClass::ReadOnly),
            ("message.list", EffectClass::ReadOnly),
            ("message.send", EffectClass::AtMostOnce),
            ("message.move", EffectClass::AtMostOnce),
            ("message.update", EffectClass::InventoryOnly),
            (
                "message.delete",
                EffectClass::ProviderIdempotentNaturalMethod,
            ),
            ("draft.create", EffectClass::AtMostOnce),
            ("draft.send", EffectClass::AtMostOnce),
            ("event.get", EffectClass::ReadOnly),
            ("event.list", EffectClass::ReadOnly),
            ("event.create", EffectClass::InventoryOnly),
            ("event.update", EffectClass::AtMostOnce),
            ("event.delete", EffectClass::ProviderIdempotentNaturalMethod),
            ("contact.get", EffectClass::ReadOnly),
            ("contact.list", EffectClass::ReadOnly),
            ("folder.list", EffectClass::ReadOnly),
        ],
    );

    assert_eq!(
        microsoft_outlook::connector().admit_operation("message.forward"),
        Err(OperationRejection::Undeclared),
        "an operation this binary does not compile cannot be enabled"
    );
}

/// The permission table is complete, is per operation group, and never asks a
/// read-only deployment for a write permission. The *startup* half is in
/// `crates/server/tests/microsoft_365.rs`.
#[test]
fn microsoft_outlook_permissions_are_declared_per_operation_group() {
    let connector = microsoft_outlook::connector();
    let least = microsoft_graph::declared_permissions(
        connector.operations(),
        microsoft_outlook::permissions,
    )
    .expect("every operation declares the permissions Microsoft documents for it");
    assert_eq!(
        least,
        vec![
            "Mail.ReadBasic",
            "Mail.Send",
            "Mail.ReadWrite",
            "Calendars.ReadBasic",
            "Calendars.ReadWrite",
            "Contacts.Read",
        ],
        "six groups, in declaration order"
    );

    let reads = ["message.get".to_owned(), "message.list".to_owned()];
    let read_only = vec!["Mail.ReadBasic".to_owned()];
    assert!(
        microsoft_graph::permission_report(microsoft_outlook::permissions, &reads, &read_only)
            .is_empty(),
        "a read-only deployment is never forced to grant a write permission"
    );

    let with_write = [reads.as_slice(), &["message.delete".to_owned()]].concat();
    let report =
        microsoft_graph::permission_report(microsoft_outlook::permissions, &with_write, &read_only);
    assert_eq!(
        report.missing,
        vec![("message.delete".to_owned(), "Mail.ReadWrite")]
    );

    // `Mail.ReadWrite` alone authorizes both, and is then not surplus.
    assert!(
        microsoft_graph::permission_report(
            microsoft_outlook::permissions,
            &with_write,
            &["Mail.ReadWrite".to_owned()],
        )
        .is_empty()
    );
}

/// `microsoft_outlook_output_contract`: the declared pointers are complete and
/// typed, and a missing required pointer is a validation failure, not a null.
#[test]
fn microsoft_outlook_output_contract() {
    let get = operation("message.get");
    assert_eq!(
        microsoft_outlook::decode(
            get,
            200,
            &reqwest::header::HeaderMap::new(),
            &serde_json::to_vec(&message_body()).expect("a fixture body serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": MESSAGE,
            "subject": "Meet for lunch?",
            "body_preview": "The new cafeteria is open.",
            "body_content": "The new cafeteria is open.",
            "received_at": "2026-08-10T12:00:00Z",
            "is_read": false,
            "is_draft": false,
            "has_attachments": false,
            "from_address": "adelev@contoso.com",
            "parent_folder_id": "AQMkADYAAAIBDAAAAA==",
            "conversation_id": "AAQkAGI2THVS",
            "web_link": "https://outlook.office365.com/owa/?ItemID=AAMkAGI2THVSAAA%3D",
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );

    for body in [
        br#"{"subject":"no id"}"#.as_slice(),
        br#"{"id":null,"subject":"null id"}"#.as_slice(),
        br#"{"id":7}"#.as_slice(),
        br#"not json at all"#.as_slice(),
    ] {
        assert_eq!(
            microsoft_outlook::decode(get, 200, &reqwest::header::HeaderMap::new(), body)
                .expect_err("a missing, mistyped, or unparseable body is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    // An optional pointer that is absent is a declared null rather than a
    // missing key, so a Process binds one shape.
    let sparse = microsoft_outlook::decode(
        get,
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"id":"AAMkAGI2THVSAAA="}"#,
    )
    .expect("the required pointers are satisfied");
    assert_eq!(sparse.get("subject"), Some(&JsonValue::Null));
    assert_eq!(sparse.get("web_link"), Some(&JsonValue::Null));

    // A documented empty success — `202 Accepted` for a send, `204 No Content`
    // for a delete — decodes as the empty answer Microsoft documents.
    for (id, status) in [("message.send", 202u16), ("message.delete", 204)] {
        assert_eq!(
            microsoft_outlook::decode(
                operation(id),
                status,
                &reqwest::header::HeaderMap::new(),
                b"",
            )
            .expect("a documented empty success is a success"),
            json!({}),
            "{id}"
        );
    }
}
