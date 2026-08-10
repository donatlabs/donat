//! Discord's HTTP API, v10.
//!
//! Ground truth is Discord's own published documentation, read on 2026-08-10:
//!
//! * <https://docs.discord.com/developers/reference> — "Base URL:
//!   `https://discord.com/api`", "You should specify which version to use by
//!   including it in the request path like
//!   `https://discord.com/api/v{version_number}`", with version 10 marked
//!   *Available* and default, and "For all authentication types, authentication
//!   is performed with the `Authorization` HTTP header in the format
//!   `Authorization: TOKEN_TYPE TOKEN`", with the worked example
//!   `Authorization: Bot MTk4NjIyNDgzNDcxOTI1MjQ4.Cl2FMQ.…`.
//! * <https://docs.discord.com/developers/resources/message>,
//!   <https://docs.discord.com/developers/resources/channel>, and
//!   <https://docs.discord.com/developers/resources/guild> — every route,
//!   parameter, and field below.
//! * <https://docs.discord.com/developers/topics/opcodes-and-status-codes> —
//!   the HTTP response-code table this module's error map is built from.
//! * <https://docs.discord.com/developers/topics/rate-limits> — "In the case
//!   that a rate limit is exceeded, the API will return a HTTP 429 response code
//!   with a JSON body. Your application should rely on the `Retry-After` header
//!   or `retry_after` field to determine when to retry the request."
//!
//! # `Bot` is a scheme, not a spelling
//!
//! Discord's `TOKEN_TYPE` is part of the credential's identity: the same token
//! sent under RFC 6750's `Bearer` authenticates as a user rather than as the
//! bot, which is a different principal with different permissions. The SDK
//! therefore carries the scheme as declaration material —
//! `AuthPlan::api_key_authorization_scheme("Bot")` — exactly as it already
//! carries one for a *stored* OAuth2 token
//! ([[064-a-credentials-scheme-and-its-username-are-the-providers]]). See
//! `knowledgebase/declarative-saas/decisions/075-*`.
//!
//! # The gateway is not here
//!
//! Discord's event stream is a long-lived WebSocket, which this engine does not
//! open (spec 025 §5). This connector is the REST surface only, and it declares
//! no trigger: a Discord deployment that needs events needs a gateway client,
//! which is a different program.
//!
//! # A message list has no cursor a plan can walk
//!
//! `GET /channels/{channel.id}/messages` answers with a bare array and takes
//! `before` and `after` as *message ids* — "Get messages before this message
//! ID". The continuation is therefore the id of the last element of the page,
//! which no plan in the SDK's closed set can derive: every one of them reads a
//! declared pointer, a `Link` relation, or an offset, and none of them reads
//! "the last item's field". So this connector declares no plan at all, publishes
//! `before` as a caller input, and every attempt is one request
//! ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]] is the neighbouring
//! finding; this one is a cursor that is not in the response at all).
//!
//! # Effect classification
//!
//! **`message.send` is `InventoryOnly`, and the evidence is Discord's own.**
//! Discord publishes a deduplication mechanism for exactly this endpoint:
//!
//! * `nonce?` — "Can be used to verify a message was sent (up to 25
//!   characters). Value will appear in the Message Create event."
//! * `enforce_nonce?` — "If true and nonce is present, it will be checked for
//!   uniqueness in the past few minutes. If another message was created by the
//!   same author with the same nonce, that message will be returned and no new
//!   message will be created."
//!
//! That is a binding and a uniqueness scope, and it is **not** a retention.
//! Spec 023 §3 requires all three cited, and
//! [[073-a-retention-is-read-from-the-reference-that-owns-the-operation]] settles
//! what to do when the reference that owns the operation declines to give a
//! number: "silence there is a refusal rather than a licence to use the number
//! found elsewhere". "The past few minutes" is not a window a send horizon can
//! be derived from, and a class that promised deduplication for a window nobody
//! published would be the most expensive kind of wrong.
//!
//! ADR 063's at-most-once class does not reach it either, and this is the
//! sharper half: that class is admitted on **evidence of an absence**, and there
//! is no absence here — Discord publishes the mechanism. Reaching for the weaker
//! class to route around a missing number is the promotion-by-proximity ADR 042
//! exists to prevent, and ADR 073 refused exactly that move for
//! `paypal.refund.create`. `providers/INVENTORY.md` records the near-miss, and
//! the module says what Discord would have to publish to change it.
//!
//! A second, independent bar sits underneath: `nonce` is published as "up to 25
//! characters", and a durable activity's stable key is longer than that. Even
//! with a retention, this connector could not bind the key Discord publishes a
//! slot for without truncating it into a value that is no longer unique.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "discord";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "Base URL: `https://discord.com/api`."
const ORIGIN: &str = "https://discord.com";

/// The `TOKEN_TYPE` Discord publishes for a bot credential.
pub const AUTHORIZATION_SCHEME: &str = "Bot";

/// "`limit` — Max number of messages to return (1-100)."
const MESSAGE_PAGE_SIZE: &str = "100";

/// "`limit` — max number of members to return (1-1000)", with a published
/// default of 1. 100 is this declaration's choice: it is a value Discord admits
/// and a page the SDK's response ceiling carries.
const MEMBER_PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Discord's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::api_key_authorization_scheme(AUTHORIZATION_SCHEME)
                    .expect("Discord's published token type is a valid scheme"),
            ))
            .operations(operations().expect("the Discord declarations are valid"))
            .build()
            .expect("the Discord declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from Discord's published HTTP response-code table.
///
/// It is keyed on the status alone. Discord publishes a second, finer code — "our
/// API can also return more detailed error codes through a `code` key in the
/// JSON error response" — and this map deliberately does not read it: the table
/// has hundreds of values, Discord binds none of them to an HTTP status, and a
/// map keyed on an unbounded set would have holes in it. The status table is the
/// contract Discord publishes as a contract.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 (BAD REQUEST) — The request was improperly formatted, or the
            // server couldn't understand it."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 (UNAUTHORIZED) — The `Authorization` header was missing or
            // invalid."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "403 (FORBIDDEN) — The `Authorization` token you passed did not
            // have permission to the resource", "404 (NOT FOUND) — The resource
            // at the location specified doesn't exist", "405 (METHOD NOT
            // ALLOWED)". A `403` is deliberately not `authentication`: Discord
            // documents it as a permission on the resource, and re-authorizing
            // does not change it.
            .on_statuses([403, 404, 405], ConnectorErrorClass::Permanent)
            // "429 (TOO MANY REQUESTS) — You are being rate limited."
            .on_status(429, ConnectorErrorClass::Http429)
            // "502 (GATEWAY UNAVAILABLE) — There was not a gateway available to
            // process your request. Wait a bit and retry." and "5xx (SERVER
            // ERROR) — The server had an error processing your request."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Discord error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection: none, for the reason in the module
/// header — Discord's continuation is the id of the last item of a bare array.
pub fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The fields of one message, as Discord's message object declares them.
fn message_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "channel_id",
            "/channel_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("content", "/content", ValueScalar::String, Required::No)
        .output_pointer("timestamp", "/timestamp", ValueScalar::String, Required::No)
        .output_pointer(
            "edited_timestamp",
            "/edited_timestamp",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("author", "/author", ValueScalar::Json, Required::No)
}

/// The fields of one channel.
fn channel_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        // "the type of channel" — an integer in Discord's own object.
        .output_pointer("type", "/type", ValueScalar::Int64, Required::No)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("guild_id", "/guild_id", ValueScalar::String, Required::No)
        .output_pointer("topic", "/topic", ValueScalar::String, Required::No)
        .output_pointer("parent_id", "/parent_id", ValueScalar::String, Required::No)
}

/// Every operation this connector publishes.
///
/// The set is spec 025 §3's messaging surface and nothing else: send, read one,
/// read a page, read a channel, list a guild's channels, list its members.
/// Discord's threads, reactions, invites, roles, bans, and webhooks are its own
/// object model and are not ported here.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Post a message to a guild text or DM channel." The one required field is
    // `content` for an ordinary message — "Message contents (up to 2000
    // characters)".
    let message_send = message_outputs(
        common(Operation::post(
            "message.send",
            "/api/v10/channels/{channel_id}/messages",
        ))
        .path_param("channel_id", ValueScalar::String)
        .body(JsonTemplate::object([(
            "content",
            JsonTemplate::input("content"),
        )]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Discord publishes a deduplication mechanism for this endpoint — `nonce` (\"Can be used to \
         verify a message was sent (up to 25 characters)\") with `enforce_nonce` (\"If true and \
         nonce is present, it will be checked for uniqueness in the past few minutes. If another \
         message was created by the same author with the same nonce, that message will be \
         returned and no new message will be created\") — and publishes no retention for it. \
         \"The past few minutes\" is not a window, so `ProviderIdempotent::ExplicitKey` is \
         refused under ADR 073, and ADR 063's at-most-once class is refused because it is \
         admitted on evidence of an *absence* and Discord published a mechanism. The published \
         25-character ceiling on `nonce` is a second bar: a durable activity's stable key does \
         not fit it.",
    )?)
    .build()?;

    // "Retrieve a specific message in the channel."
    let message_get = message_outputs(
        common(Operation::get(
            "message.get",
            "/api/v10/channels/{channel_id}/messages/{message_id}",
        ))
        .path_param("channel_id", ValueScalar::String)
        .path_param("message_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Retrieves the messages in a channel." The declared output is the bare
    // array Discord answers with, read through the whole-document pointer.
    let message_list = common(Operation::get(
        "message.list",
        "/api/v10/channels/{channel_id}/messages",
    ))
    .path_param("channel_id", ValueScalar::String)
    .query_static("limit", MESSAGE_PAGE_SIZE)
    // "`before` — Get messages before this message ID." It is the caller's,
    // because it is the only continuation Discord publishes and no plan in the
    // SDK can spend it.
    .query_input("before", "before")
    .success_statuses([StatusCode::OK])
    // The collection is a bare JSON array, so the whole document is the output.
    .declared_output("messages", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Get a channel by ID."
    let channel_get = channel_outputs(
        common(Operation::get(
            "channel.get",
            "/api/v10/channels/{channel_id}",
        ))
        .path_param("channel_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Returns a list of guild channel objects. Does not include threads."
    // Discord publishes no pagination for it at all, so one request is the whole
    // answer rather than a page of one.
    let channel_list = common(Operation::get(
        "channel.list",
        "/api/v10/guilds/{guild_id}/channels",
    ))
    .path_param("guild_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    // The collection is a bare JSON array, so the whole document is the output.
    .declared_output("channels", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Returns a list of guild member objects that are members of the guild.
    // This endpoint requires the `GUILD_MEMBERS` Privileged Intent." `after` is
    // published as "the highest user id in the previous page", which is the same
    // shape as the message continuation and is the caller's for the same reason.
    let member_list = common(Operation::get(
        "member.list",
        "/api/v10/guilds/{guild_id}/members",
    ))
    .path_param("guild_id", ValueScalar::String)
    .query_static("limit", MEMBER_PAGE_SIZE)
    .query_input("after", "after")
    .success_statuses([StatusCode::OK])
    // The collection is a bare JSON array, so the whole document is the output.
    .declared_output("members", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        message_send,
        message_get,
        message_list,
        channel_get,
        channel_list,
        member_list,
    ])
}
