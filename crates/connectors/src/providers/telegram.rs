//! The Telegram Bot API and its shared-secret webhook deliveries.
//!
//! Ground truth is Telegram's own published documentation, read on 2026-08-10
//! (the Bot API page carried "Bot API 10.2, July 14, 2026"):
//!
//! * <https://core.telegram.org/bots/api> — "All queries to the Telegram Bot API
//!   must be served over HTTPS and need to be presented in this form:
//!   `https://api.telegram.org/bot<token>/METHOD_NAME`", "We support **GET** and
//!   **POST** HTTP methods", "All methods in the Bot API are case-insensitive",
//!   and the response envelope: "The response contains a JSON object, which
//!   always has a Boolean field 'ok' … If 'ok' equals True, the request was
//!   successful and the result of the query can be found in the 'result' field."
//! * <https://core.telegram.org/bots/features> — "Keep your token secure and
//!   store it safely, it can be used by anyone to control your bot."
//! * <https://core.telegram.org/bots/faq> — the published rate guidance: "In a
//!   single chat, avoid sending more than one message per second. We may allow
//!   short bursts that go over this limit, but eventually you'll begin receiving
//!   429 errors."
//! * `setWebhook`, for the inbound half: "A secret token to be sent in a header
//!   'X-Telegram-Bot-Api-Secret-Token' in every webhook request, 1-256
//!   characters. Only characters A-Z, a-z, 0-9, _ and - are allowed. The header
//!   is useful to ensure that the request comes from a webhook set by you."
//!
//! # The credential is a path segment
//!
//! This is the batch's one `ApiKeyPathSegment` (spec 013 §1). Telegram publishes
//! no header form of authentication at all — the bot token is the first path
//! segment, behind the literal prefix `bot` — so the SDK's plan writes it there,
//! percent-encoded, and marks the rendered request as carrying a credential in
//! its URL. `RequestPlan`'s `Debug` then prints `<origin>/<redacted>` rather than
//! the URL, and `RequestPlan::redacted_url` is what a log line, a metric label, a
//! diagnostic, or a fingerprint may use. No connector code here ever reads or
//! formats the token.
//!
//! # What Telegram does not publish
//!
//! Three negatives shape this module, and each is recorded rather than filled in
//! from a community source.
//!
//! * **No error table.** Telegram publishes no list of HTTP statuses or
//!   `error_code` values, and says of `error_code` itself that "its contents are
//!   subject to change in the future". The error map is therefore keyed on
//!   statuses alone, with the classes HTTP itself defines, and never on
//!   `description` — which Telegram documents as "a human-readable description",
//!   i.e. prose.
//! * **No token grammar.** The token's shape is published only as two examples
//!   (`123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11`). This module therefore
//!   validates nothing about the token's contents; the SDK percent-encodes it so
//!   that whatever it contains stays inside one path segment.
//! * **No idempotency of any kind.** The words "idempotent" and "dedup" do not
//!   occur anywhere in the Bot API documentation, and the only "duplicate" in it
//!   is `getUpdates` advice about the inbound `offset`.
//!
//! # Effect classification
//!
//! `chat.get`, `chat.member_get`, and `file.get` are `GET`s and are read-only by
//! their method.
//!
//! `message.send` is `AtMostOnce` (ADR 063): Telegram publishes no idempotency
//! key anywhere and `sendMessage` answers with a fresh `Message` per call, so a
//! repeat delivers a second message — the consequence an operator accepts when
//! they write the opt-in.
//!
//! `message.edit_text` and `message.delete` stay `InventoryOnly`, and the reason
//! is the same for both: **Telegram publishes no statement about what a repeat
//! does.** ADR 063 admits a class on a *recorded consequence*, and there is none
//! to record here. Spec 013 §2 anticipated the two edits as
//! `NaturalMethod`, and the shape is right — an edit and a delete both name a
//! fixed `(chat_id, message_id)` — but ADR 042's gate admits *evidence*, not
//! shapes, and there are two independent reasons the evidence is not here. The
//! Bot API is not REST: `editMessageText` and `deleteMessage` are `POST`s, and
//! spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only. And Telegram
//! documents skip-if-missing behaviour for the *plural* `deleteMessages` ("If
//! some of the specified messages can't be found, they are skipped.") and for the
//! singular method says only "Returns True on success" — so the one adjacent
//! statement Telegram publishes is explicitly about a different method. Both
//! operations stay declared, typed, and tested, with the fact recorded on them.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::providers::inbound::{EventIdentifier, TriggerEvent};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec, Trigger};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::webhook::WebhookVerifier;

/// The connector name a deployment selects.
pub const NAME: &str = "telegram";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "All queries to the Telegram Bot API must be served over HTTPS."
const ORIGIN: &str = "https://api.telegram.org";

/// The literal that precedes the token in the path: "remember to prefix the
/// word 'bot' to your token."
pub const TOKEN_PREFIX: &str = "bot";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        let mut builder = Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Telegram's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::api_key_path_segment(TOKEN_PREFIX)
                    .expect("Telegram's published token prefix is valid"),
            ))
            .operations(operations().expect("the Telegram declarations are valid"));
        for event in events() {
            builder = builder.trigger(
                Trigger::webhook(event.provider_event(), VERSION, verification())
                    .expect("a Telegram trigger declaration is valid"),
            );
        }
        builder.build().expect("the Telegram declaration is valid")
    });
    &CONNECTOR
}

/// "`X-Telegram-Bot-Api-Secret-Token`" — the header `setWebhook`'s
/// `secret_token` is delivered in.
pub const SECRET_TOKEN_HEADER: &str = "X-Telegram-Bot-Api-Secret-Token";

/// Telegram's inbound verification.
///
/// It is the only shared-secret scheme in this batch: Telegram signs nothing.
/// The whole check is that the header carries the secret a deployment set with
/// `setWebhook`, and the SDK compares it in constant time. Because there is no
/// signature over the body and no timestamp, the raw bytes of a delivery are
/// authenticated only by the fact that the sender knew the secret — which is
/// exactly what Telegram claims for it ("useful to ensure that the request comes
/// from a webhook set by you") and no more.
pub fn verification() -> WebhookVerifier {
    WebhookVerifier::shared_secret_header(SECRET_TOKEN_HEADER)
        .expect("the Telegram secret-token header is valid")
}

/// The inbound events this connector declares (spec 013 §3).
///
/// The event identifier is `update_id`: "The update's unique identifier. Update
/// identifiers start from a certain positive number and increase sequentially.
/// This identifier becomes especially handy if you're using webhooks, since it
/// allows you to ignore repeated updates". Telegram also warns that "If there
/// are no new updates for at least a week, then identifier of the next update
/// will be chosen randomly instead of sequentially", so it is an identity and
/// never an ordering.
///
/// `message.from` is optional in Telegram's own table — "may be empty for
/// messages sent to channels" — and `message.text` is optional because a message
/// need not be text, so neither is declared required.
pub fn events() -> &'static [TriggerEvent] {
    static EVENTS: LazyLock<Vec<TriggerEvent>> = LazyLock::new(|| {
        vec![
            TriggerEvent::declare(
                "message",
                EventIdentifier::BodyPointer("/update_id"),
                [
                    (
                        "message_id",
                        "/message/message_id",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    ("date", "/message/date", ValueScalar::Int64, Required::Yes),
                    (
                        "chat_id",
                        "/message/chat/id",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    (
                        "from_id",
                        "/message/from/id",
                        ValueScalar::Int64,
                        Required::No,
                    ),
                    ("text", "/message/text", ValueScalar::String, Required::No),
                ],
            )
            .expect("the Telegram message event declaration is valid"),
            TriggerEvent::declare(
                "callback_query",
                EventIdentifier::BodyPointer("/update_id"),
                [
                    (
                        "callback_query_id",
                        "/callback_query/id",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "from_id",
                        "/callback_query/from/id",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    (
                        "chat_instance",
                        "/callback_query/chat_instance",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    // "Exactly one of the fields `data` or `game_short_name`
                    // will be present", so neither is required on its own.
                    (
                        "data",
                        "/callback_query/data",
                        ValueScalar::String,
                        Required::No,
                    ),
                ],
            )
            .expect("the Telegram callback_query event declaration is valid"),
        ]
    });
    &EVENTS
}

/// The ordered error map.
///
/// Telegram publishes no status table, so this map assigns each status the class
/// HTTP itself defines for it, and reads nothing from the body: `description` is
/// prose and `error_code`'s "contents are subject to change in the future". The
/// one documented machine-readable retry signal is `parameters.retry_after`,
/// which the SDK reads from the `Retry-After` header rather than the body — a
/// `429` therefore classifies as `Http429` and any retry delay Telegram sends as
/// a header is clamped by the SDK's own ceiling.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 409], ConnectorErrorClass::Permanent)
            // "eventually you'll begin receiving 429 errors."
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Telegram error map is a valid declaration")
    });
    &MAP
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// Every operation this connector publishes.
///
/// The reads are `GET`s with a query string, which is one of the four documented
/// ways to pass parameters, and the writes are `POST`s with an
/// `application/json` body, which is another. Method names are used exactly as
/// Telegram spells them even though "All methods in the Bot API are
/// case-insensitive".
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Use this method to send text messages. On success, the sent Message is
    // returned."
    let message_send = common(Operation::post("message.send", "/sendMessage"))
        .body(JsonTemplate::object([
            ("chat_id", JsonTemplate::input("chat_id")),
            ("text", JsonTemplate::input("text")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "message_id",
            "/result/message_id",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer("date", "/result/date", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "chat_id",
            "/result/chat/id",
            ValueScalar::Int64,
            Required::Yes,
        )
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "the Telegram Bot API publishes no idempotency key, client-supplied request \
             identifier, or deduplication anywhere; the strings `idempot` and `dedup` do not \
             occur on the Bot API, webhooks, features, or FAQ pages",
            "a second delivered message with a new `message_id`, which sendMessage answers with \
             on every call",
        )?))
        .build()?;

    // "On success, if the edited message is not an inline message, the edited
    // Message is returned, otherwise True is returned." The declaration reads
    // the `Message` form, which is the one a `chat_id`/`message_id` edit
    // produces.
    let message_edit_text = common(Operation::post("message.edit_text", "/editMessageText"))
        .body(JsonTemplate::object([
            ("chat_id", JsonTemplate::input("chat_id")),
            ("message_id", JsonTemplate::input("message_id")),
            ("text", JsonTemplate::input("text")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "message_id",
            "/result/message_id",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer(
            "chat_id",
            "/result/chat/id",
            ValueScalar::Int64,
            Required::Yes,
        )
        .effect(Effect::inventory_only(
            "an edit names a fixed chat and message id, but the Bot API expresses it as a POST — \
             which spec 010 §7 does not admit for NaturalMethod — and Telegram publishes no \
             statement at all about repeating one; the widely observed \"message is not modified\" \
             refusal appears nowhere in Telegram's own documentation",
        )?)
        .build()?;

    // "Returns True on success", with the documented 48-hour and permission
    // limitations. There is no `result` object to read, so the declaration
    // publishes the envelope's own `ok`.
    let message_delete = common(Operation::post("message.delete", "/deleteMessage"))
        .body(JsonTemplate::object([
            ("chat_id", JsonTemplate::input("chat_id")),
            ("message_id", JsonTemplate::input("message_id")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("ok", "/ok", ValueScalar::Boolean, Required::Yes)
        .output_pointer("result", "/result", ValueScalar::Boolean, Required::Yes)
        .effect(Effect::inventory_only(
            "deleteMessage names a fixed chat and message id, but the Bot API expresses it as a \
             POST — which spec 010 §7 does not admit for NaturalMethod — and Telegram documents \
             skip-if-missing for the plural deleteMessages (\"If some of the specified messages \
             can't be found, they are skipped.\") and nothing at all for the singular method",
        )?)
        .build()?;

    // "Returns a ChatFullInfo object on success."
    let chat_get = common(Operation::get("chat.get", "/getChat"))
        .query_input("chat_id", "chat_id")
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/result/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("type", "/result/type", ValueScalar::String, Required::Yes)
        .output_pointer("title", "/result/title", ValueScalar::String, Required::No)
        .output_pointer(
            "username",
            "/result/username",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Returns a ChatMember object on success." `ChatMember` is a union
    // discriminated by `status`, so the declaration reads the discriminant and
    // the member's identity rather than a shape that only one variant has.
    let chat_member_get = common(Operation::get("chat.member_get", "/getChatMember"))
        .query_input("chat_id", "chat_id")
        .query_input("user_id", "user_id")
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "status",
            "/result/status",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "user_id",
            "/result/user/id",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer(
            "is_bot",
            "/result/user/is_bot",
            ValueScalar::Boolean,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    // "On success, a File object is returned." `file_path` is documented as
    // Optional, so it is not required here; the download URL a caller would
    // build from it carries the bot token in *its* path too, which is why this
    // connector returns the path and never a URL.
    let file_get = common(Operation::get("file.get", "/getFile"))
        .query_input("file_id", "file_id")
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "file_id",
            "/result/file_id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "file_unique_id",
            "/result/file_unique_id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "file_size",
            "/result/file_size",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "file_path",
            "/result/file_path",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        message_send,
        message_edit_text,
        message_delete,
        chat_get,
        chat_member_get,
        file_get,
    ])
}
