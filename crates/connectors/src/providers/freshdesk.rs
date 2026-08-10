//! Freshdesk's API v2 — the batch's second templated helpdesk host.
//!
//! Ground truth is Freshdesk's own published reference, read on 2026-08-10. The
//! whole v2 API is published as one document, `https://developers.freshdesk.com/api/`,
//! with fragment anchors; every quotation below is from it and the anchor is
//! named where it matters.
//!
//! * `#authentication` — "All URIs follow a specific format and that format is:
//!   `https://your_helpdesk_domain_name/api/v2/resource_name`", and "You can use
//!   your personal API key to authenticate the request. If you use the API key,
//!   there is no need for a password. You can use any set of characters as a
//!   dummy password", beside the published example `curl -v -u apikey:X`.
//! * `#introduction` — "All API requests should hit the secured endpoint, that
//!   is, only HTTPS."
//! * `#schema` — "POST requests will contain the Location Header in the response
//!   that points to the URL of the created resource", with "HTTP STATUS: HTTP 201
//!   Created"; "All timestamps are returned in the UTC format,
//!   YYYY-MM-DDTHH:MM:SSZ"; "Blank fields are included as null instead of being
//!   omitted."
//! * `#pagination` — "By default, the number of objects returned per page is 30.
//!   This can be adjusted by adding the `per_page` parameter to the query string.
//!   The maximum number of objects that can be retrieved per page is 100", and
//!   "The `'link'` header in the response will hold the next page url if exists.
//!   If you have reached the last page of objects, then the link header will not
//!   be set."
//! * `#ratelimit` — the `429`, the `Retry-After` header, and the
//!   `X-RateLimit-*` headers.
//! * `#error` — the status table and the machine-readable `code` set this
//!   module's error map is built from.
//!
//! # The API key is the Basic *username*
//!
//! Freshdesk publishes no header form for its key other than HTTP Basic, and it
//! puts the key where a username goes: `-u apikey:X`. That is the opposite of
//! every other Basic connector in this workspace, and it is why the SDK gained
//! [`AuthPlan::basic_secret_username`]: a plan whose username is declaration
//! material would have had to carry the key, which would put a secret into the
//! declaration, its `Debug`, and its published credential contract.
//!
//! # The domain is deploy-time configuration
//!
//! `https://{domain}.freshdesk.com` is an `OriginSpec::TemplatedHost` filled only
//! from `config.settings.domain`. Freshdesk publishes the matching constraint
//! itself — "Works only via Freshdesk domains and not via custom CNAMEs" — so
//! the single host label the SDK admits is not a narrowing this module invented.
//!
//! # Two collection shapes, and only one of them is walked
//!
//! A list endpoint answers a bare JSON array at the document root, so those
//! operations publish the whole document and their plan collects the root. The
//! search endpoints answer `{"total": n, "results": [...]}` instead — and they
//! are deliberately **not** walked: Freshdesk caps them at "page number ... should
//! not exceed 10" with 30 results a page and publishes no `link` header for them,
//! so the page is a declared input a Process carries rather than a continuation
//! the SDK can end on an absence
//! (`knowledgebase/declarative-saas/decisions/055-*`).
//!
//! # A success status is published once, for creates only
//!
//! Freshdesk publishes exactly one success status — the generic "POST → HTTP 201
//! Created" with a `Location` header — and no per-endpoint status anywhere else.
//! The `200` this module declares for every read and update is therefore this
//! declaration's reading of its own verb table ("GET | Fetch one or more
//! objects", "PUT | Update an object") rather than a sentence Freshdesk
//! published, and it is recorded here because an undeclared status is a failure
//! in the SDK rather than a silent success.
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur once in the 3.2 MB v2 reference, nor in the v1 reference, nor in the
//! widget API: no request header, no body attribute, no response field. The four
//! creates are therefore `AtMostOnce` (ADR 063). The two updates stay
//! `InventoryOnly`: Freshdesk publishes nothing at all about `PUT` semantics
//! beyond "PUT | Update an object", so neither spec 010 §7's `NaturalMethod`
//! evidence nor ADR 063's consequence is there to cite.
//!
//! One near-miss is recorded rather than dropped, because a reviewer will find
//! it: `unique_external_id` is published as an upsert-by-lookup key on the
//! *ticket* requester — "If no contact exists with this external ID in Freshdesk,
//! they will be added as a new contact" — and as a unique field on a contact. It
//! deduplicates contacts, not requests, and it carries no retention or replay
//! guarantee.

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
pub const NAME: &str = "freshdesk";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key that fills the templated host: the
/// helpdesk's own `freshdesk.com` label.
pub const DOMAIN: &str = "domain";

/// "You can use any set of characters as a dummy password", and `X` is the
/// character every published example uses.
const DUMMY_PASSWORD: &str = "X";

/// "The maximum number of objects that can be retrieved per page is 100."
const PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::templated_host("https", "{domain}.freshdesk.com", None)
                    .expect("Freshdesk's published host form is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::basic_secret_username(DUMMY_PASSWORD)
                    .expect("Freshdesk's published credential form is valid"),
            ))
            .operations(operations().expect("the Freshdesk declarations are valid"))
            .build()
            .expect("the Freshdesk declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// The code rules come first, because the same `400` is a missing field, a bad
/// value, or a payload Freshdesk could not parse, and only the credential case
/// is worth telling a Process apart from the rest. `code` is the field Freshdesk
/// publishes as machine-readable — "code — Custom error code that is
/// machine-parseable" — and its documented values are a closed list on the same
/// page.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/errors/0/code")
            // "invalid_credentials — Incorrect or missing API credentials",
            // "access_denied — Insufficient privileges to perform this action."
            .on_code("invalid_credentials", ConnectorErrorClass::Authentication)
            .on_code("access_denied", ConnectorErrorClass::Authentication)
            // "missing_field", "invalid_value", "datatype_mismatch",
            // "invalid_field", "invalid_json" — each needs a different request.
            .on_code("missing_field", ConnectorErrorClass::Validation)
            .on_code("invalid_value", ConnectorErrorClass::Validation)
            .on_code("datatype_mismatch", ConnectorErrorClass::Validation)
            .on_code("invalid_field", ConnectorErrorClass::Validation)
            .on_code("invalid_json", ConnectorErrorClass::Validation)
            // "duplicate_value — Indicates that this value already exists",
            // which is the answer to creating a contact whose email is taken.
            .on_code("duplicate_value", ConnectorErrorClass::Permanent)
            // "400 Client or Validation Error — The request body/query string is
            // not in the correct format."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Authentication Failure — Indicates that the Authorization
            // header is either missing or incorrect", "403 Access Denied".
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Requested Resource not Found", "405 Method not allowed",
            // "406 Unsupported Accept Header", "409 Inconsistent/Conflicting
            // State", "415 Unsupported Content-type".
            .on_statuses([404, 405, 406, 409, 415], ConnectorErrorClass::Permanent)
            // "429 Rate Limit Exceeded — The API rate limit allotted for your
            // Freshdesk domain has been exhausted."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Unexpected Server Error — ... This indicates an error at
            // Freshdesk's side."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Freshdesk error map is a valid declaration")
    });
    &MAP
}

/// Decode one Freshdesk response: the declared success statuses, then the declared
/// contract.
///
/// It is the declaration-driven answer written out per module rather than
/// inherited, so that the serving runtime asks every connector in this batch the
/// same question and each one answers with its own error map.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    operation.decode_response(status, body)
}

/// The continuation plan of each collection.
///
/// Every walked collection is a bare JSON array at the document root, so the
/// items pointer is the empty pointer (RFC 6901's whole document), and the
/// continuation is Freshdesk's own `link` header: "The `'link'` header in the
/// response will hold the next page url if exists. If you have reached the last
/// page of objects, then the link header will not be set" — an absence, which is
/// exactly what the plan ends on.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("", "next").expect("the Freshdesk link plan is valid")
    });
    match operation_id {
        "ticket.list" | "contact.list" | "conversation.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        // "Only `application/json` and `*/*` are supported."
        .static_header("Accept", "application/json")
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in Freshdesk's published v2 API \
                      reference — one 3.2 MB document covering authentication, schema, errors, \
                      pagination, rate limits, and every ticket, conversation, and contact \
                      endpoint — nor in its v1 reference or its widget API: no request header, no \
                      body attribute, and no response field carries a client-supplied request \
                      identifier or a deduplication behaviour";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The reason both updates carry.
const PARTIAL_UPDATE: &str = "Freshdesk publishes nothing about what a `PUT` does beyond its verb \
                              table's \"PUT | Update an object\": no statement that the request \
                              replaces the resource, which is spec 010 §7's NaturalMethod \
                              evidence, and no statement of what a second identical send produces, \
                              which is what ADR 063 admits a class on. Its update parameter tables \
                              mark no attribute mandatory, which is the shape of a partial update \
                              rather than a write to a fixed resource identity";

/// The published ticket attributes a process reads.
fn ticket_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("subject", "/subject", ValueScalar::String, Required::No)
        // "status | number", "priority | number" — the numeric ticket
        // properties, not their display names.
        .output_pointer("status", "/status", ValueScalar::Int64, Required::No)
        .output_pointer("priority", "/priority", ValueScalar::Int64, Required::No)
        .output_pointer(
            "requester_id",
            "/requester_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "responder_id",
            "/responder_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "updated_at",
            "/updated_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The published contact attributes a process reads.
fn contact_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("email", "/email", ValueScalar::String, Required::No)
        .output_pointer("phone", "/phone", ValueScalar::String, Required::No)
        .output_pointer("active", "/active", ValueScalar::Boolean, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "updated_at",
            "/updated_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The published conversation attributes a process reads.
fn conversation_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("body_text", "/body_text", ValueScalar::String, Required::No)
        .output_pointer("private", "/private", ValueScalar::Boolean, Required::No)
        .output_pointer("incoming", "/incoming", ValueScalar::Boolean, Required::No)
        .output_pointer("user_id", "/user_id", ValueScalar::Int64, Required::No)
        .output_pointer("ticket_id", "/ticket_id", ValueScalar::Int64, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let ticket_get = ticket_output(
        common(Operation::get("ticket.get", "/api/v2/tickets/{ticket_id}"))
            .path_param("ticket_id", ValueScalar::Int64)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "By default, only tickets that have been created within the past 30 days
    // will be returned. For older tickets, use the `updated_since` filter", so
    // the filter is a declared input rather than an inherited default.
    let ticket_list = common(Operation::get("ticket.list", "/api/v2/tickets"))
        .query_input("updated_since", "updated_since")
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        // The collection is a bare JSON array, so the whole document is the
        // output.
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // "Query string must be enclosed between a pair of double quotes and can
    // have up to 512 characters", and "The query must be URL encoded" — which
    // the SDK's query renderer does.
    let ticket_search = common(Operation::get("ticket.search", "/api/v2/search/tickets"))
        .query_input("query", "query")
        .query_input("page", "page")
        .success_statuses([StatusCode::OK])
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes)
        .output_pointer("total", "/total", ValueScalar::Int64, Required::No)
        .effect(Effect::read_only())
        .build()?;

    let ticket_create = ticket_output(
        common(Operation::post("ticket.create", "/api/v2/tickets"))
            .body(JsonTemplate::object([
                ("subject", JsonTemplate::input("subject")),
                ("description", JsonTemplate::input("description")),
                ("email", JsonTemplate::input("email")),
                ("requester_id", JsonTemplate::input("requester_id")),
                ("status", JsonTemplate::input("status")),
                ("priority", JsonTemplate::input("priority")),
                ("type", JsonTemplate::input("type")),
                ("custom_fields", JsonTemplate::input("custom_fields")),
            ]))
            .declared_input("custom_fields", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second ticket with a new id on the same requester; Freshdesk publishes no duplicate \
         detection for tickets at all, and its own `external_id`-style key is a contact lookup \
         rather than a request identifier",
    )?)
    .build()?;

    let ticket_update = ticket_output(
        common(Operation::put(
            "ticket.update",
            "/api/v2/tickets/{ticket_id}",
        ))
        .path_param("ticket_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("status", JsonTemplate::input("status")),
            ("priority", JsonTemplate::input("priority")),
            ("responder_id", JsonTemplate::input("responder_id")),
            ("custom_fields", JsonTemplate::input("custom_fields")),
        ]))
        .declared_input("custom_fields", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    let conversation_list = common(Operation::get(
        "conversation.list",
        "/api/v2/tickets/{ticket_id}/conversations",
    ))
    .path_param("ticket_id", ValueScalar::Int64)
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Create a Note": `body` and `structured_body` carry "Any of the two
    // attributes is mandatory", and "private ... The default value is true."
    let note_add = conversation_output(
        common(Operation::post(
            "note.add",
            "/api/v2/tickets/{ticket_id}/notes",
        ))
        .path_param("ticket_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("body", JsonTemplate::input("body")),
            ("private", JsonTemplate::input("private")),
            ("notify_emails", JsonTemplate::input("notify_emails")),
        ]))
        .declared_input("body", ValueScalar::String, Required::Yes)
        .declared_input("notify_emails", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second private note on the same ticket, with a new id — and one more step towards the \
         5,000-conversation ceiling Freshdesk publishes for a ticket",
    )?)
    .build()?;

    let reply_add = conversation_output(
        common(Operation::post(
            "reply.add",
            "/api/v2/tickets/{ticket_id}/reply",
        ))
        .path_param("ticket_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("body", JsonTemplate::input("body")),
            ("cc_emails", JsonTemplate::input("cc_emails")),
            ("bcc_emails", JsonTemplate::input("bcc_emails")),
        ]))
        .declared_input("body", ValueScalar::String, Required::Yes)
        .declared_input("cc_emails", ValueScalar::Json, Required::Yes)
        .declared_input("bcc_emails", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second reply on the same ticket — and, unlike a note, a second email delivered to the \
         requester",
    )?)
    .build()?;

    let contact_get = contact_output(
        common(Operation::get(
            "contact.get",
            "/api/v2/contacts/{contact_id}",
        ))
        .path_param("contact_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let contact_list = common(Operation::get("contact.list", "/api/v2/contacts"))
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let contact_create = contact_output(
        common(Operation::post("contact.create", "/api/v2/contacts"))
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("email", JsonTemplate::input("email")),
                ("phone", JsonTemplate::input("phone")),
                (
                    "unique_external_id",
                    JsonTemplate::input("unique_external_id"),
                ),
                ("custom_fields", JsonTemplate::input("custom_fields")),
            ]))
            .declared_input("custom_fields", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second contact with a new id, unless the payload carries a value on a field Freshdesk \
         enforces as unique — an `email`, a `unique_external_id`, a unique custom field — where \
         the repeat is refused with `409` and `duplicate_value` instead. Freshdesk publishes both \
         outcomes and neither is the first send's",
    )?)
    .build()?;

    let contact_update = contact_output(
        common(Operation::put(
            "contact.update",
            "/api/v2/contacts/{contact_id}",
        ))
        .path_param("contact_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("name", JsonTemplate::input("name")),
            ("phone", JsonTemplate::input("phone")),
            ("custom_fields", JsonTemplate::input("custom_fields")),
        ]))
        .declared_input("custom_fields", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    Ok(vec![
        ticket_get,
        ticket_list,
        ticket_search,
        ticket_create,
        ticket_update,
        conversation_list,
        note_add,
        reply_add,
        contact_get,
        contact_list,
        contact_create,
        contact_update,
    ])
}
