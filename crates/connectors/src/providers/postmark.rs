//! Postmark API (email, messages, bounces, templates).
//!
//! Ground truth is Postmark's own published developer documentation, read on
//! 2026-08-10:
//!
//! * <https://postmarkapp.com/developer/api/overview> — base URL
//!   `https://api.postmarkapp.com`; the `X-Postmark-Server-Token` header for
//!   server-level operations; `Accept` and `Content-Type` must be
//!   `application/json`; the status codes 200 "Everything went smooth.", 401
//!   "Missing or incorrect API token in header.", 404 "You made a request for a
//!   resource/entity that does not exist.", 422 "Something with the message
//!   isn't quite right, this could be malformed JSON or incorrect fields.", 429
//!   "Rate Limit Exceeded", 500 "This is an issue with Postmark's servers
//!   processing your request.", 503 "During planned service outages, Postmark
//!   API services will return this HTTP response."; and the numeric `ErrorCode`
//!   list this module's error map keys on.
//! * <https://postmarkapp.com/developer/api/email-api>,
//!   <https://postmarkapp.com/developer/api/templates-api>,
//!   <https://postmarkapp.com/developer/api/messages-api>,
//!   <https://postmarkapp.com/developer/api/bounce-api>.
//!
//! # Two Postmark shapes worth naming
//!
//! Postmark answers a *rejected* send with `200` and a non-zero `ErrorCode`,
//! not only with `422`. The send declarations therefore require `MessageID` in
//! their output contract: a `200` that carries no message identifier does not
//! satisfy the declared contract and is a validation failure rather than a
//! delivered email. The error map keys on the documented `ErrorCode` as well as
//! on the status, so the same code classifies the same way whichever status
//! carries it.
//!
//! `count` and `offset` are documented as *required* on the list endpoints, so
//! they are owned by each operation's pagination plan rather than by input: a
//! list operation is always used through its plan, which is what keeps
//! "Count + Offset cannot exceed 10,000 messages" a bound the connector
//! respects rather than a value a caller chooses.
//!
//! # Effect classification
//!
//! Postmark's documentation for `POST /email` and `POST /email/withTemplate`
//! documents the complete request contract and contains no idempotency key, no
//! client-supplied request identifier, and no deduplication of a repeated send;
//! its `ErrorCode` list has no code for a replayed request either. Both sends
//! are `AtMostOnce` (ADR 063) — a repeat delivers a second email with a new
//! `MessageID` — and are reachable only from a Process activity that declared
//! `at_most_once` and a route for an outcome nobody can know (see
//! `INVENTORY.md`). Everything else here is a `GET`.

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
pub const NAME: &str = "postmark";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Postmark's one published API origin.
const ORIGIN: &str = "https://api.postmarkapp.com";

/// "X-Postmark-Server-Token" — the server-level credential header.
pub const SERVER_TOKEN_HEADER: &str = "X-Postmark-Server-Token";

/// "count (required, max 500)" with "Count + Offset cannot exceed 10,000
/// messages": the declared page size stays well inside both.
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Postmark's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::api_key_header(SERVER_TOKEN_HEADER)
                    .expect("Postmark's documented token header is a valid header name"),
            ))
            .operations(operations().expect("the Postmark declarations are valid"))
            .build()
            .expect("the Postmark declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Postmark publishes a stable numeric `ErrorCode` beside the message, so the
/// code rules come first: the same code means the same thing whether Postmark
/// carried it on a `422` or on a `200`.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/ErrorCode")
            // "10: Bad or missing API token" — Postmark answers this on a 422
            // as well as on a 401.
            .on_code("10", ConnectorErrorClass::Authentication)
            // "405: Not allowed to send" (the account has exhausted its
            // credits) and "406: Inactive recipient" are both permanent for
            // this request: sending it again changes nothing.
            .on_code("405", ConnectorErrorClass::Permanent)
            .on_code("406", ConnectorErrorClass::Permanent)
            // "402: Syntactically incorrect JSON", "409: missing Accept /
            // Content-Type headers", "410: batch request exceeds 500 messages"
            // are this deployment's request to fix.
            .on_code("402", ConnectorErrorClass::Validation)
            .on_code("409", ConnectorErrorClass::Validation)
            .on_code("410", ConnectorErrorClass::Validation)
            // "100: Maintenance mode" is Postmark's own planned outage.
            .on_code("100", ConnectorErrorClass::Http5xx)
            // "401: Missing or incorrect API token in header."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "404: You made a request for a resource/entity that does not
            // exist."
            .on_status(404, ConnectorErrorClass::Permanent)
            // "422: Something with the message isn't quite right, this could be
            // malformed JSON or incorrect fields." and the 415 Postmark returns
            // when the request headers are missing.
            .on_statuses([415, 422], ConnectorErrorClass::Validation)
            // "429: Rate Limit Exceeded".
            .on_status(429, ConnectorErrorClass::Http429)
            // "500: This is an issue with Postmark's servers processing your
            // request." and "503: During planned service outages...".
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Postmark error map is a valid declaration")
    });
    &MAP
}

/// The pagination plan of an operation Postmark documents as paginated.
///
/// Every list endpoint takes the same required `count`/`offset` pair — the
/// templates endpoint spells it `Count`/`Offset` — and answers with a total and
/// an array. A page shorter than the requested count ends the walk.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static MESSAGES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/Messages", "offset", "count", PAGE_SIZE)
            .expect("the Postmark message pagination plan is valid")
    });
    static BOUNCES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/Bounces", "offset", "count", PAGE_SIZE)
            .expect("the Postmark bounce pagination plan is valid")
    });
    static TEMPLATES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/Templates", "Offset", "Count", PAGE_SIZE)
            .expect("the Postmark template pagination plan is valid")
    });
    match operation_id {
        "message.list_outbound" => Some(&MESSAGES),
        "bounce.list" => Some(&BOUNCES),
        "template.list" => Some(&TEMPLATES),
        _ => None,
    }
}

/// Postmark documents `Accept: application/json` (and `Content-Type` on the
/// endpoints with a body) as required, answering `409`/`415` without it.
fn json_operation(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Accept", "application/json")
        .success_statuses([StatusCode::OK])
}

/// The four output fields Postmark documents for both send endpoints.
fn send_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer(
            "message_id",
            "/MessageID",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "submitted_at",
            "/SubmittedAt",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("to", "/To", ValueScalar::String, Required::Yes)
        .output_pointer(
            "error_code",
            "/ErrorCode",
            ValueScalar::Int64,
            Required::Yes,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // <https://postmarkapp.com/developer/api/email-api>: `POST /email`, with
    // the documented required fields `From`, `To`, `Subject`, and one of
    // `TextBody`/`HtmlBody`. This declaration carries the text body; an HTML
    // body is a second declared operation rather than an optional slot.
    let send = send_outputs(
        json_operation(Operation::post("email.send", "/email")).body(JsonTemplate::object([
            ("From", JsonTemplate::input("from")),
            ("To", JsonTemplate::input("to")),
            ("Subject", JsonTemplate::input("subject")),
            ("TextBody", JsonTemplate::input("text_body")),
        ])),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "Postmark's Email API documents the complete request contract for POST /email and \
             publishes no idempotency key, request identifier, or deduplication; none of the \
             fifty-odd published `ErrorCode` values describes a replayed request",
        "a second delivered email with a new `MessageID`",
    )?))
    .build()?;

    // <https://postmarkapp.com/developer/api/templates-api>: `POST
    // /email/withTemplate`, required `TemplateId` or `TemplateAlias`,
    // `TemplateModel`, `From`, `To`. This declaration carries the alias.
    let send_template = send_outputs(
        json_operation(Operation::post(
            "email.send_template",
            "/email/withTemplate",
        ))
        .body(JsonTemplate::object([
            ("From", JsonTemplate::input("from")),
            ("To", JsonTemplate::input("to")),
            ("TemplateAlias", JsonTemplate::input("template_alias")),
            ("TemplateModel", JsonTemplate::input("template_model")),
        ])),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "Postmark's Templates API documents the complete request contract for POST \
             /email/withTemplate and publishes no idempotency key, request identifier, or \
             deduplication; it shares its delivery path with POST /email",
        "a second delivered email with a new `MessageID`",
    )?))
    .build()?;

    // <https://postmarkapp.com/developer/api/messages-api>
    let message_get = json_operation(Operation::get(
        "message.get",
        "/messages/outbound/{message_id}/details",
    ))
    .path_param("message_id", ValueScalar::String)
    .output_pointer(
        "message_id",
        "/MessageID",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer("status", "/Status", ValueScalar::String, Required::Yes)
    .output_pointer("to", "/To", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "message_events",
        "/MessageEvents",
        ValueScalar::Json,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let message_list = json_operation(Operation::get(
        "message.list_outbound",
        "/messages/outbound",
    ))
    .output_pointer(
        "total_count",
        "/TotalCount",
        ValueScalar::Int64,
        Required::Yes,
    )
    .output_pointer("messages", "/Messages", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // <https://postmarkapp.com/developer/api/bounce-api>
    let bounce_list = json_operation(Operation::get("bounce.list", "/bounces"))
        .output_pointer(
            "total_count",
            "/TotalCount",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer("bounces", "/Bounces", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let bounce_get = json_operation(Operation::get("bounce.get", "/bounces/{bounce_id}"))
        .path_param("bounce_id", ValueScalar::Int64)
        .output_pointer("id", "/ID", ValueScalar::Int64, Required::Yes)
        .output_pointer("type", "/Type", ValueScalar::String, Required::Yes)
        .output_pointer("email", "/Email", ValueScalar::String, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // <https://postmarkapp.com/developer/api/templates-api>
    let template_list = json_operation(Operation::get("template.list", "/templates"))
        .output_pointer(
            "total_count",
            "/TotalCount",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer("templates", "/Templates", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let template_get = json_operation(Operation::get("template.get", "/templates/{template_id}"))
        .path_param("template_id", ValueScalar::String)
        .output_pointer(
            "template_id",
            "/TemplateId",
            ValueScalar::Int64,
            Required::Yes,
        )
        // A template without an alias carries a null one, which the declaration
        // publishes as an explicit null rather than refusing.
        .output_pointer("alias", "/Alias", ValueScalar::String, Required::No)
        .output_pointer("name", "/Name", ValueScalar::String, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        send,
        send_template,
        message_get,
        message_list,
        bounce_list,
        bounce_get,
        template_list,
        template_get,
    ])
}
