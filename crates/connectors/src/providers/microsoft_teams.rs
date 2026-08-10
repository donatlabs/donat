//! Microsoft Teams channels and messages through the Microsoft Graph v1.0 API.
//!
//! Ground truth is Microsoft's own v1.0 reference on `learn.microsoft.com`,
//! read on 2026-08-10; the shared facts (origin, error envelope, throttling,
//! `@odata.nextLink`, permissions) are in
//! [`crate::providers::microsoft_graph`].
//!
//! * `GET /teams/{team-id}/channels/{channel-id}` — "Retrieve the properties
//!   and relationships of a channel."
//! * `GET /teams/{team-id}/channels` — "Retrieve the list of channels in this
//!   team."
//! * `POST /teams/{team-id}/channels` — "Create a new channel in a team, as
//!   specified in the request body."
//! * `GET /teams/{team-id}/channels/{channel-id}/messages` — "Retrieve the list
//!   of messages (without the replies) in a channel of a team."
//! * `POST /teams/{team-id}/channels/{channel-id}/messages` — "Send a new
//!   chatMessage in the specified channel or a chat."
//! * `GET /chats/{chat-id}/messages/{message-id}` — "Retrieve a single message
//!   or a message reply in a channel or a chat."
//! * `GET /chats/{chat-id}/messages` — "Retrieve the list of messages in a
//!   chat."
//! * `POST /chats/{chat-id}/messages` — "Send a new chatMessage in the
//!   specified chat."
//!
//! # Effect classification
//!
//! None of the three reference pages contains a statement about idempotency,
//! duplicate suppression, or the behaviour of a repeated request, so all three
//! writes are `AtMostOnce` (ADR 063) rather than provider-idempotent: each leaves
//! a second channel or a second message behind, and a Process reaches them only
//! by declaring `at_most_once` and a route for an outcome nobody can know. Each
//! is recorded in `providers/INVENTORY.md`.
//!
//! One near-miss is recorded there rather than dropped, because a reviewer
//! reading the same page will find it: *Send chatMessage in a chat* documents
//! that "The **createdDateTime** must be unique down to the millisecond within
//! the target chat. If a message with the same **createdDateTime** exists, the
//! request fails with `409 Conflict`." That is a uniqueness constraint and not
//! an idempotency key — it is documented **only** for the import/migration path,
//! which needs `Teamwork.Migrate.All` and a chat in migration mode, and a `409`
//! is a different outcome from the first call rather than the same one.
//!
//! `channel.create` fixes `membershipType` to Microsoft's own `standard`
//! literal rather than taking it from input. Two reasons, both published: a
//! shared channel answers "`202 Accepted` … and a link to the
//! teamsAsyncOperation" instead of the `201 Created` and channel body this
//! operation declares, and "Adding multiple owners results in a `400 Bad
//! Request` error code" is a private/shared-channel rule this declaration does
//! not carry. An operation named `channel.create` that could create three
//! different kinds of channel with three different answers would be describing
//! a request nobody reviewed.
//!
//! # Continuations
//!
//! "When the result set spans multiple pages, the response includes an
//! **@odata.nextLink** property with a URL for retrieving the next page of
//! results." All three collections here are walked through the one constructor
//! in [`microsoft_graph::next_link`], so a continuation off the compiled origin
//! is refused before a request is made.
//!
//! # `$top`, and the query options Teams does not support
//!
//! Channel messages: "Apply `$top` to specify the number of channel messages
//! returned per page in the response. The default page size is 20 messages. You
//! can extend up to 50 channel messages per page." Chat messages: "Maximum
//! allowed `$top` value is 50." Both pages then say "The other OData query
//! parameters aren't currently supported", which is why neither message
//! operation declares a `$select` mask — an unsupported query option is not a
//! parameter this connector may send — while the channel operations do, on
//! Microsoft's own performance advice: "Populating the **email** and **summary**
//! property for a channel is an expensive operation that results in slow
//! performance. Use `$select` to exclude the **email** and **summary** property."
//!
//! # The one header these operations declare
//!
//! `Prefer: include-unknown-enum-members` is optional on both message listings,
//! and it is exactly the header that makes this connector's declared
//! `message_type` output true: without it Microsoft returns
//! `"messageType": "unknownFutureValue"` in place of the real value, and with it
//! the same message returns `"messageType": "systemEventMessage"`. Publishing an
//! output whose value the request asked to have hidden would be a contract this
//! connector does not have, so **every operation that carries a `chatMessage`
//! declares it** — the two listings and the three single-message operations —
//! and it is declaration material, never derived from input. The channel
//! operations do not, because a `channel` has no evolvable enum in the fields
//! this connector selects.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use crate::providers::microsoft_graph::{self, PermissionRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "microsoft_teams";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

const CHANNEL_READ_BASIC: &str = "Channel.ReadBasic.All";
const CHANNEL_SETTINGS_READ: &str = "ChannelSettings.Read.All";
const CHANNEL_SETTINGS_READ_WRITE: &str = "ChannelSettings.ReadWrite.All";
const CHANNEL_CREATE: &str = "Channel.Create";
const CHANNEL_MESSAGE_READ: &str = "ChannelMessage.Read.All";
const CHANNEL_MESSAGE_SEND: &str = "ChannelMessage.Send";
const CHAT_READ: &str = "Chat.Read";
const CHAT_READ_WRITE: &str = "Chat.ReadWrite";
const CHAT_MESSAGE_SEND: &str = "ChatMessage.Send";
const DIRECTORY_READ: &str = "Directory.Read.All";
const DIRECTORY_READ_WRITE: &str = "Directory.ReadWrite.All";
const GROUP_READ: &str = "Group.Read.All";
const GROUP_READ_WRITE: &str = "Group.ReadWrite.All";

/// `channel-get` and `channel-list`: least privileged `Channel.ReadBasic.All`,
/// higher privileged the six the pages list. Microsoft's own note — "The
/// Group.Read.All, Group.ReadWrite.All, Directory.Read.All, and
/// Directory.ReadWrite.All permissions are supported only for backward
/// compatibility" — is why they are accepted but never asked for.
const CHANNEL_READ_PERMISSIONS: &[&str] = &[
    CHANNEL_READ_BASIC,
    CHANNEL_SETTINGS_READ,
    CHANNEL_SETTINGS_READ_WRITE,
    DIRECTORY_READ,
    DIRECTORY_READ_WRITE,
    GROUP_READ,
    GROUP_READ_WRITE,
];

/// `channel-post`: least privileged `Channel.Create`.
const CHANNEL_CREATE_PERMISSIONS: &[&str] =
    &[CHANNEL_CREATE, DIRECTORY_READ_WRITE, GROUP_READ_WRITE];

/// `channel-list-messages`. (The sibling `chatmessage-get` page publishes the
/// same set for a *channel* message; this connector's `chat_message.get`
/// declares the chat form, which has its own permissions.)
const CHANNEL_MESSAGE_READ_PERMISSIONS: &[&str] =
    &[CHANNEL_MESSAGE_READ, GROUP_READ, GROUP_READ_WRITE];

/// `chatmessage-post` for a channel.
const CHANNEL_MESSAGE_SEND_PERMISSIONS: &[&str] = &[CHANNEL_MESSAGE_SEND, GROUP_READ_WRITE];

/// `chat-list-messages` and `chatmessage-get` for a chat message: "Permissions
/// for chat — Delegated (work or school account): Chat.Read, Chat.ReadWrite."
const CHAT_READ_PERMISSIONS: &[&str] = &[CHAT_READ, CHAT_READ_WRITE];

/// `chat-post-messages`: least privileged `ChatMessage.Send`, higher privileged
/// `Chat.ReadWrite, Group.ReadWrite.All`.
const CHAT_MESSAGE_SEND_PERMISSIONS: &[&str] =
    &[CHAT_MESSAGE_SEND, CHAT_READ_WRITE, GROUP_READ_WRITE];

/// "Use `$select` to exclude the **email** and **summary** property to improve
/// performance."
const CHANNEL_FIELDS: &str =
    "id,displayName,description,membershipType,webUrl,createdDateTime,isArchived";

/// "The default page size is 20 messages. You can extend up to 50 channel
/// messages per page." / "Maximum allowed `$top` value is 50."
const MESSAGE_PAGE_SIZE: &str = "50";

/// The one header these operations declare; see the module documentation.
const PREFER_ENUMS: &str = "include-unknown-enum-members";

/// "membershipType — … The possible values are: `standard`, `private`,
/// `unknownFutureValue`, `shared`." This connector publishes the one whose
/// documented answer is the `201 Created` its output contract describes.
pub const STANDARD_MEMBERSHIP: &str = "standard";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::fixed(microsoft_graph::ORIGIN)
                    .expect("Microsoft's published Graph origin is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Microsoft Teams declarations are valid"))
            .build()
            .expect("the Microsoft Teams declaration is valid")
    });
    &CONNECTOR
}

/// The shared Microsoft Graph error map; see `providers/microsoft_graph.rs`.
pub fn error_map() -> &'static ErrorMap {
    microsoft_graph::error_map()
}

/// The continuation plan of each collection.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        microsoft_graph::next_link(microsoft_graph::ITEMS_POINTER)
            .expect("the Graph collection plan is valid")
    });
    match operation_id {
        "channel.list" | "channel_message.list" | "chat_message.list" => Some(&COLLECTION),
        _ => None,
    }
}

/// The delegated permissions one operation is authorized by, as its own Graph
/// reference page lists them.
pub fn permissions(operation_id: &str) -> Option<PermissionRequirement> {
    match operation_id {
        "channel.get" | "channel.list" => Some(PermissionRequirement::documented(
            CHANNEL_READ_BASIC,
            CHANNEL_READ_PERMISSIONS,
        )),
        "channel.create" => Some(PermissionRequirement::documented(
            CHANNEL_CREATE,
            CHANNEL_CREATE_PERMISSIONS,
        )),
        "channel_message.list" => Some(PermissionRequirement::documented(
            CHANNEL_MESSAGE_READ,
            CHANNEL_MESSAGE_READ_PERMISSIONS,
        )),
        "channel_message.create" => Some(PermissionRequirement::documented(
            CHANNEL_MESSAGE_SEND,
            CHANNEL_MESSAGE_SEND_PERMISSIONS,
        )),
        "chat_message.get" | "chat_message.list" => Some(PermissionRequirement::documented(
            CHAT_READ,
            CHAT_READ_PERMISSIONS,
        )),
        "chat_message.create" => Some(PermissionRequirement::documented(
            CHAT_MESSAGE_SEND,
            CHAT_MESSAGE_SEND_PERMISSIONS,
        )),
        _ => None,
    }
}

/// Decode one response.
///
/// None of these operations publishes a per-item failure shape, so the only
/// guard is the shared fail-closed one.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    microsoft_graph::decode(
        operation,
        status,
        headers,
        body,
        microsoft_graph::no_partial_failures,
    )
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The output contract of one channel resource, under [`CHANNEL_FIELDS`].
fn channel_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "display_name",
            "/displayName",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "description",
            "/description",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "membership_type",
            "/membershipType",
            ValueScalar::String,
            Required::No,
        )
        // A provider-chosen URL on `teams.microsoft.com`. It is data in the
        // output contract, and nothing here can spend it as a destination.
        .output_pointer("web_url", "/webUrl", ValueScalar::String, Required::No)
        .output_pointer(
            "created_at",
            "/createdDateTime",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "is_archived",
            "/isArchived",
            ValueScalar::Boolean,
            Required::No,
        )
}

/// The output contract of one `chatMessage` resource.
fn message_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "created_at",
            "/createdDateTime",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "message_type",
            "/messageType",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "body_content",
            "/body/content",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "body_content_type",
            "/body/contentType",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "from_user_id",
            "/from/user/id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "from_display_name",
            "/from/user/displayName",
            ValueScalar::String,
            Required::No,
        )
        // "chatId identifies the chat that contains this message" — `null` for
        // a channel message — and "channelIdentity identifies the team and
        // channel that contains this message", `null` for a chat message.
        .output_pointer("chat_id", "/chatId", ValueScalar::String, Required::No)
        .output_pointer(
            "channel_id",
            "/channelIdentity/channelId",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("web_url", "/webUrl", ValueScalar::String, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let channel_get = channel_outputs(
        common(Operation::get(
            "channel.get",
            "/v1.0/teams/{team_id}/channels/{channel_id}",
        ))
        .path_param("team_id", ValueScalar::String)
        .path_param("channel_id", ValueScalar::String)
        .query_static("$select", CHANNEL_FIELDS)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let channel_list = common(Operation::get(
        "channel.list",
        "/v1.0/teams/{team_id}/channels",
    ))
    .path_param("team_id", ValueScalar::String)
    .query_static("$select", CHANNEL_FIELDS)
    .success_statuses([StatusCode::OK])
    .output_pointer("channels", "/value", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "next_link",
        microsoft_graph::NEXT_LINK_POINTER,
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Create a new channel in a team, as specified in the request body."
    let channel_create = channel_outputs(
        common(Operation::post(
            "channel.create",
            "/v1.0/teams/{team_id}/channels",
        ))
        .path_param("team_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("displayName", JsonTemplate::input("display_name")),
            ("description", JsonTemplate::input("description")),
            (
                "membershipType",
                JsonTemplate::literal(json!(STANDARD_MEMBERSHIP)),
            ),
        ]))
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "Microsoft publishes the complete request contract of `POST /teams/{id}/channels` — a \
             display name, a description, a membership type, and the members of a private \
             channel — and no idempotency key, client request identifier, or deduplication \
             behaviour appears in it or anywhere else on the page",
        "a second channel, or an outcome Microsoft does not document at all: nothing is \
             published about creating a channel whose display name already exists",
    )?))
    .build()?;

    // "Retrieve the list of messages (without the replies) in a channel of a
    // team."
    let channel_message_list = common(Operation::get(
        "channel_message.list",
        "/v1.0/teams/{team_id}/channels/{channel_id}/messages",
    ))
    .path_param("team_id", ValueScalar::String)
    .path_param("channel_id", ValueScalar::String)
    .query_static("$top", MESSAGE_PAGE_SIZE)
    .static_header("Prefer", PREFER_ENUMS)
    .success_statuses([StatusCode::OK])
    .output_pointer("messages", "/value", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "next_link",
        microsoft_graph::NEXT_LINK_POINTER,
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Send a new chatMessage in the specified channel or a chat." "Only the
    // body property is mandatory; other properties are optional."
    let channel_message_create = message_outputs(
        common(Operation::post(
            "channel_message.create",
            "/v1.0/teams/{team_id}/channels/{channel_id}/messages",
        ))
        .path_param("team_id", ValueScalar::String)
        .path_param("channel_id", ValueScalar::String)
        .static_header("Prefer", PREFER_ENUMS)
        .body(JsonTemplate::object([(
            "body",
            JsonTemplate::object([
                ("content", JsonTemplate::input("content")),
                ("contentType", JsonTemplate::input("content_type")),
            ]),
        )]))
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the channel-message reference page enumerates its complete request contract and \
             publishes no idempotency key, client request identifier, or deduplication behaviour \
             of any kind",
        "a second message in the channel, with a new id, read by everyone in it — and \
             Microsoft's own caution that \"Only send messages that people will read\" is the \
             opposite of a contract that absorbs a duplicate",
    )?))
    .build()?;

    // "Retrieve a single message or a message reply in a channel or a chat."
    // "This method doesn't support the OData query parameters to customize the
    // response", so this operation declares none.
    let chat_message_get = message_outputs(
        common(Operation::get(
            "chat_message.get",
            "/v1.0/chats/{chat_id}/messages/{message_id}",
        ))
        .path_param("chat_id", ValueScalar::String)
        .path_param("message_id", ValueScalar::String)
        .static_header("Prefer", PREFER_ENUMS)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let chat_message_list = common(Operation::get(
        "chat_message.list",
        "/v1.0/chats/{chat_id}/messages",
    ))
    .path_param("chat_id", ValueScalar::String)
    .query_static("$top", MESSAGE_PAGE_SIZE)
    .static_header("Prefer", PREFER_ENUMS)
    .success_statuses([StatusCode::OK])
    .output_pointer("messages", "/value", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "next_link",
        microsoft_graph::NEXT_LINK_POINTER,
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Send a new chatMessage in the specified chat. This API can't create a
    // new chat; you must use the list chats method to retrieve the ID of an
    // existing chat before you can create a chat message."
    let chat_message_create = message_outputs(
        common(Operation::post(
            "chat_message.create",
            "/v1.0/chats/{chat_id}/messages",
        ))
        .path_param("chat_id", ValueScalar::String)
        .static_header("Prefer", PREFER_ENUMS)
        .body(JsonTemplate::object([(
            "body",
            JsonTemplate::object([
                ("content", JsonTemplate::input("content")),
                ("contentType", JsonTemplate::input("content_type")),
            ]),
        )]))
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the chat-message reference page enumerates its complete request contract and publishes \
             no idempotency key for it; the one uniqueness rule it does publish — a \
             `createdDateTime` unique to the millisecond, answered `409 Conflict` otherwise — \
             belongs to the import path, which needs `Teamwork.Migrate.All` and a chat in \
             migration mode",
        "a second message in the chat, with a new id",
    )?))
    .build()?;

    Ok(vec![
        channel_get,
        channel_list,
        channel_create,
        channel_message_list,
        channel_message_create,
        chat_message_get,
        chat_message_list,
        chat_message_create,
    ])
}
