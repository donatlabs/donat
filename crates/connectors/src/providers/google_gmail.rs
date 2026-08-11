//! The Gmail API v1.
//!
//! Ground truth is Google's own discovery document,
//! `https://gmail.googleapis.com/$discovery/rest?version=v1`, read on
//! 2026-08-10 at revision `20260803`, plus *Handle Gmail API errors* for the
//! failure half. Every quoted sentence below is that document's own
//! `description` text.
//!
//! * `"baseUrl": "https://gmail.googleapis.com/"`.
//! * `users.messages.get` — "Gets the specified message."
//! * `users.messages.list` — "Lists the messages in the user's mailbox."
//! * `users.messages.send` — "Sends the specified message to the recipients in
//!   the `To`, `Cc`, and `Bcc` headers."
//! * `users.messages.modify` — "Modifies the labels and the Classification
//!   Label values on the specified message."
//! * `users.messages.trash` — "Moves the specified message to the trash."
//! * `users.drafts.get` / `users.drafts.list` — "Gets the specified draft." /
//!   "Lists the drafts in the user's mailbox."
//! * `users.labels.list` — "Lists all labels in the user's mailbox."
//! * `users.labels.create` — "Creates a label."
//! * `users.labels.delete` — "Immediately and permanently deletes the specified
//!   label and removes it from any messages and threads that it's applied to."
//! * `users.threads.get` / `users.threads.list` — "Gets the specified thread." /
//!   "Lists the threads in the user's mailbox."
//!
//! # The mailbox is always `me`
//!
//! Every Gmail path takes a `userId` whose documented default is the literal
//! `me`. Spec 014 §6 puts domain-wide delegation and service-account
//! impersonation out of scope, so `me` — the one account this instance's stored
//! credential was authorized for — is the only mailbox this connector can name,
//! and it is written into the path rather than declared as an input. An input
//! that could choose a mailbox would be an input choosing an identity.
//!
//! # Effect classification
//!
//! `label.delete` is `NaturalMethod`: a `DELETE` on `labels/{id}`, a fixed
//! resource identity, which Google documents as deleting the label
//! "Immediately and permanently". A repeat names a label that is gone and
//! answers `404`.
//!
//! `message.modify_labels` and `message.trash` are idempotent *in effect* —
//! adding a label twice leaves one label, trashing a trashed message leaves it
//! trashed — and are still `InventoryOnly`, because Google publishes both as
//! `POST`s and spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only.
//! ADR 063 does not reach them either: at-most-once is for a write whose repeat
//! leaves a second thing behind, and these two leave nothing, so what they need
//! is a class that permits the retry rather than one that forbids it.
//!
//! `message.send` and `label.create` are `AtMostOnce` (ADR 063): Gmail
//! publishes no idempotency key at all — neither `idempot` nor `dedup` occurs
//! in the discovery document — and a repeat delivers a second email or answers
//! `409` for a label name that already exists, which is a different outcome
//! from the first call rather than the same one. Both are reachable only from a
//! Process activity that declared `at_most_once` and a route for an outcome
//! nobody can know.
//!
//! # Continuations
//!
//! The three listings publish `nextPageToken`, "Token to retrieve the next page
//! of results in the list", declared as a token in the body so it can only be
//! spent as a query value on this connector's compiled origin. `labels.list`
//! publishes none — `ListLabelsResponse` has exactly one property, `labels` —
//! so it declares no plan.
//!
//! # Partial failures
//!
//! Gmail publishes no per-item failure shape for any operation declared here:
//! none of the response schemas carries an `errors` collection, and the batch
//! endpoints that could are out of this batch's scope. The only guard is
//! therefore the shared fail-closed one in `providers/google.rs`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::providers::google::{self, ScopeRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "google_gmail";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `"baseUrl": "https://gmail.googleapis.com/"`.
const ORIGIN: &str = "https://gmail.googleapis.com";

/// Gmail's full-access scope, which the discovery document lists first for
/// every method here.
const MAIL_GOOGLE_COM: &str = "https://mail.google.com/";
const GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
const GMAIL_METADATA: &str = "https://www.googleapis.com/auth/gmail.metadata";
const GMAIL_MODIFY: &str = "https://www.googleapis.com/auth/gmail.modify";
const GMAIL_SEND: &str = "https://www.googleapis.com/auth/gmail.send";
const GMAIL_COMPOSE: &str = "https://www.googleapis.com/auth/gmail.compose";
const GMAIL_LABELS: &str = "https://www.googleapis.com/auth/gmail.labels";
const GMAIL_ADDONS_MESSAGE_ACTION: &str =
    "https://www.googleapis.com/auth/gmail.addons.current.message.action";
const GMAIL_ADDONS_MESSAGE_METADATA: &str =
    "https://www.googleapis.com/auth/gmail.addons.current.message.metadata";
const GMAIL_ADDONS_MESSAGE_READONLY: &str =
    "https://www.googleapis.com/auth/gmail.addons.current.message.readonly";
const GMAIL_ADDONS_ACTION_COMPOSE: &str =
    "https://www.googleapis.com/auth/gmail.addons.current.action.compose";

/// `users.messages.get` and `users.threads.get`.
const RESOURCE_READ_SCOPES: &[&str] = &[
    GMAIL_READONLY,
    GMAIL_METADATA,
    GMAIL_MODIFY,
    MAIL_GOOGLE_COM,
    GMAIL_ADDONS_MESSAGE_ACTION,
    GMAIL_ADDONS_MESSAGE_METADATA,
    GMAIL_ADDONS_MESSAGE_READONLY,
];

/// `users.messages.list` and `users.threads.list`.
const LIST_SCOPES: &[&str] = &[
    GMAIL_READONLY,
    GMAIL_METADATA,
    GMAIL_MODIFY,
    MAIL_GOOGLE_COM,
];

/// `users.drafts.get` and `users.drafts.list`.
const DRAFT_READ_SCOPES: &[&str] = &[GMAIL_READONLY, GMAIL_COMPOSE, GMAIL_MODIFY, MAIL_GOOGLE_COM];

/// `users.messages.send`.
const SEND_SCOPES: &[&str] = &[
    GMAIL_SEND,
    GMAIL_COMPOSE,
    GMAIL_MODIFY,
    MAIL_GOOGLE_COM,
    GMAIL_ADDONS_ACTION_COMPOSE,
];

/// `users.messages.modify` and `users.messages.trash`.
const MODIFY_SCOPES: &[&str] = &[GMAIL_MODIFY, MAIL_GOOGLE_COM];

/// `users.labels.list`.
const LABEL_READ_SCOPES: &[&str] = &[
    GMAIL_READONLY,
    GMAIL_LABELS,
    GMAIL_METADATA,
    GMAIL_MODIFY,
    MAIL_GOOGLE_COM,
];

/// `users.labels.create` and `users.labels.delete`.
const LABEL_WRITE_SCOPES: &[&str] = &[GMAIL_LABELS, GMAIL_MODIFY, MAIL_GOOGLE_COM];

/// "This field defaults to 100. The maximum allowed value for this field is
/// 500." A listing page carries identifiers only, so the documented maximum
/// fits the SDK's response ceiling comfortably and costs fewer calls.
const PAGE_SIZE: &str = "500";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Google's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Gmail declarations are valid"))
            .build()
            .expect("the Gmail declaration is valid")
    });
    &CONNECTOR
}

/// The shared Google error map; see `providers/google.rs`.
pub fn error_map() -> &'static ErrorMap {
    google::error_map()
}

/// The continuation plan of each listing.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static MESSAGES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/messages", "/nextPageToken", "pageToken")
            .expect("the Gmail message listing plan is valid")
    });
    static DRAFTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/drafts", "/nextPageToken", "pageToken")
            .expect("the Gmail draft listing plan is valid")
    });
    static THREADS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/threads", "/nextPageToken", "pageToken")
            .expect("the Gmail thread listing plan is valid")
    });
    match operation_id {
        "message.list" => Some(&MESSAGES),
        "draft.list" => Some(&DRAFTS),
        "thread.list" => Some(&THREADS),
        _ => None,
    }
}

/// The scopes one operation is authorized by, as the discovery document lists
/// them for that exact method.
pub fn scopes(operation_id: &str) -> Option<ScopeRequirement> {
    match operation_id {
        "message.get" | "thread.get" => Some(ScopeRequirement::documented(
            GMAIL_READONLY,
            RESOURCE_READ_SCOPES,
        )),
        "message.list" | "thread.list" => {
            Some(ScopeRequirement::documented(GMAIL_READONLY, LIST_SCOPES))
        }
        "draft.get" | "draft.list" => Some(ScopeRequirement::documented(
            GMAIL_READONLY,
            DRAFT_READ_SCOPES,
        )),
        "message.send" => Some(ScopeRequirement::documented(GMAIL_SEND, SEND_SCOPES)),
        "message.modify_labels" | "message.trash" => {
            Some(ScopeRequirement::documented(GMAIL_MODIFY, MODIFY_SCOPES))
        }
        "label.list" => Some(ScopeRequirement::documented(
            GMAIL_READONLY,
            LABEL_READ_SCOPES,
        )),
        "label.create" | "label.delete" => Some(ScopeRequirement::documented(
            GMAIL_LABELS,
            LABEL_WRITE_SCOPES,
        )),
        _ => None,
    }
}

/// Decode one response. Gmail publishes no per-item failure shape for these
/// operations, so the only guard is the shared fail-closed one.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    google::decode(
        operation,
        status,
        headers,
        body,
        google::no_partial_failures,
    )
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The output contract of one message resource.
fn message_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("thread_id", "/threadId", ValueScalar::String, Required::Yes)
        .output_pointer("label_ids", "/labelIds", ValueScalar::Json, Required::No)
        .output_pointer("snippet", "/snippet", ValueScalar::String, Required::No)
        .output_pointer(
            "internal_date",
            "/internalDate",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "size_estimate",
            "/sizeEstimate",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer("payload", "/payload", ValueScalar::Json, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "`format` — The format to return the message in", whose documented values
    // are `minimal`, `full`, `raw`, and `metadata`. It is declared rather than
    // pinned because the four differ by orders of magnitude in size and only a
    // deployment knows which it needs.
    let message_get = message_outputs(
        common(Operation::get(
            "message.get",
            "/gmail/v1/users/me/messages/{id}",
        ))
        .path_param("id", ValueScalar::String)
        .query_input("format", "format")
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "`q` — Only return messages matching the specified query." An empty
    // string is Gmail's own "no filter", so a deployment that means every
    // message says so rather than omitting the slot.
    let message_list = common(Operation::get(
        "message.list",
        "/gmail/v1/users/me/messages",
    ))
    .query_input("q", "q")
    .query_static("maxResults", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    // Gmail omits `messages` entirely when a query matches nothing, which
    // is why the declaration does not require it.
    .output_pointer("messages", "/messages", ValueScalar::Json, Required::No)
    .output_pointer(
        "next_page_token",
        "/nextPageToken",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "result_size_estimate",
        "/resultSizeEstimate",
        ValueScalar::Int64,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Sends the specified message to the recipients in the `To`, `Cc`, and
    // `Bcc` headers." `raw` is Gmail's base64url-encoded RFC 2822 message.
    let message_send = message_outputs(
        common(Operation::post(
            "message.send",
            "/gmail/v1/users/me/messages/send",
        ))
        .body(JsonTemplate::object([("raw", JsonTemplate::input("raw"))]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in the `gmail:v1` discovery document",
        "a second delivered email with a new message id — the most visible external consequence \
             of a duplicate in this connector",
    )?))
    .build()?;

    // "Modifies the labels … on the specified message."
    let message_modify_labels = message_outputs(
        common(Operation::post(
            "message.modify_labels",
            "/gmail/v1/users/me/messages/{id}/modify",
        ))
        .path_param("id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("addLabelIds", JsonTemplate::input("add_label_ids")),
            ("removeLabelIds", JsonTemplate::input("remove_label_ids")),
        ]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Adding a label twice leaves one label, so this operation is idempotent in effect — but \
         Google publishes it as `POST .../messages/{id}/modify`, and spec 010 §7 admits \
         `NaturalMethod` for `PUT` and `DELETE` only. ADR 063 does not admit it either: \
         at-most-once is for a write whose repeat leaves a second thing behind, and adding a \
         label twice leaves one label.",
    )?)
    .build()?;

    // "Moves the specified message to the trash."
    let message_trash = message_outputs(
        common(Operation::post(
            "message.trash",
            "/gmail/v1/users/me/messages/{id}/trash",
        ))
        .path_param("id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Trashing a trashed message leaves it trashed, so this operation is idempotent in effect \
         — and Google publishes it as a `POST`, which spec 010 §7 does not admit. ADR 063 does \
         not admit it either: this operation needs a class that permits a retry, not one that \
         forbids it.",
    )?)
    .build()?;

    let draft_get = common(Operation::get(
        "draft.get",
        "/gmail/v1/users/me/drafts/{id}",
    ))
    .path_param("id", ValueScalar::String)
    .query_input("format", "format")
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("message", "/message", ValueScalar::Json, Required::No)
    .effect(Effect::read_only())
    .build()?;

    let draft_list = common(Operation::get("draft.list", "/gmail/v1/users/me/drafts"))
        .query_input("q", "q")
        .query_static("maxResults", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("drafts", "/drafts", ValueScalar::Json, Required::No)
        .output_pointer(
            "next_page_token",
            "/nextPageToken",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Lists all labels in the user's mailbox." `ListLabelsResponse` has one
    // property and no continuation.
    let label_list = common(Operation::get("label.list", "/gmail/v1/users/me/labels"))
        .success_statuses([StatusCode::OK])
        .output_pointer("labels", "/labels", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let label_create = common(Operation::post("label.create", "/gmail/v1/users/me/labels"))
        .body(JsonTemplate::object([(
            "name",
            JsonTemplate::input("name"),
        )]))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("type", "/type", ValueScalar::String, Required::No)
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::MachineReadableDescription,
            "neither `idempot` nor `dedup` occurs anywhere in the `gmail:v1` discovery document",
            "either a second label with a new id, or a `409` for the duplicate name — a different \
             outcome from the first call rather than the same one, so a repeat cannot tell \
             \"I created it\" from \"somebody else did\"",
        )?))
        .build()?;

    // "Immediately and permanently deletes the specified label and removes it
    // from any messages and threads that it's applied to."
    let label_delete = common(Operation::delete(
        "label.delete",
        "/gmail/v1/users/me/labels/{id}",
    ))
    .path_param("id", ValueScalar::String)
    // Google documents the successful response body as empty and publishes no
    // status code in the discovery document, so both of the statuses it uses
    // for an empty success are declared rather than one of them guessed.
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Google documents `users.labels.delete` as `DELETE /gmail/v1/users/{userId}/labels/{id}` — \
         a fixed resource identity — which \"Immediately and permanently deletes the specified \
         label\". A repeat names the same label, finds it gone, and answers `404`; it never \
         deletes a second label.",
    )?)
    .build()?;

    let thread_get = common(Operation::get(
        "thread.get",
        "/gmail/v1/users/me/threads/{id}",
    ))
    .path_param("id", ValueScalar::String)
    .query_input("format", "format")
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("messages", "/messages", ValueScalar::Json, Required::No)
    .output_pointer("snippet", "/snippet", ValueScalar::String, Required::No)
    .output_pointer(
        "history_id",
        "/historyId",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let thread_list = common(Operation::get("thread.list", "/gmail/v1/users/me/threads"))
        .query_input("q", "q")
        .query_static("maxResults", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("threads", "/threads", ValueScalar::Json, Required::No)
        .output_pointer(
            "next_page_token",
            "/nextPageToken",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        message_get,
        message_list,
        message_send,
        message_modify_labels,
        message_trash,
        draft_get,
        draft_list,
        label_list,
        label_create,
        label_delete,
        thread_get,
        thread_list,
    ])
}
