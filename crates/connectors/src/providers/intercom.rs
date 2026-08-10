//! Intercom's REST API — the batch's documented upsert that the gate still
//! refuses.
//!
//! Ground truth is Intercom's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://developers.intercom.com/docs/references/rest-api/errors/http-responses>
//!   — the status table: "401 Unauthorized -- The API Key was not authorised (or
//!   no API Key was found)", "402 Payment Required -- The API is not available on
//!   your current plan", "409 Conflict - Multiple existing users match this email
//!   address - must be more specific using user_id", "429 Too Many Requests --
//!   The client has reached or exceeded a rate limit, or the server is
//!   overloaded."
//! * <https://developers.intercom.com/docs/references/rest-api/errors/error-objects>
//!   — the error envelope `{"type": "error.list", "errors": [{"code": …,
//!   "message": …, "field": …}]}`, where `code` is "A string indicating the kind
//!   of error, used to further qualify the HTTP response code", and the
//!   published code vocabulary (`server_error`, `client_error`, `type_mismatch`,
//!   `parameter_not_found`, `parameter_invalid`, `action_forbidden`, `conflict`,
//!   `api_plan_restricted`, `rate_limit_exceeded`, `unsupported`,
//!   `token_revoked`, `token_blocked`, `token_not_found`, `token_unauthorized`,
//!   `token_expired`, `missing_authorization`, `retry_after`, `job_closed`,
//!   `not_restorable`, `team_not_found`, `team_unavailable`, `admin_not_found`).
//! * <https://developers.intercom.com/docs/build-an-integration/learn-more/rest-apis/pagination>
//!   — list APIs take `per_page` and `starting_after` as query parameters and
//!   publish the next cursor at `pages.next.starting_after`; search APIs carry
//!   the same two names inside a `pagination` object in the request body; the
//!   maximum `per_page` across all APIs is 150.
//! * The published OpenAPI description
//!   (<https://github.com/intercom/Intercom-OpenAPI>, `descriptions/2.16/api.intercom.io.yaml`)
//!   for every path, parameter, request schema, and response schema below.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` occurs exactly once in
//! Intercom's whole 2.16 description, and it is on an endpoint this connector
//! does not publish: the banner dismiss, "The request is idempotent: dismissing
//! an already-dismissed banner succeeds". No request header, body field, or
//! query parameter anywhere in the description carries a client-supplied request
//! identifier.
//!
//! Spec 016 §2 proposes `company.create_or_update` as `NaturalMethod` and asks
//! for the documented upsert semantics to be verified. They are real and they are
//! quoted on the operation: "Companies are looked up via `company_id` in a `POST`
//! request, if not found via `company_id`, the new company will be created, if
//! found, that company will be updated." The class is still refused, and the
//! reason is the sentence's own middle word: it is a `POST`, and spec 010 §7
//! admits `NaturalMethod` for `PUT` and `DELETE` only. This is the same shape as
//! `aws_sqs.message.delete` — a provider documenting repeat-safety that the two
//! admitted classes cannot express. ADR 063 deliberately leaves it here rather
//! than admitting it as `AtMostOnce`: a documented upsert absorbs the duplicate,
//! so the class it needs is one that permits a retry, not one that trades a
//! retry away.
//!
//! `contact.update` is a `PUT` and is still inventory-only: Intercom documents it
//! as "You can update an existing contact" with an all-optional request body, and
//! publishes no statement that the endpoint replaces the resource. A `PUT` whose
//! body is partial is not a write to a fixed resource identity.
//!
//! # A search cursor is a body field
//!
//! `contact.search` is a `POST` whose continuation lives in the request body —
//! "search APIs require pagination settings within the JSON request body's
//! `pagination` object" — which no SDK pagination plan can spend. It therefore
//! declares `starting_after` as an input the caller echoes back verbatim and
//! publishes `next_starting_after` as an output, with the page size fixed by this
//! declaration. The `GET` collections use the ordinary query-parameter plans. See
//! `knowledgebase/declarative-saas/decisions/055-*`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "intercom";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The published base URL.
const ORIGIN: &str = "https://api.intercom.io";

/// The pinned contract version.
///
/// "All API endpoints require the `Intercom-Version` header to specify which API
/// version to use", and the version increments in published steps; `2.16` is the
/// version whose description this declaration was written against.
pub const API_VERSION: &str = "2.16";

/// "The maximum `per_page` value across all APIs is 150."
const PAGE_SIZE: u32 = 150;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Intercom's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Intercom declarations are valid"))
            .build()
            .expect("the Intercom declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Intercom publishes a stable machine-readable `code` on the first element of
/// its `errors` array and describes it as the field that "further qualif[ies] the
/// HTTP response code", so the map reads it first and falls back to the
/// documented status table.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/errors/0/code")
            .on_code("rate_limit_exceeded", ConnectorErrorClass::Http429)
            // A published code whose whole meaning is "wait": it belongs with
            // the rate limit rather than with the permanent failures.
            .on_code("retry_after", ConnectorErrorClass::Http429)
            .on_code("token_not_found", ConnectorErrorClass::Authentication)
            .on_code("token_revoked", ConnectorErrorClass::Authentication)
            .on_code("token_blocked", ConnectorErrorClass::Authentication)
            .on_code("token_unauthorized", ConnectorErrorClass::Authentication)
            .on_code("token_expired", ConnectorErrorClass::Authentication)
            .on_code("missing_authorization", ConnectorErrorClass::Authentication)
            .on_code("action_forbidden", ConnectorErrorClass::Authentication)
            .on_code("parameter_not_found", ConnectorErrorClass::Validation)
            .on_code("parameter_invalid", ConnectorErrorClass::Validation)
            .on_code("type_mismatch", ConnectorErrorClass::Validation)
            .on_code("client_error", ConnectorErrorClass::Validation)
            .on_code("server_error", ConnectorErrorClass::Http5xx)
            // Everything left needs a change on this deployment's side or the
            // workspace's, and repeating the request cannot help.
            .on_code("api_plan_restricted", ConnectorErrorClass::Permanent)
            .on_code("conflict", ConnectorErrorClass::Permanent)
            .on_code("unsupported", ConnectorErrorClass::Permanent)
            // The documented status table, for a response that carries no
            // envelope at all.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([400, 415, 422], ConnectorErrorClass::Validation)
            .on_statuses([402, 404, 405, 406, 409], ConnectorErrorClass::Permanent)
            .on_status(408, ConnectorErrorClass::Timeout)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Intercom error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each `GET` collection.
///
/// The two cursor walks stop where Intercom says they do: `pages.next` is
/// documented as nullable, so an absent cursor is the end of the collection.
/// `company.list` is the odd one out because its own endpoint is: Intercom
/// documents `page` and `per_page` on `GET /companies`, not `starting_after`.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static CONTACTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/data",
            "starting_after",
            "/pages/next/starting_after",
            "per_page",
            PAGE_SIZE,
        )
        .expect("the Intercom contact cursor plan is valid")
    });
    static CONVERSATIONS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/conversations",
            "starting_after",
            "/pages/next/starting_after",
            "per_page",
            PAGE_SIZE,
        )
        .expect("the Intercom conversation cursor plan is valid")
    });
    static COMPANIES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/data", "page", "per_page", PAGE_SIZE)
            .expect("the Intercom company page plan is valid")
    });
    match operation_id {
        "contact.list" => Some(&CONTACTS),
        "conversation.list" => Some(&CONVERSATIONS),
        "company.list" => Some(&COMPANIES),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Intercom-Version", API_VERSION)
}

/// The identity fields every Intercom object carries.
///
/// Intercom's own description marks **no** property of `contact`, `company`, or
/// `conversation` as required, so only the two that identify the object are
/// declared required here; everything else is published as possibly absent
/// rather than as a promise the provider did not make.
fn identity(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("type", "/type", ValueScalar::String, Required::Yes)
}

/// The reason every write in this module carries.
const NO_KEY: &str = "Intercom publishes no idempotency key, client-supplied request identifier, \
                      or deduplication for this endpoint: the string `idempot` occurs once in its \
                      whole published 2.16 description, on the banner-dismiss endpoint this \
                      connector does not publish";

/// One write whose repeat would leave a second thing behind (ADR 063).
///
/// The search is the module's and the consequence is the operation's: both are
/// what a Process author accepts when they declare `at_most_once`.
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let contact_get = identity(common(Operation::get(
        "contact.get",
        "/contacts/{contact_id}",
    )))
    .path_param("contact_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("email", "/email", ValueScalar::String, Required::No)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("role", "/role", ValueScalar::String, Required::No)
    .output_pointer(
        "external_id",
        "/external_id",
        ValueScalar::String,
        Required::No,
    )
    // "created_at (integer)" — a Unix timestamp in seconds, not a string.
    .output_pointer(
        "created_at",
        "/created_at",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer(
        "updated_at",
        "/updated_at",
        ValueScalar::Int64,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let contact_list = common(Operation::get("contact.list", "/contacts"))
        .query_static("per_page", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_count",
            "/total_count",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "next_starting_after",
            "/pages/next/starting_after",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "To search for contacts, you need to send a `POST` request to
    // `https://api.intercom.io/contacts/search`. This will accept a query object
    // in the body which will define your filters."
    let contact_search = common(Operation::post("contact.search", "/contacts/search"))
        .body(JsonTemplate::object([
            ("query", JsonTemplate::input("query")),
            (
                "pagination",
                JsonTemplate::object([
                    (
                        "per_page",
                        JsonTemplate::literal(serde_json::json!(PAGE_SIZE)),
                    ),
                    ("starting_after", JsonTemplate::input("starting_after")),
                ]),
            ),
        ]))
        .declared_input("query", ValueScalar::Json, Required::Yes)
        // Nullable: the first page of a walk has no cursor yet.
        .declared_input("starting_after", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_count",
            "/total_count",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "next_starting_after",
            "/pages/next/starting_after",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only_documented(
            "Intercom's contact search is a read that reaches the API as a POST because its query \
             object does not fit a query string — \"This will accept a query object in the body \
             which will define your filters in order to search for contacts\" — and it creates and \
             changes nothing",
        )?)
        .build()?;

    let contact_create = identity(common(Operation::post("contact.create", "/contacts")))
        .body(JsonTemplate::object([
            ("role", JsonTemplate::input("role")),
            ("external_id", JsonTemplate::input("external_id")),
            ("email", JsonTemplate::input("email")),
            ("name", JsonTemplate::input("name")),
        ]))
        .declared_input("role", ValueScalar::String, Required::Yes)
        .declared_input("external_id", ValueScalar::Json, Required::Yes)
        .declared_input("email", ValueScalar::Json, Required::Yes)
        .declared_input("name", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK])
        .effect(at_most_once("a second contact with a new id")?)
        .build()?;

    let contact_update = identity(common(Operation::put(
        "contact.update",
        "/contacts/{contact_id}",
    )))
    .path_param("contact_id", ValueScalar::String)
    .body(JsonTemplate::object([
        ("email", JsonTemplate::input("email")),
        ("name", JsonTemplate::input("name")),
    ]))
    .declared_input("email", ValueScalar::Json, Required::Yes)
    .declared_input("name", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::OK])
    .effect(Effect::inventory_only(
        "Intercom documents `PUT /contacts/{contact_id}` as \"You can update an existing contact\" \
         with an all-optional request body and publishes no statement that the endpoint replaces \
         the resource, so it is a partial update rather than a write to a fixed resource identity; \
         no idempotency key is published for it either",
    )?)
    .build()?;

    let company_get = identity(common(Operation::get(
        "company.get",
        "/companies/{company_id}",
    )))
    .path_param("company_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    // The workspace's own identifier for the company, which is the field the
    // upsert below is looked up by.
    .output_pointer(
        "company_id",
        "/company_id",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "created_at",
        "/created_at",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer(
        "updated_at",
        "/updated_at",
        ValueScalar::Int64,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "You can fetch all companies and filter by `segment_id` or `tag_id` as a
    // query parameter", with `page` and `per_page` — this is the one collection
    // in the module that is not cursor-paginated.
    let company_list = common(Operation::get("company.list", "/companies"))
        .query_static("per_page", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_count",
            "/total_count",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Companies are looked up via `company_id` in a `POST` request, if not
    // found via `company_id`, the new company will be created, if found, that
    // company will be updated." A real upsert on a `POST`; see the module
    // documentation for why the gate still refuses it.
    let company_create_or_update = identity(common(Operation::post(
        "company.create_or_update",
        "/companies",
    )))
    .body(JsonTemplate::object([
        ("company_id", JsonTemplate::input("company_id")),
        ("name", JsonTemplate::input("name")),
    ]))
    .declared_input("company_id", ValueScalar::String, Required::Yes)
    .declared_input("name", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "company_id",
        "/company_id",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::inventory_only(
        "Intercom documents a real upsert here — \"Companies are looked up via `company_id` in a \
         `POST` request, if not found via `company_id`, the new company will be created, if found, \
         that company will be updated\" — but spec 010 §7 admits NaturalMethod for PUT and DELETE \
         only, and this is a POST. ADR 063 deliberately leaves it here: a documented upsert is \
         repeat-*safe*, and at-most-once would saddle it with \"sometimes never\" for a duplicate \
         the provider already absorbs. It waits for a repeat-safe evidence class, which is a \
         different decision",
    )?)
    .build()?;

    let conversation_get = identity(common(Operation::get(
        "conversation.get",
        "/conversations/{conversation_id}",
    )))
    .path_param("conversation_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("state", "/state", ValueScalar::String, Required::No)
    .output_pointer("open", "/open", ValueScalar::Boolean, Required::No)
    .output_pointer("read", "/read", ValueScalar::Boolean, Required::No)
    .output_pointer(
        "created_at",
        "/created_at",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer(
        "updated_at",
        "/updated_at",
        ValueScalar::Int64,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let conversation_list = common(Operation::get("conversation.list", "/conversations"))
        .query_static("per_page", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "conversations",
            "/conversations",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "total_count",
            "/total_count",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "next_starting_after",
            "/pages/next/starting_after",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "`conversation_id` — The Intercom provisioned identifier for the
    // conversation or the string \"last\" to reply to the last part of the
    // conversation."
    let conversation_reply = identity(common(Operation::post(
        "conversation.reply",
        "/conversations/{conversation_id}/reply",
    )))
    .path_param("conversation_id", ValueScalar::String)
    .body(JsonTemplate::object([
        ("message_type", JsonTemplate::input("message_type")),
        ("type", JsonTemplate::input("type")),
        ("body", JsonTemplate::input("body")),
        ("admin_id", JsonTemplate::input("admin_id")),
    ]))
    .declared_input("message_type", ValueScalar::String, Required::Yes)
    .declared_input("type", ValueScalar::String, Required::Yes)
    .declared_input("body", ValueScalar::String, Required::Yes)
    .declared_input("admin_id", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::OK])
    .effect(at_most_once(
        "a second reply, visible to the customer in the same conversation",
    )?)
    .build()?;

    Ok(vec![
        contact_get,
        contact_list,
        contact_search,
        contact_create,
        contact_update,
        company_get,
        company_list,
        company_create_or_update,
        conversation_get,
        conversation_list,
        conversation_reply,
    ])
}
