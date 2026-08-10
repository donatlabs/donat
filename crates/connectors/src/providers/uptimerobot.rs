//! UptimeRobot's API v3 — monitors, incidents and the alerts sent for them.
//!
//! Ground truth is UptimeRobot's own published documentation and the OpenAPI
//! description its documentation loads, read on 2026-08-10:
//!
//! * <https://uptimerobot.com/api/v3/> — the v3 documentation page, and the
//!   OpenAPI it renders, `https://cdn.uptimerobot.com/api/openapi.yaml`, titled
//!   "UptimeRobot API" version `3.0` with the single server
//!   `https://api.uptimerobot.com/v3`.
//! * That document's only security scheme is `bearer`: `type: http`, `scheme:
//!   bearer`, described as "Enter your API token (without the "Bearer" prefix —
//!   it is added automatically)".
//! * The same page's rate limits: "FREE plan : 10 req/min", "PRO plan : monitor
//!   limit * 2 req/min ( with maximum value 5000 req/min )", "We will return 429
//!   HTTP status code in the response from API, when you hit the rate limits",
//!   and the headers "X-RateLimit-Limit", "X-RateLimit-Remaining",
//!   "X-RateLimit-Reset" and "Retry-After - Number of second after you should
//!   retry the call".
//!
//! # The credential is a header, because v3 is the version that publishes one
//!
//! Spec 027 §1 flagged UptimeRobot as the connector whose "API key in the body,
//! not a header" no auth plan supports. That is true of **v2**, which UptimeRobot
//! itself now links as `/api/legacy/`: its published call is
//! `curl -X POST https://api.uptimerobot.com/v2/getMonitors -d
//! 'api_key=YOUR_API_KEY_HERE&format=json'`. It is not true of v3, whose own
//! OpenAPI declares one HTTP bearer scheme for every operation. So this connector
//! declares v3 and `AuthPlan::bearer`, and the SDK gained no plan for a
//! credential in a body — see
//! `knowledgebase/declarative-saas/decisions/081-*`. A body template carrying a
//! secret is exactly what spec 027 §1 forbids: it would enter the request
//! fingerprint and the operation's declared input contract.
//!
//! # Pagination
//!
//! Every v3 collection answers `{"nextLink": …, "data": [ … ]}` and takes a
//! `cursor` query parameter, and **this connector declares no continuation plan
//! for any of them**. That is a deliberate refusal rather than an oversight, and
//! it is the shape [[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]]
//! recorded for Zoho: UptimeRobot publishes `nextLink` as an untyped `string`
//! with no description at all, and publishes `cursor` as a request parameter
//! whose *meaning* it describes ("Cursor to paginate through incidents (incident
//! ID)") but whose value nothing in a response is documented to carry. A
//! `Pagination::next_uri_in_body` plan would treat `nextLink` as a destination
//! and a `Pagination::cursor` plan would treat it as a token, and neither
//! reading rests on a published statement — a walk built on the wrong one turns
//! a collection the provider answered completely into a failed attempt.
//!
//! Each collection therefore asks for one page of the documented maximum
//! ("Maximum number of monitors to return per page. Default: 50, Min: 1, Max:
//! 200.") and publishes `nextLink` as a declared output, so a Process can see
//! that the collection was not exhausted rather than being told an incomplete
//! answer is a complete one. `cursor` is not declared as an input either: a
//! declared query input renders on *every* request, so a caller would have to
//! supply one on the first page, where UptimeRobot documents no value for it.
//!
//! A plan belongs here when UptimeRobot describes what `nextLink` contains. That
//! is a real cost — a listing reaches 200 monitors an attempt — and a bounded
//! declaration that says less is better than an unbounded one that says more.
//!
//! # Effect classification
//!
//! **Machine-readable description.** The term `idempot` occurs four times in
//! UptimeRobot's published v3 OpenAPI and every occurrence is a *repeat-safety
//! statement on a `POST`*, never a client-supplied request key: "This operation
//! is idempotent - pausing an already paused monitor will return successfully",
//! the same for starting one, and "This operation is idempotent." on pinning and
//! unpinning a status-page announcement. No endpoint publishes an idempotency
//! key, a request identifier or a deduplication field.
//!
//! `monitor.pause` is exactly the case spec 023 §3's fourth row does not admit:
//! the provider documents the write as repeat-safe, and it documents it over a
//! method spec 010 §7's `NaturalMethod` does not admit. Giving it `AtMostOnce`
//! would trade away the retry the provider is willing to absorb, so it stays
//! `InventoryOnly` with the provider's own sentence recorded — the group
//! ADR 063 named "writes a provider documents as repeat-safe".
//!
//! `incident_comment.create` publishes no such statement and is `AtMostOnce`.

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
pub const NAME: &str = "uptimerobot";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// UptimeRobot's published API host.
const ORIGIN: &str = "https://api.uptimerobot.com";

/// The single server the v3 OpenAPI declares is `https://api.uptimerobot.com/v3`.
const PREFIX: &str = "/v3";

/// "Maximum number of monitors to return per page. Default: 50, Min: 1, Max: 200."
const MONITOR_PAGE_SIZE: &str = "200";

/// "Number of comments to return (1-100, default 50)."
const COMMENT_PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("UptimeRobot's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the UptimeRobot declarations are valid"))
            .build()
            .expect("the UptimeRobot declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// The v3 OpenAPI publishes `400`, `401`, `403` and `404` per operation, and the
/// documentation page publishes `429` with `Retry-After` for the rate limit.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 409], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the UptimeRobot error map is a valid declaration")
    });
    &MAP
}

/// Decode one UptimeRobot response: the declared success statuses, then the
/// declared contract.
///
/// v2 reported failures inside a `200` with `"stat": "fail"`. v3 reports them
/// with a status, and this connector declares v3, so there is no body gate here.
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

/// The continuation plan of each collection: none, for the reason in the module
/// header.
///
/// The lookup is written out rather than omitted, because a module that named no
/// plan lookup at all would be acquiring or losing a walk by omission
/// ([[058-a-declared-walk-is-the-executors-walk]]).
pub fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// One collection page, with the continuation UptimeRobot publishes exposed as
/// data rather than followed.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .success_statuses([StatusCode::OK])
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer("nextLink", "/nextLink", ValueScalar::String, Required::No)
}

/// The searched documentation behind the at-most-once class here.
const NO_KEY: &str = "UptimeRobot publishes a machine-readable description of its whole v3 API \
                      (`https://cdn.uptimerobot.com/api/openapi.yaml`, \"UptimeRobot API\" 3.0) and \
                      the term `idempot` occurs four times in it, none of them a client-supplied \
                      request key: they are repeat-safety statements on four `POST` endpoints \
                      (\"This operation is idempotent - pausing an already paused monitor will \
                      return successfully\", the same for starting one, and \"This operation is \
                      idempotent.\" on pinning and unpinning a status-page announcement). No \
                      endpoint in the document publishes an idempotency header, a request \
                      identifier or a deduplication field";

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "List all monitors in a user's account."
    let monitor_list = collection(
        common(Operation::get(
            "monitor.list",
            &format!("{PREFIX}/monitors"),
        ))
        .query_static("limit", MONITOR_PAGE_SIZE),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get a monitor details by ID."
    let monitor_get = common(Operation::get(
        "monitor.get",
        &format!("{PREFIX}/monitors/{{monitor_id}}"),
    ))
    .path_param("monitor_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer(
        "friendlyName",
        "/friendlyName",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("url", "/url", ValueScalar::String, Required::No)
    .output_pointer("status", "/status", ValueScalar::String, Required::No)
    .output_pointer("type", "/type", ValueScalar::String, Required::No)
    .output_pointer("interval", "/interval", ValueScalar::Int64, Required::No)
    .output_pointer(
        "lastIncidentId",
        "/lastIncidentId",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "createDateTime",
        "/createDateTime",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Pauses a single monitor by ID. The monitor will stop being checked until
    // it is resumed. This operation is idempotent - pausing an already paused
    // monitor will return successfully".
    let monitor_pause = common(Operation::post(
        "monitor.pause",
        &format!("{PREFIX}/monitors/{{monitor_id}}/pause"),
    ))
    .path_param("monitor_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK, StatusCode::CREATED])
    .no_content_statuses([StatusCode::OK, StatusCode::CREATED])
    .effect(Effect::inventory_only(
        "UptimeRobot documents this write as repeat-safe — \"This operation is idempotent - pausing \
         an already paused monitor will return successfully\" — over a `POST`, and spec 010 §7's \
         NaturalMethod admits that evidence only for a `PUT` or a `DELETE` against a fixed resource \
         identity. ADR 063's AtMostOnce is the wrong home for it in the other direction: it would \
         trade away a retry this provider has said in writing it will absorb. It stays declared, \
         typed and unreachable until the class ADR 063 names as still open — a provider-idempotent \
         class whose evidence is a documented repeat-safe write on a method HTTP does not define \
         repeat-safety for — exists",
    )?)
    .build()?;

    // "List all incidents in a user's account with optional filtering."
    let incident_list = collection(common(Operation::get(
        "incident.list",
        &format!("{PREFIX}/incidents"),
    )))
    .effect(Effect::read_only())
    .build()?;

    // "Get incident details including root cause information".
    let incident_get = common(Operation::get(
        "incident.get",
        &format!("{PREFIX}/incidents/{{incident_id}}"),
    ))
    .path_param("incident_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("status", "/status", ValueScalar::String, Required::No)
    .output_pointer("reason", "/reason", ValueScalar::String, Required::No)
    .output_pointer("duration", "/duration", ValueScalar::Int64, Required::No)
    .output_pointer("startedAt", "/startedAt", ValueScalar::String, Required::No)
    .output_pointer(
        "resolvedAt",
        "/resolvedAt",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Returns all alerts that were sent for a specific incident, including
    // recipient information and delivery status." This endpoint publishes no
    // cursor at all, so it is one page by the provider's own contract.
    let incident_alert_list = common(Operation::get(
        "incident_alert.list",
        &format!("{PREFIX}/incidents/{{incident_id}}/alerts"),
    ))
    .path_param("incident_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Returns paginated comments for a specific incident, ordered by creation
    // date ascending (oldest first)."
    let incident_comment_list = collection(
        common(Operation::get(
            "incident_comment.list",
            &format!("{PREFIX}/incidents/{{incident_id}}/comments"),
        ))
        .path_param("incident_id", ValueScalar::String)
        .query_static("limit", COMMENT_PAGE_SIZE),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Create a new comment on an incident".
    let incident_comment_create = common(Operation::post(
        "incident_comment.create",
        &format!("{PREFIX}/incidents/{{incident_id}}/comments"),
    ))
    .path_param("incident_id", ValueScalar::String)
    .body(JsonTemplate::object([(
        "content",
        JsonTemplate::input("content"),
    )]))
    .declared_input("content", ValueScalar::String, Required::Yes)
    .success_statuses([StatusCode::CREATED])
    // The v3 document publishes no response schema for this `201`, so the
    // declaration admits an empty success rather than demanding a body the
    // provider never promised.
    .no_content_statuses([StatusCode::CREATED])
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        "a second comment with the same text on the same incident, visible to everyone reading the \
         incident's activity log",
    )?))
    .build()?;

    Ok(vec![
        monitor_list,
        monitor_get,
        monitor_pause,
        incident_list,
        incident_get,
        incident_alert_list,
        incident_comment_list,
        incident_comment_create,
    ])
}
