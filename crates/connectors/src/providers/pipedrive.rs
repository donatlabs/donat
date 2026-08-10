//! Pipedrive's CRM API v2, with the one v1 resource that has no v2 form.
//!
//! Ground truth is Pipedrive's own published documentation and its own published
//! OpenAPI descriptions, read on 2026-08-10:
//!
//! * <https://developers.pipedrive.com/docs/api/v2/openapi.yaml> and
//!   <https://developers.pipedrive.com/docs/api/v1/openapi.yaml> — the two
//!   `openapi: 3.0.1` documents behind every path, parameter, required field,
//!   and success status below. Their `servers` are
//!   `https://api.pipedrive.com/api/v2` and `https://api.pipedrive.com/v1`,
//!   which is why one origin serves both prefixes here.
//! * <https://pipedrive.readme.io/docs/core-api-concepts-authentication> — "The
//!   API token must be provided in the `x-api-token` header for all requests".
//! * <https://pipedrive.readme.io/docs/core-api-concepts-responses> — the
//!   envelope: "Each response sent from the API contains a `success` parameter,
//!   which is of boolean type, indicating whether the request was successful.
//!   Upon success being false, an optional `error` parameter (string) may be
//!   given. In case of success is true, the response is always contained within
//!   a data parameter, and additional metadata may be carried inside an
//!   `additional_data` parameter."
//! * <https://pipedrive.readme.io/docs/core-api-concepts-pagination> — "Within
//!   the response's `additional_data` object, the `next_cursor` field will be
//!   returned, indicating the first item on the next page. The value of the
//!   `next_cursor` field will be `null` if you have reached the end of the
//!   dataset and there are no more pages to be returned."
//! * <https://pipedrive.readme.io/docs/core-api-concepts-rate-limiting> and
//!   <https://pipedrive.readme.io/docs/core-api-concepts-http-status-codes> —
//!   the status table this module's error map is built from.
//!
//! # The origin is the published `servers` entry, not the company domain
//!
//! Pipedrive publishes two hosts. The prose advises the per-company one — "We
//! advise everyone to use `{COMPANYDOMAIN}.pipedrive.com` for faster requests as
//! it helps us to better determine which data center your request should go to"
//! — and both OpenAPI documents publish `api.pipedrive.com`. This connector
//! declares the OpenAPI origin, because it is the one Pipedrive publishes as a
//! contract rather than as advice, and because the same host serves `/api/v2`
//! and `/v1` while the company domain serves the v1 resources under `/api/v1`
//! instead. A deployment that needs data-centre affinity needs a templated host
//! and a second declaration, which is a change to this module rather than a
//! configuration key.
//!
//! # Version: v2 everywhere it exists, v1 for notes
//!
//! "Only `/api/v2/...` prefix is supported. Previously both `/api/v1/...` and
//! `/v1/...` could be used." The v2 list — "Activities API, Deals API, ...,
//! Organizations API, ..., Persons API, ..., Search API" — does not include
//! notes, so `note.add` and `note.list` are the v1 endpoints, which are not on
//! the deprecation list. Every v2 entity here went out of support in v1 on
//! 2026-08-01, so nothing in this module declares a deprecated path.
//!
//! # A `200` is not by itself a success
//!
//! Pipedrive's envelope carries its own `success` boolean, and its published
//! error body is `{"success": false, "error": …, "error_info": …}`. Pipedrive
//! does **not** publish a statement that a failure arrives under a `200` — its
//! one worked example of `success: false` is bound to a `403` — so
//! [`decode`]'s body gate is **this declaration's own fail-closed rule and not a
//! Pipedrive statement**, exactly as the Google connectors' `error`-envelope
//! refusal is. It costs a documented success nothing: `success` is published as
//! present on every response, and `true` is the only value a success has. See
//! `knowledgebase/declarative-saas/decisions/056-*`.
//!
//! # A search result is not shaped like a list result
//!
//! `GET /api/v2/deals` publishes `data` as the array of deals; `GET
//! /api/v2/deals/search` publishes `data` as an *object* carrying `items`, whose
//! members are `{ "result_score": …, "item": … }`. The two therefore declare
//! different output pointers and different item lists, and the search's plan
//! collects `/data/items` rather than `/data`.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! either published OpenAPI document — 1,781,741 bytes of v1 and 1,015,045 bytes
//! of v2 — nor in any of the core-concept pages above: no request header, no
//! body property, no response field. `deal.create`, `person.create`, and
//! `note.add` are therefore `AtMostOnce` (ADR 063), each leaving a second record
//! with a new id behind.
//!
//! `deal.update` and `person.update` stay `InventoryOnly`. Pipedrive moved them
//! to `PATCH` for exactly the reason spec 010 §7 does not admit the method —
//! "V1 endpoints, which were using HTTP PUT method have been switched to use
//! HTTP PATCH method in v2 for compliance with REST best practices", beside its
//! own verb table's "`PATCH` | Used for updating some parts of a resource" — and
//! what a repeated partial update produces is not something Pipedrive publishes.

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
pub const NAME: &str = "pipedrive";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ url: 'https://api.pipedrive.com/api/v2' }]`, with the path half
/// declared per operation.
const ORIGIN: &str = "https://api.pipedrive.com";

/// "`limit` — For pagination, the limit of entries to be returned. If not
/// provided, 100 items will be returned. Please note that a maximum value of 500
/// is allowed."
///
/// 100 is Pipedrive's own default and is inside its published ceiling; it is
/// this declaration's choice rather than a number read off a page.
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Pipedrive's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::api_key_header(TOKEN_HEADER)
                    .expect("Pipedrive's published credential header is valid"),
            ))
            .operations(operations().expect("the Pipedrive declarations are valid"))
            .build()
            .expect("the Pipedrive declaration is valid")
    });
    &CONNECTOR
}

/// "The API token must be provided in the `x-api-token` header for all
/// requests."
pub const TOKEN_HEADER: &str = "x-api-token";

/// The ordered error map.
///
/// It is keyed on the documented status table alone. Pipedrive publishes a
/// machine-readable `code` in exactly one scenario — the `403` feature-capping
/// example's `"code": "feature_capping_deals_limit"` — and that value is not
/// part of the "All error responses follow the same schema" block, so there is
/// no stable code set to key on. Its own change policy classifies "Changing an
/// error message" as a non-breaking change, which is the provider saying the
/// prose is not a contract.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — Request not understood", and "415 Unsupported
            // Media Type — Feature is not enabled" arrives on a body Pipedrive
            // will not accept from this deployment.
            .on_statuses([400, 415, 422], ConnectorErrorClass::Validation)
            // "401 Unauthorized — Invalid API token."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "402 Payment Required — Company account is not open", "403
            // Forbidden — Request not allowed. User account has reached a limit
            // for an entity", "404 Not Found — Resource unavailable", "405
            // Method not allowed", "410 Gone — Old resource permanently
            // unavailable". None of them is fixed by sending the same request
            // again, and the `403` is deliberately *not* `authentication`:
            // Pipedrive documents it as an account limit, and it is also what a
            // deployment that ignored a `429` is served — "users abusing our
            // system by not respecting our rate limits ... will also get the
            // `403` response code", with an HTML body rather than the envelope.
            .on_statuses([402, 403, 404, 405, 410], ConnectorErrorClass::Permanent)
            // "429 Too Many Requests — Rate limit has been exceeded."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error — Generic server error", "501 Not
            // Implemented", "503 Service Unavailable — Scheduled maintenance".
            .on_statuses([500, 501, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Pipedrive error map is a valid declaration")
    });
    &MAP
}

/// Whether Pipedrive's own envelope reports success.
///
/// `Some(true)` and `Some(false)` are the two answers the published contract
/// admits; `None` means the body is not a JSON object with a boolean `success`,
/// which the response contract does not describe at all.
fn envelope_success(body: &[u8]) -> Option<bool> {
    serde_json::from_slice::<JsonValue>(body)
        .ok()?
        .get("success")?
        .as_bool()
}

/// Decode one Pipedrive response: the status, then the envelope, then the
/// declared contract.
///
/// The order is the contract. A `200` carrying `success: false` can never reach
/// [`Operation::decode_response`], so there is no path by which a provider
/// failure is reported as an activity success.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    match envelope_success(body) {
        Some(true) => operation.decode_response(status, body),
        Some(false) => Err(error_map().classify(status, headers, body)),
        None => Err(ConnectorFailure::invariant(
            "connector provider answered outside its declared contract",
        )),
    }
}

/// The continuation plan of each collection.
///
/// v2 walks the published cursor; the v1 note collection walks `start`, which is
/// what v1 publishes — "The parameters that control this type of pagination are
/// `start` and `limit`, indicating the desired offset and the number of items to
/// be returned per page."
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    fn cursor(items: &str) -> Pagination {
        Pagination::cursor(
            items,
            "cursor",
            "/additional_data/next_cursor",
            "limit",
            PAGE_SIZE,
        )
        .expect("the Pipedrive cursor plan is valid")
    }
    static RECORDS: LazyLock<Pagination> = LazyLock::new(|| cursor("/data"));
    static SEARCH: LazyLock<Pagination> = LazyLock::new(|| cursor("/data/items"));
    static NOTES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/data", "start", "limit", PAGE_SIZE)
            .expect("the Pipedrive offset plan is valid")
    });
    match operation_id {
        "deal.list" | "person.list" => Some(&RECORDS),
        "deal.search" | "person.search" => Some(&SEARCH),
        "note.list" => Some(&NOTES),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in either of Pipedrive's published \
                      OpenAPI documents — 1.78 MB of v1 and 1.02 MB of v2 — nor in its published \
                      requests, responses, authentication, pagination, rate-limiting, or status \
                      code pages: no request header, no body property, and no response field \
                      carries a client-supplied request identifier or a deduplication behaviour";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The reason both updates carry.
const PARTIAL_UPDATE: &str = "Pipedrive publishes `PATCH` for this update — \"V1 endpoints, which \
                              were using HTTP PUT method have been switched to use HTTP PATCH \
                              method in v2 for compliance with REST best practices\", beside its \
                              own verb table's \"PATCH | Used for updating some parts of a \
                              resource\" — which spec 010 §7 admits for neither mutating class, \
                              and its request body declares no required field, so what a repeated \
                              partial update produces is what Pipedrive does not publish";

/// The fields every single-record read and write publishes back.
fn record_output(builder: OperationBuilder, title: &'static str) -> OperationBuilder {
    builder
        .output_pointer("id", "/data/id", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            title,
            &format!("/data/{title}"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "owner_id",
            "/data/owner_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "add_time",
            "/data/add_time",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "update_time",
            "/data/update_time",
            ValueScalar::String,
            Required::No,
        )
}

/// The two fields a v2 collection publishes.
fn collection_output(builder: OperationBuilder, items: &str) -> OperationBuilder {
    builder
        .output_pointer("items", items, ValueScalar::Json, Required::Yes)
        // "The value of the `next_cursor` field will be `null` if you have
        // reached the end of the dataset."
        .output_pointer(
            "next_cursor",
            "/additional_data/next_cursor",
            ValueScalar::String,
            Required::No,
        )
}

/// One v2 entity's read of a single record.
fn record_get(
    id: &'static str,
    collection: &str,
    title: &'static str,
) -> Result<Operation, OperationError> {
    record_output(
        common(Operation::get(
            id,
            &format!("/api/v2/{collection}/{{record_id}}"),
        ))
        .path_param("record_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
        title,
    )
    .effect(Effect::read_only())
    .build()
}

/// One v2 entity's read of a page of records.
fn record_list(id: &'static str, collection: &str) -> Result<Operation, OperationError> {
    collection_output(
        common(Operation::get(id, &format!("/api/v2/{collection}")))
            // "`sort_by` — The field to sort by. Supported fields: `id`,
            // `update_time`, `add_time`." Which one a caller wants is the
            // caller's, and Pipedrive publishes `id` as its own default, so a
            // caller that does not care has a value to send.
            .query_input("sort_by", "sort_by")
            .query_static("limit", &PAGE_SIZE.to_string())
            .success_statuses([StatusCode::OK]),
        "/data",
    )
    .effect(Effect::read_only())
    .build()
}

/// One v2 entity's search.
///
/// "`term` — The search term to look for. Minimum 2 characters (or 1 if using
/// `exact_match`). Please note that the search term has to be URL encoded."
fn record_search(id: &'static str, collection: &str) -> Result<Operation, OperationError> {
    collection_output(
        common(Operation::get(id, &format!("/api/v2/{collection}/search")))
            .query_input("term", "term")
            .query_static("limit", &PAGE_SIZE.to_string())
            .success_statuses([StatusCode::OK]),
        "/data/items",
    )
    .effect(Effect::read_only())
    .build()
}

/// Every operation this connector publishes.
///
/// The set is the surface a business process drives: each entity's read, list,
/// search, create, and update, plus the note the two entities hang off.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Adds a new deal." The body's one required field is `title`; everything
    // else here is what a process actually sets when it opens one.
    let deal_create = record_output(
        common(Operation::post("deal.create", "/api/v2/deals"))
            .body(JsonTemplate::object([
                ("title", JsonTemplate::input("title")),
                ("value", JsonTemplate::input("value")),
                ("currency", JsonTemplate::input("currency")),
                ("person_id", JsonTemplate::input("person_id")),
                ("org_id", JsonTemplate::input("org_id")),
                ("pipeline_id", JsonTemplate::input("pipeline_id")),
                ("stage_id", JsonTemplate::input("stage_id")),
                ("owner_id", JsonTemplate::input("owner_id")),
            ]))
            .declared_input("title", ValueScalar::String, Required::Yes)
            // Every published `POST /api/v2/deals` create returns `200`, not
            // `201`: the v2 description declares `201` for thirteen other
            // operations and `200` for this one.
            .success_statuses([StatusCode::OK]),
        "title",
    )
    .effect(at_most_once(
        "a second deal with a new id, in the same pipeline stage",
    )?)
    .build()?;

    let deal_update = record_output(
        common(Operation::patch("deal.update", "/api/v2/deals/{record_id}"))
            .path_param("record_id", ValueScalar::Int64)
            .body(JsonTemplate::object([
                ("title", JsonTemplate::input("title")),
                ("value", JsonTemplate::input("value")),
                ("status", JsonTemplate::input("status")),
                ("stage_id", JsonTemplate::input("stage_id")),
            ]))
            .success_statuses([StatusCode::OK]),
        "title",
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    // "Adds a new person." The v2 description declares no required body field
    // at all, so nothing here is marked required either.
    let person_create = record_output(
        common(Operation::post("person.create", "/api/v2/persons"))
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("emails", JsonTemplate::input("emails")),
                ("phones", JsonTemplate::input("phones")),
                ("org_id", JsonTemplate::input("org_id")),
                ("owner_id", JsonTemplate::input("owner_id")),
            ]))
            .declared_input("emails", ValueScalar::Json, Required::Yes)
            .declared_input("phones", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK]),
        "name",
    )
    .effect(at_most_once(
        "a second person with a new id; Pipedrive publishes duplicate detection as an import and \
         user-interface feature and documents none for the API",
    )?)
    .build()?;

    let person_update = record_output(
        common(Operation::patch(
            "person.update",
            "/api/v2/persons/{record_id}",
        ))
        .path_param("record_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("name", JsonTemplate::input("name")),
            ("emails", JsonTemplate::input("emails")),
            ("phones", JsonTemplate::input("phones")),
            ("owner_id", JsonTemplate::input("owner_id")),
        ]))
        .declared_input("emails", ValueScalar::Json, Required::Yes)
        .declared_input("phones", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
        "name",
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    // "Adds a new note." Notes have no v2 form, so this is the v1 path on the
    // same origin. `content` is required, and one parent id is required beside
    // it: "This property is required unless one of
    // (`lead_id/person_id/org_id/project_id/task_id`) is specified."
    let note_add = common(Operation::post("note.add", "/v1/notes"))
        .body(JsonTemplate::object([
            ("content", JsonTemplate::input("content")),
            ("deal_id", JsonTemplate::input("deal_id")),
            ("person_id", JsonTemplate::input("person_id")),
            ("org_id", JsonTemplate::input("org_id")),
        ]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/data/id", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "add_time",
            "/data/add_time",
            ValueScalar::String,
            Required::No,
        )
        .effect(at_most_once(
            "a second note on the same record, with a new id",
        )?)
        .build()?;

    // "Returns all notes", filtered by the parent this declaration is for. Every
    // declared query slot is a value the caller must send, so this operation is
    // one deal's notes rather than the whole account's: Pipedrive's other parent
    // filters (`person_id`, `org_id`, `lead_id`, `project_id`, `task_id`) are
    // published and are not declared here.
    let note_list = common(Operation::get("note.list", "/v1/notes"))
        .query_input("deal_id", "deal_id")
        .query_static("limit", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("items", "/data", ValueScalar::Json, Required::Yes)
        // "The `additional_data.pagination` object will contain the given
        // `start` and `limit` values, as well as the `more_items_in_collection`
        // flag."
        .output_pointer(
            "more_items_in_collection",
            "/additional_data/pagination/more_items_in_collection",
            ValueScalar::Boolean,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        record_get("deal.get", "deals", "title")?,
        record_list("deal.list", "deals")?,
        record_search("deal.search", "deals")?,
        deal_create,
        deal_update,
        record_get("person.get", "persons", "name")?,
        record_list("person.list", "persons")?,
        record_search("person.search", "persons")?,
        person_create,
        person_update,
        note_add,
        note_list,
    ])
}
