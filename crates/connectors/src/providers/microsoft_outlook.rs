//! Outlook mail, calendar, and contacts through the Microsoft Graph v1.0 API.
//!
//! Ground truth is Microsoft's own v1.0 reference on `learn.microsoft.com`,
//! read on 2026-08-10. Every quoted sentence below is from the reference page
//! of the operation it appears under; the shared facts (origin, error envelope,
//! throttling, `@odata.nextLink`, permissions) are in
//! [`crate::providers::microsoft_graph`].
//!
//! * `GET /me/messages/{id}` — "Retrieve the properties and relationships of a
//!   message object."
//! * `GET /me/messages` — "Get the messages in the signed-in user's mailbox".
//! * `POST /me/sendMail` — "Send the message specified in the request body …
//!   this method returns `202 Accepted` response code. It doesn't return
//!   anything in the response body."
//! * `POST /me/messages/{id}/move` — "Move a message to another folder within
//!   the specified user's mailbox. **This creates a new copy of the message in
//!   the destination folder and removes the original message.**"
//! * `PATCH /me/messages/{id}` — "Update the properties of a message object."
//! * `DELETE /me/messages/{id}` — "Delete a message in the specified user's
//!   mailbox".
//! * `POST /me/messages` — "Create a draft of a new message… By default, this
//!   operation saves the draft in the Drafts folder."
//! * `POST /me/messages/{id}/send` — "Send an existing draft message."
//! * `GET /me/events/{id}`, `GET /me/events`, `POST /me/events`,
//!   `PATCH /me/events/{id}`, `DELETE /me/events/{id}`.
//! * `GET /me/contacts/{id}`, `GET /me/contacts`, `GET /me/mailFolders`.
//!
//! # Effect classification
//!
//! Two operations are executable mutations, both `NaturalMethod`:
//! `message.delete` and `event.delete`. Each is a `DELETE` against a fixed
//! resource identity — `/me/messages/{id}`, `/me/events/{id}` — which Microsoft
//! documents as removing *that* item and answering "`204 No Content` … It
//! doesn't return anything in the response body". A repeat names the same
//! identity and so cannot remove a second item; the answer it gets is either
//! the same `204` or `itemNotFound`, which this connector classifies
//! `permanent`. Microsoft publishes no sentence about the repeat itself, and
//! this module says so rather than implying one.
//!
//! Everything else that mutates is classified for a documented reason, all
//! recorded in `providers/INVENTORY.md`:
//!
//! * `message.update` is a `PATCH` whose own page defines partial-merge
//!   semantics — "supply the values for relevant fields that should be updated.
//!   Existing properties that are not included in the request body will
//!   maintain their previous values **or be recalculated based on changes to
//!   other property values**" — which spec 010 §7 admits for neither mutating
//!   class, and whose repeat sets the same value again, so ADR 063 leaves it
//!   `InventoryOnly` too.
//! * `event.update` is the same `PATCH` shape with one difference that decides
//!   it: Microsoft documents that an update "sends a meeting update" to
//!   attendees, so a repeat *is* observable outside the resource. It is
//!   `AtMostOnce`.
//! * `message.move` is documented as creating a *new copy* and removing the
//!   original, so the message identity changes and a repeat cannot find its
//!   subject. It is `AtMostOnce`.
//! * `message.send`, `draft.send`, and `draft.create` emit or create something
//!   new each time, and are `AtMostOnce`.
//! * `event.create` is the exception, and stays `InventoryOnly`: Microsoft
//!   publishes a client-supplied deduplicating key for it, and a key a
//!   connector could bind is not something ADR 063 lets a deployment step past.
//!
//! `event.create` is the batch's sharpest near-miss and is recorded as one.
//! Microsoft publishes `transactionId` — "A custom identifier specified by a
//! client app for the server to avoid redundant POST operations in case of
//! client retries to create the same event" — and its own example "sets the
//! **transactionId** property to reduce unnecessary retries on the server".
//! That is a client-supplied key with a documented deduplicating purpose, and
//! it is still not `ProviderIdempotent::ExplicitKey`, because
//! `ExplicitKeyEvidence::documented` also requires a published minimum
//! retention window to keep a send horizon inside, and Microsoft publishes
//! none — not on the event resource, not on the create page. A key whose
//! retention is unknown cannot bound a durable retry.
//!
//! # Continuations
//!
//! Every collection here is walked by `@odata.nextLink`, through the one
//! constructor in [`microsoft_graph::next_link`], so a continuation that points
//! at another host, scheme, or port is refused before a request is made.
//! Microsoft documents a default page size of 10 for messages ("Use `$top` to
//! customize the page size, within the range of 1 and 1000") and for mail
//! folders, and publishes none for events or contacts; the page size this
//! connector pins is its own declaration either way.
//!
//! # Two headers this connector always sends, and why they are static
//!
//! Outlook item ids are not stable. *Outlook immutable identifiers*: "their IDs
//! change. It doesn't happen often, only if the item is moved, but it can cause
//! real problems for apps that store IDs offline for later use", and
//! `message.move` is exactly such a move. The remedy Microsoft publishes is a
//! request header — `Prefer: IdType="ImmutableId"` — with the warning that
//! "**This header only applies to the request it is included with. If you want
//! to always use immutable IDs, you must include this header with every API
//! request.**"
//!
//! So every operation here that names or returns an Outlook item id declares
//! it, statically. It is declaration material exactly like a path: an id shape
//! that could be switched per request from operation input would let one
//! Process store an id under one shape and another read it under the other,
//! which is the wrong-answer failure mode rather than a failure. `folder.list`
//! does not declare it, because Microsoft says "Container types (mailFolder,
//! calendar, etc.) don't support immutable ID", and `message.send` does not,
//! because it neither takes nor returns an item id.
//!
//! The same header carries `outlook.body-content-type="text"`, which is what
//! makes the declared `body_content` output one shape: "If the header is not
//! specified, the **body** and **uniqueBody** properties are returned in HTML
//! format." An operation whose output contract says "text" and whose request
//! did not ask for text would be publishing a shape it does not have.
//!
//! # What a declared input means here
//!
//! Every input slot the SDK renders is required at render, so these operations
//! declare only what Microsoft documents as required, and take Microsoft's own
//! defaults for the rest. The `$select` masks are static declaration material
//! for the same reason Batch C's Drive masks are: an output pointer for a
//! property the request never asked for is a contract this connector does not
//! have.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::providers::microsoft_graph::{self, PermissionRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "microsoft_outlook";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "Mail.ReadBasic — Allows the app to read the properties of the signed-in
/// user's mail, except body, bodyPreview, uniqueBody, attachments…"
const MAIL_READ_BASIC: &str = "Mail.ReadBasic";
const MAIL_READ: &str = "Mail.Read";
const MAIL_READ_WRITE: &str = "Mail.ReadWrite";
const MAIL_SEND: &str = "Mail.Send";
const CALENDARS_READ_BASIC: &str = "Calendars.ReadBasic";
const CALENDARS_READ: &str = "Calendars.Read";
const CALENDARS_READ_WRITE: &str = "Calendars.ReadWrite";
const CONTACTS_READ: &str = "Contacts.Read";
const CONTACTS_READ_WRITE: &str = "Contacts.ReadWrite";

/// The mail reads. `user-list-messages` publishes the full set — least
/// privileged `Mail.ReadBasic`, higher privileged `Mail.ReadWrite, Mail.Read` —
/// and `message-get`'s older table omits `Mail.ReadWrite`. The union is used
/// for both, because a deployment holding `Mail.ReadWrite` for a write plainly
/// may also read, and refusing it would be this connector inventing a limit
/// Microsoft does not have.
const MAIL_READ_PERMISSIONS: &[&str] = &[MAIL_READ_BASIC, MAIL_READ, MAIL_READ_WRITE];

/// The mail writes: "Least privileged permission: Mail.ReadWrite. Higher
/// privileged permissions: Not available."
const MAIL_WRITE_PERMISSIONS: &[&str] = &[MAIL_READ_WRITE];

/// Sending: "Least privileged permission: Mail.Send. Higher privileged
/// permissions: Not available."
const MAIL_SEND_PERMISSIONS: &[&str] = &[MAIL_SEND];

const CALENDAR_READ_PERMISSIONS: &[&str] =
    &[CALENDARS_READ_BASIC, CALENDARS_READ, CALENDARS_READ_WRITE];
const CALENDAR_WRITE_PERMISSIONS: &[&str] = &[CALENDARS_READ_WRITE];
const CONTACT_READ_PERMISSIONS: &[&str] = &[CONTACTS_READ, CONTACTS_READ_WRITE];

/// `Prefer` for every operation that names or returns an Outlook item id.
///
/// RFC 7240 preferences are comma-separated tokens in one header, and these are
/// the two Microsoft documents for these resources.
const PREFER_ITEM: &str = "IdType=\"ImmutableId\", outlook.body-content-type=\"text\"";

/// The same, for the calendar reads: Microsoft returns event times "in UTC"
/// when `outlook.timezone` is absent, and this connector says so rather than
/// depending on an absence.
const PREFER_EVENT: &str =
    "IdType=\"ImmutableId\", outlook.body-content-type=\"text\", outlook.timezone=\"UTC\"";

/// The message properties this connector declares outputs for.
const MESSAGE_FIELDS: &str = "id,subject,bodyPreview,body,receivedDateTime,sentDateTime,isRead,\
                              isDraft,hasAttachments,importance,webLink,parentFolderId,\
                              conversationId,from";

/// The event properties this connector declares outputs for.
const EVENT_FIELDS: &str = "id,subject,bodyPreview,start,end,isAllDay,isCancelled,organizer,\
                            webLink,iCalUId,transactionId";

/// The contact properties this connector declares outputs for.
const CONTACT_FIELDS: &str =
    "id,displayName,givenName,surname,emailAddresses,companyName,jobTitle,mobilePhone";

/// The mail folder properties this connector declares outputs for.
const FOLDER_FIELDS: &str =
    "id,displayName,parentFolderId,childFolderCount,unreadItemCount,totalItemCount,isHidden";

/// "The default page size is 10 messages. Use `$top` to customize the page
/// size, within the range of 1 and 1000." Fifty is this connector's own choice:
/// large enough that a walk is not ten calls, small enough that one page of
/// fully-selected messages stays well inside the SDK's 1 MiB response ceiling.
const PAGE_SIZE: &str = "50";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::fixed(microsoft_graph::ORIGIN)
                    .expect("Microsoft's published Graph origin is valid"),
            )
            // Microsoft 365 is authorization-code OAuth2 and nothing else: the
            // access token is the credential store's, per attempt, and this
            // connector configures no secret of its own (spec 011 §2).
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Microsoft Outlook declarations are valid"))
            .build()
            .expect("the Microsoft Outlook declaration is valid")
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
        "message.list" | "event.list" | "contact.list" | "folder.list" => Some(&COLLECTION),
        _ => None,
    }
}

/// The delegated permissions one operation is authorized by, as its own Graph
/// reference page lists them.
pub fn permissions(operation_id: &str) -> Option<PermissionRequirement> {
    let mail_read = PermissionRequirement::documented(MAIL_READ_BASIC, MAIL_READ_PERMISSIONS);
    let mail_write = PermissionRequirement::documented(MAIL_READ_WRITE, MAIL_WRITE_PERMISSIONS);
    let mail_send = PermissionRequirement::documented(MAIL_SEND, MAIL_SEND_PERMISSIONS);
    let calendar_read =
        PermissionRequirement::documented(CALENDARS_READ_BASIC, CALENDAR_READ_PERMISSIONS);
    let calendar_write =
        PermissionRequirement::documented(CALENDARS_READ_WRITE, CALENDAR_WRITE_PERMISSIONS);
    let contact_read = PermissionRequirement::documented(CONTACTS_READ, CONTACT_READ_PERMISSIONS);
    match operation_id {
        "message.get" | "message.list" | "folder.list" => Some(mail_read),
        "message.move" | "message.update" | "message.delete" | "draft.create" => Some(mail_write),
        "message.send" | "draft.send" => Some(mail_send),
        "event.get" | "event.list" => Some(calendar_read),
        "event.create" | "event.update" | "event.delete" => Some(calendar_write),
        "contact.get" | "contact.list" => Some(contact_read),
        _ => None,
    }
}

/// Decode one response.
///
/// None of these operations publishes a per-item failure shape — no response
/// schema here carries an `errors` collection or a partial-success flag — so
/// the only guard is the shared fail-closed one.
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

/// The output contract of one message resource, under [`MESSAGE_FIELDS`].
fn message_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("subject", "/subject", ValueScalar::String, Required::No)
        .output_pointer(
            "body_preview",
            "/bodyPreview",
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
            "received_at",
            "/receivedDateTime",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("is_read", "/isRead", ValueScalar::Boolean, Required::No)
        .output_pointer("is_draft", "/isDraft", ValueScalar::Boolean, Required::No)
        .output_pointer(
            "has_attachments",
            "/hasAttachments",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer(
            "from_address",
            "/from/emailAddress/address",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "parent_folder_id",
            "/parentFolderId",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "conversation_id",
            "/conversationId",
            ValueScalar::String,
            Required::No,
        )
        // A provider-chosen URL on `outlook.office365.com`. It is data in the
        // output contract, and nothing in this connector can spend it as a
        // destination.
        .output_pointer("web_link", "/webLink", ValueScalar::String, Required::No)
}

/// The output contract of one event resource, under [`EVENT_FIELDS`].
fn event_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("subject", "/subject", ValueScalar::String, Required::No)
        .output_pointer(
            "body_preview",
            "/bodyPreview",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "start_at",
            "/start/dateTime",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "start_time_zone",
            "/start/timeZone",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("end_at", "/end/dateTime", ValueScalar::String, Required::No)
        .output_pointer(
            "end_time_zone",
            "/end/timeZone",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "is_all_day",
            "/isAllDay",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer(
            "is_cancelled",
            "/isCancelled",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer(
            "organizer_address",
            "/organizer/emailAddress/address",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("ical_uid", "/iCalUId", ValueScalar::String, Required::No)
        .output_pointer("web_link", "/webLink", ValueScalar::String, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Retrieve the properties and relationships of a message object."
    let message_get = message_outputs(
        common(Operation::get(
            "message.get",
            "/v1.0/me/messages/{message_id}",
        ))
        .path_param("message_id", ValueScalar::String)
        .query_static("$select", MESSAGE_FIELDS)
        .static_header("Prefer", PREFER_ITEM)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get the messages in the signed-in user's mailbox".
    let message_list = common(Operation::get("message.list", "/v1.0/me/messages"))
        .query_static("$select", MESSAGE_FIELDS)
        .query_static("$top", PAGE_SIZE)
        .static_header("Prefer", PREFER_ITEM)
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

    // "Send the message specified in the request body … The message is saved in
    // the Sent Items folder." `202 Accepted`, empty body.
    let message_send = common(Operation::post("message.send", "/v1.0/me/sendMail"))
        .body(JsonTemplate::object([
            (
                "message",
                JsonTemplate::object([
                    ("subject", JsonTemplate::input("subject")),
                    (
                        "body",
                        JsonTemplate::object([
                            ("contentType", JsonTemplate::input("body_content_type")),
                            ("content", JsonTemplate::input("body_content")),
                        ]),
                    ),
                    ("toRecipients", JsonTemplate::input("to_recipients")),
                ]),
            ),
            (
                "saveToSentItems",
                JsonTemplate::literal(JsonValue::Bool(true)),
            ),
        ]))
        .success_statuses([StatusCode::ACCEPTED])
        .no_content_statuses([StatusCode::ACCEPTED])
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "Microsoft's reference page for `POST /me/sendMail` enumerates the complete request \
             contract — permissions, request headers, and every body property — and publishes no \
             idempotency key, client request identifier, or deduplication behaviour; \
             `transactionId` exists on the event resource and on nothing in mail",
            "a second delivered copy of the mail, which cannot be recalled — and Microsoft's own \
             note that \"202 Accepted … doesn't indicate that the request processing has \
             completed\" means the first send's outcome is not knowable from the response either",
        )?))
        .build()?;

    // "Move a message to another folder … This creates a new copy of the
    // message in the destination folder and removes the original message."
    let message_move = message_outputs(
        common(Operation::post(
            "message.move",
            "/v1.0/me/messages/{message_id}/move",
        ))
        .path_param("message_id", ValueScalar::String)
        .query_static("$select", MESSAGE_FIELDS)
        .static_header("Prefer", PREFER_ITEM)
        // "destinationId — The destination folder ID, or a well-known folder
        // name."
        .body(JsonTemplate::object([(
            "destinationId",
            JsonTemplate::input("destination_id"),
        )]))
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the move's reference page enumerates its complete request contract and publishes no \
             idempotency key that would bind a retry to the first attempt",
        "not the same result twice: Microsoft documents the move as \"This creates a new copy of \
             the message in the destination folder and removes the original message\", so a \
             repeat names an id the mailbox no longer holds",
    )?))
    .build()?;

    // "Update the properties of a message object." Partial merge semantics.
    let message_update = message_outputs(
        common(Operation::patch(
            "message.update",
            "/v1.0/me/messages/{message_id}",
        ))
        .path_param("message_id", ValueScalar::String)
        .query_static("$select", MESSAGE_FIELDS)
        .static_header("Prefer", PREFER_ITEM)
        // `isRead` is the one updatable property of a received message that a
        // Process realistically owns; the rest of the updatable set applies
        // only "if isDraft = true".
        .body(JsonTemplate::object([(
            "isRead",
            JsonTemplate::input("is_read"),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Microsoft publishes this as a `PATCH` whose body is \"the values for relevant fields that \
         should be updated. Existing properties that are not included in the request body will \
         maintain their previous values or be recalculated based on changes to other property \
         values\", and spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only. A patch \
         body of absolute values is repeat-safe and one whose intent is relative is not, and Graph \
         publishes nothing that tells the two apart.",
    )?)
    .build()?;

    // "Delete a message in the specified user's mailbox … `204 No Content`."
    let message_delete = common(Operation::delete(
        "message.delete",
        "/v1.0/me/messages/{message_id}",
    ))
    .path_param("message_id", ValueScalar::String)
    .static_header("Prefer", PREFER_ITEM)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Microsoft documents `DELETE /me/messages/{id}` as \"Delete a message in the specified \
         user's mailbox\" — a fixed resource identity — answering \"`204 No Content` … It doesn't \
         return anything in the response body\". A repeat names the same message and therefore \
         cannot delete a second one; it is answered either with the same `204` or with \
         `itemNotFound`, which this connector classifies `permanent`. Microsoft publishes no \
         sentence about the repeat itself, and the evidence admitted here is the fixed identity of \
         the documented request.",
    )?)
    .build()?;

    // "Create a draft of a new message… By default, this operation saves the
    // draft in the Drafts folder."
    let draft_create = message_outputs(
        common(Operation::post("draft.create", "/v1.0/me/messages"))
            .query_static("$select", MESSAGE_FIELDS)
            .static_header("Prefer", PREFER_ITEM)
            .body(JsonTemplate::object([
                ("subject", JsonTemplate::input("subject")),
                (
                    "body",
                    JsonTemplate::object([
                        ("contentType", JsonTemplate::input("body_content_type")),
                        ("content", JsonTemplate::input("body_content")),
                    ]),
                ),
                ("toRecipients", JsonTemplate::input("to_recipients")),
            ]))
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the draft create's reference page enumerates its complete request contract and Graph \
             publishes no idempotency key for mail",
        "a second draft in the Drafts folder, with a new message id",
    )?))
    .build()?;

    // "Send an existing draft message." `202 Accepted`, empty body.
    let draft_send = common(Operation::post(
        "draft.send",
        "/v1.0/me/messages/{message_id}/send",
    ))
    .path_param("message_id", ValueScalar::String)
    .static_header("Prefer", PREFER_ITEM)
    .success_statuses([StatusCode::ACCEPTED])
    .no_content_statuses([StatusCode::ACCEPTED])
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the draft send's reference page enumerates its complete request contract, publishes \
             nothing about repeating it, and binds no key to a first attempt; Microsoft notes \
             that \"202 Accepted … doesn't indicate that the request processing has completed\"",
        "a second delivered copy of the draft, which cannot be recalled",
    )?))
    .build()?;

    let event_get = event_outputs(
        common(Operation::get("event.get", "/v1.0/me/events/{event_id}"))
            .path_param("event_id", ValueScalar::String)
            .query_static("$select", EVENT_FIELDS)
            .static_header("Prefer", PREFER_EVENT)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get a list of event objects in the user's mailbox. The list contains
    // single instance meetings and series masters."
    let event_list = common(Operation::get("event.list", "/v1.0/me/events"))
        .query_static("$select", EVENT_FIELDS)
        .query_static("$top", PAGE_SIZE)
        .static_header("Prefer", PREFER_EVENT)
        .success_statuses([StatusCode::OK])
        .output_pointer("events", "/value", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_link",
            microsoft_graph::NEXT_LINK_POINTER,
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Create an event in the user's default calendar or specified calendar."
    let event_create = event_outputs(
        common(Operation::post("event.create", "/v1.0/me/events"))
            .query_static("$select", EVENT_FIELDS)
            .static_header("Prefer", PREFER_EVENT)
            .body(JsonTemplate::object([
                ("subject", JsonTemplate::input("subject")),
                (
                    "start",
                    JsonTemplate::object([
                        ("dateTime", JsonTemplate::input("start_at")),
                        ("timeZone", JsonTemplate::input("time_zone")),
                    ]),
                ),
                (
                    "end",
                    JsonTemplate::object([
                        ("dateTime", JsonTemplate::input("end_at")),
                        ("timeZone", JsonTemplate::input("time_zone")),
                    ]),
                ),
                ("attendees", JsonTemplate::input("attendees")),
                // Microsoft's own example sets this "to reduce unnecessary
                // retries on the server". It is declared because it is part of
                // the request Microsoft documents; it is not an idempotency
                // binding, because no retention window is published for it.
                ("transactionId", JsonTemplate::input("transaction_id")),
            ]))
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::inventory_only(
        "Microsoft publishes `transactionId` — \"A custom identifier specified by a client app for \
         the server to avoid redundant POST operations in case of client retries to create the \
         same event\" — which is a client-supplied key with a documented deduplicating purpose, \
         and publishes no retention window for it anywhere. `ExplicitKeyEvidence::documented` \
         requires a documented minimum retention to keep a durable send horizon inside, so this \
         operation cannot reach that class; each call otherwise creates an event with a new id.",
    )?)
    .build()?;

    let event_update = event_outputs(
        common(Operation::patch(
            "event.update",
            "/v1.0/me/events/{event_id}",
        ))
        .path_param("event_id", ValueScalar::String)
        .query_static("$select", EVENT_FIELDS)
        .static_header("Prefer", PREFER_EVENT)
        .body(JsonTemplate::object([(
            "subject",
            JsonTemplate::input("subject"),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the event update's reference page enumerates its complete request contract and \
             publishes no idempotency key or client request identifier for it",
        "a second meeting update sent to every attendee: Microsoft documents that an update \
             \"sends a meeting update\", so the repeat is observable outside the resource even \
             where the resource itself would be unchanged",
    )?))
    .build()?;

    let event_delete = common(Operation::delete(
        "event.delete",
        "/v1.0/me/events/{event_id}",
    ))
    .path_param("event_id", ValueScalar::String)
    .static_header("Prefer", PREFER_EVENT)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Microsoft documents `DELETE /me/events/{id}` as \"Removes the specified event from the \
         containing calendar\" — a fixed resource identity — answering \"`204 No Content` … It \
         doesn't return anything in the response body\". A repeat names the same event and cannot \
         remove a second one; it is answered with the same `204` or with `itemNotFound`, which \
         this connector classifies `permanent`.",
    )?)
    .build()?;

    let contact_get = common(Operation::get(
        "contact.get",
        "/v1.0/me/contacts/{contact_id}",
    ))
    .path_param("contact_id", ValueScalar::String)
    .query_static("$select", CONTACT_FIELDS)
    .static_header("Prefer", PREFER_ITEM)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer(
        "display_name",
        "/displayName",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "email_addresses",
        "/emailAddresses",
        ValueScalar::Json,
        Required::No,
    )
    .output_pointer(
        "company_name",
        "/companyName",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("job_title", "/jobTitle", ValueScalar::String, Required::No)
    .output_pointer(
        "mobile_phone",
        "/mobilePhone",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let contact_list = common(Operation::get("contact.list", "/v1.0/me/contacts"))
        .query_static("$select", CONTACT_FIELDS)
        .query_static("$top", PAGE_SIZE)
        .static_header("Prefer", PREFER_ITEM)
        .success_statuses([StatusCode::OK])
        .output_pointer("contacts", "/value", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_link",
            microsoft_graph::NEXT_LINK_POINTER,
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Get the mail folder collection directly under the root folder of the
    // signed-in user." No `Prefer: IdType` — "Container types (mailFolder,
    // calendar, etc.) don't support immutable ID".
    let folder_list = common(Operation::get("folder.list", "/v1.0/me/mailFolders"))
        .query_static("$select", FOLDER_FIELDS)
        .query_static("$top", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("folders", "/value", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_link",
            microsoft_graph::NEXT_LINK_POINTER,
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        message_get,
        message_list,
        message_send,
        message_move,
        message_update,
        message_delete,
        draft_create,
        draft_send,
        event_get,
        event_list,
        event_create,
        event_update,
        event_delete,
        contact_get,
        contact_list,
        folder_list,
    ])
}
