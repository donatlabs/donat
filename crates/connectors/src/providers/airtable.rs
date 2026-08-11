//! Airtable Web API.
//!
//! Ground truth is Airtable's own published Web API reference, read on
//! 2026-08-10:
//!
//! * <https://airtable.com/developers/web/api/introduction>
//! * <https://airtable.com/developers/web/api/authentication> — "Authorization:
//!   Bearer YOUR_TOKEN".
//! * <https://airtable.com/developers/web/api/errors> — the documented status
//!   codes and error types this module's error map is built from.
//! * <https://airtable.com/developers/web/api/rate-limits> — "5 requests per
//!   second per base", "50 requests per second for all traffic using personal
//!   access tokens from a given user", and a `429` after which a caller must
//!   "wait 30 seconds before subsequent requests will succeed".
//! * the per-endpoint pages cited on each operation below.
//!
//! # The base identifier is deploy-time material
//!
//! Every record path is scoped by a base ID. Airtable's personal access tokens
//! are granted per base, so which base a deployment talks to is configuration,
//! not operation input: [`base_scoped_input`] fills `base_id` from the
//! deployment's [`ConnectorConfiguration`] and refuses an input that carries
//! one of its own. The table is genuinely selected per call — a token that
//! covers a base covers its tables — so `table` stays an operation input.
//!
//! # Effect classification
//!
//! Airtable's Web API reference documents no idempotency key, no client-
//! supplied request identifier, and no deduplication of a repeated write.
//! `record.create` is therefore `AtMostOnce` (ADR 063): a repeat leaves a second
//! record with a new id, so a Process reaches it only by declaring
//! `at_most_once` and a route for an outcome nobody can know.
//! `record.update_patch` stays `InventoryOnly` — Airtable publishes nothing that
//! tells an absolute patch body from a relative one, so what a repeat produces
//! is not recorded, and ADR 063 admits a class only where it is
//! (see `INVENTORY.md`). `record.replace` and `record.delete` are
//! `ProviderIdempotent::NaturalMethod`: both act on a fixed record identity in
//! the path, and Airtable documents the `PUT` as a whole-record replacement —
//! "A PATCH request will only update the fields you specify, leaving the rest
//! as they were. A PUT request will perform a destructive update and clear all
//! unspecified cell values."
//! (<https://airtable.com/developers/web/api/update-record>) — which is the
//! property that makes a repeat send leave one record in one state. The delete
//! answers with the same body for the record it names — `{"id": ..., "deleted":
//! true}` (<https://airtable.com/developers/web/api/delete-record>).

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{
    Connector, ConnectorConfiguration, CredentialSpec, FieldClassification, OriginSpec,
};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "airtable";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Airtable's one published API origin.
const ORIGIN: &str = "https://api.airtable.com";

/// The deploy-time configuration key holding the base this instance talks to.
pub const BASE_ID: &str = "base_id";

/// The documented ceiling on `pageSize`: "The number of records returned in
/// each request. Must be less than or equal to 100"
/// (<https://airtable.com/developers/web/api/list-records>).
const MAX_PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Airtable's published origin is valid"))
            .credential(
                CredentialSpec::for_plan(AuthPlan::bearer())
                    // Not a secret: a base ID is an identifier Airtable prints
                    // in its own URLs. It is still deploy-time material, which
                    // is why it is declared here and not accepted as input.
                    .with_field(BASE_ID, FieldClassification::NonSecret),
            )
            .operations(operations().expect("the Airtable declarations are valid"))
            .build()
            .expect("the Airtable declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from Airtable's documented status codes
/// (<https://airtable.com/developers/web/api/errors>).
///
/// Airtable publishes its machine-readable type at `error.type`, so the map
/// declares that pointer; the two rules keyed on a type are the ones whose
/// class the status alone would not decide.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/type")
            // "503 Service Unavailable ... RETRIABLE_ERROR": Airtable names
            // this one retriable itself.
            .on_code("RETRIABLE_ERROR", ConnectorErrorClass::Http5xx)
            // "422 Invalid Request ... LIST_RECORDS_ITERATOR_NOT_AVAILABLE":
            // the paging iterator expired, which is a request that must be
            // started again rather than a permanent refusal.
            .on_code(
                "LIST_RECORDS_ITERATOR_NOT_AVAILABLE",
                ConnectorErrorClass::Validation,
            )
            // "400 Bad Request: The request encoding is invalid; the request
            // can't be parsed as a valid JSON."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized: Accessing a protected resource without
            // authorization or with invalid credentials."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "403 Forbidden: Accessing a protected resource with API
            // credentials that don't have access to that resource."
            .on_status(403, ConnectorErrorClass::Authentication)
            // "404 Not Found: Route or resource is not found."
            .on_status(404, ConnectorErrorClass::Permanent)
            // "413 Request Entity Too Large: The request exceeded the maximum
            // allowed payload size." and "422 Invalid Request: The request data
            // is invalid." are both this deployment's request to fix.
            .on_statuses([413, 422], ConnectorErrorClass::Validation)
            // "429 Too Many Requests: The API is limited to 5 requests per
            // second per base."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error", "502 Bad Gateway", "503 Service
            // Unavailable".
            .on_statuses([500, 502, 503], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Airtable error map is a valid declaration")
    });
    &MAP
}

/// The pagination plan of an operation Airtable documents as paginated.
///
/// Airtable's protocol is one opaque `offset` echoed back as a query parameter:
/// "If there are more records, the response will contain an offset"
/// (<https://airtable.com/developers/web/api/list-records>). The last page
/// simply omits it, which is where each walk stops.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static RECORDS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor("/records", "offset", "/offset", "pageSize", MAX_PAGE_SIZE)
            .expect("the Airtable record pagination plan is valid")
    });
    // The metadata endpoints echo the same `offset` and document no page size
    // of their own — "returns the list of bases the token can access, 1000
    // bases at a time" — so nothing here invents one.
    static BASES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/bases", "/offset", "offset")
            .expect("the Airtable base pagination plan is valid")
    });
    match operation_id {
        "record.list" => Some(&RECORDS),
        "base.list" => Some(&BASES),
        _ => None,
    }
}

/// Bind the deployment's configured base to one operation input.
///
/// The base is the account-scoped identifier of spec 012 §1: it comes from
/// deploy-time configuration and never from operation input, so an input that
/// carries a `base_id` of its own is refused rather than overwritten.
pub fn base_scoped_input(
    configuration: &ConnectorConfiguration,
    input: &JsonValue,
) -> Result<JsonValue, ConnectorFailure> {
    let Some(fields) = input.as_object() else {
        return Err(ConnectorFailure::invariant(
            "an airtable operation input is a JSON object",
        ));
    };
    if fields.contains_key(BASE_ID) {
        return Err(ConnectorFailure::invariant(
            "the airtable base is deploy-time configuration and cannot be chosen by input",
        ));
    }
    let base = configuration.get(BASE_ID).ok_or_else(|| {
        ConnectorFailure::invariant("the airtable base is not configured for this instance")
    })?;
    // Airtable prints base IDs as opaque alphanumeric identifiers (`appXXXX…`).
    // The path renderer would percent-encode anything else safely; refusing it
    // here means a mistyped deployment fails at the boundary instead of asking
    // Airtable about a resource that cannot exist.
    if base.is_empty() || base.len() > 64 || !base.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(ConnectorFailure::invariant(
            "an airtable base identifier is alphanumeric",
        ));
    }
    let mut bound = JsonMap::clone(fields);
    bound.insert(BASE_ID.to_owned(), JsonValue::String(base.to_owned()));
    Ok(JsonValue::Object(bound))
}

/// Every operation this connector publishes.
///
/// Airtable documents one success status for the whole API — "200 OK: Request
/// completed successfully" (<https://airtable.com/developers/web/api/errors>) —
/// so every declaration below admits exactly that.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let record_path = "/v0/{base_id}/{table}";
    let one_record = "/v0/{base_id}/{table}/{record_id}";

    // <https://airtable.com/developers/web/api/list-records>
    let list = Operation::get("record.list", record_path)
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .path_param("table", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("records", "/records", ValueScalar::Json, Required::Yes)
        .output_pointer("offset", "/offset", ValueScalar::String, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // <https://airtable.com/developers/web/api/get-record>
    let get = Operation::get("record.get", one_record)
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .path_param("table", ValueScalar::String)
        .path_param("record_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "created_time",
            "/createdTime",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("fields", "/fields", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // <https://airtable.com/developers/web/api/create-records>: the documented
    // request body is `fields` (or `records`), with `typecast` optional. No
    // request header, body field, or query parameter carries a client-supplied
    // key, and nothing in the reference documents deduplication of a repeated
    // create, so this operation cannot show the evidence spec 010 §7 admits.
    let create = Operation::post("record.create", record_path)
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .path_param("table", ValueScalar::String)
        .body(JsonTemplate::object([(
            "fields",
            JsonTemplate::input("fields"),
        )]))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "created_time",
            "/createdTime",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("fields", "/fields", ValueScalar::Json, Required::Yes)
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "Airtable's Web API reference documents the complete request contract for a record create \
             — `fields` or `records`, `typecast`, `returnFieldsByFieldId`, and the one \
             `Authorization` header — and no idempotency key, request identifier, or \
             deduplication appears in it",
            "a second record with a new record ID: Airtable does not deduplicate on field values, so \
             the two have to be reconciled by whoever finds them",
        )?))
        .build()?;

    // <https://airtable.com/developers/web/api/update-record>: a PATCH is a
    // partial update, so repeating it is safe only for the fields it names and
    // never for the record as a whole. Airtable documents no key that would
    // make the send itself repeat-safe.
    let update_patch = Operation::patch("record.update_patch", one_record)
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .path_param("table", ValueScalar::String)
        .path_param("record_id", ValueScalar::String)
        .body(JsonTemplate::object([(
            "fields",
            JsonTemplate::input("fields"),
        )]))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("fields", "/fields", ValueScalar::Json, Required::Yes)
        .effect(Effect::inventory_only(
            "Airtable documents PATCH as a partial update and publishes no idempotency key for \
             it; a PATCH is not one of the two methods HTTP defines repeat-safety for",
        )?)
        .build()?;

    // The `PUT` half of the same page, on a fixed record identity.
    let replace = Operation::put("record.replace", one_record)
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .path_param("table", ValueScalar::String)
        .path_param("record_id", ValueScalar::String)
        .body(JsonTemplate::object([(
            "fields",
            JsonTemplate::input("fields"),
        )]))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("fields", "/fields", ValueScalar::Json, Required::Yes)
        .effect(Effect::provider_idempotent_natural_method(
            "Airtable documents the record PUT as a whole-record replacement on the record ID in \
             the path: \"A PATCH request will only update the fields you specify, leaving the \
             rest as they were. A PUT request will perform a destructive update and clear all \
             unspecified cell values.\"",
        )?)
        .build()?;

    // <https://airtable.com/developers/web/api/delete-record>
    let delete = Operation::delete("record.delete", one_record)
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .path_param("table", ValueScalar::String)
        .path_param("record_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("deleted", "/deleted", ValueScalar::Boolean, Required::Yes)
        .effect(Effect::provider_idempotent_natural_method(
            "Airtable documents the record DELETE against the record ID in the path, answering \
             with that same record: \"id: string (Record ID)\" and \"deleted: true\"",
        )?)
        .build()?;

    // <https://airtable.com/developers/web/api/list-bases>
    let bases = Operation::get("base.list", "/v0/meta/bases")
        .version(VERSION)
        .success_statuses([StatusCode::OK])
        .output_pointer("bases", "/bases", ValueScalar::Json, Required::Yes)
        .output_pointer("offset", "/offset", ValueScalar::String, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // <https://airtable.com/developers/web/api/get-base-schema>
    let schema = Operation::get("base.schema", "/v0/meta/bases/{base_id}/tables")
        .version(VERSION)
        .path_param(BASE_ID, ValueScalar::String)
        // Deploy-time material: `base_scoped_input` fills it and refuses an
        // input that names one of its own.
        .supplied_input(BASE_ID)
        .success_statuses([StatusCode::OK])
        .output_pointer("tables", "/tables", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        list,
        get,
        create,
        update_patch,
        replace,
        delete,
        bases,
        schema,
    ])
}
