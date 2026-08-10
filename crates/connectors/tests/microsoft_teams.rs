//! Microsoft Teams connector proofs (spec 012 §3 plus spec 015 §3), against the
//! SDK's local provider stub.
//!
//! No test here reaches Microsoft, and no test carries a real credential. The
//! deployment-level proofs — `microsoft_teams_rotation_survives_crash` and the
//! startup half of the permission check — live in
//! `crates/server/tests/microsoft_365.rs`.

mod microsoft_graph_support;

use donat_connectors::providers::{microsoft_graph, microsoft_teams};
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

/// Teams identifiers carry `:` and `@`, which is why a path value is
/// percent-encoded rather than interpolated.
const TEAM: &str = "893075dd-2487-4122-925f-022c42e20265";
const CHANNEL: &str = "19:561fbdbbfca848a484f0a6f00ce9dbbd@thread.tacv2";
const CHAT: &str = "19:2da4c29f6d7041eca70b638b43d45437@thread.v2";
const MESSAGE: &str = "1616990032035";

const CHANNEL_PATH: &str = "/v1.0/teams/893075dd%2D2487%2D4122%2D925f%2D022c42e20265/channels/\
     19%3A561fbdbbfca848a484f0a6f00ce9dbbd%40thread%2Etacv2";
const CHANNELS_PATH: &str = "/v1.0/teams/893075dd%2D2487%2D4122%2D925f%2D022c42e20265/channels";
const CHAT_MESSAGES_PATH: &str =
    "/v1.0/chats/19%3A2da4c29f6d7041eca70b638b43d45437%40thread%2Ev2/messages";

const CHANNEL_SELECT: &str = "$select=id%2CdisplayName%2Cdescription%2CmembershipType%2CwebUrl%2C\
                              createdDateTime%2CisArchived";
const PREFER_ENUMS: &str = "include-unknown-enum-members";

fn operation(id: &str) -> &'static Operation {
    microsoft_teams::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the microsoft_teams declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    microsoft_graph_support::render(stub, operation(id), input)
}

fn channel_body() -> JsonValue {
    json!({
        "id": CHANNEL,
        "displayName": "Architecture Discussion",
        "description": "This channel is where we debate all future architecture plans",
        "membershipType": "standard",
        "webUrl": "https://teams.microsoft.com/l/channel/19%3a561fbdbbfca848a484f0a6f00ce9dbbd",
        "createdDateTime": "2026-08-10T12:00:00Z",
        "isArchived": false,
    })
}

fn message_body() -> JsonValue {
    json!({
        "id": MESSAGE,
        "createdDateTime": "2026-08-10T12:00:00Z",
        "messageType": "message",
        "body": { "contentType": "text", "content": "Hello World" },
        "from": { "user": { "id": "8ea0e38b-efb3-4757-924a-5f94061cf8c2",
                            "displayName": "Robin Kline" } },
        "chatId": CHAT,
        "channelIdentity": JsonValue::Null,
        "webUrl": JsonValue::Null,
    })
}

/// `microsoft_teams_request_shape`: exact method, path, query, headers, and
/// body for every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn microsoft_teams_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", CHANNEL_PATH)
            .query(CHANNEL_SELECT)
            .no_body()
            .respond_json(200, channel_body()),
        Expectation::new("GET", CHANNELS_PATH)
            .query(CHANNEL_SELECT)
            .respond_json(200, json!({ "value": [channel_body()] })),
        Expectation::new("POST", CHANNELS_PATH)
            .header("content-type", "application/json")
            .json_body(json!({
                "displayName": "Architecture Discussion",
                "description": "This channel is where we debate all future architecture plans",
                "membershipType": "standard",
            }))
            .respond_json(201, channel_body()),
        Expectation::new("GET", &format!("{CHANNEL_PATH}/messages"))
            .query("$top=50")
            .header("prefer", PREFER_ENUMS)
            .respond_json(200, json!({ "value": [message_body()] })),
        Expectation::new("POST", &format!("{CHANNEL_PATH}/messages"))
            .header("prefer", PREFER_ENUMS)
            .json_body(json!({ "body": { "content": "Hello World", "contentType": "text" } }))
            .respond_json(201, message_body()),
        Expectation::new("GET", &format!("{CHAT_MESSAGES_PATH}/1616990032035"))
            .query("")
            .header("prefer", PREFER_ENUMS)
            .respond_json(200, message_body()),
        Expectation::new("GET", CHAT_MESSAGES_PATH)
            .query("$top=50")
            .header("prefer", PREFER_ENUMS)
            .respond_json(200, json!({ "value": [message_body()] })),
        Expectation::new("POST", CHAT_MESSAGES_PATH)
            .header("prefer", PREFER_ENUMS)
            .json_body(json!({ "body": { "content": "Hello world", "contentType": "text" } }))
            .respond_json(201, message_body()),
        // A hostile team identifier stays one percent-encoded path segment.
        Expectation::new(
            "GET",
            "/v1.0/teams/%2E%2E%2F%2E%2E%2Fv1%2E0%2Fme%2FsendMail%3Fx%3D1/channels",
        )
        .respond_json(200, json!({ "value": [] })),
    ])
    .await;

    for (id, input) in [
        (
            "channel.get",
            json!({ "team_id": TEAM, "channel_id": CHANNEL }),
        ),
        ("channel.list", json!({ "team_id": TEAM })),
        (
            "channel.create",
            json!({
                "team_id": TEAM,
                "display_name": "Architecture Discussion",
                "description": "This channel is where we debate all future architecture plans",
            }),
        ),
        (
            "channel_message.list",
            json!({ "team_id": TEAM, "channel_id": CHANNEL }),
        ),
        (
            "channel_message.create",
            json!({
                "team_id": TEAM,
                "channel_id": CHANNEL,
                "content": "Hello World",
                "content_type": "text",
            }),
        ),
        (
            "chat_message.get",
            json!({ "chat_id": CHAT, "message_id": MESSAGE }),
        ),
        ("chat_message.list", json!({ "chat_id": CHAT })),
        (
            "chat_message.create",
            json!({ "chat_id": CHAT, "content": "Hello world", "content_type": "text" }),
        ),
    ] {
        let request = render(&stub, id, input);
        stub.send(request).await.expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "channel.list",
        json!({ "team_id": "../../v1.0/me/sendMail?x=1" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();

    // A channel is created `standard` by declaration: an input that spells the
    // membership type changes nothing on the wire.
    let fixed = operation("channel.create")
        .plan_request(
            &stub.origin(),
            &json!({
                "team_id": TEAM,
                "display_name": "d",
                "description": "x",
                "membership_type": "shared",
                "membershipType": "shared",
            }),
        )
        .expect("the declared request renders");
    assert_eq!(
        serde_json::from_slice::<JsonValue>(fixed.body()).expect("the body is JSON")["membershipType"],
        json!(microsoft_teams::STANDARD_MEMBERSHIP)
    );
}

/// `microsoft_teams_auth_is_applied`: the stored access token reaches the wire
/// as `Authorization: Bearer <token>`, and appears in no error or diagnostic.
#[tokio::test]
async fn microsoft_teams_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", CHANNEL_PATH)
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, channel_body())])
    .await;

    let request = render(
        &stub,
        "channel.get",
        json!({ "team_id": TEAM, "channel_id": CHANNEL }),
    );
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let failure = microsoft_teams::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        microsoft_teams::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    assert!(
        microsoft_teams::connector()
            .credential()
            .fields()
            .is_empty()
    );
    stub.assert_satisfied();

    let mut bare = operation("channel.get")
        .plan_request(
            &stub.origin(),
            &json!({ "team_id": TEAM, "channel_id": CHANNEL }),
        )
        .expect("the declared request renders");
    assert_eq!(
        AuthPlan::oauth2_authorization_code()
            .apply(&Credential::from_fields([]), &mut bare, None)
            .expect_err("an unauthorized Graph request is never sent")
            .class(),
        ConnectorErrorClass::Invariant
    );
}

/// `microsoft_teams_error_map`: every documented failure reaches exactly one of
/// the eight classes, with a Donat-owned message and no provider text.
#[tokio::test]
async fn microsoft_teams_error_map() {
    for (status, code, expected) in documented_failures() {
        let stub = ProviderStub::start([
            Expectation::new("GET", CHANNEL_PATH).respond_json(status, graph_error(code))
        ])
        .await;
        let response = stub
            .send(render(
                &stub,
                "channel.get",
                json!({ "team_id": TEAM, "channel_id": CHANNEL }),
            ))
            .await
            .expect("the stub answers");

        let failure = microsoft_teams::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert!(
            microsoft_teams::decode(
                operation("channel.get"),
                status,
                response.headers(),
                response.body(),
            )
            .is_err(),
            "status {status} is not a declared success"
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `microsoft_teams_odata_error_is_typed`.
#[test]
fn microsoft_teams_odata_error_is_typed() {
    assert_odata_error_is_typed_by_code();

    let failure = microsoft_teams::decode(
        operation("channel.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"error":{"code":"accessDenied","message":"Forbidden"}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(
        failure.code(),
        microsoft_graph::SUCCESS_CARRIES_ERROR.code()
    );
}

/// `microsoft_teams_throttling_is_classified`.
#[tokio::test]
async fn microsoft_teams_throttling_is_classified() {
    assert_throttling_is_classified(
        "GET",
        CHANNEL_PATH,
        operation("channel.get"),
        json!({ "team_id": TEAM, "channel_id": CHANNEL }),
    )
    .await;
}

/// `microsoft_teams_next_link_stays_on_origin` (and
/// `microsoft_teams_pagination_is_bounded`).
#[tokio::test]
async fn microsoft_teams_next_link_stays_on_origin() {
    for (id, path, input) in [
        ("channel.list", CHANNELS_PATH, json!({ "team_id": TEAM })),
        (
            "channel_message.list",
            &format!("{CHANNEL_PATH}/messages") as &str,
            json!({ "team_id": TEAM, "channel_id": CHANNEL }),
        ),
        (
            "chat_message.list",
            CHAT_MESSAGES_PATH,
            json!({ "chat_id": CHAT }),
        ),
    ] {
        let plan = microsoft_teams::pagination(id)
            .unwrap_or_else(|| panic!("{id} declares a continuation plan"));
        assert_next_link_stays_on_origin(plan, operation(id), input, "GET", path, "/value").await;
    }

    for id in ["channel.get", "chat_message.get", "channel.create"] {
        assert!(microsoft_teams::pagination(id).is_none(), "{id}");
    }
}

/// `microsoft_teams_consistency_headers_are_declared`.
#[test]
fn microsoft_teams_consistency_headers_are_declared() {
    assert_headers_are_static(
        microsoft_teams::connector(),
        &[
            ("channel_message.list", &[("prefer", PREFER_ENUMS)]),
            ("channel_message.create", &[("prefer", PREFER_ENUMS)]),
            ("chat_message.get", &[("prefer", PREFER_ENUMS)]),
            ("chat_message.list", &[("prefer", PREFER_ENUMS)]),
            ("chat_message.create", &[("prefer", PREFER_ENUMS)]),
        ],
    );

    // An operation that publishes `message_type` must have asked for it: an
    // output whose value the request told Microsoft to hide would be a contract
    // this connector does not have. The two listings carry `chatMessage`s
    // inside `/value` rather than through a pointer, and the table above holds
    // them to the same header.
    for operation in microsoft_teams::connector().operations() {
        let projection = operation.project();
        if projection
            .outputs()
            .iter()
            .any(|output| output.name() == "message_type")
        {
            assert!(
                projection
                    .headers()
                    .iter()
                    .any(|header| header.name() == "prefer"),
                "`{}` publishes `message_type` without asking for the real value",
                operation.id()
            );
        }
    }
}

/// `microsoft_teams_effects_are_classified`.
#[test]
fn microsoft_teams_effects_are_classified() {
    assert_effects(
        microsoft_teams::connector(),
        &[
            ("channel.get", EffectClass::ReadOnly),
            ("channel.list", EffectClass::ReadOnly),
            ("channel.create", EffectClass::AtMostOnce),
            ("channel_message.list", EffectClass::ReadOnly),
            ("channel_message.create", EffectClass::AtMostOnce),
            ("chat_message.get", EffectClass::ReadOnly),
            ("chat_message.list", EffectClass::ReadOnly),
            ("chat_message.create", EffectClass::AtMostOnce),
        ],
    );

    assert_eq!(
        microsoft_teams::connector().admit_operation("team.list"),
        Err(OperationRejection::Undeclared)
    );
}

/// The permission table is complete and per operation group.
#[test]
fn microsoft_teams_permissions_are_declared_per_operation_group() {
    let connector = microsoft_teams::connector();
    let least =
        microsoft_graph::declared_permissions(connector.operations(), microsoft_teams::permissions)
            .expect("every operation declares the permissions Microsoft documents for it");
    assert_eq!(
        least,
        vec![
            "Channel.ReadBasic.All",
            "Channel.Create",
            "ChannelMessage.Read.All",
            "ChannelMessage.Send",
            "Chat.Read",
            "ChatMessage.Send",
        ]
    );

    // Reading channels never asks for a permission that could send a message.
    let reads = ["channel.get".to_owned(), "channel.list".to_owned()];
    assert!(
        microsoft_graph::permission_report(
            microsoft_teams::permissions,
            &reads,
            &["Channel.ReadBasic.All".to_owned()],
        )
        .is_empty()
    );
    let with_send = [reads.as_slice(), &["chat_message.create".to_owned()]].concat();
    assert_eq!(
        microsoft_graph::permission_report(
            microsoft_teams::permissions,
            &with_send,
            &["Channel.ReadBasic.All".to_owned()],
        )
        .missing,
        vec![("chat_message.create".to_owned(), "ChatMessage.Send")]
    );
}

/// `microsoft_teams_output_contract`.
#[test]
fn microsoft_teams_output_contract() {
    let get = operation("chat_message.get");
    assert_eq!(
        microsoft_teams::decode(
            get,
            200,
            &reqwest::header::HeaderMap::new(),
            &serde_json::to_vec(&message_body()).expect("a fixture body serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": MESSAGE,
            "created_at": "2026-08-10T12:00:00Z",
            "message_type": "message",
            "body_content": "Hello World",
            "body_content_type": "text",
            "from_user_id": "8ea0e38b-efb3-4757-924a-5f94061cf8c2",
            "from_display_name": "Robin Kline",
            "chat_id": CHAT,
            "channel_id": null,
            "web_url": null,
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );

    for body in [
        br#"{"createdDateTime":"2026-08-10T12:00:00Z"}"#.as_slice(),
        br#"{"id":null}"#.as_slice(),
        br#"{"id":7}"#.as_slice(),
        br#"not json at all"#.as_slice(),
    ] {
        assert_eq!(
            microsoft_teams::decode(get, 200, &reqwest::header::HeaderMap::new(), body)
                .expect_err("a missing, mistyped, or unparseable body is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }
}
