//! Slack's Web API — the provider whose failures arrive inside a `200 OK`.
//!
//! Ground truth is Slack's own published documentation, read on 2026-08-10:
//!
//! * <https://docs.slack.dev/apis/web-api/> — "all with URLs in the form
//!   `https://slack.com/api/METHOD_FAMILY.method`", "We prefer tokens to be sent
//!   in the `Authorization` HTTP header of your outbound requests" with "the
//!   string `Bearer` pre-pended to it, indicating the OAuth 2.0 authentication
//!   scheme", "All Web API responses contain a JSON object, which will always
//!   contain a top-level boolean property `ok` that indicates success or
//!   failure", the failure example `{"ok": false, "error": "something_bad"}`,
//!   and "You must explicitly set the `Content-type` HTTP header to
//!   `application/json`" for a JSON write.
//! * <https://docs.slack.dev/apis/web-api/rate-limits> — "HTTP/1.1 429 Too Many
//!   Requests" with a "`Retry-After` HTTP header containing the number of
//!   seconds until you can retry".
//! * <https://docs.slack.dev/apis/web-api/pagination> — "Cursor-paginated
//!   methods accept `cursor` and `limit` parameters", "Paginated responses
//!   include a top-level `response_metadata` object that includes a
//!   `next_cursor`", "An empty, null, or non-existent `next_cursor` in the
//!   response indicates no further results", and "The `limit` parameter maximum
//!   is `1000` and subject to change and may vary per method."
//! * The method references for
//!   [`chat.postMessage`](https://docs.slack.dev/reference/methods/chat.postMessage/),
//!   [`chat.update`](https://docs.slack.dev/reference/methods/chat.update/),
//!   [`chat.delete`](https://docs.slack.dev/reference/methods/chat.delete/),
//!   [`chat.getPermalink`](https://docs.slack.dev/reference/methods/chat.getPermalink/),
//!   [`conversations.list`](https://docs.slack.dev/reference/methods/conversations.list/),
//!   [`conversations.history`](https://docs.slack.dev/reference/methods/conversations.history/),
//!   [`conversations.replies`](https://docs.slack.dev/reference/methods/conversations.replies/),
//!   [`conversations.info`](https://docs.slack.dev/reference/methods/conversations.info/),
//!   [`users.info`](https://docs.slack.dev/reference/methods/users.info/),
//!   [`users.list`](https://docs.slack.dev/reference/methods/users.list/),
//!   [`users.lookupByEmail`](https://docs.slack.dev/reference/methods/users.lookupByEmail/),
//!   [`reactions.add`](https://docs.slack.dev/reference/methods/reactions.add/), and
//!   [`reactions.list`](https://docs.slack.dev/reference/methods/reactions.list/).
//!
//! # A `200` is not a success
//!
//! This is the connector spec 010 §9's error map exists for, and it is the
//! reason this module owns a [`decode`] of its own rather than running on the
//! declaration alone. Slack answers a rejected request with `200 OK` and
//! `{"ok": false, "error": "channel_not_found"}`; the HTTP status carries no
//! information at all. A declaration-driven runtime would call that a success,
//! extract the declared pointers from a body that has none of them, and report
//! a `validation` failure — or worse, report success for an operation that did
//! not happen.
//!
//! [`decode`] therefore asks Slack's own question before the declared contract
//! is read: `ok` must be present and `true`. `ok: false` routes the body through
//! this module's ordered error map, which is keyed on the `error` string
//! precisely because that is the machine-readable code Slack publishes. A body
//! with no `ok` at all is outside the published contract entirely and is an
//! `invariant` failure, not a success and not a guess.
//!
//! The same reading holds when the status *is* a failure: a `429` still reaches
//! the map, and its `ratelimited` code maps to the same class the status does,
//! so the two agree rather than race.
//!
//! # Effect classification
//!
//! Slack publishes **no** idempotency key, client-supplied request identifier,
//! or request deduplication for any method in this set. The complete published
//! argument list for `chat.postMessage` is `channel`, the message content, and
//! presentation options (`thread_ts`, `parse`, `unfurl_links`, `unfurl_media`,
//! `link_names`, `mrkdwn`, `reply_broadcast`, `metadata`, `username`, `icon_*`);
//! none of them is a key, and each call answers with a fresh `ts`, which is the
//! provider's own evidence that a repeat posts a second message.
//!
//! `message.update`, `message.delete` and `reaction.add` are inventory-only for
//! a second, structural reason as well: the Web API is not REST — every write is
//! a `POST` to a method name — and spec 010 §7 admits `NaturalMethod` for `PUT`
//! and `DELETE` only. `reactions.add` is the near-miss worth recording: Slack
//! documents an `already_reacted` error, so a repeat is in practice refused
//! rather than doubled, but that is an error string rather than a published
//! repeat-safety statement, and the method is a `POST`.
//!
//! Every read here is a `GET`, so each is read-only by its method.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "slack";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "all with URLs in the form `https://slack.com/api/METHOD_FAMILY.method`".
const ORIGIN: &str = "https://slack.com";

/// The page size every cursor-paginated read declares.
///
/// "The `limit` parameter maximum is `1000` and subject to change and may vary
/// per method" — so a declaration that asked for the documented maximum would be
/// asking for a number Slack says is not one number. 200 is inside every
/// per-method ceiling in this set (`conversations.history` documents 999) and is
/// what Slack's own pagination guidance recommends fetching at a time.
const PAGE_SIZE: &str = "200";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Slack's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Slack declarations are valid"))
            .build()
            .expect("the Slack declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// It is keyed on Slack's own machine-readable `error` string first and on the
/// HTTP status second, and that order is the point: the status is `200` for
/// almost every failure Slack reports, so a status-first map would classify a
/// rejected write as a success and a rate limit as nothing at all.
///
/// Provider prose never crosses this boundary — Slack's `error` is matched, not
/// forwarded, and the `needed`/`provided` scope hints and `warning` strings that
/// travel with it are never read.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "All Web API responses contain a JSON object, which will always
            // contain a top-level boolean property `ok`"; when it is `false`,
            // "the response will also contain an `error` property".
            .code_pointer("/error")
            // "`ratelimited` — The request has been ratelimited." The Web API
            // spells the same condition two ways across its method references,
            // and both are declared rather than guessed at.
            .on_code("ratelimited", ConnectorErrorClass::Http429)
            .on_code("rate_limited", ConnectorErrorClass::Http429)
            // The authentication family, verbatim from the shared errors table:
            // "`not_authed` — No authentication token provided",
            // "`invalid_auth` — Some aspect of authentication cannot be
            // validated", "`account_inactive` — Authentication token is for a
            // deleted user or workspace when using a `bot` token",
            // "`token_expired` — Authentication token has expired",
            // "`token_revoked` — Authentication token is for a deleted user or
            // workspace or the app has been removed", "`missing_scope` — The
            // token used is not granted the specific scope permissions required
            // to complete this request", "`no_permission` — The workspace token
            // used in this request does not have the permissions necessary to
            // complete this request".
            .on_code("not_authed", ConnectorErrorClass::Authentication)
            .on_code("invalid_auth", ConnectorErrorClass::Authentication)
            .on_code("account_inactive", ConnectorErrorClass::Authentication)
            .on_code("token_expired", ConnectorErrorClass::Authentication)
            .on_code("token_revoked", ConnectorErrorClass::Authentication)
            .on_code("missing_scope", ConnectorErrorClass::Authentication)
            .on_code("no_permission", ConnectorErrorClass::Authentication)
            .on_code("access_denied", ConnectorErrorClass::Authentication)
            .on_code("ekm_access_denied", ConnectorErrorClass::Authentication)
            .on_code(
                "not_allowed_token_type",
                ConnectorErrorClass::Authentication,
            )
            .on_code("org_login_required", ConnectorErrorClass::Authentication)
            .on_code(
                "two_factor_setup_required",
                ConnectorErrorClass::Authentication,
            )
            .on_code(
                "team_access_not_granted",
                ConnectorErrorClass::Authentication,
            )
            // The malformed-request family: "`invalid_arg_name`",
            // "`invalid_arguments`", "`invalid_array_arg`", "`invalid_charset`",
            // "`invalid_form_data`", "`invalid_post_type`",
            // "`missing_post_type`", and `chat.postMessage`'s own
            // "`invalid_blocks`", "`msg_too_long`", "`no_text`".
            .on_code("invalid_arg_name", ConnectorErrorClass::Validation)
            .on_code("invalid_arguments", ConnectorErrorClass::Validation)
            .on_code("invalid_array_arg", ConnectorErrorClass::Validation)
            .on_code("invalid_charset", ConnectorErrorClass::Validation)
            .on_code("invalid_form_data", ConnectorErrorClass::Validation)
            .on_code("invalid_post_type", ConnectorErrorClass::Validation)
            .on_code("missing_post_type", ConnectorErrorClass::Validation)
            .on_code("invalid_blocks", ConnectorErrorClass::Validation)
            .on_code("msg_too_long", ConnectorErrorClass::Validation)
            .on_code("no_text", ConnectorErrorClass::Validation)
            // "`fatal_error` — The server could not complete your operation(s)",
            // "`internal_error`", "`service_unavailable`", "`request_timeout`".
            .on_code("fatal_error", ConnectorErrorClass::Http5xx)
            .on_code("internal_error", ConnectorErrorClass::Http5xx)
            .on_code("service_unavailable", ConnectorErrorClass::Http5xx)
            .on_code("request_timeout", ConnectorErrorClass::Timeout)
            // The transport-level statuses, for the cases Slack answers with a
            // status rather than with an envelope: "HTTP/1.1 429 Too Many
            // Requests" and the ordinary gateway family.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Slack error map is a valid declaration")
    });
    &MAP
}

/// Whether Slack's own envelope reports success.
///
/// `Some(true)` and `Some(false)` are the two answers the published contract
/// admits; `None` means the body is not a JSON object with a boolean `ok`, which
/// is not something the Web API documents at all.
fn envelope_ok(body: &[u8]) -> Option<bool> {
    serde_json::from_slice::<JsonValue>(body)
        .ok()?
        .get("ok")?
        .as_bool()
}

/// Decode one Slack response: the status, then the envelope, then the declared
/// contract.
///
/// This is the whole of the module's runtime behaviour, and the order is the
/// contract. A `200` carrying `ok: false` can never reach
/// [`Operation::decode_response`], so there is no path by which a provider
/// failure is reported as an activity success.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    match envelope_ok(body) {
        Some(true) => operation.decode_response(status, body),
        Some(false) => Err(error_map().classify(status, headers, body)),
        None => Err(ConnectorFailure::invariant(
            "connector provider answered outside its declared contract",
        )),
    }
}

/// The continuation plan of each cursor-paginated read.
///
/// Every one of them is Slack's own documented cursor: the token is echoed back
/// verbatim in `cursor`, it is never parsed or rebuilt here, and the walk stops
/// exactly where Slack says it does — "An empty, null, or non-existent
/// `next_cursor` in the response indicates no further results", which is the
/// three cases the SDK's plan treats as the end.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    fn plan(items: &str) -> Pagination {
        Pagination::cursor(
            items,
            "cursor",
            "/response_metadata/next_cursor",
            "limit",
            PAGE_SIZE
                .parse()
                .expect("the declared page size is a number"),
        )
        .expect("the Slack cursor plan is valid")
    }
    static CHANNELS: LazyLock<Pagination> = LazyLock::new(|| plan("/channels"));
    static MESSAGES: LazyLock<Pagination> = LazyLock::new(|| plan("/messages"));
    static MEMBERS: LazyLock<Pagination> = LazyLock::new(|| plan("/members"));
    static ITEMS: LazyLock<Pagination> = LazyLock::new(|| plan("/items"));
    match operation_id {
        "conversation.list" => Some(&CHANNELS),
        "conversation.history" | "conversation.replies" => Some(&MESSAGES),
        "user.list" => Some(&MEMBERS),
        "reaction.list" => Some(&ITEMS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The two fields every paginated read publishes beside its collection.
fn cursor_output(builder: OperationBuilder) -> OperationBuilder {
    builder.output_pointer(
        "next_cursor",
        "/response_metadata/next_cursor",
        ValueScalar::String,
        Required::No,
    )
}

/// The search every write in this module stands on.
const NO_KEY: &str = "Slack publishes no idempotency key, client-supplied request identifier, or \
                      request deduplication anywhere in the Web API; the Web API is not REST, so \
                      every write is a POST to a method name and spec 010 §7's NaturalMethod is \
                      not reachable either";

/// One write whose repeat would leave a second thing behind (ADR 063).
///
/// The search is the module's, the consequence is the operation's, and both
/// are what a Process author accepts when they declare `at_most_once`.
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let message_post = common(Operation::post("message.post", "/api/chat.postMessage"))
        .body(JsonTemplate::object([
            ("channel", JsonTemplate::input("channel")),
            ("text", JsonTemplate::input("text")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("channel", "/channel", ValueScalar::String, Required::Yes)
        .output_pointer("ts", "/ts", ValueScalar::String, Required::Yes)
        .output_pointer("message", "/message", ValueScalar::Json, Required::No)
        .effect(at_most_once(
            "a second message in the channel with a new `ts`, which every successful \
             chat.postMessage answers with",
        )?)
        .build()?;

    let message_update = common(Operation::post("message.update", "/api/chat.update"))
        .body(JsonTemplate::object([
            ("channel", JsonTemplate::input("channel")),
            ("ts", JsonTemplate::input("ts")),
            ("text", JsonTemplate::input("text")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("channel", "/channel", ValueScalar::String, Required::Yes)
        .output_pointer("ts", "/ts", ValueScalar::String, Required::Yes)
        .output_pointer("text", "/text", ValueScalar::String, Required::No)
        .effect(Effect::inventory_only(NO_KEY)?)
        .build()?;

    let message_delete = common(Operation::post("message.delete", "/api/chat.delete"))
        .body(JsonTemplate::object([
            ("channel", JsonTemplate::input("channel")),
            ("ts", JsonTemplate::input("ts")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("channel", "/channel", ValueScalar::String, Required::Yes)
        .output_pointer("ts", "/ts", ValueScalar::String, Required::Yes)
        .effect(Effect::inventory_only(NO_KEY)?)
        .build()?;

    let message_permalink = common(Operation::get(
        "message.permalink",
        "/api/chat.getPermalink",
    ))
    .query_input("channel", "channel")
    .query_input("message_ts", "message_ts")
    .success_statuses([StatusCode::OK])
    .output_pointer("channel", "/channel", ValueScalar::String, Required::Yes)
    .output_pointer(
        "permalink",
        "/permalink",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    // "`types` — Mix and match channel types by providing a comma-separated list
    // ... Default: `public_channel`". The declaration asks for the value the
    // caller wants rather than inheriting a provider default silently.
    let conversation_list = cursor_output(
        common(Operation::get(
            "conversation.list",
            "/api/conversations.list",
        ))
        .query_input("types", "types")
        .query_static("limit", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("channels", "/channels", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only())
    .build()?;

    let conversation_history = cursor_output(
        common(Operation::get(
            "conversation.history",
            "/api/conversations.history",
        ))
        .query_input("channel", "channel")
        .query_static("limit", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("messages", "/messages", ValueScalar::Json, Required::Yes)
        .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::No),
    )
    .effect(Effect::read_only())
    .build()?;

    let conversation_replies = cursor_output(
        common(Operation::get(
            "conversation.replies",
            "/api/conversations.replies",
        ))
        .query_input("channel", "channel")
        // "`ts` — Unique identifier of either a thread's parent message or a
        // message in the thread."
        .query_input("ts", "ts")
        .query_static("limit", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("messages", "/messages", ValueScalar::Json, Required::Yes)
        .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::No),
    )
    .effect(Effect::read_only())
    .build()?;

    let conversation_info = common(Operation::get(
        "conversation.info",
        "/api/conversations.info",
    ))
    .query_input("channel", "channel")
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/channel/id", ValueScalar::String, Required::Yes)
    // A direct-message conversation has no `name`, so the declaration does not
    // claim one: Slack's own `conversations.info` example for an IM carries
    // `is_im: true` and no name at all.
    .output_pointer("name", "/channel/name", ValueScalar::String, Required::No)
    .output_pointer(
        "is_private",
        "/channel/is_private",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer(
        "is_archived",
        "/channel/is_archived",
        ValueScalar::Boolean,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let user_info = common(Operation::get("user.info", "/api/users.info"))
        .query_input("user", "user")
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/user/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/user/name", ValueScalar::String, Required::Yes)
        .output_pointer(
            "team_id",
            "/user/team_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "real_name",
            "/user/real_name",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("is_bot", "/user/is_bot", ValueScalar::Boolean, Required::No)
        .effect(Effect::read_only())
        .build()?;

    let user_list = cursor_output(
        common(Operation::get("user.list", "/api/users.list"))
            .query_static("limit", PAGE_SIZE)
            .success_statuses([StatusCode::OK])
            .output_pointer("members", "/members", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only())
    .build()?;

    let user_lookup_by_email = common(Operation::get(
        "user.lookup_by_email",
        "/api/users.lookupByEmail",
    ))
    // "`email` — An email address belonging to a user in the workspace."
    .query_input("email", "email")
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/user/id", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/user/name", ValueScalar::String, Required::Yes)
    .output_pointer(
        "team_id",
        "/user/team_id",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "`timestamp` — Timestamp of the message to add reaction to",
    // "`name` — Reaction (emoji) name." The documented success body is
    // `{"ok": true}` and nothing else, so that is the whole declared output.
    let reaction_add = common(Operation::post("reaction.add", "/api/reactions.add"))
        .body(JsonTemplate::object([
            ("channel", JsonTemplate::input("channel")),
            ("timestamp", JsonTemplate::input("timestamp")),
            ("name", JsonTemplate::input("name")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("ok", "/ok", ValueScalar::Boolean, Required::Yes)
        .effect(at_most_once(
            "not a second reaction but an `already_reacted` error, which is a different outcome \
             from the first call rather than the same one — so a repeat cannot tell a caller \
             whether it added the reaction or somebody else did",
        )?)
        .build()?;

    // "`user` — Show reactions made by this user. Defaults to the authed user."
    // Declared rather than inherited, for the same reason `types` is above.
    let reaction_list = cursor_output(
        common(Operation::get("reaction.list", "/api/reactions.list"))
            .query_input("user", "user")
            .query_static("limit", PAGE_SIZE)
            .success_statuses([StatusCode::OK])
            .output_pointer("items", "/items", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        message_post,
        message_update,
        message_delete,
        message_permalink,
        conversation_list,
        conversation_history,
        conversation_replies,
        conversation_info,
        user_info,
        user_list,
        user_lookup_by_email,
        reaction_add,
        reaction_list,
    ])
}
