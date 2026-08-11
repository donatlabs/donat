//! Zoom's Meeting API, v2.
//!
//! Ground truth is Zoom's own published OpenAPI description and its own
//! documentation, read on 2026-08-10:
//!
//! * <https://developers.zoom.us/api-hub/meetings/methods/endpoints.json> —
//!   Zoom's published `openapi: 3.0.0` description of the Meetings API, whose
//!   one `servers` entry is `{"url": "https://api.zoom.us/v2"}`, and from which
//!   every path, parameter, required body field, success status, and documented
//!   error code below is taken.
//! * <https://developers.zoom.us/docs/api/rate-limits/> — the rate-limit labels
//!   each method publishes (`LIGHT`, `MEDIUM`, `HEAVY`) and the error table:
//!   "Per-second | 429 | You have reached the maximum per-second rate limit for
//!   this API. Try again later" and "Daily | 429 | You have reached the maximum
//!   daily rate limit for this API. Refer to the response header for details on
//!   when you can make another request."
//!
//! # The credential is authorization-code OAuth2, and server-to-server is not
//!
//! Spec 025 asks for Zoom's server-to-server credential. Zoom's server-to-server
//! app publishes its own grant — `grant_type=account_credentials` with an
//! `account_id` parameter — which is **not** RFC 6749 §4.4, and the SDK's
//! `AuthPlan::oauth2_client_credentials` renders exactly §4.4's
//! `grant_type=client_credentials` exchange
//! ([[072-a-minted-credential-is-spent-inside-one-attempt]]). Describing Zoom's
//! grant with it would send an exchange Zoom does not publish; describing it
//! properly is a new auth plan, which is an SDK change with its own tests and
//! its own ADR. This connector therefore declares the authorization-code plan
//! Zoom publishes for a general OAuth app, which the spec 011 stored-credential
//! seam already serves end to end. `knowledgebase/declarative-saas/decisions/075-*`
//! records the refusal to widen.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Zoom's published 1.19 MB Meetings description, and neither does `dedup`: no
//! request header, no body property, and no response field of `POST
//! /users/{userId}/meetings` carries a client-supplied request identifier.
//!
//! `meeting.create` is therefore `AtMostOnce` (ADR 063). Its consequence is the
//! one an operator has to accept out loud: a second scheduled meeting, with a
//! new `id`, a new `join_url`, and — where the deployment's Zoom settings send
//! them — a second set of invitations to the host and the alternative hosts.
//! Zoom's own user-level ceiling makes the accumulation visible: "100
//! create/update requests per day (UTC) per user".
//!
//! `meeting.delete` is `ProviderIdempotent::NaturalMethod`: `DELETE
//! /meetings/{meetingId}` against a fixed identity, whose second send Zoom
//! publishes as "**HTTP Status Code:** `404` Not Found — **Error Code:** `3001`
//! — Meeting does not exist: {meetingId}." The `occurrence_id` parameter is
//! deliberately not declared, because Zoom publishes that its presence changes
//! *which* thing is deleted — "For recurring meetings, the `occurrence_id` is
//! required to delete a specific occurrence. If not provided, the entire
//! recurring series will be deleted" — and an operation whose identity a caller
//! can narrow is not one fixed identity.
//!
//! `PATCH /meetings/{meetingId}` is not declared at all: it is a partial update
//! over a method spec 010 §7 admits for neither mutating class, and spec 025 §3
//! does not ask for it.

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
pub const NAME: &str = "zoom";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ "url": "https://api.zoom.us/v2" }]`, with the path half declared
/// per operation.
const ORIGIN: &str = "https://api.zoom.us";

/// The route prefix every endpoint here is served from.
const PREFIX: &str = "/v2";

/// "`meeting:read`" and "`meeting:write`" — the two classic scopes Zoom
/// publishes for this surface, which a deployment's OAuth app must hold.
pub const READ_SCOPE: &str = "meeting:read";
pub const WRITE_SCOPE: &str = "meeting:write";

/// "`page_size` — The number of records returned within a single API call",
/// published with a default of 30 and a maximum of 300.
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Zoom's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Zoom declarations are valid"))
            .build()
            .expect("the Zoom declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from the statuses Zoom's own description declares on
/// every method in this connector.
///
/// It is keyed on the status alone. Zoom publishes a numeric `code` in every
/// error body and publishes it *per endpoint* rather than as one set — `3001`
/// means "Meeting does not exist" on the meeting routes and something else
/// elsewhere — so keying on it would be keying on a value whose meaning depends
/// on the path.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — Error Code: 300 — Invalid parameter:
            // `occurrence_id`", "Error Code: 3000 — Cannot access webinar info."
            .on_status(400, ConnectorErrorClass::Validation)
            // Zoom publishes `401` as the answer to an invalid or expired
            // access token throughout its OAuth documentation.
            .on_status(401, ConnectorErrorClass::Authentication)
            // "403 Forbidden — Error Code: 2306 — Not allowed to view meetings
            // scheduled for others", "404 Not Found — Error Code: 3001 —
            // Meeting does not exist", "409 Conflict".
            .on_statuses([403, 404, 405, 409], ConnectorErrorClass::Permanent)
            // "429 Too Many Requests. For more information, see rate limits."
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // Zoom echoes a request identifier on every response; it is the
            // handle its own support asks for.
            .correlation_header("request_id", "x-zm-trackingid")
            .build()
            .expect("the Zoom error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of the one collection here.
///
/// "`next_page_token` — Use the next page token to paginate through large result
/// sets. A next page token is returned whenever the set of available results
/// exceeds the current page size." Zoom answers the last page with an empty
/// string, which the SDK's cursor plan reads as absent and stops on.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static MEETINGS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/meetings",
            "next_page_token",
            "/next_page_token",
            "page_size",
            PAGE_SIZE,
        )
        .expect("the Zoom cursor plan is valid")
    });
    match operation_id {
        "meeting.list" => Some(&MEETINGS),
        _ => None,
    }
}

/// The scopes Zoom publishes for one operation, for the deploy-time check that a
/// deployment authorized what it enabled.
#[must_use]
pub fn scopes(operation_id: &str) -> &'static [&'static str] {
    match operation_id {
        "meeting.get" | "meeting.list" => &[READ_SCOPE, WRITE_SCOPE],
        _ => &[WRITE_SCOPE],
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The fields of one meeting, as Zoom's own description declares them.
fn meeting_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        // "When storing this value in your database, store it as a long format
        // integer and **not** an integer. Meeting IDs can be more than 10
        // digits."
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("uuid", "/uuid", ValueScalar::String, Required::No)
        .output_pointer("topic", "/topic", ValueScalar::String, Required::No)
        .output_pointer("type", "/type", ValueScalar::Int64, Required::No)
        .output_pointer(
            "start_time",
            "/start_time",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("duration", "/duration", ValueScalar::Int64, Required::No)
        .output_pointer("timezone", "/timezone", ValueScalar::String, Required::No)
        .output_pointer("join_url", "/join_url", ValueScalar::String, Required::No)
        .output_pointer("host_id", "/host_id", ValueScalar::String, Required::No)
}

/// Every operation this connector publishes: spec 025 §3's Zoom surface —
/// meeting read, list, create, delete — and nothing else. Registrants, polls,
/// livestreams, and recordings are their own surfaces, and the recording
/// download is out of scope (spec 025 §5).
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Retrieve the given meeting's details."
    let meeting_get = meeting_outputs(
        common(Operation::get(
            "meeting.get",
            &format!("{PREFIX}/meetings/{{meeting_id}}"),
        ))
        .path_param("meeting_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "List a meeting host user's scheduled meetings. For user-level apps, pass
    // the `me` value instead of the `userId` parameter." The path parameter is
    // therefore a string rather than a number.
    let meeting_list = common(Operation::get(
        "meeting.list",
        &format!("{PREFIX}/users/{{user_id}}/meetings"),
    ))
    .path_param("user_id", ValueScalar::String)
    // "`type` — The meeting type. `scheduled` — All valid previous (unexpired)
    // meetings, live meetings, and upcoming scheduled meetings." Which of them
    // a deployment means is the caller's.
    .query_input("type", "type")
    .success_statuses([StatusCode::OK])
    .output_pointer("meetings", "/meetings", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "total_records",
        "/total_records",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer(
        "next_page_token",
        "/next_page_token",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Creates a meeting for a user." Zoom's own description declares no
    // required body field; `topic`, `type`, `start_time`, `duration`, and
    // `timezone` are the ones a process actually sets when it schedules one.
    let meeting_create = meeting_outputs(
        common(Operation::post(
            "meeting.create",
            &format!("{PREFIX}/users/{{user_id}}/meetings"),
        ))
        .path_param("user_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("topic", JsonTemplate::input("topic")),
            ("type", JsonTemplate::input("type")),
            ("start_time", JsonTemplate::input("start_time")),
            ("duration", JsonTemplate::input("duration")),
            ("timezone", JsonTemplate::input("timezone")),
            ("agenda", JsonTemplate::input("agenda")),
        ]))
        .declared_input("topic", ValueScalar::String, Required::Yes)
        // "201 — Meeting created."
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in Zoom's published 1.19 MB OpenAPI \
         description of the Meetings API: the create declares `agenda`, `default_password`, \
         `duration`, `password`, `pre_schedule`, `recurrence`, `schedule_for`, `settings`, \
         `start_time`, `template_id`, and `timezone`, and no request header, body property, or \
         response field carries a client-supplied request identifier",
        "a second scheduled meeting with a new id and a new join URL, and — where the account's \
         settings send them — a second set of invitations to the host and alternative hosts; it \
         also spends a second unit of Zoom's published ceiling of \"100 create/update requests per \
         day (UTC) per user\"",
    )?))
    .build()?;

    // "Delete a meeting." Zoom documents the success as `204` with no body.
    let meeting_delete = common(Operation::delete(
        "meeting.delete",
        &format!("{PREFIX}/meetings/{{meeting_id}}"),
    ))
    .path_param("meeting_id", ValueScalar::Int64)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Zoom publishes this as `DELETE /meetings/{meetingId}` — a fixed resource identity — with \
         \"**HTTP Status Code**: `204` Meeting deleted\", and publishes what a second send \
         answers: \"**HTTP Status Code:** `404` Not Found — **Error Code:** `3001` — Meeting does \
         not exist: {meetingId}.\" A repeat names the same meeting, finds it gone, and answers \
         `404`, which this connector classifies `permanent`; it never deletes a second meeting.",
    )?)
    .build()?;

    Ok(vec![
        meeting_get,
        meeting_list,
        meeting_create,
        meeting_delete,
    ])
}
