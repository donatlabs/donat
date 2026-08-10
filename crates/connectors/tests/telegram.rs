//! Telegram connector proofs (spec 013 §4), against the SDK's local provider
//! stub. No test reaches Telegram, and no test carries a real bot token.

mod webhook_support;

use donat_connectors::providers::inbound::EventIdentifier;
use donat_connectors::providers::telegram;
use donat_connectors::sdk::testing::{Expectation, ProviderStub};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan, WebhookRejection,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use webhook_support as inbound;

/// A token in the shape Telegram publishes. It is this repository's own, and it
/// doubles as the sentinel every redaction assertion looks for.
const BOT_TOKEN: &str = "8100000:donat-token-sentinel-do-not-log";

/// The prefix Telegram's own path form puts the token behind, with the token's
/// one non-unreserved character percent-encoded.
const TOKEN_SEGMENT: &str = "/bot8100000%3Adonat-token-sentinel-do-not-log";

fn credential() -> Credential {
    Credential::secret(BOT_TOKEN)
}

fn operation(id: &str) -> &'static Operation {
    telegram::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the telegram declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_path_segment(telegram::TOKEN_PREFIX)
        .expect("Telegram's published prefix is valid")
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn message() -> JsonValue {
    json!({
        "ok": true,
        "result": { "message_id": 11, "date": 1_700_000_000, "chat": { "id": -1001 } },
    })
}

/// `telegram_request_shape`: exact method, path, query, and body for every
/// operation, with the credential in the one place Telegram publishes it.
#[tokio::test]
async fn telegram_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", &format!("{TOKEN_SEGMENT}/sendMessage"))
            .query("")
            .json_body(json!({ "chat_id": -1001, "text": "hello" }))
            .respond_json(200, message()),
        Expectation::new("POST", &format!("{TOKEN_SEGMENT}/editMessageText"))
            .json_body(json!({ "chat_id": -1001, "message_id": 11, "text": "edited" }))
            .respond_json(200, message()),
        Expectation::new("POST", &format!("{TOKEN_SEGMENT}/deleteMessage"))
            .json_body(json!({ "chat_id": -1001, "message_id": 11 }))
            .respond_json(200, json!({ "ok": true, "result": true })),
        Expectation::new("GET", &format!("{TOKEN_SEGMENT}/getChat"))
            .query("chat_id=%2D1001")
            .no_body()
            .respond_json(
                200,
                json!({ "ok": true, "result": { "id": -1001, "type": "supergroup" } }),
            ),
        Expectation::new("GET", &format!("{TOKEN_SEGMENT}/getChatMember"))
            .query("chat_id=%2D1001&user_id=99")
            .respond_json(
                200,
                json!({
                    "ok": true,
                    "result": { "status": "member", "user": { "id": 99, "is_bot": false } },
                }),
            ),
        Expectation::new("GET", &format!("{TOKEN_SEGMENT}/getFile"))
            .query("file_id=AgAC")
            .respond_json(
                200,
                json!({
                    "ok": true,
                    "result": { "file_id": "AgAC", "file_unique_id": "u1", "file_size": 3, "file_path": "photos/1.jpg" },
                }),
            ),
    ])
    .await;

    for (id, input) in [
        ("message.send", json!({ "chat_id": -1001, "text": "hello" })),
        (
            "message.edit_text",
            json!({ "chat_id": -1001, "message_id": 11, "text": "edited" }),
        ),
        (
            "message.delete",
            json!({ "chat_id": -1001, "message_id": 11 }),
        ),
        ("chat.get", json!({ "chat_id": -1001 })),
        (
            "chat.member_get",
            json!({ "chat_id": -1001, "user_id": 99 }),
        ),
        ("file.get", json!({ "file_id": "AgAC" })),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `telegram_auth_is_applied` (spec 013 §1): the bot token becomes the request's
/// first path segment, percent-encoded, and a sentinel placed there appears in
/// nothing this crate can print.
#[tokio::test]
async fn telegram_auth_is_applied() {
    let stub = ProviderStub::start(
        [Expectation::new("GET", &format!("{TOKEN_SEGMENT}/getChat"))
            .without_header("authorization")
            .without_header("x-api-key")
            .respond_json(
                200,
                json!({ "ok": true, "result": { "id": -1001, "type": "supergroup" } }),
            )],
    )
    .await;

    let request = render(&stub, "chat.get", json!({ "chat_id": -1001 }));
    assert_eq!(
        request.url().path(),
        format!("{TOKEN_SEGMENT}/getChat"),
        "the credential is the first path segment, behind Telegram's `bot` prefix"
    );
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none(),
        "Telegram publishes no header form of authentication and this plan sends none"
    );

    // The redaction proof. The URL handed to the transport carries the token;
    // nothing else does.
    assert!(request.url_carries_credential());
    assert_eq!(
        request.redacted_url(),
        format!(
            "{}<redacted>",
            stub.base_url().trim_end_matches('/').to_owned() + "/"
        ),
    );
    let surface = format!(
        "{request:?} {} {:?} {:?} {:?}",
        request.redacted_url(),
        request.headers(),
        telegram::connector().credential(),
        credential(),
    );
    assert!(
        !surface.contains(BOT_TOKEN) && !surface.contains("donat-token-sentinel"),
        "the bot token must not appear in a log line, a diagnostic, or a fingerprint: {surface}"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = telegram::error_map().classify_response(&response);
    assert!(
        !format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        )
        .contains("donat-token-sentinel")
    );
    stub.assert_satisfied();

    // A hostile token value cannot leave its segment or re-aim the request.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        "/bot%2E%2E%2F%2E%2E%2Fadmin%3Fx%3D1/getChat",
    )
    .respond_json(
        200,
        json!({ "ok": true, "result": { "id": 1, "type": "private" } }),
    )])
    .await;
    let mut hostile = operation("chat.get")
        .plan_request(&stub.origin(), &json!({ "chat_id": 1 }))
        .expect("the declared request renders");
    AuthPlan::api_key_path_segment(telegram::TOKEN_PREFIX)
        .expect("a static prefix is valid")
        .apply(&Credential::secret("../../admin?x=1"), &mut hostile, None)
        .expect("the path segment applies");
    assert_eq!(hostile.url().host_str(), stub.origin().as_url().host_str());
    assert_eq!(hostile.url().query(), Some("chat_id=1"));
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `telegram_error_map`: Telegram publishes no status table, so each status
/// takes the class HTTP defines for it and the body is never read.
#[tokio::test]
async fn telegram_error_map() {
    for (status, expected) in [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ] {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("{TOKEN_SEGMENT}/getChat"),
        )
        .respond_json(
            status,
            json!({
                "ok": false,
                "error_code": status,
                "description": format!("Forbidden: bot {BOT_TOKEN} was blocked by db-7"),
                "parameters": { "retry_after": 12 },
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "chat.get", json!({ "chat_id": -1001 })))
            .await
            .expect("the stub answers");
        let failure = telegram::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in ["donat-token-sentinel", "db-7", "was blocked"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `telegram_pagination_is_bounded`: none of these methods publishes a
/// continuation at all, so none declares a plan and none can walk.
#[test]
fn telegram_pagination_is_bounded() {
    for operation in telegram::connector().operations() {
        assert!(
            operation.project().query().len() <= 2,
            "{}: a Telegram request carries only the parameters its method documents",
            operation.id()
        );
    }
}

/// `telegram_effects_are_classified`: the reads are read-only by their method,
/// the send is at-most-once on Telegram's own silence (ADR 063), and the two
/// edits stay inventory-only because Telegram publishes nothing about repeating
/// them at all — an absence of a key is not the same as a known consequence.
#[test]
fn telegram_effects_are_classified() {
    let connector = telegram::connector();
    let expected = [
        ("message.send", EffectClass::AtMostOnce),
        ("message.edit_text", EffectClass::InventoryOnly),
        ("message.delete", EffectClass::InventoryOnly),
        ("chat.get", EffectClass::ReadOnly),
        ("chat.member_get", EffectClass::ReadOnly),
        ("file.get", EffectClass::ReadOnly),
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
    let send = operation("message.send")
        .effect()
        .cloned()
        .expect("classified");
    let evidence = send
        .no_idempotency_evidence()
        .expect("an at-most-once class carries the search that found no key");
    assert!(evidence.searched_documentation().contains("`idempot`"));
    assert!(
        evidence
            .repeat_produces()
            .contains("a second delivered message")
    );

    // The two edits *look* naturally idempotent and are not admitted as such:
    // the Bot API expresses them as POSTs and Telegram publishes no repeat
    // statement for either.
    for id in ["message.edit_text", "message.delete"] {
        assert_eq!(
            connector.admit_operation(id),
            Err(OperationRejection::InventoryOnly)
        );
        assert!(
            operation(id)
                .effect()
                .and_then(donat_connectors::sdk::Effect::inventory_reason)
                .is_some_and(|reason| reason.contains("POST")),
            "{id}: the recorded reason names the method the gate refuses"
        );
    }
}

/// `telegram_output_contract`: the declared pointers read Telegram's `ok`/
/// `result` envelope, and a missing required pointer is a validation failure.
#[test]
fn telegram_output_contract() {
    let send = operation("message.send");
    assert_eq!(
        send.decode_response(
            200,
            br#"{"ok":true,"result":{"message_id":11,"date":1700000000,"chat":{"id":-1001,"type":"supergroup"},"text":"hello"}}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({ "message_id": 11, "date": 1_700_000_000, "chat_id": -1001 })
    );
    assert_eq!(
        send.decode_response(200, br#"{"ok":false,"description":"Bad Request"}"#)
            .expect_err("a failed envelope does not satisfy the declared contract")
            .class(),
        ConnectorErrorClass::Validation
    );

    // `file_path` is documented Optional, so it is published as an explicit
    // null rather than demanded.
    assert_eq!(
        operation("file.get")
            .decode_response(
                200,
                br#"{"ok":true,"result":{"file_id":"a","file_unique_id":"u"}}"#
            )
            .expect("the optional pointers are published as explicit nulls"),
        json!({ "file_id": "a", "file_unique_id": "u", "file_size": null, "file_path": null })
    );
}

// ---------------------------------------------------------------------------
// Inbound (spec 013 §4)
// ---------------------------------------------------------------------------

/// Telegram's published inbound check, transcribed here: "the request will
/// contain a header 'X-Telegram-Bot-Api-Secret-Token' with the secret token as
/// content."
fn sign(_body: &[u8]) -> HeaderMap {
    inbound::headers(&[(telegram::SECRET_TOKEN_HEADER, inbound::WEBHOOK_SECRET)])
}

#[test]
fn telegram_signature_precedes_parse() {
    inbound::signature_precedes_parse(
        telegram::connector(),
        sign,
        inbound::headers(&[(telegram::SECRET_TOKEN_HEADER, "not-the-secret")]),
        WebhookRejection::InvalidSignature,
    );
    inbound::nothing_prints_the_secret(telegram::connector());
}

/// `telegram_signature_is_exact`, written out rather than shared, because
/// Telegram's scheme is the one in this batch that does **not** cover the body.
///
/// The secret decides the answer to the byte; the body does not enter it at all.
/// That is Telegram's published contract — the header is "useful to ensure that
/// the request comes from a webhook set by you", and nothing more — and this
/// test asserts the limitation rather than hiding it.
#[test]
fn telegram_signature_is_exact() {
    const BODY: &[u8] =
        br#"{"update_id":10000,"message":{"message_id":1,"date":1,"chat":{"id":1}}}"#;
    let connector = telegram::connector();

    assert_eq!(inbound::verify(connector, &sign(BODY), BODY), Ok(()));

    // One byte of the secret, in either direction, and a prefix of it.
    for candidate in [
        "donat-inbound-secret-sentinel-do-not-lop",
        "donat-inbound-secret-sentinel-do-not-lo",
        "donat-inbound-secret-sentinel-do-not-logg",
        "",
    ] {
        assert_eq!(
            inbound::verify(
                connector,
                &inbound::headers(&[(telegram::SECRET_TOKEN_HEADER, candidate)]),
                BODY,
            )
            .expect_err("a secret that differs in one byte is rejected"),
            WebhookRejection::InvalidSignature,
            "candidate {candidate}"
        );
    }

    // A different configured secret rejects the same delivery.
    assert_eq!(
        inbound::trigger(connector)
            .verify(
                &sign(BODY),
                BODY,
                &donat_connectors::sdk::Secret::new("another-secret"),
                inbound::NOW,
            )
            .expect_err("a delivery under another secret is rejected"),
        WebhookRejection::InvalidSignature
    );

    // And the limitation, asserted: the body is not covered, so one more byte
    // of it changes nothing. Replay protection for Telegram therefore has to
    // come from `update_id`, which is why the declaration records it.
    let mut modified = BODY.to_vec();
    modified.push(b' ');
    assert_eq!(
        inbound::verify(connector, &sign(BODY), &modified),
        Ok(()),
        "Telegram's shared-secret scheme authenticates the sender, not the bytes"
    );

    inbound::triggers_share_one_scheme(connector);
    inbound::events_match_triggers(connector, telegram::events());
    for event in telegram::events() {
        assert_eq!(
            event.event_identifier(),
            &EventIdentifier::BodyPointer("/update_id")
        );
    }
}

#[test]
fn telegram_comparison_is_constant_time() {
    inbound::comparison_is_constant_time("telegram.rs", &inbound::module_source("telegram"));
}

#[test]
fn telegram_body_limit_precedes_verification() {
    inbound::body_limit_precedes_verification(telegram::connector(), sign);
}
