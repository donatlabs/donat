//! Eventbrite's API v3 — the event, attendee and order surface.
//!
//! Ground truth is Eventbrite's own published v3 API description (the API
//! Blueprint behind <https://www.eventbrite.com/platform/api>), read on
//! 2026-08-10:
//!
//! * the host `https://www.eventbriteapi.com/v3/`, and the credential
//!   `Authorization: Bearer <PRIVATE_TOKEN>`;
//! * `GET /users/me/`;
//! * `GET /events/{event_id}/` and `POST /events/{event_id}/`, the second of
//!   which takes an `event` object of the attributes to change;
//! * `GET /organizations/{organization_id}/events/` and
//!   `POST /organizations/{organization_id}/events/`, whose body is an `event`
//!   object carrying the `Event Create` attributes;
//! * `GET /events/{event_id}/attendees/` and `GET /events/{event_id}/orders/`;
//! * paginated responses, whose envelope is a `pagination` object of
//!   `object_count`, `page_number`, `page_size`, `page_count`, `continuation`
//!   and `has_more_items`, with the continuation spent as
//!   `?continuation=<token>` and **absent from the response once every record
//!   has been returned**;
//! * the error envelope `{"status_code": …, "error": …, "error_description": …}`
//!   and its enumerated names — `ARGUMENTS_ERROR`, `INVALID_AUTH`,
//!   `NOT_AUTHORIZED`, `NOT_FOUND`, `HIT_RATE_LIMIT`, `INTERNAL_ERROR`.
//!
//! # The organization is a compiled path prefix
//!
//! An Eventbrite private token belongs to a user, and a user may administer
//! several organizations; the event collection and the event create are scoped
//! to one of them by a **path segment**. Which organization this deployment
//! manages is therefore deploy-time configuration, compiled into those paths by
//! [`connector`] rather than filled from operation input — the Basecamp shape of
//! `knowledgebase/declarative-saas/decisions/066-*`. A `{organization_id}` path
//! *binding* would have been a slot a Process fills, and a Process filling it
//! would be a Process choosing whose events to create.
//!
//! `event_id` is deliberately not that. It names one event inside the surface
//! the token already reaches, `event.list` is where a Process gets it, and the
//! token's own permissions govern what it may read.
//!
//! # Pagination
//!
//! Eventbrite publishes its continuation as a **token in the response body**
//! that is spent as a **query value** — `?continuation=…` — which is the SDK's
//! `TokenInBody` plan exactly. It is not the body-carried *next URI* plan:
//! Eventbrite does not publish a URL, it publishes an opaque token, and a token
//! treated as a destination would be a value a provider chose becoming a request
//! target. `TokenInBody` can only ever spend it as a query value, so a body that
//! spelled a URL would become a percent-encoded parameter on this origin rather
//! than somewhere else.
//!
//! The walk ends on an absence, which is what every plan in the SDK's closed set
//! reads: once every record has been returned, the `continuation` key is not in
//! the response. `has_more_items` is a *flag*, no plan reads one, and it is not
//! needed here — Eventbrite publishes the absence beside it, which is the
//! difference between this connector and Zendesk's cursor
//! ([[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]],
//! where a flag was the only end of the walk on offer and the plan was declined).
//!
//! # Effect classification
//!
//! Eventbrite publishes no idempotency mechanism: its v3 description carries no
//! idempotency header, no client-supplied request identifier in any endpoint's
//! parameter set, and no deduplication or replay behaviour beside its paginated
//! responses, its error envelope or its rate limit.
//!
//! * `event.create` is `AtMostOnce` (ADR 063): a repeat is a second event, with
//!   a second id and a second public URL, in the organization's own listing.
//! * `event.update` stays `InventoryOnly`. Eventbrite publishes the update over
//!   a **`POST`** — a method spec 010 §7 does not admit for `NaturalMethod`,
//!   because HTTP defines repeat-safety for `PUT` and `DELETE` — and publishes
//!   nothing about what a second identical send does, so ADR 063's bar is not
//!   met either. It joins the partial-update group in `INVENTORY.md`.
//! * Everything else here is a `GET`.

use std::sync::LazyLock;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, FieldClassification, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "eventbrite";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the organization whose events this
/// instance lists and creates.
///
/// It is not a secret: Eventbrite prints the organization id in its own
/// organizer URLs, and holding it authorizes nothing.
pub const ORGANIZATION_ID: &str = "organization_id";

/// The host every v3 URL begins with.
const ORIGIN: &str = "https://www.eventbriteapi.com";

/// Eventbrite publishes no per-operation deadline, so this is the module's own
/// bound on one attempt.
const OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// One deployment's declaration.
///
/// `organization_id` is compiled into the two organization-scoped paths, so
/// nothing in operation input, a provider response, or a continuation can move
/// it.
pub fn connector(organization_id: &str) -> Result<Connector, OperationError> {
    validate_organization_id(organization_id)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN)?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::bearer())
                // Not a secret: the organization id appears in Eventbrite's own
                // organizer URLs, and it names an organization rather than
                // authorizing one.
                .with_field(ORGANIZATION_ID, FieldClassification::NonSecret),
        )
        .operations(operations(organization_id)?)
        .build()
}

/// The declaration a reviewer and the registry read, with a placeholder
/// organization no deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("123456789012")
}

/// Eventbrite's own grammar for the value: its identifiers are numeric strings,
/// and this one is a path segment.
///
/// The check is the narrow one, because a path segment a deployment types is the
/// one value here that could reach outside its own segment.
pub fn validate_organization_id(organization_id: &str) -> Result<(), OperationError> {
    if organization_id.is_empty()
        || organization_id.len() > 24
        || !organization_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err(OperationError::new(
            "the Eventbrite organization id must be the numeric identifier Eventbrite publishes \
             in every organization-scoped URL",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Eventbrite publishes a machine-readable `error` name beside every failure, so
/// the two rules whose class the status alone would not settle are read from it;
/// everything else is decided by the status.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error")
            // "INVALID_AUTH": the private token was rejected.
            .on_code("INVALID_AUTH", ConnectorErrorClass::Authentication)
            // "NOT_AUTHORIZED — You do not have permission to access the
            // resource you requested."
            .on_code("NOT_AUTHORIZED", ConnectorErrorClass::Authentication)
            // "HIT_RATE_LIMIT": Eventbrite's published throttle.
            .on_code("HIT_RATE_LIMIT", ConnectorErrorClass::Http429)
            // "ARGUMENTS_ERROR": invalid parameters, with an `error_detail`
            // naming the offending ones.
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "NOT_FOUND".
            .on_statuses([404, 410], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            // "INTERNAL_ERROR — The server encountered an internal error."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Eventbrite error map is a valid declaration")
    });
    &MAP
}

/// Decode one Eventbrite response: the declared success statuses, then the
/// declared contract.
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

/// The continuation plan of each walked collection.
///
/// Eventbrite's continuation is an opaque token in the response body spent as a
/// query value, and it is absent from the response once every record has been
/// returned. `TokenInBody` is the plan that describes exactly that, and it is
/// the plan that cannot turn a provider value into a destination.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static EVENTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/events", "/pagination/continuation", "continuation")
            .expect("the Eventbrite event continuation plan is valid")
    });
    static ATTENDEES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/attendees", "/pagination/continuation", "continuation")
            .expect("the Eventbrite attendee continuation plan is valid")
    });
    static ORDERS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/orders", "/pagination/continuation", "continuation")
            .expect("the Eventbrite order continuation plan is valid")
    });
    match operation_id {
        "event.list" => Some(&EVENTS),
        "attendee.list" => Some(&ATTENDEES),
        "order.list" => Some(&ORDERS),
        _ => None,
    }
}

/// One organization-scoped path, under this deployment's organization.
fn organization_path(organization_id: &str, suffix: &str) -> String {
    format!("/v3/organizations/{organization_id}{suffix}")
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .deadline(OPERATION_DEADLINE)
        .static_header("Accept", "application/json")
}

/// What every walked collection declares beside its own item pointer.
fn collection(builder: OperationBuilder, items: &str, pointer: &str) -> OperationBuilder {
    builder
        .success_statuses([StatusCode::OK])
        .output_pointer(items, pointer, ValueScalar::Json, Required::Yes)
        // The continuation Eventbrite publishes, carried as data. Only the
        // declared plan spends it, and only as a query value on this origin.
        .output_pointer("pagination", "/pagination", ValueScalar::Json, Required::No)
        .effect(Effect::read_only())
}

/// The reason this connector's keyless write carries.
const NO_KEY: &str = "Eventbrite's published v3 API description carries no idempotency mechanism \
                      anywhere: no idempotency request header, no client-supplied request \
                      identifier in any endpoint's parameter set, and no deduplication or replay \
                      behaviour beside the three cross-cutting sections that would carry one — \
                      paginated responses, the error envelope, and the rate limit. The whole \
                      documented body of this endpoint is an `event` object of the event's own \
                      attributes, and the only identity it returns is the one Eventbrite mints";

/// The published event attributes a Process reads.
fn event_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        // Eventbrite publishes its human strings as multipart objects of `text`
        // and `html`, and its instants as objects of `timezone`, `local` and
        // `utc`, so the contract carries the provider's own shapes
        // (`knowledgebase/declarative-saas/decisions/071-*`).
        .output_pointer("name", "/name", ValueScalar::Json, Required::No)
        .output_pointer("start", "/start", ValueScalar::Json, Required::No)
        .output_pointer("end", "/end", ValueScalar::Json, Required::No)
        .output_pointer("url", "/url", ValueScalar::String, Required::No)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("currency", "/currency", ValueScalar::String, Required::No)
        .output_pointer(
            "organization_id",
            "/organization_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("created", "/created", ValueScalar::String, Required::No)
        .output_pointer("changed", "/changed", ValueScalar::String, Required::No)
}

/// The `event` object both writes send: the documented attributes and nothing
/// else, each rendered in the multipart shape Eventbrite publishes.
fn event_body() -> JsonTemplate {
    JsonTemplate::object([(
        "event",
        JsonTemplate::object([
            (
                "name",
                JsonTemplate::object([("html", JsonTemplate::input("name_html"))]),
            ),
            (
                "start",
                JsonTemplate::object([
                    ("timezone", JsonTemplate::input("timezone")),
                    ("utc", JsonTemplate::input("start_utc")),
                ]),
            ),
            (
                "end",
                JsonTemplate::object([
                    ("timezone", JsonTemplate::input("timezone")),
                    ("utc", JsonTemplate::input("end_utc")),
                ]),
            ),
            ("currency", JsonTemplate::input("currency")),
            ("listed", JsonTemplate::input("listed")),
        ]),
    )])
}

/// Every operation this connector publishes, under one deployment's
/// organization.
fn operations(organization_id: &str) -> Result<Vec<Operation>, OperationError> {
    let organization_events = organization_path(organization_id, "/events/");
    let one_event = "/v3/events/{event_id}/";
    let attendees = "/v3/events/{event_id}/attendees/";
    let orders = "/v3/events/{event_id}/orders/";

    // "GET /users/me/": how a Process learns which user this token belongs to.
    let user_me = common(Operation::get("user.me", "/v3/users/me/"))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("emails", "/emails", ValueScalar::Json, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "GET /events/{event_id}/".
    let event_get = event_output(
        common(Operation::get("event.get", one_event)).path_param("event_id", ValueScalar::String),
    )
    .success_statuses([StatusCode::OK])
    .effect(Effect::read_only())
    .build()?;

    // "GET /organizations/{organization_id}/events/", with the documented
    // filters a Process actually drives.
    let event_list = collection(
        common(Operation::get("event.list", &organization_events))
            .query_input("status", "status")
            .query_input("order_by", "order_by"),
        "events",
        "/events",
    )
    .build()?;

    // "POST /organizations/{organization_id}/events/", whose body is an `event`
    // object of the `Event Create` attributes.
    let event_create = event_output(
        common(Operation::post("event.create", &organization_events))
            .body(event_body())
            .declared_input("name_html", ValueScalar::String, Required::Yes)
            .declared_input("timezone", ValueScalar::String, Required::Yes)
            .declared_input("start_utc", ValueScalar::String, Required::Yes)
            .declared_input("end_utc", ValueScalar::String, Required::Yes)
            .declared_input("currency", ValueScalar::String, Required::Yes),
    )
    .success_statuses([StatusCode::OK, StatusCode::CREATED])
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        "a second event in the organization's own listing, with a second id and a second public \
         URL that attendees can find and buy tickets against",
    )?))
    .build()?;

    // "POST /events/{event_id}/", which takes an `event` object of the
    // attributes to change.
    let event_update = event_output(
        common(Operation::post("event.update", one_event))
            .path_param("event_id", ValueScalar::String)
            .body(event_body())
            .declared_input("name_html", ValueScalar::String, Required::Yes)
            .declared_input("timezone", ValueScalar::String, Required::Yes),
    )
    .success_statuses([StatusCode::OK])
    .effect(Effect::inventory_only(PARTIAL_UPDATE_OVER_POST)?)
    .build()?;

    // "GET /events/{event_id}/attendees/".
    let attendee_list = collection(
        common(Operation::get("attendee.list", attendees))
            .path_param("event_id", ValueScalar::String)
            .query_input("status", "status")
            .query_input("changed_since", "changed_since"),
        "attendees",
        "/attendees",
    )
    .build()?;

    // "GET /events/{event_id}/orders/".
    let order_list = collection(
        common(Operation::get("order.list", orders))
            .path_param("event_id", ValueScalar::String)
            .query_input("status", "status")
            .query_input("changed_since", "changed_since"),
        "orders",
        "/orders",
    )
    .build()?;

    Ok(vec![
        user_me,
        event_get,
        event_list,
        event_create,
        event_update,
        attendee_list,
        order_list,
    ])
}

/// The reason `event.update` carries: a partial update over a method the gate
/// does not admit, whose repeat the provider never described.
const PARTIAL_UPDATE_OVER_POST: &str = "Eventbrite publishes this update over a `POST`, taking an `event` object of the attributes \
     to change, and publishes nothing at all about what a second identical send does. Spec 010 §7 \
     admits NaturalMethod for PUT and DELETE only, because HTTP defines repeat-safety for those \
     two, and ADR 063's at-most-once class is admitted on a recorded absence *and* a recorded \
     consequence: a partial update that writes the same attributes a second time has no \
     consequence to record. So the operation stays declared, typed, tested, and unreachable";
