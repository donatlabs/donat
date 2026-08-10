//! The Google Calendar API v3.
//!
//! Ground truth is Google's own discovery document,
//! `https://www.googleapis.com/discovery/v1/apis/calendar/v3/rest`, read on
//! 2026-08-10 at revision `20260803`, plus *Errors* for the failure half. Every
//! quoted sentence below is that document's own `description` text.
//!
//! * `"baseUrl": "https://www.googleapis.com/calendar/v3/"`.
//! * `events.get` — "Returns an event based on its Google Calendar ID."
//! * `events.list` — "Returns events on the specified calendar."
//! * `events.insert` — "Creates an event."
//! * `events.update` — "Updates an event." (`PUT`; the sibling `events.patch`
//!   is the one Google documents as "This method supports patch semantics",
//!   which is how the two are told apart.)
//! * `events.delete` — "Deletes an event."
//! * `freebusy.query` — "Returns free/busy information for a set of calendars."
//!
//! # Effect classification
//!
//! `event.update` is `NaturalMethod`: a `PUT` on
//! `calendars/{calendarId}/events/{eventId}`, a fixed resource identity, and
//! Google distinguishes it from `events.patch` precisely by *not* giving it
//! patch semantics — it is the whole-event write, so a repeat leaves the same
//! event holding the same fields.
//!
//! `event.delete` is `NaturalMethod` on Google's own statement about a repeat:
//! the *Errors* page documents `410 Gone` as covering the case where "a request
//! attempts to delete an event that has already been deleted", so the second
//! `DELETE` is answered rather than acted on. This connector classifies `410`
//! `permanent`.
//!
//! `event.insert` is `InventoryOnly`: each call creates an event with a new
//! `id`, and Calendar publishes no idempotency key — neither `idempot` nor
//! `dedup` occurs in the discovery document. Calendar *does* let a caller
//! supply its own `id` on insert, which is the nearest thing to a key it
//! publishes; it is not one, because Google answers a duplicate with `409` and
//! a `409` is a different outcome from the first call rather than the same one.
//! That near-miss is why ADR 063 leaves this operation `InventoryOnly` too: a
//! client-supplied identifier a connector could bind is not something a
//! deployment steps past with an at-most-once opt-in. It is recorded in
//! `providers/INVENTORY.md` rather than dropped.
//!
//! `freebusy.query` is a `POST` that Google documents as "Returns free/busy
//! information", creating and changing nothing. It is therefore declared with
//! [`Effect::read_only_documented`] rather than by its method — the case ADR
//! 042 admits evidence for.
//!
//! # A per-item failure inside a `200`
//!
//! `freeBusy` is the one operation in this batch with a documented partial
//! failure. `FreeBusyResponse.calendars` maps each requested calendar to a
//! `FreeBusyCalendar { busy, errors }`, and `errors` is a list of
//! `Error { reason, domain }` whose documented reasons are "`groupTooBig` — The
//! group of users requested is too large for a single query", "
//! `tooManyCalendarsRequested`", "`notFound` — The requested resource was not
//! found", and "`internalError` — The API service has encountered an internal
//! error", with the note that "Additional error types may be added in the
//! future, so clients should gracefully handle additional error statuses not
//! included in this list."
//!
//! A reply that carries any of them is a failure, not an output: half a
//! free/busy answer is indistinguishable downstream from a complete one that
//! happened to find nothing busy. The reason selects the class, and an
//! unrecognized reason — which Google says to expect — takes `permanent`, the
//! class that means the same question gets the same answer.
//!
//! # Continuations
//!
//! `events.list` publishes `nextPageToken`, "Token used to access the next page
//! of this result. Omitted if no further results are available", declared as a
//! token in the body so it can only be spent as a query value on this
//! connector's compiled origin. `nextSyncToken` is deliberately not a
//! continuation: Google documents it as a token "used at a later point in time
//! to retrieve only the entries that have changed", which is a different
//! request made by a different activity.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::providers::google::{self, ScopeRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "google_calendar";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `"baseUrl": "https://www.googleapis.com/calendar/v3/"`.
const ORIGIN: &str = "https://www.googleapis.com";

const CALENDAR: &str = "https://www.googleapis.com/auth/calendar";
const CALENDAR_READONLY: &str = "https://www.googleapis.com/auth/calendar.readonly";
const CALENDAR_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
const CALENDAR_EVENTS_READONLY: &str = "https://www.googleapis.com/auth/calendar.events.readonly";
const CALENDAR_EVENTS_OWNED: &str = "https://www.googleapis.com/auth/calendar.events.owned";
const CALENDAR_EVENTS_OWNED_READONLY: &str =
    "https://www.googleapis.com/auth/calendar.events.owned.readonly";
const CALENDAR_EVENTS_PUBLIC_READONLY: &str =
    "https://www.googleapis.com/auth/calendar.events.public.readonly";
const CALENDAR_EVENTS_FREEBUSY: &str = "https://www.googleapis.com/auth/calendar.events.freebusy";
const CALENDAR_FREEBUSY: &str = "https://www.googleapis.com/auth/calendar.freebusy";
const CALENDAR_APP_CREATED: &str = "https://www.googleapis.com/auth/calendar.app.created";

/// `events.get` and `events.list`: the nine scopes the discovery document
/// lists.
const EVENT_READ_SCOPES: &[&str] = &[
    CALENDAR_EVENTS_READONLY,
    CALENDAR_EVENTS_OWNED_READONLY,
    CALENDAR_EVENTS_PUBLIC_READONLY,
    CALENDAR_EVENTS_FREEBUSY,
    CALENDAR_EVENTS,
    CALENDAR_EVENTS_OWNED,
    CALENDAR_READONLY,
    CALENDAR,
    CALENDAR_APP_CREATED,
];

/// `events.insert`, `events.update`, and `events.delete`.
const EVENT_WRITE_SCOPES: &[&str] = &[
    CALENDAR_EVENTS,
    CALENDAR_EVENTS_OWNED,
    CALENDAR,
    CALENDAR_APP_CREATED,
];

/// `freebusy.query`: the four scopes the discovery document lists.
const FREEBUSY_SCOPES: &[&str] = &[
    CALENDAR_FREEBUSY,
    CALENDAR_EVENTS_FREEBUSY,
    CALENDAR_READONLY,
    CALENDAR,
];

/// "By default the value is 250 events. The page size can never be larger than
/// 2500 events." The documented default is what this connector pins: an event
/// carries attendees, reminders, and conference data, and 2500 of them would
/// not fit the SDK's 1 MiB response ceiling.
const PAGE_SIZE: &str = "250";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Google's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Google Calendar declarations are valid"))
            .build()
            .expect("the Google Calendar declaration is valid")
    });
    &CONNECTOR
}

/// The shared Google error map; see `providers/google.rs`.
pub fn error_map() -> &'static ErrorMap {
    google::error_map()
}

/// The continuation plan of the one listing.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static EVENTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/items", "/nextPageToken", "pageToken")
            .expect("the Calendar event listing plan is valid")
    });
    match operation_id {
        "event.list" => Some(&EVENTS),
        _ => None,
    }
}

/// The scopes one operation is authorized by, as the discovery document lists
/// them for that exact method.
pub fn scopes(operation_id: &str) -> Option<ScopeRequirement> {
    match operation_id {
        "event.get" | "event.list" => Some(ScopeRequirement::documented(
            CALENDAR_EVENTS_READONLY,
            EVENT_READ_SCOPES,
        )),
        "event.insert" | "event.update" | "event.delete" => Some(ScopeRequirement::documented(
            CALENDAR_EVENTS,
            EVENT_WRITE_SCOPES,
        )),
        "freebusy.query" => Some(ScopeRequirement::documented(
            CALENDAR_FREEBUSY,
            FREEBUSY_SCOPES,
        )),
        _ => None,
    }
}

/// Decode one response, refusing a `freeBusy` reply that reports a per-calendar
/// or per-group failure.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    google::decode(operation, status, headers, body, refuse_partial_freebusy)
}

/// The class one documented `freeBusy` reason belongs to.
///
/// Google says to expect reasons outside this list, so the fallback is
/// `permanent` — the class that means asking the same question again produces
/// the same answer — rather than a retry of something that may never succeed.
fn freebusy_failure(reason: Option<&str>) -> ConnectorFailure {
    match reason {
        // "The group of users requested is too large for a single query." /
        // "The number of calendars requested is too large for a single query."
        // Both are answered by changing the request, which is what `validation`
        // means.
        Some("groupTooBig" | "tooManyCalendarsRequested") => ConnectorFailure::new(
            ConnectorErrorClass::Validation,
            "google_partial_failure",
            "the provider refused part of the request as too large",
        ),
        // "The API service has encountered an internal error."
        Some("internalError") => ConnectorFailure::new(
            ConnectorErrorClass::Http5xx,
            "google_partial_failure",
            "the provider reported an internal error for part of the request",
        ),
        // "notFound", and everything Google adds later.
        _ => google::PARTIAL_FAILURE,
    }
}

/// `FreeBusyResponse.calendars` and `.groups` are maps whose values each carry
/// an `errors` list. A non-empty one anywhere is a failure for the whole reply.
fn refuse_partial_freebusy(operation_id: &str, value: &JsonValue) -> Result<(), ConnectorFailure> {
    if operation_id != "freebusy.query" {
        return Ok(());
    }
    for section in ["calendars", "groups"] {
        let Some(JsonValue::Object(entries)) = value.get(section) else {
            continue;
        };
        for entry in entries.values() {
            let errors = entry.get("errors");
            if google::reports_item_errors(errors) {
                let reason = errors
                    .and_then(|errors| errors.get(0))
                    .and_then(|first| first.get("reason"))
                    .and_then(JsonValue::as_str);
                return Err(freebusy_failure(reason));
            }
        }
    }
    Ok(())
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The output contract of one event resource.
fn event_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("summary", "/summary", ValueScalar::String, Required::No)
        .output_pointer("start", "/start", ValueScalar::Json, Required::No)
        .output_pointer("end", "/end", ValueScalar::Json, Required::No)
        .output_pointer("attendees", "/attendees", ValueScalar::Json, Required::No)
        .output_pointer("html_link", "/htmlLink", ValueScalar::String, Required::No)
        .output_pointer("updated", "/updated", ValueScalar::String, Required::No)
        .output_pointer("etag", "/etag", ValueScalar::String, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let event_get = event_outputs(
        common(Operation::get(
            "event.get",
            "/calendar/v3/calendars/{calendar_id}/events/{event_id}",
        ))
        .path_param("calendar_id", ValueScalar::String)
        .path_param("event_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // The window is declared rather than optional. Google documents `timeMin`
    // and `timeMax` as optional and the listing as returning every event on the
    // calendar without them, which is an unbounded amount of work for a durable
    // activity; a deployment says which window it means.
    let event_list = common(Operation::get(
        "event.list",
        "/calendar/v3/calendars/{calendar_id}/events",
    ))
    .path_param("calendar_id", ValueScalar::String)
    .query_input("timeMin", "time_min")
    .query_input("timeMax", "time_max")
    .query_static("maxResults", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .output_pointer("items", "/items", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "next_page_token",
        "/nextPageToken",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "next_sync_token",
        "/nextSyncToken",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("time_zone", "/timeZone", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    let event_insert = event_outputs(
        common(Operation::post(
            "event.insert",
            "/calendar/v3/calendars/{calendar_id}/events",
        ))
        .path_param("calendar_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("summary", JsonTemplate::input("summary")),
            ("start", JsonTemplate::input("start")),
            ("end", JsonTemplate::input("end")),
        ]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Each call creates an event with a new `id`, and Calendar publishes no idempotency key. \
         The caller-supplied `id` on insert is the nearest thing Google publishes and is not one: \
         a duplicate is answered `409`, which is a different outcome from the first call rather \
         than the same one.",
    )?)
    .build()?;

    // "Updates an event." Google's sibling `events.patch` is the one documented
    // as supporting patch semantics, which is what makes this the whole-event
    // write.
    let event_update = event_outputs(
        common(Operation::put(
            "event.update",
            "/calendar/v3/calendars/{calendar_id}/events/{event_id}",
        ))
        .path_param("calendar_id", ValueScalar::String)
        .path_param("event_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("summary", JsonTemplate::input("summary")),
            ("start", JsonTemplate::input("start")),
            ("end", JsonTemplate::input("end")),
        ]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::provider_idempotent_natural_method(
        "Google documents `events.update` as `PUT \
         /calendar/v3/calendars/{calendarId}/events/{eventId}` — a fixed resource identity — and \
         distinguishes it from `events.patch`, the method it documents as \"This method supports \
         patch semantics\". A repeat of the same `PUT` therefore leaves the same event holding \
         the same fields.",
    )?)
    .build()?;

    let event_delete = common(Operation::delete(
        "event.delete",
        "/calendar/v3/calendars/{calendar_id}/events/{event_id}",
    ))
    .path_param("calendar_id", ValueScalar::String)
    .path_param("event_id", ValueScalar::String)
    // Google documents the successful response body as empty and publishes no
    // status code in the discovery document, so both of the statuses it uses
    // for an empty success are declared rather than one of them guessed.
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Google documents `events.delete` as `DELETE \
         /calendar/v3/calendars/{calendarId}/events/{eventId}` — a fixed resource identity — and \
         its *Errors* page documents the repeat directly: `410 Gone` \"can also occur if a \
         request attempts to delete an event that has already been deleted\". The second delete \
         is answered, not acted on.",
    )?)
    .build()?;

    // "Returns free/busy information for a set of calendars."
    let freebusy_query = common(Operation::post("freebusy.query", "/calendar/v3/freeBusy"))
        .body(JsonTemplate::object([
            ("timeMin", JsonTemplate::input("time_min")),
            ("timeMax", JsonTemplate::input("time_max")),
            ("items", JsonTemplate::input("items")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("calendars", "/calendars", ValueScalar::Json, Required::Yes)
        .output_pointer("time_min", "/timeMin", ValueScalar::String, Required::No)
        .output_pointer("time_max", "/timeMax", ValueScalar::String, Required::No)
        .effect(Effect::read_only_documented(
            "Google documents `freebusy.query` as \"Returns free/busy information for a set of \
             calendars\". It is a `POST` because the set of calendars is a request body rather \
             than a query string; it creates no resource, changes none, and its response is the \
             same for the same request.",
        )?)
        .build()?;

    Ok(vec![
        event_get,
        event_list,
        event_insert,
        event_update,
        event_delete,
        freebusy_query,
    ])
}
