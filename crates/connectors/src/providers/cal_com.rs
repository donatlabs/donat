//! Cal.com's API v2.
//!
//! Ground truth is Cal.com's own published documentation, read on 2026-08-10:
//! its API v2 reference and agent guide at <https://cal.com/docs> (served
//! verbatim at <https://cal.com/docs/llms-full.txt>) and the OpenAPI 3.0
//! document it publishes at
//! <https://cal.com/docs/api-reference/v2/openapi.json>. Every quotation below
//! is one of those two.
//!
//! * The origin and the credential:
//!   `curl -X GET "https://api.cal.com/v2/me" -H "Authorization: Bearer
//!   cal_live_xxxxxxxxxxxx" -H "cal-api-version: 2024-08-13"`, and the OpenAPI's
//!   own parameter description — "value must be `Bearer <token>` where `<token>`
//!   is api key prefixed with cal_, managed user access token, or OAuth access
//!   token".
//! * `GET /v2/event-types` — "Hidden event types are returned only if
//!   authentication is provided and it belongs to the event type owner."
//! * `GET /v2/event-types/{eventTypeId}` — "This endpoint fetches an event type
//!   by ID and returns it only if the authenticated user is authorized."
//! * `GET /v2/bookings` — "Cursor-based pagination. Pass the
//!   `pagination.nextCursor` from the previous response as the `cursor` query
//!   parameter to fetch the next page. Omit `cursor` to fetch the first page.
//!   `pagination.hasMore` is `false` and `pagination.nextCursor` is `null` when
//!   you've reached the last page."
//! * `GET /v2/bookings/{bookingUid}`, `POST /v2/bookings` — "POST /v2/bookings
//!   is used to create regular bookings, recurring bookings and instant
//!   bookings" — and `POST /v2/bookings/{bookingUid}/cancel` — "Cancel a
//!   booking".
//! * "There are three authentication methods for the API, and each of them has
//!   the following rate limits: 1. API Key - 120 requests per minute."
//!
//! # v1 is decommissioned, and that is why the credential is a header
//!
//! Spec 028 §1 listed this provider's credential as "`ApiKeyQuery` or `Bearer` —
//! verify the current version", and the verification is unambiguous. Cal.com's
//! own v1 discovery document answers
//! `{"message": "API v1 has been decommissioned. Please migrate to API v2:
//! https://cal.com/docs/api-reference/v2"}` with HTTP `410 Gone`, and its
//! migration checklist opens with "Update authentication to use `Authorization`
//! header instead of query parameter". The query form is the superseded version,
//! exactly the shape
//! [[081-a-credential-is-an-authentication-parameter-and-a-body-credential-is-a-version-that-was-superseded]]
//! records, so only v2's bearer header is declared here.
//!
//! # The API version is per operation, not per connector
//!
//! `cal-api-version` is a **required header** on every v2 endpoint — "The
//! `cal-api-version` header is required for all v2 endpoints. If you omit it,
//! requests will return a 404" — and the value differs per endpoint. The OpenAPI
//! publishes it as a `required` parameter with a per-endpoint description, and
//! the three this connector needs are different from each other: "Must be set to
//! 2026-05-01" on the booking collection, "Must be set to 2026-02-25" on the
//! booking read, create and cancel, and "Must be set to 2024-06-14" on the event
//! types. Each is therefore a static header of the *operation* rather than a
//! constant of the connector.
//!
//! Cal.com's agent guide says "Always include `cal-api-version: 2024-08-13`",
//! which contradicts its own OpenAPI on all three. The reference that owns each
//! operation wins, which is the rule
//! [[073-a-retention-is-read-from-the-reference-that-owns-the-operation]] states
//! for a retention and which applies to any per-endpoint fact.
//!
//! # Pagination
//!
//! The booking collection publishes an opaque cursor and both halves of its
//! termination: `pagination.nextCursor` — "Opaque cursor to fetch the next page"
//! — and `pagination.hasMore`. The declared plan spends the cursor as a query
//! value and stops when the provider stops publishing one, so a page shorter
//! than the size asked for never ends the walk and a body that spells a URL
//! never becomes a destination. The event-type collection publishes no
//! pagination parameter of any kind, so it declares no plan.
//!
//! # A `200` can carry a failure
//!
//! Every v2 response is an envelope whose first member is a two-valued status:
//! "`status`: Either "success" or "error"", "`error`: Error details (only
//! present when status is "error")", with the failure shape published as
//! `{"status": "error", "error": {"code": "NOT_FOUND", "message": "Event type
//! not found"}}`. The OpenAPI constrains `status` to `["success", "error"]` on
//! the *success* schema of every endpoint declared here, so a declared success
//! status is not on its own evidence that the body is one. [`decode`] gates the
//! envelope between the status check and the declared output pointers
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).
//!
//! # Effect classification
//!
//! **Machine-readable description, and the mechanism is on another endpoint.**
//! The term `idempot` occurs exactly twice in Cal.com's whole published OpenAPI,
//! and both occurrences are on `POST /v2/credits/charge` — "Uses externalRef for
//! idempotency to prevent double-charging" and the `externalRef` property,
//! "Unique external reference for idempotency". That endpoint is not declared by
//! this connector, and no request header, query parameter or body property of
//! any endpoint that *is* declared carries a client-supplied request identifier
//! or a deduplication behaviour. This is the plain "mechanism on another
//! endpoint" near-miss
//! ([[076-a-published-mechanism-with-no-window-is-not-a-class-and-a-transport-choice-can-close-one]]),
//! and it is a near-miss twice over: Cal.com publishes no retention for
//! `externalRef` either.
//!
//! `booking.create` is `AtMostOnce` (ADR 063) on that absence and on a recorded
//! consequence: a second send books the slot a second time wherever the event
//! type still has capacity, which is a second calendar event and a second
//! confirmation to attendee and host.
//!
//! `booking.cancel` stays **`InventoryOnly`**. It is a `POST`, so spec 010 §7's
//! `NaturalMethod` does not reach it whatever its effect is, and ADR 063 is
//! admitted on a recorded consequence of a second send — which Cal.com does not
//! publish for a cancel. That is the same group `grafana.alert_rule.update` and
//! `pagerduty.incident.update` are already in: a state change for which no
//! consequence of a repeat is recorded at all.

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
pub const NAME: &str = "cal_com";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Cal.com's one published API origin.
const ORIGIN: &str = "https://api.cal.com";

/// The version prefix of every path.
const PREFIX: &str = "/v2";

/// "The `cal-api-version` header is required for all v2 endpoints."
pub const VERSION_HEADER: &str = "cal-api-version";

/// "Must be set to 2026-05-01" — the booking collection.
const BOOKING_LIST_VERSION: &str = "2026-05-01";

/// "Must be set to 2026-02-25" — the booking read, create and cancel.
const BOOKING_VERSION: &str = "2026-02-25";

/// "Must be set to 2024-06-14. If not set to this value, the endpoint will
/// default to an older version." — the event types.
const EVENT_TYPE_VERSION: &str = "2024-06-14";

/// The declared page of the one collection that paginates. Cal.com publishes
/// `limit` — "The number of items to return" — without a maximum, which is
/// harmless for a cursor walk: the walk ends when the provider stops publishing
/// a `nextCursor`, never when a page came back shorter than it asked for.
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Cal.com's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Cal.com declarations are valid"))
            .build()
            .expect("the Cal.com declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Cal.com publishes no error-code table, only the failure envelope's shape and
/// one sample code, so the map is keyed on the documented statuses and reads
/// `error.code` only for the one value the documentation names. `message` is
/// prose and is never matched on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/code")
            // "If you omit required fields, the API returns a 400 error."
            .on_status(400, ConnectorErrorClass::Validation)
            // "API requests without authentication will also fail", and the
            // access-control page's role, permission and OAuth-scope refusals.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404" is both the missing resource and, notably, the answer to a
            // request that omitted `cal-api-version` — which this connector
            // cannot produce, because every operation declares one.
            .on_statuses([404, 409, 410], ConnectorErrorClass::Permanent)
            // "Exceeding the limit returns a 429 response", with "a `Retry-After`
            // header carrying the number of seconds to wait before retrying.
            // Clients should honor this hint."
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // The one code Cal.com's own error sample publishes:
            // `{"status": "error", "error": {"code": "NOT_FOUND", …}}`.
            .on_code("NOT_FOUND", ConnectorErrorClass::Permanent)
            .build()
            .expect("the Cal.com error map is a valid declaration")
    });
    &MAP
}

/// The envelope's own status, when the body is the envelope Cal.com publishes.
fn envelope_status(body: &[u8]) -> Option<String> {
    let value: JsonValue = serde_json::from_slice(body).ok()?;
    value
        .pointer("/status")
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
}

/// Decode one Cal.com response: the declared success statuses, then the
/// envelope's own status, then the declared contract.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    match envelope_status(body).as_deref() {
        Some("success") => operation.decode_response(status, body),
        Some(_) => Err(error_map().classify(status, headers, body)),
        None => Err(ConnectorFailure::invariant(
            "connector provider answered outside its declared contract",
        )),
    }
}

/// The continuation plan of the one collection that publishes one.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static BOOKINGS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/data",
            "cursor",
            "/pagination/nextCursor",
            "limit",
            PAGE_SIZE,
        )
        .expect("the Cal.com cursor plan is valid")
    });
    match operation_id {
        "booking.list" => Some(&BOOKINGS),
        _ => None,
    }
}

fn common(builder: OperationBuilder, api_version: &str) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header(VERSION_HEADER, api_version)
}

/// The envelope every v2 response carries.
fn envelope(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
}

/// The searched documentation behind this connector's one at-most-once class.
const NO_KEY: &str = "the term `idempot` occurs exactly twice in Cal.com's own published OpenAPI \
                      3.0 document (`cal.com/docs/api-reference/v2/openapi.json`) and both \
                      occurrences are on `POST /v2/credits/charge` — \"Uses externalRef for \
                      idempotency to prevent double-charging\" and the `externalRef` property, \
                      \"Unique external reference for idempotency\". That endpoint is not \
                      declared by this connector, and Cal.com publishes no retention for it \
                      either. No request header, query parameter or body property of any \
                      endpoint declared here carries a client-supplied request identifier or a \
                      deduplication behaviour";

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // Cal.com publishes seven filters for this collection and no value of any of
    // them meaning "everything". A declared query input renders on every request
    // and would therefore be mandatory, so none is declared and the list is the
    // authenticated user's event types.
    let event_type_list = envelope(
        common(
            Operation::get("event_type.list", &format!("{PREFIX}/event-types")),
            EVENT_TYPE_VERSION,
        )
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let event_type_get = envelope(
        common(
            Operation::get(
                "event_type.get",
                &format!("{PREFIX}/event-types/{{event_type_id}}"),
            ),
            EVENT_TYPE_VERSION,
        )
        .path_param("event_type_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let booking_list = envelope(
        common(
            Operation::get("booking.list", &format!("{PREFIX}/bookings")),
            BOOKING_LIST_VERSION,
        )
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "next_cursor",
            "/pagination/nextCursor",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "has_more",
            "/pagination/hasMore",
            ValueScalar::Boolean,
            Required::No,
        ),
    )
    .effect(Effect::read_only())
    .build()?;

    let booking_get = envelope(
        common(
            Operation::get("booking.get", &format!("{PREFIX}/bookings/{{booking_uid}}")),
            BOOKING_VERSION,
        )
        .path_param("booking_uid", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // The `eventTypeId` form of the create. Cal.com publishes two — "there are 2
    // ways to book an event type belonging to an individual user" — and the
    // other one needs an event slug *and* a username *and*, for a team, an
    // organization slug. One declared shape is one contract a Process binds;
    // `event_type.list` is how a Process gets the id.
    //
    // `bookingFieldsResponses` is declared because Cal.com makes it load-bearing:
    // "Most Cal.com users have required questions on their booking pages … If you
    // omit required fields, the API returns a 400 error." A Process with no
    // custom questions sends `{}`.
    let booking_create = envelope(
        common(
            Operation::post("booking.create", &format!("{PREFIX}/bookings")),
            BOOKING_VERSION,
        )
        .body(JsonTemplate::object([
            ("eventTypeId", JsonTemplate::input("event_type_id")),
            ("start", JsonTemplate::input("start")),
            (
                "attendee",
                JsonTemplate::object([
                    ("name", JsonTemplate::input("attendee_name")),
                    ("email", JsonTemplate::input("attendee_email")),
                    ("timeZone", JsonTemplate::input("attendee_time_zone")),
                ]),
            ),
            (
                "bookingFieldsResponses",
                JsonTemplate::input("booking_fields_responses"),
            ),
        ]))
        .declared_input("event_type_id", ValueScalar::Int64, Required::Yes)
        // "The start time of the booking in ISO 8601 format in UTC timezone."
        .declared_input("start", ValueScalar::String, Required::Yes)
        .declared_input("attendee_name", ValueScalar::String, Required::Yes)
        .declared_input("attendee_email", ValueScalar::String, Required::Yes)
        .declared_input("attendee_time_zone", ValueScalar::String, Required::Yes)
        .declared_input("booking_fields_responses", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        "a second booking of the same slot wherever the event type still has capacity for one — a \
         second calendar event, a second confirmation to the attendee and to the host, and a \
         second `BOOKING_CREATED` webhook delivery to every subscriber",
    )?))
    .build()?;

    // "optionally cancellationReason in the request body". It is declared rather
    // than omitted because the reason is what the attendee and host are told,
    // and a Process that has none sends an empty string.
    let booking_cancel = envelope(
        common(
            Operation::post(
                "booking.cancel",
                &format!("{PREFIX}/bookings/{{booking_uid}}/cancel"),
            ),
            BOOKING_VERSION,
        )
        .path_param("booking_uid", ValueScalar::String)
        .body(JsonTemplate::object([(
            "cancellationReason",
            JsonTemplate::input("cancellation_reason"),
        )]))
        .declared_input("cancellation_reason", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Cal.com publishes the cancel as a POST — \"Cancel a booking\" on \
         `/v2/bookings/{bookingUid}/cancel` — so spec 010 §7's NaturalMethod does not reach it \
         whatever its effect is, and it publishes no consequence of a second send: neither that a \
         repeat is absorbed nor what a repeat of an already-cancelled booking produces. ADR 063's \
         AtMostOnce is admitted on a recorded consequence, which an unstated outcome is not.",
    )?)
    .build()?;

    Ok(vec![
        event_type_list,
        event_type_get,
        booking_list,
        booking_get,
        booking_create,
        booking_cancel,
    ])
}
