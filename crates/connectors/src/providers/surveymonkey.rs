//! SurveyMonkey's API v3.
//!
//! Ground truth is SurveyMonkey's own published API documentation at
//! <https://api.surveymonkey.com/v3/docs>, read on 2026-08-10:
//!
//! * "Base URLs: `https://api.surveymonkey.com/v3`", and the authentication
//!   header its every code sample carries: `Authorization: Bearer {access-token}`.
//! * "Our access tokens don't currently expire but may in the future. We'll warn
//!   all developers before making changes."
//! * `GET /surveys` — "Returns a list of surveys owned or shared with the
//!   authenticated user. Public App users need access to the `View Surveys`
//!   scope".
//! * `GET /surveys/{id}` — "Returns a survey's details. To get an expanded
//!   version showing all pages and questions use `/surveys/{survey_id}/details`."
//! * `GET /surveys/{id}/details` — the expanded design, which the provider's own
//!   quick start uses to "associate the selected answer id" with its value.
//! * `GET /surveys/{id}/responses/bulk` — "Retrieves a list of full expanded
//!   responses, including answers to all questions. Public App users need access
//!   to the `View Response Details` scope", with "`per_page` — Number of
//!   resources to return per page. Max of 100 allowed per page. Defaults to 50".
//! * `GET /surveys/{survey_id}/responses/{response_id}/details` — "Retrieve a
//!   full expanded response, including answers to all questions."
//! * `DELETE /surveys/{survey_id}/responses/{response_id}` — "Deletes a
//!   response. Public App users need access to the `Create/Modify Responses`
//!   scope".
//! * The "Error Codes" table — every `id` and its HTTP status — and the
//!   "Headers" table with its `X-Ratelimit-App-Global-Minute-*` and
//!   `X-Ratelimit-App-Global-Day-*` family.
//!
//! # One origin, and the two this connector deliberately does not declare
//!
//! SurveyMonkey publishes three: "Depending on the originating datacenter of the
//! SurveyMonkey account, the API access URL may be different than
//! `https://api.surveymonkey.com`. The API for the EU datacenter is
//! `https://api.eu.surveymonkey.com` and the API for the Canadian datacenter is
//! `https://api.surveymonkey.ca`." It also publishes where the right one comes
//! from: "The correct API access URL for each SurveyMonkey account is returned in
//! the response body of the code for token exchange under the `access_url` key."
//!
//! An origin a provider response can move is not a fixed origin, so `access_url`
//! is never read here. This declaration is the global datacentre's origin, which
//! is Typeform's answer to the same question, and a deployment in another
//! datacentre is a second connector rather than a second mode of this one
//! ([[074-a-second-origin-is-a-second-connector-and-a-download-is-composed-under-its-bound]]).
//! SurveyMonkey makes the mismatch loud rather than silent, which is why this is
//! safe: its own error table publishes "1018 — 403 Permission Error — The user
//! does not have permission to access the host in this region. See Access URL",
//! and this module's map classifies that as an authentication failure.
//!
//! # Pagination is the provider's own continuation URI
//!
//! SurveyMonkey publishes both a page number and a continuation: "`links.next` —
//! Resource URL for the subsequent page of results", and its own quick start
//! walks the second one — "Fetch the next page of 100 responses using the
//! resource url returned in the `links.next` field." This connector declares the
//! URI, because it is the only one of the two that cannot silently truncate: a
//! `page`/`per_page` walk stops when a page is shorter than the size it asked
//! for, and SurveyMonkey publishes a maximum for exactly one of its two
//! collections, so a declared page size the provider quietly capped would end a
//! walk early with a *wrong answer* rather than a failure. `links.next` is
//! absent on the last page, which is where the walk ends.
//!
//! `NextUriInBody` is a destination rather than a query value, so it is resolved
//! against this connector's compiled origin and refused anywhere else — the same
//! treatment a `Link` continuation gets, and the reason ADR 047 kept the two
//! plans apart.
//!
//! # A `200` can carry a failure
//!
//! Two failure envelopes are published, and neither is guaranteed to arrive with
//! a matching HTTP status. The first is the error object — `{"error": {"docs":
//! …, "message": …, "id": "1050", "name": "Internal Server Error",
//! "http_status_code": 500}}` — which carries its own status *inside the body*.
//! The second is the revocation envelope: "Access tokens can be revoked by the
//! user. If this happens, you'll get a JSON-encoded response body including a key
//! `status` with a value of `1` and a key `errmsg` with the value of `Client
//! revoked access grant` when making an API request." SurveyMonkey names no HTTP
//! status for that one at all.
//!
//! [`decode`] therefore gates the body between the status check and the declared
//! output pointers: a declared success carrying either envelope is classified as
//! the failure it is, and there is no spelling in which a revoked grant reads as
//! an activity success
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur once anywhere in SurveyMonkey's v3 documentation — not in its
//! authentication guide, its pagination and headers sections, its error-code
//! table, or the request contract of any endpoint. Every endpoint entry
//! enumerates its complete query-string table and its request body schema, and
//! none names a client-supplied request identifier or a deduplication behaviour;
//! no response header in the published "Headers" table carries one either.
//!
//! Five of the six operations here are `GET`s. The sixth, `response.delete`,
//! stays **`InventoryOnly`** for the reason `jotform.submission.delete` does:
//! it is a `DELETE` against a fixed resource identity, but spec 010 §7's
//! `NaturalMethod` is admitted on the provider's own repeat statement and
//! SurveyMonkey publishes none — "Deletes a response." is the whole of it — while
//! ADR 063's `AtMostOnce` is admitted on a recorded consequence of a second send,
//! which an unstated outcome is not.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "surveymonkey";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The global datacentre's origin; see the module documentation for the two this
/// connector deliberately does not declare.
const ORIGIN: &str = "https://api.surveymonkey.com";

/// The version prefix of every path, from "Base URLs:
/// `https://api.surveymonkey.com/v3`".
const PREFIX: &str = "/v3";

/// "Max of 100 allowed per page. Defaults to 50" — the one per-page maximum
/// SurveyMonkey publishes anywhere in its v3 documentation.
const PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("SurveyMonkey's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the SurveyMonkey declarations are valid"))
            .build()
            .expect("the SurveyMonkey declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// The documented statuses decide first, because SurveyMonkey's own table maps
/// every error id to exactly one of them; the ids then refine anything the table
/// does not name, which is what makes a failure reported inside a `200` classify
/// as the failure it is. `message` is prose and is never matched on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        // The published ids, for any status the status table does not name —
        // including a `2xx` whose body carries the error object.
        let documented_ids = [
            // "1000–1004 — 400 Bad Request", "1056/1059/1060/1062 — 400".
            ("1000", ConnectorErrorClass::Validation),
            ("1001", ConnectorErrorClass::Validation),
            ("1002", ConnectorErrorClass::Validation),
            ("1003", ConnectorErrorClass::Validation),
            ("1004", ConnectorErrorClass::Validation),
            ("1056", ConnectorErrorClass::Validation),
            ("1059", ConnectorErrorClass::Validation),
            ("1060", ConnectorErrorClass::Validation),
            ("1062", ConnectorErrorClass::Validation),
            // "1030 — 413 Request Entity Too Large".
            ("1030", ConnectorErrorClass::Validation),
            // "1010–1013, 1057, 1063 — 401" and "1014–1018, 1058 — 403".
            ("1010", ConnectorErrorClass::Authentication),
            ("1011", ConnectorErrorClass::Authentication),
            ("1012", ConnectorErrorClass::Authentication),
            ("1013", ConnectorErrorClass::Authentication),
            ("1014", ConnectorErrorClass::Authentication),
            ("1015", ConnectorErrorClass::Authentication),
            ("1016", ConnectorErrorClass::Authentication),
            ("1017", ConnectorErrorClass::Authentication),
            ("1018", ConnectorErrorClass::Authentication),
            ("1057", ConnectorErrorClass::Authentication),
            ("1058", ConnectorErrorClass::Authentication),
            ("1063", ConnectorErrorClass::Authentication),
            // "1005 — 402", "1020/1052 — 404", "1025/1026 — 409",
            // "1053 — 410", "1061 — 405".
            ("1005", ConnectorErrorClass::Permanent),
            ("1020", ConnectorErrorClass::Permanent),
            ("1025", ConnectorErrorClass::Permanent),
            ("1026", ConnectorErrorClass::Permanent),
            ("1052", ConnectorErrorClass::Permanent),
            ("1053", ConnectorErrorClass::Permanent),
            ("1061", ConnectorErrorClass::Permanent),
            // "1040 — 429 Rate Limit Reached".
            ("1040", ConnectorErrorClass::Http429),
            // "1050 — 500", "1051 — 503", "1054 — 502", "1055 — 504".
            ("1050", ConnectorErrorClass::Http5xx),
            ("1051", ConnectorErrorClass::Http5xx),
            ("1054", ConnectorErrorClass::Http5xx),
            ("1055", ConnectorErrorClass::Http5xx),
        ];
        let mut builder = ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/id")
            // "1000–1004, 1056, 1059, 1060, 1062 — 400 Bad Request".
            .on_status(400, ConnectorErrorClass::Validation)
            // "1010–1013, 1057 — 401 Authorization Error" and "1063 — 401 Bad
            // Unauthorized"; "1014–1018, 1058 — 403 Permission Error", which
            // includes the wrong-region refusal.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "1005 — 402 Payment Required — Account must be upgraded to access
            // this feature." Repeating it changes nothing for this deployment.
            .on_status(402, ConnectorErrorClass::Permanent)
            // "1020 — 404 Resource Not Found", "1052 — 404 User Soft Deleted",
            // "1061 — 405 Method Not Allowed", "1025/1026 — 409 Resource
            // Conflict", "1053 — 410 User Deleted".
            .on_statuses([404, 405, 409, 410], ConnectorErrorClass::Permanent)
            // "1030 — 413 Request Entity Too Large — The requested entity is too
            // large, it can not be returned." A smaller request is the fix.
            .on_status(413, ConnectorErrorClass::Validation)
            // "1040 — 429 Rate Limit Reached — Too many requests were made, try
            // again later."
            .on_status(429, ConnectorErrorClass::Http429)
            // "1050 — 500", "1051 — 503", "1054 — 502 Bad Gateway Error",
            // "1055 — 504 Gateway Timeout".
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx);
        for (code, class) in documented_ids {
            builder = builder.on_code(code, class);
        }
        builder
            .build()
            .expect("the SurveyMonkey error map is a valid declaration")
    });
    &MAP
}

/// Whether a body carries one of the two published failure envelopes.
///
/// The first is the error object; the second is the revocation envelope, whose
/// `status` is documented as `1` for a revoked grant and for which SurveyMonkey
/// publishes no HTTP status at all. Both are read structurally — the presence of
/// the object, and a numeric `status` that is not `0` beside an `errmsg` — so no
/// provider prose is matched on.
fn reports_failure(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<JsonValue>(body) else {
        return false;
    };
    if value.pointer("/error").is_some_and(JsonValue::is_object) {
        return true;
    }
    value.pointer("/errmsg").is_some()
        && value
            .pointer("/status")
            .and_then(JsonValue::as_i64)
            .is_some_and(|status| status != 0)
}

/// Decode one SurveyMonkey response: the declared success statuses, then the
/// published failure envelopes, then the declared contract.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) || reports_failure(body) {
        return Err(error_map().classify(status, headers, body));
    }
    operation.decode_response(status, body)
}

/// The continuation plan of each collection; see the module documentation for
/// why it is the URI rather than the page number.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/data", "/links/next")
            .expect("the SurveyMonkey continuation plan is valid")
    });
    match operation_id {
        "survey.list" | "response.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The list envelope every collection publishes.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer("page", "/page", ValueScalar::Int64, Required::No)
        .output_pointer("per_page", "/per_page", ValueScalar::Int64, Required::No)
        .output_pointer("total", "/total", ValueScalar::Int64, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let survey_list = collection(common(Operation::get(
        "survey.list",
        &format!("{PREFIX}/surveys"),
    )))
    .effect(Effect::read_only())
    .build()?;

    let survey_get = common(Operation::get(
        "survey.get",
        &format!("{PREFIX}/surveys/{{survey_id}}"),
    ))
    .path_param("survey_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
    .output_pointer("nickname", "/nickname", ValueScalar::String, Required::No)
    .output_pointer("language", "/language", ValueScalar::String, Required::No)
    .output_pointer(
        "date_created",
        "/date_created",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "date_modified",
        "/date_modified",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("href", "/href", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // Declared because a response's answers carry question and choice *ids*, and
    // this is the endpoint SurveyMonkey's own quick start uses to resolve them:
    // "This call returns the survey's design with all question ids and answer
    // option ids, as well as the values associated with them."
    let survey_details = common(Operation::get(
        "survey.details",
        &format!("{PREFIX}/surveys/{{survey_id}}/details"),
    ))
    .path_param("survey_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("title", "/title", ValueScalar::String, Required::No)
    .output_pointer("pages", "/pages", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // The bulk collection rather than the bare one: `/responses` publishes ids
    // and hrefs only, and a Process that has to fetch each response separately
    // is the N+1 this workspace does not build.
    let response_list = collection(common(Operation::get(
        "response.list",
        &format!("{PREFIX}/surveys/{{survey_id}}/responses/bulk"),
    )))
    .path_param("survey_id", ValueScalar::String)
    .effect(Effect::read_only())
    .build()?;

    let response_get = common(Operation::get(
        "response.get",
        &format!("{PREFIX}/surveys/{{survey_id}}/responses/{{response_id}}/details"),
    ))
    .path_param("survey_id", ValueScalar::String)
    .path_param("response_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("survey_id", "/survey_id", ValueScalar::String, Required::No)
    .output_pointer(
        "collector_id",
        "/collector_id",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "response_status",
        "/response_status",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "date_created",
        "/date_created",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("pages", "/pages", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "DELETE: Deletes a response." SurveyMonkey publishes no response schema
    // for it, so an empty success is the documented answer rather than a
    // malformed one.
    let response_delete = common(Operation::delete(
        "response.delete",
        &format!("{PREFIX}/surveys/{{survey_id}}/responses/{{response_id}}"),
    ))
    .path_param("survey_id", ValueScalar::String)
    .path_param("response_id", ValueScalar::String)
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .effect(Effect::inventory_only(
        "SurveyMonkey documents the delete as \"Deletes a response.\" against the fixed response \
         identity in the path, and publishes no statement about a repeat and no response schema \
         at all. Spec 010 §7's NaturalMethod is admitted on the provider's own repeat statement \
         rather than on the method, and ADR 063's AtMostOnce is admitted on a recorded \
         consequence of a second send, which an unstated outcome is not.",
    )?)
    .build()?;

    Ok(vec![
        survey_list,
        survey_get,
        survey_details,
        response_list,
        response_get,
        response_delete,
    ])
}
