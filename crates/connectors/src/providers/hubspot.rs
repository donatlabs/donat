//! HubSpot's CRM v3 API.
//!
//! Ground truth is HubSpot's own published documentation and its own published
//! OpenAPI descriptions, read on 2026-08-10:
//!
//! * The public spec catalogue at
//!   <https://api.hubspot.com/public/api/spec/v1/specs> and the stable v3
//!   descriptions it links for Contacts, Companies, Deals, and Tickets. Every
//!   one of them declares `servers: [{ "url": "https://api.hubapi.com" }]`, the
//!   paths below, the `after`/`limit` query parameters — "The paging cursor
//!   token of the last successfully read resource will be returned as the
//!   `paging.next.after` JSON property" — and the `Error` schema whose required
//!   fields are `category`, `correlationId`, and `message`.
//! * <https://developers.hubspot.com/docs/api-reference/error-handling> — the
//!   status table this module's error map is built from: "401 Unauthorized —
//!   Returned when the authentication provided is invalid", "403 Forbidden —
//!   Returned when authentication lacks proper permissions for the specific
//!   URL", "414 — Returned when the request URI is too long", "423 Locked —
//!   Returned when attempting to sync a large volume of data", "429 Too many
//!   requests — Returned when over API rate limits", "477 Migration in Progress
//!   — Returned when a HubSpot account is being migrated between data hosting
//!   locations", "502/504 timeouts", "503 service temporarily unavailable", and
//!   the `521`–`526` edge statuses.
//!
//! # The error map is keyed on the status alone
//!
//! HubSpot's error body does carry a `category`, and this map deliberately does
//! not read it, because HubSpot tells a client not to depend on it: "The fields
//! in the example response above should all be treated as optional in any error
//! parsing. The specific fields included can vary between different APIs, so any
//! error parsing should allow for specific fields to be missing from the
//! response." The only category value that appears in the published OpenAPI
//! descriptions is `VALIDATION_ERROR`, which the status already says. A map keyed
//! on a field the provider calls optional would be a map with holes in it, so
//! this one is keyed on the documented status table and reads no body at all.
//!
//! # `form.submit` is not here, and the reason is the origin
//!
//! Spec 016 §2 lists `form.submit`. HubSpot serves form submissions from a
//! **different host**: `POST
//! https://api.hsforms.com/submissions/v3/integration/submit/{portalId}/{formGuid}`,
//! not `api.hubapi.com`. Spec 010 §4 makes a connector's origin a compile-time
//! constant that "nothing in a request, a credential, a provider response, a
//! pagination cursor, or a webhook payload may change", so one connector cannot
//! serve both hosts. Declaring the submission under this connector would mean
//! either a second origin or a request that leaves the compiled one; both are
//! refused. A HubSpot forms connector is its own module with its own origin, its
//! own credential contract, and its own batch.
//!
//! # A search cursor is a body field
//!
//! The `GET` collections take `after` and `limit` as query parameters, which is
//! exactly [`Pagination::cursor`]. The `POST` searches take the same two names
//! inside the request body, which no SDK plan can spend, so those operations
//! declare `after` as an input the caller echoes back verbatim and publish
//! `next_after` as an output. See
//! `knowledgebase/declarative-saas/decisions/055-*`.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur anywhere in
//! the published Contacts, Companies, Deals, or Tickets v3 descriptions: no
//! request header parameter, no body property, no response field. `contact.create`
//! and `deal.create` are therefore `AtMostOnce` (ADR 063) — a repeat leaves a
//! second object with a new id — and the two updates stay `InventoryOnly`:
//! HubSpot's update is a `PATCH` ("Provided property values will overwrite
//! existing values") which spec 010 §7 admits for neither mutating class, and a
//! repeat of it sets the same properties to the same values, which is not the
//! consequence ADR 063 exists to bound.
//!
//! What HubSpot *does* publish, and what a later batch would have to weigh, is a
//! batch **upsert** — `POST /crm/v3/objects/{type}/batch/upsert` keyed on a
//! unique property — which is a genuine upsert on business data rather than an
//! idempotency key, exactly as Airtable's `performUpsert` is. It is out of this
//! batch's operation set.

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
pub const NAME: &str = "hubspot";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ "url": "https://api.hubapi.com" }]`.
const ORIGIN: &str = "https://api.hubapi.com";

/// The page size every collection declares.
///
/// HubSpot's search request documents its own ceiling — "The maximum results to
/// return, up to 200 objects" — and its list `limit` carries only "The maximum
/// number of results to display per page." 100 is inside both, and it is this
/// declaration's choice rather than a number read off a page.
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("HubSpot's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the HubSpot declarations are valid"))
            .build()
            .expect("the HubSpot declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map; see the module documentation for why it reads no body.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // A malformed or over-long request needs a different request.
            .on_statuses([400, 414, 422], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404" for a record this portal does not have, "409" for a
            // conflicting write, and "423 Locked", for which HubSpot publishes
            // no retry hint at all — so this deployment stops rather than
            // continuing to push volume the provider just refused.
            .on_statuses([404, 409, 423], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            // "477 Migration in Progress" is HubSpot's own temporary state and
            // is the one status it documents a `Retry-After` for.
            .on_statuses(
                [477, 500, 502, 503, 504, 521, 522, 523, 524, 525, 526],
                ConnectorErrorClass::Http5xx,
            )
            .build()
            .expect("the HubSpot error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each `GET` collection.
///
/// "The paging cursor token of the last successfully read resource will be
/// returned as the `paging.next.after` JSON property", and a response with no
/// `paging.next` is the documented end of the collection.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static RESULTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/results",
            "after",
            "/paging/next/after",
            "limit",
            PAGE_SIZE,
        )
        .expect("the HubSpot cursor plan is valid")
    });
    match operation_id {
        "contact.list" | "company.list" | "deal.list" | "ticket.list" => Some(&RESULTS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The fields `SimplePublicObject` declares required: "archived", "createdAt",
/// "id", "properties", "updatedAt".
fn object_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "properties",
            "/properties",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer("archived", "/archived", ValueScalar::Boolean, Required::Yes)
        .output_pointer(
            "created_at",
            "/createdAt",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "updated_at",
            "/updatedAt",
            ValueScalar::String,
            Required::Yes,
        )
}

/// The two fields every collection publishes.
fn collection_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_after",
            "/paging/next/after",
            ValueScalar::String,
            Required::No,
        )
}

/// One object type's read of a single record.
fn object_get(id: &'static str, object: &str) -> Result<Operation, OperationError> {
    object_output(
        common(Operation::get(
            id,
            &format!("/crm/v3/objects/{object}/{{object_id}}"),
        ))
        .path_param("object_id", ValueScalar::String)
        // "A comma separated list of the properties to be returned in the
        // response" — which ones a caller wants is the caller's, because
        // HubSpot's default set is small and per object type.
        .query_input("properties", "properties")
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()
}

/// One object type's read of a page of records.
fn object_list(id: &'static str, object: &str) -> Result<Operation, OperationError> {
    collection_output(
        common(Operation::get(id, &format!("/crm/v3/objects/{object}")))
            .query_input("properties", "properties")
            .query_static("limit", &PAGE_SIZE.to_string())
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()
}

/// One object type's search.
fn object_search(id: &'static str, object: &str) -> Result<Operation, OperationError> {
    collection_output(
        common(Operation::post(
            id,
            &format!("/crm/v3/objects/{object}/search"),
        ))
        .body(JsonTemplate::object([
            ("filterGroups", JsonTemplate::input("filter_groups")),
            ("query", JsonTemplate::input("query")),
            ("sorts", JsonTemplate::input("sorts")),
            ("properties", JsonTemplate::input("properties")),
            ("limit", JsonTemplate::literal(serde_json::json!(PAGE_SIZE))),
            ("after", JsonTemplate::input("after")),
        ]))
        // "Up to 6 groups of filters defining additional query criteria."
        .declared_input("filter_groups", ValueScalar::Json, Required::Yes)
        // "The search query string, up to 3000 characters."
        .declared_input("query", ValueScalar::Json, Required::Yes)
        .declared_input("sorts", ValueScalar::Json, Required::Yes)
        .declared_input("properties", ValueScalar::Json, Required::Yes)
        // Nullable: the first page of a walk has no cursor yet.
        .declared_input("after", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only_documented(
        "HubSpot's CRM search reaches the API as a POST because its filter groups and sorts do not \
         fit a query string — \"Search for contacts by filtering on properties, searching through \
         associations, and sorting results\" — and it creates and changes nothing",
    )?)
    .build()
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur anywhere in HubSpot's published CRM v3 \
                      descriptions for contacts, companies, deals, or tickets: no request header, \
                      no body property, and no response field carries a client-supplied request \
                      identifier or a deduplication behaviour";

/// One object type's create.
fn object_create(id: &'static str, object: &str) -> Result<Operation, OperationError> {
    object_output(
        common(Operation::post(id, &format!("/crm/v3/objects/{object}")))
            .body(JsonTemplate::object([
                ("properties", JsonTemplate::input("properties")),
                ("associations", JsonTemplate::input("associations")),
            ]))
            .declared_input("properties", ValueScalar::Json, Required::Yes)
            .declared_input("associations", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        &format!(
            "a second {object} object with a new object id; HubSpot deduplicates some flows by \
             email, but that is a portal setting rather than a published request contract, and \
             the API answers a create of an existing email with a `409` rather than with the \
             existing record"
        ),
    )?))
    .build()
}

/// One object type's update.
fn object_update(id: &'static str, object: &str) -> Result<Operation, OperationError> {
    object_output(
        common(Operation::patch(
            id,
            &format!("/crm/v3/objects/{object}/{{object_id}}"),
        ))
        .path_param("object_id", ValueScalar::String)
        .body(JsonTemplate::object([(
            "properties",
            JsonTemplate::input("properties"),
        )]))
        .declared_input("properties", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(NO_KEY)?)
    .build()
}

/// Every operation this connector publishes.
///
/// The object type name is the path segment HubSpot's own guides document
/// (`contacts`, `companies`, `deals`, `tickets`); its OpenAPI renders the deal
/// path with the object type *id* `0-3` instead, and the two name the same
/// collection.
fn operations() -> Result<Vec<Operation>, OperationError> {
    Ok(vec![
        object_get("contact.get", "contacts")?,
        object_list("contact.list", "contacts")?,
        object_search("contact.search", "contacts")?,
        object_create("contact.create", "contacts")?,
        object_update("contact.update", "contacts")?,
        object_get("company.get", "companies")?,
        object_list("company.list", "companies")?,
        object_get("deal.get", "deals")?,
        object_list("deal.list", "deals")?,
        object_create("deal.create", "deals")?,
        object_update("deal.update", "deals")?,
        object_get("ticket.get", "tickets")?,
        object_list("ticket.list", "tickets")?,
    ])
}
