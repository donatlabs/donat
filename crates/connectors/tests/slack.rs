//! Slack connector proofs (spec 016 §3), against the SDK's local provider stub.
//!
//! The proof this connector exists for is `slack_ok_false_is_a_failure`: Slack
//! reports almost every failure as `200 OK` with `{"ok": false, "error": …}`, so
//! "the status was 2xx" is not evidence of anything here.

use std::time::Duration;

use donat_connectors::providers::slack;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

const CHANNEL: &str = "C123ABC456";
const MESSAGE_TS: &str = "1503435956.000247";

fn operation(id: &str) -> &'static Operation {
    slack::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the slack declaration publishes {id}"))
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

/// Every operation of the declaration, with an input that satisfies it and a
/// documented success body that satisfies its output contract.
fn cases() -> Vec<(&'static str, JsonValue, JsonValue)> {
    vec![
        (
            "message.post",
            json!({ "channel": CHANNEL, "text": "Here's a message for you" }),
            json!({ "ok": true, "channel": CHANNEL, "ts": MESSAGE_TS,
                    "message": { "text": "Here's a message for you", "type": "message" } }),
        ),
        (
            "message.update",
            json!({ "channel": CHANNEL, "ts": MESSAGE_TS, "text": "updated" }),
            json!({ "ok": true, "channel": CHANNEL, "ts": MESSAGE_TS, "text": "updated" }),
        ),
        (
            "message.delete",
            json!({ "channel": CHANNEL, "ts": MESSAGE_TS }),
            json!({ "ok": true, "channel": CHANNEL, "ts": MESSAGE_TS }),
        ),
        (
            "message.permalink",
            json!({ "channel": CHANNEL, "message_ts": MESSAGE_TS }),
            json!({ "ok": true, "channel": CHANNEL,
                    "permalink": "https://ghostbusters.slack.com/archives/C1H9RESGA/p135854651500008" }),
        ),
        (
            "conversation.list",
            json!({ "types": "public_channel,private_channel" }),
            json!({ "ok": true, "channels": [], "response_metadata": { "next_cursor": "" } }),
        ),
        (
            "conversation.history",
            json!({ "channel": CHANNEL }),
            json!({ "ok": true, "messages": [], "has_more": false }),
        ),
        (
            "conversation.replies",
            json!({ "channel": CHANNEL, "ts": MESSAGE_TS }),
            json!({ "ok": true, "messages": [], "has_more": false }),
        ),
        (
            "conversation.info",
            json!({ "channel": CHANNEL }),
            json!({ "ok": true, "channel": { "id": CHANNEL, "name": "general",
                                             "is_private": false, "is_archived": false } }),
        ),
        (
            "user.info",
            json!({ "user": "W012A3CDE" }),
            json!({ "ok": true, "user": { "id": "W012A3CDE", "team_id": "T012AB3C4",
                                          "name": "spengler", "real_name": "Egon Spengler",
                                          "is_bot": false } }),
        ),
        (
            "user.list",
            json!({}),
            json!({ "ok": true, "members": [], "response_metadata": { "next_cursor": "" } }),
        ),
        (
            "user.lookup_by_email",
            json!({ "email": "spengler@ghostbusters.example.com" }),
            json!({ "ok": true, "user": { "id": "W012A3CDE", "team_id": "T012AB3C4",
                                          "name": "spengler" } }),
        ),
        (
            "reaction.add",
            json!({ "channel": CHANNEL, "timestamp": MESSAGE_TS, "name": "thumbsup" }),
            json!({ "ok": true }),
        ),
        (
            "reaction.list",
            json!({ "user": "W012A3CDE" }),
            json!({ "ok": true, "items": [], "response_metadata": { "next_cursor": "" } }),
        ),
    ]
}

/// `slack_request_shape`: exact method, path, query, headers, and body for every
/// operation the declaration publishes.
#[tokio::test]
async fn slack_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", "/api/chat.postMessage")
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("content-type", "application/json")
            .json_body(json!({ "channel": CHANNEL, "text": "Here's a message for you" }))
            .respond_json(
                200,
                json!({ "ok": true, "channel": CHANNEL, "ts": MESSAGE_TS }),
            ),
        Expectation::new("POST", "/api/chat.update")
            .json_body(json!({ "channel": CHANNEL, "ts": MESSAGE_TS, "text": "updated" }))
            .respond_json(
                200,
                json!({ "ok": true, "channel": CHANNEL, "ts": MESSAGE_TS }),
            ),
        Expectation::new("POST", "/api/chat.delete")
            .json_body(json!({ "channel": CHANNEL, "ts": MESSAGE_TS }))
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/chat.getPermalink")
            .query(&format!("channel={CHANNEL}&message_ts=1503435956%2E000247"))
            .no_body()
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/conversations.list")
            .query("types=public%5Fchannel%2Cprivate%5Fchannel&limit=200")
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/conversations.history")
            .query(&format!("channel={CHANNEL}&limit=200"))
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/conversations.replies")
            .query(&format!(
                "channel={CHANNEL}&ts=1503435956%2E000247&limit=200"
            ))
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/conversations.info")
            .query(&format!("channel={CHANNEL}"))
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/users.info")
            .query("user=W012A3CDE")
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/users.list")
            .query("limit=200")
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/users.lookupByEmail")
            .query("email=spengler%40ghostbusters%2Eexample%2Ecom")
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("POST", "/api/reactions.add")
            .json_body(json!({ "channel": CHANNEL, "timestamp": MESSAGE_TS, "name": "thumbsup" }))
            .respond_json(200, json!({ "ok": true })),
        Expectation::new("GET", "/api/reactions.list")
            .query("user=W012A3CDE&limit=200")
            .respond_json(200, json!({ "ok": true })),
    ])
    .await;

    for (id, input, _) in cases() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `slack_ok_false_is_a_failure` (spec 016 §3 proof 1): a `200` carrying a
/// provider error never becomes a success, and its code maps to exactly one
/// class.
///
/// The first half runs the whole declaration through it: for every operation,
/// the documented success body decodes and the same status with `ok: false`
/// fails. The second half walks Slack's own error strings.
#[tokio::test]
async fn slack_ok_false_is_a_failure() {
    for (id, _, success) in cases() {
        let operation = operation(id);
        let headers = HeaderMap::new();

        let decoded = slack::decode(
            operation,
            200,
            &headers,
            &serde_json::to_vec(&success).expect("a fixture body serializes"),
        )
        .expect("the documented success decodes");
        assert!(decoded.is_object(), "{id} decodes to its declared contract");

        // The same status, the same operation, one field different.
        let failure = slack::decode(
            operation,
            200,
            &headers,
            br#"{"ok":false,"error":"channel_not_found"}"#,
        )
        .expect_err("a 200 carrying ok:false is never a success");
        assert_eq!(failure.class(), ConnectorErrorClass::Permanent, "{id}");
        assert_eq!(failure.provider_status(), Some(200), "{id}");

        // A body Slack never publishes — no envelope at all — is outside the
        // contract rather than a success with missing fields.
        assert_eq!(
            slack::decode(operation, 200, &headers, b"{}")
                .expect_err("an envelope-less body is not a success")
                .class(),
            ConnectorErrorClass::Invariant,
            "{id}"
        );
        assert_eq!(
            slack::decode(operation, 200, &headers, b"<html>gateway</html>")
                .expect_err("a non-JSON body is not a success")
                .class(),
            ConnectorErrorClass::Invariant,
            "{id}"
        );
        // `ok` has to be the boolean Slack documents, not a truthy string.
        assert_eq!(
            slack::decode(operation, 200, &headers, br#"{"ok":"true"}"#)
                .expect_err("a non-boolean ok is not a success")
                .class(),
            ConnectorErrorClass::Invariant,
            "{id}"
        );
    }

    // Every documented error string reaches exactly one class, at the `200`
    // Slack actually sends it with.
    let classified = [
        ("ratelimited", ConnectorErrorClass::Http429),
        ("rate_limited", ConnectorErrorClass::Http429),
        ("not_authed", ConnectorErrorClass::Authentication),
        ("invalid_auth", ConnectorErrorClass::Authentication),
        ("account_inactive", ConnectorErrorClass::Authentication),
        ("token_expired", ConnectorErrorClass::Authentication),
        ("token_revoked", ConnectorErrorClass::Authentication),
        ("missing_scope", ConnectorErrorClass::Authentication),
        ("no_permission", ConnectorErrorClass::Authentication),
        ("access_denied", ConnectorErrorClass::Authentication),
        ("invalid_arguments", ConnectorErrorClass::Validation),
        ("invalid_blocks", ConnectorErrorClass::Validation),
        ("msg_too_long", ConnectorErrorClass::Validation),
        ("no_text", ConnectorErrorClass::Validation),
        ("fatal_error", ConnectorErrorClass::Http5xx),
        ("internal_error", ConnectorErrorClass::Http5xx),
        ("service_unavailable", ConnectorErrorClass::Http5xx),
        ("request_timeout", ConnectorErrorClass::Timeout),
        // Unmapped strings take the declared fallback rather than "unknown".
        ("channel_not_found", ConnectorErrorClass::Permanent),
        ("message_not_found", ConnectorErrorClass::Permanent),
        ("users_not_found", ConnectorErrorClass::Permanent),
        ("already_reacted", ConnectorErrorClass::Permanent),
    ];
    let post = operation("message.post");
    for (code, expected) in classified {
        let body = format!(
            r#"{{"ok":false,"error":"{code}","needed":"chat:write","provided":"{SECRET_SENTINEL}"}}"#
        );
        let failure = slack::decode(post, 200, &HeaderMap::new(), body.as_bytes())
            .expect_err("ok:false is a failure whatever the code");
        assert_eq!(failure.class(), expected, "error {code}");
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, code, "chat:write"] {
            assert!(!surface.contains(leaked), "error {code} leaked: {surface}");
        }
    }
}

/// The gate holds on the wire too, not only against a fixture body: a real
/// response from the stub, decoded through the same path a deployment uses.
#[tokio::test]
async fn slack_ok_false_is_a_failure_on_the_wire() {
    let stub = ProviderStub::start([
        Expectation::new("POST", "/api/chat.postMessage")
            .respond_json(200, json!({ "ok": false, "error": "channel_not_found" })),
        Expectation::new("POST", "/api/chat.postMessage").respond_json(
            200,
            json!({ "ok": true, "channel": CHANNEL, "ts": MESSAGE_TS }),
        ),
    ])
    .await;

    let input = json!({ "channel": CHANNEL, "text": "hello" });
    let refused = stub
        .send(render(&stub, "message.post", input.clone()))
        .await
        .expect("the stub answers");
    assert_eq!(refused.status.as_u16(), 200, "Slack answered 200");
    assert_eq!(
        slack::decode(
            operation("message.post"),
            refused.status.as_u16(),
            refused.headers(),
            refused.body(),
        )
        .expect_err("a 200 with ok:false is a failure")
        .class(),
        ConnectorErrorClass::Permanent
    );

    let accepted = stub
        .send(render(&stub, "message.post", input))
        .await
        .expect("the stub answers");
    assert_eq!(
        slack::decode(
            operation("message.post"),
            accepted.status.as_u16(),
            accepted.headers(),
            accepted.body(),
        )
        .expect("a 200 with ok:true decodes"),
        json!({ "channel": CHANNEL, "ts": MESSAGE_TS, "message": null })
    );
    stub.assert_satisfied();
}

/// `slack_auth_is_applied`: the bot token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else.
#[tokio::test]
async fn slack_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/api/users.info")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(
            200,
            json!({ "ok": true, "user": { "id": "W1", "name": "n" } }),
        )])
    .await;

    let request = render(&stub, "user.info", json!({ "user": "W1" }));
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
        slack::connector().credential(),
        slack::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `slack_rate_limit_is_classified` (spec 016 §3 proof 5): Slack's documented
/// rate-limit response reaches `http_429` with its retry hint clamped.
#[tokio::test]
async fn slack_rate_limit_is_classified() {
    // "HTTP/1.1 429 Too Many Requests" with "a `Retry-After` HTTP header
    // containing the number of seconds until you can retry."
    let stub = ProviderStub::start([
        Expectation::new("POST", "/api/chat.postMessage")
            .respond_header("retry-after", "30")
            .respond_json(429, json!({ "ok": false, "error": "ratelimited" })),
        // The same limit reported inside a `200`, which is what several methods
        // do; the two must reach the same class.
        Expectation::new("POST", "/api/chat.postMessage")
            .respond_json(200, json!({ "ok": false, "error": "ratelimited" })),
        // A provider that asks for a week is held to the SDK's ceiling.
        Expectation::new("POST", "/api/chat.postMessage")
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "ok": false, "error": "ratelimited" })),
    ])
    .await;

    let input = json!({ "channel": CHANNEL, "text": "hello" });
    let mut failures = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(&stub, "message.post", input.clone()))
            .await
            .expect("the stub answers");
        failures.push(
            slack::decode(
                operation("message.post"),
                response.status.as_u16(),
                response.headers(),
                response.body(),
            )
            .expect_err("a rate limit is a failure"),
        );
    }

    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(30)));
    assert_eq!(
        failures[1].class(),
        ConnectorErrorClass::Http429,
        "a rate limit reported inside a 200 is the same class as one reported by the status"
    );
    assert_eq!(failures[1].provider_status(), Some(200));
    assert_eq!(
        failures[2].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `slack_cursor_is_opaque_and_bounded` (spec 016 §3 proof 4): the cursor is
/// echoed verbatim, is never parsed or constructed here, and the loop stops at
/// every budget.
#[tokio::test]
async fn slack_cursor_is_opaque_and_bounded() {
    let plan = slack::pagination("conversation.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    // A cursor whose bytes are hostile in every direction a value can be
    // hostile: it is data, so none of it means anything here.
    const CURSOR: &str = "dGVhbTpDMDYxRkE1UEI=&limit=9999#../../etc/passwd";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/conversations.list")
            .query("types=public%5Fchannel&limit=200")
            .respond_json(
                200,
                json!({ "ok": true, "channels": [{ "id": "C1" }],
                        "response_metadata": { "next_cursor": CURSOR } }),
            ),
        Expectation::new("GET", "/api/conversations.list")
            // The cursor comes back percent-encoded and otherwise byte for byte.
            .query(
                "types=public%5Fchannel&limit=200&cursor=dGVhbTpDMDYxRkE1UEI%3D%26limit%3D9999%23%2E%2E%2F%2E%2E%2Fetc%2Fpasswd",
            )
            .respond_json(
                200,
                // "An empty, null, or non-existent `next_cursor` in the response
                // indicates no further results."
                json!({ "ok": true, "channels": [{ "id": "C2" }],
                        "response_metadata": { "next_cursor": "" } }),
            ),
    ])
    .await;
    let channels = plan
        .collect(
            render(
                &stub,
                "conversation.list",
                json!({ "types": "public_channel" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one cursor and stops on the empty one");
    assert_eq!(channels.len(), 2);
    stub.assert_satisfied();

    for id in [
        "message.post",
        "message.update",
        "message.delete",
        "message.permalink",
        "conversation.info",
        "user.info",
        "user.lookup_by_email",
        "reaction.add",
    ] {
        assert!(
            slack::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `slack_pagination_is_bounded`: the declared plan terminates, respects the
/// call, page, item, and byte budgets, and cannot leave the compiled origin.
#[tokio::test]
async fn slack_pagination_is_bounded() {
    let plan = slack::pagination("conversation.list").expect("the list declares a plan");

    // Each ceiling in turn, against a provider that never stops offering one.
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/conversations.list").respond_json(
                200,
                json!({ "ok": true, "channels": [{ "id": "C1" }, { "id": "C2" }],
                        "response_metadata": { "next_cursor": "more" } }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "conversation.list",
                    json!({ "types": "public_channel" }),
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

    // A cursor is spent as a query value on the compiled origin and can never
    // become a destination: a token that spells an absolute URL to another host
    // comes back percent-encoded on this connector's own origin.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/conversations.list").respond_json(
            200,
            json!({ "ok": true, "channels": [{ "id": "C1" }],
                    "response_metadata": { "next_cursor": "https://attacker.invalid/api/conversations.list" } }),
        ),
        Expectation::new("GET", "/api/conversations.list")
            .query(
                "types=public%5Fchannel&limit=200&cursor=https%3A%2F%2Fattacker%2Einvalid%2Fapi%2Fconversations%2Elist",
            )
            .respond_json(
                200,
                json!({ "ok": true, "channels": [], "response_metadata": { "next_cursor": "" } }),
            ),
    ])
    .await;
    let channels = plan
        .collect(
            render(
                &stub,
                "conversation.list",
                json!({ "types": "public_channel" }),
            ),
            &stub.origin(),
            &PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a cursor that spells a URL is spent as a query value");
    assert_eq!(channels.len(), 1);
    stub.assert_satisfied();
}

/// `slack_error_map`: every documented `error` string and every HTTP status
/// Slack answers with reaches exactly one closed class, with a Donat-owned
/// message and no provider text.
#[tokio::test]
async fn slack_error_map() {
    // The statuses Slack answers with rather than reporting in the envelope.
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];
    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", "/api/users.info")
            .respond_header("retry-after", "2")
            .respond_bytes(status, "<html>gateway</html>")])
        .await;
        let response = stub
            .send(render(&stub, "user.info", json!({ "user": "W1" })))
            .await
            .expect("the stub answers");
        let failure = slack::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            !format!("{} {}", failure.safe_message(), failure.diagnostic()).contains("gateway"),
            "status {status}"
        );
        stub.assert_satisfied();
    }

    // Every status reaches exactly one of the eight closed classes: there is no
    // response this connector answers with "unclassified".
    let headers = HeaderMap::new();
    for status in 100_u16..=599 {
        let failure = slack::error_map().classify(status, &headers, b"not json at all");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(!failure.safe_message().is_empty());
    }
}

/// `slack_effects_are_classified`: every operation carries a class, and every
/// write is inventory-only on Slack's own complete published argument list.
#[test]
fn slack_effects_are_classified() {
    let connector = slack::connector();
    let expected = [
        ("message.post", EffectClass::AtMostOnce),
        ("message.update", EffectClass::InventoryOnly),
        ("message.delete", EffectClass::InventoryOnly),
        ("message.permalink", EffectClass::ReadOnly),
        ("conversation.list", EffectClass::ReadOnly),
        ("conversation.history", EffectClass::ReadOnly),
        ("conversation.replies", EffectClass::ReadOnly),
        ("conversation.info", EffectClass::ReadOnly),
        ("user.info", EffectClass::ReadOnly),
        ("user.list", EffectClass::ReadOnly),
        ("user.lookup_by_email", EffectClass::ReadOnly),
        ("reaction.add", EffectClass::AtMostOnce),
        ("reaction.list", EffectClass::ReadOnly),
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
        connector.admit_operation("message.update"),
        Err(OperationRejection::InventoryOnly),
        "an update whose repeat sets the same text is not what the at-most-once class is for"
    );
    // ADR 063: the two writes that leave something behind are executable, and
    // each one carries the search that found no key together with what a second
    // send would produce.
    for (id, consequence) in [
        ("message.post", "a new `ts`"),
        ("reaction.add", "already_reacted"),
    ] {
        let effect = operation(id).effect().cloned().expect("classified");
        let evidence = effect
            .no_idempotency_evidence()
            .expect("an at-most-once class carries its absence evidence");
        assert!(
            evidence
                .searched_documentation()
                .contains("Slack publishes no idempotency key"),
            "{id}"
        );
        assert!(evidence.repeat_produces().contains(consequence), "{id}");
        assert!(connector.admit_operation(id).is_ok(), "{id}");
    }
}

/// `slack_output_contract`: the declared pointers read Slack's own envelope, and
/// a body that does not satisfy them is a validation failure rather than a null.
#[test]
fn slack_output_contract() {
    let info = operation("conversation.info");
    assert_eq!(
        slack::decode(
            info,
            200,
            &HeaderMap::new(),
            br#"{"ok":true,"channel":{"id":"C123ABC456","name":"general","is_private":false,"is_archived":false,"created":1449252889}}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({ "id": CHANNEL, "name": "general", "is_private": false, "is_archived": false })
    );
    // A direct message carries no `name`, which the declaration admits as absent
    // rather than requiring.
    assert_eq!(
        slack::decode(
            info,
            200,
            &HeaderMap::new(),
            br#"{"ok":true,"channel":{"id":"D123","is_im":true}}"#,
        )
        .expect("an IM has no name and the declaration does not claim one"),
        json!({ "id": "D123", "name": null, "is_private": null, "is_archived": null })
    );
    // A required pointer that is missing is a validation failure.
    assert_eq!(
        slack::decode(info, 200, &HeaderMap::new(), br#"{"ok":true,"channel":{}}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // A message timestamp is a JSON string in every Slack payload, and the
    // declaration types it as one.
    assert_eq!(
        slack::decode(
            operation("message.post"),
            200,
            &HeaderMap::new(),
            br#"{"ok":true,"channel":"C123ABC456","ts":1503435956.000247}"#,
        )
        .expect_err("a mistyped required pointer is a validation failure")
        .class(),
        ConnectorErrorClass::Validation
    );
}
