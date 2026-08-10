//! Discord connector proofs (spec 025 §4), against the SDK's local provider
//! stub.
//!
//! This file carries spec 025 §4's second addition —
//! `discord_message_send_is_classified_from_documentation` — whose evidence is a
//! quotation from Discord's own documentation rather than a guess.

use std::time::Duration;

use donat_connectors::providers::discord;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const CHANNEL_ID: &str = "41771983423143937";
const MESSAGE_ID: &str = "41771983423143999";
const GUILD_ID: &str = "41771983423143936";

fn operation(id: &str) -> &'static Operation {
    discord::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the discord declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_authorization_scheme(discord::AUTHORIZATION_SCHEME)
        .expect("the declared scheme is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn message() -> JsonValue {
    json!({
        "id": MESSAGE_ID,
        "channel_id": CHANNEL_ID,
        "content": "Hello, World!",
        "timestamp": "2026-08-02T09:00:00.000000+00:00",
        "edited_timestamp": null,
        "author": { "id": "80351110224678912", "username": "donat" },
    })
}

fn channel() -> JsonValue {
    json!({
        "id": CHANNEL_ID,
        "type": 0,
        "name": "general",
        "guild_id": GUILD_ID,
        "topic": "Announcements",
        "parent_id": null,
    })
}

/// Every operation, with an input that satisfies it and the documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "message.send",
            json!({ "channel_id": CHANNEL_ID, "content": "Hello, World!" }),
        ),
        (
            "message.get",
            json!({ "channel_id": CHANNEL_ID, "message_id": MESSAGE_ID }),
        ),
        (
            "message.list",
            json!({ "channel_id": CHANNEL_ID, "before": MESSAGE_ID }),
        ),
        ("channel.get", json!({ "channel_id": CHANNEL_ID })),
        ("channel.list", json!({ "guild_id": GUILD_ID })),
        ("member.list", json!({ "guild_id": GUILD_ID, "after": "0" })),
    ]
}

/// `discord_request_shape`: exact method, path, query, headers, and body for
/// every operation, each of them under the published `/api/v10` prefix.
#[tokio::test]
async fn discord_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", &format!("/api/v10/channels/{CHANNEL_ID}/messages"))
            .query("")
            .json_body(json!({ "content": "Hello, World!" }))
            .respond_json(200, message()),
        Expectation::new(
            "GET",
            &format!("/api/v10/channels/{CHANNEL_ID}/messages/{MESSAGE_ID}"),
        )
        .query("")
        .no_body()
        .respond_json(200, message()),
        Expectation::new("GET", &format!("/api/v10/channels/{CHANNEL_ID}/messages"))
            .query(&format!("limit=100&before={MESSAGE_ID}"))
            .respond_json(200, json!([message()])),
        Expectation::new("GET", &format!("/api/v10/channels/{CHANNEL_ID}"))
            .query("")
            .respond_json(200, channel()),
        Expectation::new("GET", &format!("/api/v10/guilds/{GUILD_ID}/channels"))
            .query("")
            .respond_json(200, json!([channel()])),
        Expectation::new("GET", &format!("/api/v10/guilds/{GUILD_ID}/members"))
            .query("limit=100&after=0")
            .respond_json(200, json!([{ "user": { "id": "1" }, "roles": [] }])),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v10/"),
            "{id} renders a published Discord path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `discord_auth_is_applied`: the bot token reaches the wire under Discord's own
/// `TOKEN_TYPE`, never as `Bearer`, and appears nowhere else.
#[tokio::test]
async fn discord_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/api/v10/channels/{CHANNEL_ID}"))
                .header("authorization", &format!("Bot {SECRET_SENTINEL}"))
                .respond_json(200, channel()),
        ])
        .await;

    let request = render(&stub, "channel.get", json!({ "channel_id": CHANNEL_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(
        !applied
            .to_str()
            .expect("a bot credential is visible ASCII")
            .starts_with("Bearer "),
        "the same token under `Bearer` authenticates as a different principal"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        discord::connector().credential(),
        discord::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `discord_error_map`: every documented status reaches exactly one closed
/// class, and none of Discord's prose crosses the boundary.
#[tokio::test]
async fn discord_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Permanent),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v10/channels/{CHANNEL_ID}"),
        )
        .respond_json(
            status,
            json!({
                "code": 50_035,
                "message": format!("acme-guild rejected token {SECRET_SENTINEL}"),
                "errors": { "content": { "_errors": [{ "code": "BASE_TYPE_REQUIRED" }] } },
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "channel.get",
                json!({ "channel_id": CHANNEL_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = discord::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-guild", "BASE_TYPE_REQUIRED"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `discord_rate_limit_is_classified`: the documented `429` is retryable, its
/// published `Retry-After` becomes the hint, and a hostile one is clamped.
#[tokio::test]
async fn discord_rate_limit_is_classified() {
    let limited =
        json!({ "message": "You are being rate limited.", "retry_after": 64.57, "global": false });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v10/channels/{CHANNEL_ID}"))
            .respond_header("retry-after", "65")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/api/v10/channels/{CHANNEL_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "channel.get",
                json!({ "channel_id": CHANNEL_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(discord::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        Some(Duration::from_secs(65)),
        "Discord's own published example is `Retry-After: 65`"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `discord_pagination_is_bounded`: this connector declares no plan, and its
/// collections are bounded by the page size the declaration pins instead.
#[tokio::test]
async fn discord_pagination_is_bounded() {
    for operation in discord::connector().operations() {
        assert!(
            discord::pagination(operation.id()).is_none(),
            "`{}` declares no plan: Discord's continuation is the last item's id",
            operation.id()
        );
    }

    // The bound that is left is the declared page size, and it is on the wire.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/v10/channels/{CHANNEL_ID}/messages"),
    )
    .query(&format!("limit=100&before={MESSAGE_ID}"))
    .respond_json(200, json!([message()]))])
    .await;
    let response = stub
        .send(render(
            &stub,
            "message.list",
            json!({ "channel_id": CHANNEL_ID, "before": MESSAGE_ID }),
        ))
        .await
        .expect("the stub answers");
    assert_eq!(response.status.as_u16(), 200);
    assert_eq!(stub.received(), 1, "one attempt is one request");
    stub.assert_satisfied();
}

/// `discord_message_send_is_classified_from_documentation` (spec 025 §4
/// addition 2): whatever the classification, the evidence is a quotation from
/// Discord's own documentation.
///
/// Discord publishes a deduplication mechanism and does **not** publish its
/// retention, so the operation is `InventoryOnly`: `ExplicitKey` needs the
/// window (ADR 073), and ADR 063's at-most-once class needs an *absence* there
/// is not one of.
#[test]
fn discord_message_send_is_classified_from_documentation() {
    let send = operation("message.send");
    assert_eq!(send.effect_class(), Some(EffectClass::InventoryOnly));
    assert!(!send.is_executable());
    assert_eq!(
        discord::connector().admit_operation("message.send"),
        Err(OperationRejection::InventoryOnly)
    );
    assert!(
        send.idempotency_binding().is_none(),
        "a class with no window binds no key"
    );

    let reason = send
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("the classification records its evidence");
    // The binding, quoted.
    assert!(
        reason.contains("Can be used to verify a message was sent (up to 25 characters)"),
        "{reason}"
    );
    // The uniqueness scope and the behaviour, quoted.
    assert!(
        reason.contains(
            "it will be checked for uniqueness in the past few minutes. If another message was \
             created by the same author with the same nonce, that message will be returned and no \
             new message will be created"
        ),
        "{reason}"
    );
    // And the missing third leg, named.
    assert!(reason.contains("is not a window"), "{reason}");
    assert!(reason.contains("ADR 073"), "{reason}");
    assert!(reason.contains("absence"), "{reason}");

    // The published slot is not one this connector could fill even with a
    // window: a durable activity's stable key is longer than Discord's ceiling.
    assert!(reason.contains("25-character ceiling"), "{reason}");
    let activity_key = "01J0Z3S4T5V6W7X8Y9ZABCDEFG-attempt-1";
    assert!(
        activity_key.len() > 25,
        "the engine's own key does not fit the slot Discord publishes"
    );

    // No request this connector renders carries a nonce it did not declare.
    let origin = donat_connectors::sdk::Origin::parse("https://discord.com")
        .expect("the published origin is valid");
    let request = send
        .plan_request(
            &origin,
            &json!({ "channel_id": CHANNEL_ID, "content": "Hello, World!" }),
        )
        .expect("the declared request renders");
    let body = String::from_utf8(request.body().to_vec()).expect("a JSON body is UTF-8");
    assert_eq!(body, r#"{"content":"Hello, World!"}"#);
    assert!(!body.contains("nonce"));
}

/// `discord_effects_are_classified`: every operation carries a class, and the
/// only mutation this connector publishes is unreachable.
#[test]
fn discord_effects_are_classified() {
    let connector = discord::connector();
    let expected = [
        ("message.send", EffectClass::InventoryOnly),
        ("message.get", EffectClass::ReadOnly),
        ("message.list", EffectClass::ReadOnly),
        ("channel.get", EffectClass::ReadOnly),
        ("channel.list", EffectClass::ReadOnly),
        ("member.list", EffectClass::ReadOnly),
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

    // The gateway is not served, and no trigger is declared for it.
    assert!(connector.triggers().is_empty());
}

/// `discord_output_contract`: the declared pointers read Discord's own message
/// and channel objects, and a bare collection is published whole.
#[test]
fn discord_output_contract() {
    let get = operation("message.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&message()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": MESSAGE_ID,
            "channel_id": CHANNEL_ID,
            "content": "Hello, World!",
            "timestamp": "2026-08-02T09:00:00.000000+00:00",
            "edited_timestamp": JsonValue::Null,
            "author": { "id": "80351110224678912", "username": "donat" },
        })
    );
    // "Because Snowflake IDs are up to 64 bits in size … they are always
    // returned as strings in the HTTP API to prevent integer overflows in some
    // languages", so a number there is a contract violation.
    assert_eq!(
        get.decode_response(200, br#"{"id":41771983423143999}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"content":"hi"}"#)
            .expect_err("an answer with no id is not a message")
            .class(),
        ConnectorErrorClass::Validation
    );

    // A bare array is the whole output, exactly as Discord answers it.
    let list = operation("message.list");
    assert_eq!(
        list.decode_response(200, br#"[{"id":"1"},{"id":"2"}]"#)
            .expect("a bare collection is the whole document"),
        json!([{ "id": "1" }, { "id": "2" }])
    );
    // A non-success status is never a success, whatever the body says.
    assert_eq!(
        list.decode_response(403, b"[]")
            .expect_err("an undeclared status is never a success")
            .class(),
        ConnectorErrorClass::Permanent
    );
}
