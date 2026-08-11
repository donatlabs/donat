//! Jotform's API v1, in the one data region a deployment's account lives in.
//!
//! Ground truth is Jotform's own published API documentation at
//! <https://api.jotform.com/docs/> — its overview, its authentication section,
//! its FAQ, and the endpoint reference the same page publishes for every
//! endpoint named below — read on 2026-08-10, together with
//! <https://www.jotform.com/help/406-daily-api-call-limits/>:
//!
//! * "You can access our API through the following URLs: Standard API Usage:
//!   Use the default API URL: `https://api.jotform.com`. For EU: Use the EU API
//!   URL: `https://eu-api.jotform.com`. For HIPAA: Use the HIPAA API URL:
//!   `https://hipaa-api.jotform.com`."
//! * "You can send your API Key with your query as a HTTP header", with the
//!   published example `curl -H "APIKEY: {myApiKey}" "https://api.jotform.com/user"`.
//! * `GET /user/forms` — "Get a list of forms for this account. Includes basic
//!   details such as title of the form, when it was created, number of new and
//!   total submissions."
//! * `GET /form/{id}` — "Get basic information about a form. Use
//!   `/form/{id}/questions` to get the list of questions."
//! * `GET /form/{id}/questions` — "Get a list of all questions on a form."
//! * `GET /form/{id}/submissions` — "List of form responses. **answers** array
//!   has the submitted data. Created_at is the date of the submission."
//! * `GET /submission/{id}` — "Similar to /form/{form-id}/submissions. But only
//!   get a single submission."
//! * `DELETE /submission/{id}` — "Delete a single submission."
//! * `offset` — "Start of each result set for form list. Useful for pagination.
//!   Default is 0."; `limit` — "Number of results in each result set for form
//!   list. Default is 20. Maximum is 1000."
//! * "API Keys are limited to: 1000 requests per day for the starter plan"
//!   through "100000 requests per day for the gold plan", and the help page's
//!   "you may begin receiving an 'API-Limit exceeded' error message" with "The
//!   daily API call count automatically resets at midnight, Eastern Standard
//!   Time (EST)."
//!
//! # The region is deploy-time configuration, and it is a closed set
//!
//! Jotform serves one account from one region and publishes three API origins
//! for them. Two of the three spell a prefix in front of `api` rather than a
//! label under a constant suffix (`eu-api.jotform.com`, `hipaa-api.jotform.com`),
//! which `OriginSpec::TemplatedHost`'s one-label grammar cannot produce, so this
//! connector's declaration is built per deployment from a **closed compiled
//! table** of the origins Jotform publishes ([`Region`]) — Zoho CRM's shape
//! ([[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]]).
//! The deployment names a region, not a host, and a region Jotform does not
//! publish does not resolve.
//!
//! Jotform also publishes a fourth shape — "Upgrade to Enterprise to make your
//! API url `your-domain.com/API` or `subdomain.jotform.com/API` instead of
//! api.jotform.com" — and this connector deliberately does not declare it. That
//! URL carries a **path** (`/API`), and an [`crate::sdk::connector::OriginSpec`]
//! is a scheme, host, and port with no path at all; an enterprise deployment is
//! a second connector rather than a fourth row of this table.
//!
//! # The API key is the secret and the region is not
//!
//! Two values complete a deployment. The API key is applied only by the auth
//! plan, is carried by `config.secret_key`, and reaches nothing else. The region
//! is an ordinary non-secret deploy-time setting: it names one of three public
//! origins, it appears in the compiled origin every diagnostic prints, and it is
//! part of the configuration fingerprint because a pinned operation against the
//! EU origin is not the same deployment as the same operation against the US
//! one.
//!
//! # Pagination
//!
//! Both collections publish the same protocol — `offset` from 0 and `limit` up
//! to 1000 — and answer with `resultSet: {offset, limit, count}` beside the
//! `content` array, so [`crate::sdk::pagination::Pagination::offset_limit`]
//! expresses it exactly. Each list also asks for the published maximum on its
//! own, so a single request that is not walked still returns a full page rather
//! than the documented default of 20.
//!
//! # A `200` can carry a failure
//!
//! Every Jotform response is an envelope, and its first member is the
//! provider's own status: `{"responseCode": 200, "message": "success",
//! "content": …, "limit-left": 4986}`. Jotform never states that the envelope's
//! `responseCode` always equals the HTTP status of the response carrying it, so
//! this module owns its [`decode`]: a declared success whose envelope reports a
//! non-success `responseCode` is classified through the error map before any
//! declared output pointer is read, and there is no spelling in which such a
//! body reads as an activity success. That is the Slack precedent, applied here
//! for the same reason
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur anywhere in Jotform's published API documentation: not in the overview,
//! not in the authentication section, not in the FAQ, and not in the request
//! contract of any endpoint in its reference — no request header, no query
//! parameter, no body field, and no response field carries a client-supplied
//! request identifier or a deduplication behaviour. Jotform's own summary of the
//! surface is "Jotform API v1 is mostly read only."
//!
//! Five of the six operations here are `GET`s and are read-only by their method.
//! The sixth, `submission.delete`, stays **`InventoryOnly`**, and the reason is
//! the one this workspace has already recorded for Trello's, monday's and
//! Todoist's deletes: it is a `DELETE` against a fixed resource identity, but
//! spec 010 §7's `NaturalMethod` is admitted on *the provider's own repeat
//! statement*, and Jotform publishes none — the only outcome its reference names
//! for the endpoint at all is "404 — User not found". ADR 063's `AtMostOnce` is
//! not the home either: that class is admitted on a recorded **consequence** of
//! a second send, and "the provider does not say what a second delete of an
//! already-deleted submission answers" is the absence of one rather than one.

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
pub const NAME: &str = "jotform";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key naming the data region.
pub const REGION: &str = "region";

/// "You can send your API Key with your query as a HTTP header":
/// `curl -H "APIKEY: {myApiKey}"`.
pub const API_KEY_HEADER: &str = "APIKEY";

/// "Number of results in each result set for form list. Default is 20. Maximum
/// is 1000."
const PAGE_SIZE: u32 = 1000;

/// The same maximum as the static a single, unwalked request asks for.
const PAGE_SIZE_LITERAL: &str = "1000";

/// One Jotform data region: the API origin this connector renders against.
///
/// The set is closed and compiled, and it is exactly the three URLs Jotform's
/// own "API Endpoints" section publishes. A deployment names a region; it never
/// names a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    name: &'static str,
    api_origin: &'static str,
}

impl Region {
    /// Every region this connector serves, in the order Jotform lists them.
    pub const ALL: [Self; 3] = [
        Self::new("us", "https://api.jotform.com"),
        Self::new("eu", "https://eu-api.jotform.com"),
        Self::new("hipaa", "https://hipaa-api.jotform.com"),
    ];

    const fn new(name: &'static str, api_origin: &'static str) -> Self {
        Self { name, api_origin }
    }

    /// The one region a deployment named, or a refusal listing what it could
    /// have named.
    pub fn parse(name: &str) -> Result<Self, OperationError> {
        Self::ALL
            .into_iter()
            .find(|region| region.name == name)
            .ok_or_else(|| {
                OperationError::new(
                    "the Jotform region must be one Jotform publishes an API URL for: us, eu, \
                     hipaa",
                )
            })
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn api_origin(&self) -> &'static str {
        self.api_origin
    }
}

/// One deployment's declaration.
///
/// The region decides the origin, so the declaration is built per deployment —
/// and, unlike a templated host, what a deployment may name is a compiled set of
/// three rather than a grammar.
pub fn connector(region: Region) -> Result<Connector, OperationError> {
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(region.api_origin())?)
        .credential(CredentialSpec::for_plan(AuthPlan::api_key_header(
            API_KEY_HEADER,
        )?))
        .operations(operations()?)
        .build()
}

/// The declaration a reviewer and the registry read, on Jotform's own default
/// API URL. A deployment is always compiled against its own configured region.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector(Region::ALL[0])
}

/// The ordered error map.
///
/// The documented HTTP statuses decide first — Jotform's reference publishes
/// "404 — User not found" on every endpoint and "400" on the writes that need a
/// parameter — and the envelope's own `responseCode` refines anything the status
/// table does not name, which is what makes a failure inside a `200` classify as
/// the failure it is rather than as the fallback. Jotform's `message` is prose
/// and is never matched on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/responseCode")
            // "400 — Question ID is not in request", "400 — No resources
            // supplied!" — the shape every documented `400` here has.
            .on_status(400, ConnectorErrorClass::Validation)
            // Jotform names no status for a bad key, but "To get started using
            // Jotform API you need a valid API key" is the whole of its
            // authentication contract; `401` and `403` are the two HTTP defines
            // for failing it.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 — User not found", the one outcome the reference publishes
            // for every endpoint declared here.
            .on_status(404, ConnectorErrorClass::Permanent)
            // The daily call limit — "1000 requests per day for the starter
            // plan" — whose exceeded state Jotform describes as an
            // "'API-Limit exceeded' error message" without naming a status.
            // `429` is the one HTTP defines for it, and the limit "resets at
            // midnight, Eastern Standard Time", which is why a `Retry-After`
            // this connector cannot invent is left to the provider.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // The envelope's own code, for any status the table above does not
            // name — including a `200` whose envelope reports a failure.
            .on_code("400", ConnectorErrorClass::Validation)
            .on_code("401", ConnectorErrorClass::Authentication)
            .on_code("403", ConnectorErrorClass::Authentication)
            .on_code("404", ConnectorErrorClass::Permanent)
            .on_code("429", ConnectorErrorClass::Http429)
            .on_code("500", ConnectorErrorClass::Http5xx)
            .on_code("503", ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Jotform error map is a valid declaration")
    });
    &MAP
}

/// The envelope's own status, when the body is the envelope Jotform publishes.
fn envelope_response_code(body: &[u8]) -> Option<u64> {
    let value: JsonValue = serde_json::from_slice(body).ok()?;
    match value.pointer("/responseCode")? {
        JsonValue::Number(code) => code.as_u64(),
        JsonValue::String(code) => code.parse().ok(),
        _ => None,
    }
}

/// Decode one Jotform response: the declared success statuses, then the
/// envelope's own `responseCode`, then the declared contract.
///
/// The body gate sits between the status check and the output pointers, so a
/// failure Jotform reported inside a `2xx` can never be read as a success; see
/// the module documentation.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    match envelope_response_code(body) {
        Some(code) if (200..300).contains(&code) => operation.decode_response(status, body),
        Some(_) => Err(error_map().classify(status, headers, body)),
        None => Err(ConnectorFailure::invariant(
            "connector provider answered outside its declared contract",
        )),
    }
}

/// The continuation plan of each collection.
///
/// Both walk the same published protocol, and both write their aggregate where
/// the declared `content` output reads it.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/content", "offset", "limit", PAGE_SIZE)
            .expect("the Jotform offset plan is valid")
    });
    match operation_id {
        "form.list" | "submission.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The envelope every operation publishes: the payload, and the daily call
/// budget Jotform reports beside it ("limit-left is the number of daily api
/// calls you can make").
fn envelope(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("content", "/content", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "limit_left",
            "/limit-left",
            ValueScalar::Int64,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let form_list = envelope(
        common(Operation::get("form.list", "/user/forms"))
            .query_static("limit", PAGE_SIZE_LITERAL)
            .success_statuses([StatusCode::OK])
            .output_pointer("result_set", "/resultSet", ValueScalar::Json, Required::No),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Form ID is the numbers you see on a form URL" — one path segment, and the
    // documented `content` of this endpoint is an array carrying the one form.
    let form_get = envelope(
        common(Operation::get("form.get", "/form/{form_id}"))
            .path_param("form_id", ValueScalar::String)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // Declared because a submission's `answers` are keyed by question id — "qid
    // is question ID" — so a Process that reads a response cannot interpret it
    // without the form's question list.
    let question_list = envelope(
        common(Operation::get("question.list", "/form/{form_id}/questions"))
            .path_param("form_id", ValueScalar::String)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // Jotform publishes `filter` and `orderby` for this collection and no value
    // of either meaning "everything". A declared query input renders on every
    // request and would therefore be mandatory, so neither is declared and the
    // list is the form's submissions.
    let submission_list = envelope(
        common(Operation::get(
            "submission.list",
            "/form/{form_id}/submissions",
        ))
        .path_param("form_id", ValueScalar::String)
        .query_static("limit", PAGE_SIZE_LITERAL)
        .success_statuses([StatusCode::OK])
        .output_pointer("result_set", "/resultSet", ValueScalar::Json, Required::No),
    )
    .effect(Effect::read_only())
    .build()?;

    let submission_get = envelope(
        common(Operation::get(
            "submission.get",
            "/submission/{submission_id}",
        ))
        .path_param("submission_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Delete a single submission." The published success carries the prose
    // `content` "Submission #{submissionID} deleted successfully." and nothing
    // else — no statement about a second send, which is why the class is
    // inventory-only rather than `NaturalMethod`.
    let submission_delete = envelope(
        common(Operation::delete(
            "submission.delete",
            "/submission/{submission_id}",
        ))
        .path_param("submission_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Jotform publishes the delete as \"Delete a single submission.\" against the fixed \
         submission identity in the path, and publishes no statement at all about a repeat: the \
         only outcome its reference names for this endpoint is \"404 — User not found\". Spec 010 \
         §7's NaturalMethod is admitted on the provider's own repeat statement rather than on the \
         method, and ADR 063's AtMostOnce is admitted on a recorded consequence of a second send, \
         which an unstated outcome is not.",
    )?)
    .build()?;

    Ok(vec![
        form_list,
        form_get,
        question_list,
        submission_list,
        submission_get,
        submission_delete,
    ])
}
