//! Twilio SendGrid v3 API (Mail Send and Marketing Campaigns).
//!
//! Ground truth is SendGrid's own published v3 API reference, read on
//! 2026-08-10:
//!
//! * <https://www.twilio.com/docs/sendgrid/api-reference/mail-send/mail-send> —
//!   `POST /v3/mail/send`, base URL `https://api.sendgrid.com`, `Authorization:
//!   Bearer <API key>`, success `202 Accepted`, response header `X-Message-Id`.
//! * <https://www.twilio.com/docs/sendgrid/api-reference/how-to-use-the-sendgrid-v3-api/responses>
//!   — the status table this module's error map is built from: 200 "OK", 201
//!   "Creation succeeded", 202 "Request accepted", 204 "Deletion succeeded",
//!   400 "Bad request", 401 "Requires authentication", 403 "From address
//!   doesn't match Verified Sender Identity", 406 "Missing the Accept header",
//!   429 "Too many requests or rate limit exceeded", 500 "Internal server
//!   error".
//! * the per-endpoint pages cited on each operation below.
//!
//! # Regions
//!
//! SendGrid publishes a second, EU-resident origin (`https://api.eu.sendgrid.com`)
//! for accounts with EU data residency. An origin is part of a connector's
//! identity, so serving both would be a templated origin or a second
//! declaration; this connector is the global origin only.
//!
//! # Pagination
//!
//! Marketing Campaigns publishes its continuation as an absolute URL inside the
//! response body (`_metadata.next`), and the contacts endpoint publishes none
//! at all — "pagination of the contacts has been deprecated"
//! (<https://www.twilio.com/docs/sendgrid/api-reference/contacts/get-sample-contacts>).
//! `Cursor` and `TokenInBody` would send that URL back as a `page_token`
//! *value*, which SendGrid does not accept, and `LinkHeader` reads a header
//! SendGrid does not send here; the plan that fits is the SDK's body-carried
//! next URI, which resolves the value against the compiled origin and refuses a
//! `next` that lands anywhere else, exactly as `LinkHeader` does. `list.list`
//! declares it; every other read here is one page whose size is fixed by the
//! declaration.
//!
//! # Effect classification
//!
//! SendGrid's v3 API reference documents no idempotency key, no client-supplied
//! request identifier, and no deduplication of a repeated send, so `mail.send`
//! and `list.create` are `AtMostOnce` (ADR 063): a repeat delivers a second
//! email or creates a second list. `list.update` stays `InventoryOnly` — it is a
//! `PATCH` whose repeat applies the same rename — and the `PUT` and `DELETE`
//! operations are `ProviderIdempotent::NaturalMethod` on the statements cited at
//! each one (see `INVENTORY.md`).

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "sendgrid";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// SendGrid's global API origin.
const ORIGIN: &str = "https://api.sendgrid.com";

/// "page_size: The number of elements you want returned on each page ... 1 to
/// 1000, defaults to 100"
/// (<https://www.twilio.com/docs/sendgrid/api-reference/lists/get-all-lists>).
/// The declaration fixes it, so a caller cannot ask for an unbounded page.
const LIST_PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("SendGrid's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the SendGrid declarations are valid"))
            .build()
            .expect("the SendGrid declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from SendGrid's documented status table.
///
/// SendGrid's error body is an `errors` array of prose messages
/// (`{"errors":[{"field":…,"message":…,"help":…}]}`) with no stable
/// machine-readable code, so this map declares no code pointer: prose is not a
/// contract, and matching on it would make a class depend on wording. The one
/// provider value retained is the documented `X-Message-Id`, as a support
/// handle.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad request" and "406 Missing the Accept header" are both
            // this deployment's request to fix, as is the Mail Send "413
            // content too large".
            .on_statuses([400, 406, 413], ConnectorErrorClass::Validation)
            // "401 Requires authentication" and "403 From address doesn't match
            // Verified Sender Identity | You are temporarily blocked from
            // sending emails".
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // Mail Send documents "404 not found" and "405 method not allowed".
            .on_statuses([404, 405], ConnectorErrorClass::Permanent)
            // "429 Too many requests or rate limit exceeded".
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal server error", plus the gateway statuses an edge
            // returns for the same condition.
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .correlation_header("message_id", "x-message-id")
            .build()
            .expect("the SendGrid error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of the one endpoint SendGrid publishes one for.
///
/// The lists collection carries `_metadata.next` — an absolute URL on
/// SendGrid's own origin — which the SDK's body-carried next-URI plan resolves
/// and then checks against the compiled origin, so a `next` naming anywhere
/// else is refused rather than followed. The contacts endpoint publishes no
/// continuation at all ("pagination of the contacts has been deprecated"), and
/// a single resource has nothing to continue.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static LISTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/result", "/_metadata/next")
            .expect("the SendGrid list pagination plan is valid")
    });
    match operation_id {
        "list.list" => Some(&LISTS),
        _ => None,
    }
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // <https://www.twilio.com/docs/sendgrid/api-reference/contacts/get-sample-contacts>:
    // "will return up to 50 of the most recent contacts uploaded or attached to
    // a list", and "pagination of the contacts has been deprecated".
    let contact_list = Operation::get("contact.list", "/v3/marketing/contacts")
        .version(VERSION)
        .success_statuses([StatusCode::OK])
        .output_pointer("result", "/result", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "contact_count",
            "/contact_count",
            ValueScalar::Int64,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/contacts/get-a-contact-by-id>
    let contact_get = Operation::get("contact.get", "/v3/marketing/contacts/{contact_id}")
        .version(VERSION)
        .path_param("contact_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("email", "/email", ValueScalar::String, Required::Yes)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "updated_at",
            "/updated_at",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("list_ids", "/list_ids", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/contacts/add-or-update-a-contact>.
    // SendGrid documents `list_ids` as optional; the SDK's body template has no
    // optional slot, so this declaration carries the required `contacts` array
    // only and leaves list membership to the lists API.
    let contact_upsert = Operation::put("contact.upsert", "/v3/marketing/contacts")
        .version(VERSION)
        .body(JsonTemplate::object([(
            "contacts",
            JsonTemplate::input("contacts"),
        )]))
        .success_statuses([StatusCode::ACCEPTED])
        .output_pointer("job_id", "/job_id", ValueScalar::String, Required::Yes)
        .effect(Effect::provider_idempotent_natural_method(
            "SendGrid documents this PUT as an upsert keyed on the contact's email: \"This \
             endpoint allows the upsert (insert or update) of up to 30,000 contacts\", and \"The \
             email field will be changed to all lower-case. If a contact is added with an email \
             that exists but contains capital letters, the existing contact with the all \
             lower-case email will be updated.\" A repeated send therefore updates the one \
             contact rather than creating a second",
        )?)
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/contacts/delete-contacts>:
    // "ids: comma-separated list of contact IDs", "Deletion jobs are processed
    // asynchronously", success `202` with a `job_id`.
    let contact_delete = Operation::delete("contact.delete", "/v3/marketing/contacts")
        .version(VERSION)
        .query_input("ids", "ids")
        .success_statuses([StatusCode::ACCEPTED])
        .output_pointer("job_id", "/job_id", ValueScalar::String, Required::Yes)
        .effect(Effect::provider_idempotent_natural_method(
            "SendGrid documents the delete against the contact identities named in the request — \
             \"ids: A comma-separated list of contact IDs\" — so a repeated send names the same \
             contacts and leaves the same account state; the second job finds them already gone",
        )?)
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/lists/create-list>:
    // `POST`, success `201`, body `{"name": …}`. Nothing on the page documents
    // an idempotency key, a request identifier, or duplicate-name protection,
    // and "You can create a maximum of 1000 lists" is a ceiling a repeated send
    // spends twice.
    let list_create = Operation::post("list.create", "/v3/marketing/lists")
        .version(VERSION)
        .body(JsonTemplate::object([(
            "name",
            JsonTemplate::input("name"),
        )]))
        .success_statuses([StatusCode::CREATED])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::Yes)
        .output_pointer(
            "contact_count",
            "/contact_count",
            ValueScalar::Int64,
            Required::Yes,
        )
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "SendGrid's Create list reference documents the whole request — a `name` body and a `201` \
             success — and publishes no idempotency key, request identifier, or duplicate-name \
             protection anywhere in the v3 Marketing API",
            "a second list with the same name and a new ID, and one more of the 1000 lists the \
             account is allowed",
        )?))
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/lists/get-a-list-by-id>
    let list_get = Operation::get("list.get", "/v3/marketing/lists/{list_id}")
        .version(VERSION)
        .path_param("list_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::Yes)
        .output_pointer(
            "contact_count",
            "/contact_count",
            ValueScalar::Int64,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/lists/get-all-lists>
    let list_list = Operation::get("list.list", "/v3/marketing/lists")
        .version(VERSION)
        .query_static("page_size", LIST_PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("result", "/result", ValueScalar::Json, Required::Yes)
        // The continuation SendGrid publishes, carried as data. Nothing in this
        // connector turns it into a request.
        .output_pointer(
            "next_page",
            "/_metadata/next",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/lists/update-list>:
    // `PATCH`, success `200`. A PATCH is not one of the two methods HTTP
    // defines repeat-safety for, and SendGrid publishes no key for it.
    let list_update = Operation::patch("list.update", "/v3/marketing/lists/{list_id}")
        .version(VERSION)
        .path_param("list_id", ValueScalar::String)
        .body(JsonTemplate::object([(
            "name",
            JsonTemplate::input("name"),
        )]))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::Yes)
        .effect(Effect::inventory_only(
            "SendGrid documents the list update as a PATCH and publishes no idempotency key for \
             it; a PATCH is not one of the two methods HTTP defines repeat-safety for",
        )?)
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/lists/delete-a-list>:
    // "200 The delete has been accepted and is processing" with a `job_id`, and
    // "204 The delete has been processed". The `204` carries no body, which the
    // declaration says with `no_content_statuses`; the `200` still carries the
    // documented job.
    let list_delete = Operation::delete("list.delete", "/v3/marketing/lists/{list_id}")
        .version(VERSION)
        .path_param("list_id", ValueScalar::String)
        .query_input("delete_contacts", "delete_contacts")
        .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
        .no_content_statuses([StatusCode::NO_CONTENT])
        .output_pointer("job_id", "/job_id", ValueScalar::String, Required::No)
        .effect(Effect::provider_idempotent_natural_method(
            "SendGrid documents the delete against the list ID in the path, answering \"200 The \
             delete has been accepted and is processing\" or \"204 The delete has been \
             processed\"; a repeated send names the same list and leaves it deleted",
        )?)
        .build()?;

    // <https://www.twilio.com/docs/sendgrid/api-reference/mail-send/mail-send>:
    // the documented required body is `personalizations`, `from`, `subject`,
    // and `content`; success is `202 Accepted` with an empty body and an
    // `X-Message-Id` header.
    let mail_send = Operation::post("mail.send", "/v3/mail/send")
        .version(VERSION)
        .body(JsonTemplate::object([
            ("personalizations", JsonTemplate::input("personalizations")),
            ("from", JsonTemplate::input("from")),
            ("subject", JsonTemplate::input("subject")),
            ("content", JsonTemplate::input("content")),
        ]))
        .success_statuses([StatusCode::ACCEPTED])
        // "an empty response body" is what SendGrid documents beside the 202.
        .no_content_statuses([StatusCode::ACCEPTED])
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "SendGrid's Mail Send reference documents the complete request contract and publishes no \
             idempotency key or client-supplied request identifier; the `X-Message-Id` it \
             publishes is server-issued and arrives in the response, which is the opposite of a \
             key a retry could carry",
            "a second delivered email, with a second `X-Message-Id`, to the same recipients",
        )?))
        .build()?;

    Ok(vec![
        contact_list,
        contact_get,
        contact_upsert,
        contact_delete,
        list_create,
        list_get,
        list_list,
        list_update,
        list_delete,
        mail_send,
    ])
}
