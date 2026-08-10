//! BambooHR's API v1 — the employee and time-off surface.
//!
//! Ground truth is BambooHR's own published API reference, read on 2026-08-10:
//!
//! * <https://documentation.bamboohr.com/docs/getting-started> — the API key
//!   credential: "Use the secret key as the username and any random string for
//!   the password", with the worked example
//!   `curl -i -u "{API Key}:x" "https://{companyDomain}.bamboohr.com/api/v1/employees/directory"`;
//!   and "If an unknown API key is used repeatedly, the API will disable access
//!   for a period of time", answered with a `403`.
//! * <https://documentation.bamboohr.com/docs/api-details> — "All requests made
//!   to our APIs must be sent over HTTPS"; "API requests are made to a URL that
//!   begins with `https://{companyDomain}.bamboohr.com/api/`"; the status
//!   families (200, 201; 400, 401, 403, 404, 406, 409, 429; 500, 502, 503) with
//!   "treat 200–299 as success … 400–499 as client request errors … 500–599 as
//!   server errors"; and "API requests can be throttled if BambooHR deems them
//!   to be too frequent. Implementations should always be ready for a `503
//!   Service Unavailable` response", which may carry a `Retry-After` header.
//! * <https://documentation.bamboohr.com/reference/get-employee> —
//!   `GET /api/v1/employees/{id}`, the optional `fields` query ("Comma-separated
//!   list of fields to include in the response"), the note that with no `fields`
//!   "the response contains only `id` — there is no implicit default field set",
//!   and the codes `200`, `400` ("More than 400 fields requested"), `401`,
//!   `403`, `404`.
//! * <https://documentation.bamboohr.com/reference/list-employees> —
//!   `GET /api/v1/employees`, the `filter`, `sort`, `fields` and `page` query
//!   parameters, cursor pagination spelled `page[limit]` / `page[after]`, the
//!   response's `data`, `meta` and `_links` (`self`, `next`, `prev`), and the
//!   codes `200`, `400`, `401`, `429`, `500`.
//! * <https://documentation.bamboohr.com/reference/add-employee-2> —
//!   `POST /api/v1/employees`; "New employees must have at least a first name
//!   and a last name"; "The ID of the newly created employee is included in the
//!   `Location` header of the response."
//! * <https://documentation.bamboohr.com/reference/update-employee> —
//!   `POST /api/v1/employees/{id}`, "Update an employee's fields by submitting a
//!   JSON object or XML document containing field name/value pairs", "Only the
//!   fields you include will be updated; omitted fields are left unchanged", and
//!   the codes `200`, `400`, `403`, `404`, `409`.
//! * <https://documentation.bamboohr.com/reference/list-time-off-requests> —
//!   `GET /api/v1/time_off/requests` with required `start` and `end`
//!   (`YYYY-MM-DD`) and the optional `id`, `action`, `employeeId`, `type` and
//!   `status`; the response is an array of time-off requests; codes `200`,
//!   `400`, `401`.
//!
//! # The API key is the HTTP Basic *username*
//!
//! BambooHR publishes exactly one form a deployment can send: the key goes in
//! the username and the password is a constant nobody chooses. That is
//! `AuthPlan::basic_secret_username`, the plan
//! `knowledgebase/declarative-saas/decisions/064-*` added for Freshdesk, and it
//! is the reason this connector does **not** use `AuthPlan::basic`: `basic`
//! takes its username where the *plan* is built, so describing BambooHR with it
//! would put the API key into the declaration, into its `Debug`, and into every
//! diagnostic that prints a connector. The key is the connector's one secret and
//! it reaches the wire only through the plan.
//!
//! The password is `x`, which is what BambooHR's own worked example sends. It is
//! a compile-time constant of this module rather than configuration, because it
//! is not a credential: a deployment that could choose it would be choosing a
//! value the provider ignores.
//!
//! # The company is a host label
//!
//! "API requests are made to a URL that begins with
//! `https://{companyDomain}.bamboohr.com/api/`", and every endpoint reference
//! publishes the same base URL. That is one lowercase DNS label inside an
//! otherwise constant host — `OriginSpec::TemplatedHost`, the Zendesk and
//! Freshdesk shape (ADR 065) — filled only from deploy-time configuration.
//! Nothing in operation input, a provider response, or a continuation can move
//! it.
//!
//! The older `https://api.bamboohr.com/api/gateway.php/{companyDomain}/v1/`
//! form that third-party guides still repeat is deliberately not declared:
//! BambooHR's own current reference publishes the per-company host on every
//! endpoint page, and a connector declares the surface its provider currently
//! publishes ([[081-a-credential-is-an-authentication-parameter-and-a-body-credential-is-a-version-that-was-superseded]]).
//!
//! # JSON is asked for, because XML is what a silent client gets
//!
//! BambooHR serves "`application/json` or `application/xml`", so every operation
//! here sends `Accept: application/json`. A declaration that left the choice to
//! the provider would be a declaration whose output pointers read a document
//! shape nobody selected.
//!
//! # Pagination
//!
//! `GET /api/v1/employees` publishes a cursor and its continuation as
//! `_links.next`, with `next` absent on the last page — the absence a plan in
//! the SDK's closed set reads (ADR 065). `page[limit]` is fixed by the
//! declaration so a caller cannot ask for an unbounded page, and the
//! continuation URI is resolved against the compiled origin and refused when it
//! lands anywhere else.
//!
//! `GET /api/v1/time_off/requests` answers a bare array with no continuation of
//! any kind, so it declares no plan and is one request — which is what its
//! declaration says
//! ([[058-a-declared-walk-is-the-executors-walk]]). The bound on it is
//! BambooHR's own: the required `start` and `end` window.
//!
//! # Effect classification
//!
//! BambooHR publishes no idempotency mechanism anywhere: not in the technical
//! overview, which is where it publishes its status families, its throttling
//! rule and its request formats; not in the getting-started guide; and not in
//! the request contract of either write this connector declares. Neither write
//! takes a client-supplied request identifier.
//!
//! * `employee.create` is `AtMostOnce` (ADR 063): a repeat is a second employee
//!   record, with a second internal id in the `Location` header, in an HR system
//!   of record.
//! * `employee.update` is `InventoryOnly`. BambooHR publishes it over a `POST` —
//!   a method spec 010 §7 does not admit for `NaturalMethod`, because HTTP
//!   defines repeat-safety for `PUT` and `DELETE` — and publishes nothing about
//!   what a second identical send does. There is no consequence to record, so
//!   ADR 063's bar is not met either, and it joins the partial-update group in
//!   `INVENTORY.md`.
//! * Everything else here is a `GET`.

use std::sync::LazyLock;
use std::time::Duration;

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
pub const NAME: &str = "bamboohr";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key filling this connector's host label.
pub const COMPANY_DOMAIN: &str = "company_domain";

/// "API requests are made to a URL that begins with
/// `https://{companyDomain}.bamboohr.com/api/`."
const HOST_TEMPLATE: &str = "{company_domain}.bamboohr.com";

/// The constant BambooHR's own example sends as the HTTP Basic password:
/// `curl -i -u "{API Key}:x"`. It is not a credential and no deployment
/// chooses it.
const BASIC_PASSWORD: &str = "x";

/// The page BambooHR's cursor regime is asked for. Its own default is
/// unspecified, so the declaration fixes one.
const PAGE_SIZE: u32 = 100;

/// BambooHR publishes no per-operation deadline, so this is the module's own
/// bound on one attempt.
const OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// This connector's declaration.
///
/// It is a `&'static`: the per-company part of BambooHR is a *host label*, which
/// the SDK resolves from configuration, and the credential's one secret is
/// resolved per instance rather than compiled in.
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::templated_host("https", HOST_TEMPLATE, None)
                    .expect("the BambooHR host template is valid"),
            )
            // "Use the secret key as the username and any random string for the
            // password": the credential is the username half, and the password
            // is the constant.
            .credential(CredentialSpec::for_plan(
                AuthPlan::basic_secret_username(BASIC_PASSWORD)
                    .expect("the BambooHR credential plan is valid"),
            ))
            .operations(operations().expect("the BambooHR operations are valid"))
            .build()
            .expect("the BambooHR declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// BambooHR publishes status families rather than a machine-readable error code
/// — its failures carry a human message, and some carry it in an
/// `X-BambooHR-Error-Message` header — so this map reads the status and no body
/// pointer.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 … Provided JSON is malformed, or required fields are
            // missing", and "More than 400 fields requested".
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized", and the `403` BambooHR answers both for a
            // permission the API user lacks and for an API key it has disabled
            // after repeated unknown-key requests.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 … The employee does not exist", and the `406` its overview
            // lists for a representation it will not produce.
            .on_statuses([404, 406], ConnectorErrorClass::Permanent)
            // "409 … A field was given an invalid value (e.g., duplicate email,
            // invalid state/country, incompatible pay type)."
            .on_status(409, ConnectorErrorClass::Validation)
            .on_status(429, ConnectorErrorClass::Http429)
            // "Implementations should always be ready for a `503 Service
            // Unavailable` response", which is the throttle as well as the
            // outage, so it is retried with the rest of the 5xx family.
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the BambooHR error map is a valid declaration")
    });
    &MAP
}

/// Decode one BambooHR response: the declared success statuses, then the
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

/// The continuation plan of the one collection BambooHR paginates.
///
/// `GET /api/v1/employees` publishes `_links` with `self`, `next` and `prev`,
/// and the walk ends where `next` is absent. Nothing else here paginates: the
/// time-off listing answers a bare array bounded by its own required date
/// window, and every other operation is a single record.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static EMPLOYEES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/data", "/_links/next")
            .expect("the BambooHR employee continuation plan is valid")
    });
    match operation_id {
        "employee.list" => Some(&EMPLOYEES),
        _ => None,
    }
}

/// What every operation carries: its own deadline, and the representation this
/// connector's output pointers were written against.
fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .deadline(OPERATION_DEADLINE)
        // BambooHR serves "application/json or application/xml"; a declaration
        // that did not choose would be reading a shape nobody selected.
        .static_header("Accept", "application/json")
}

/// The reason this connector's keyless write carries.
const NO_KEY: &str = "BambooHR's published API reference documents no idempotency mechanism of any \
                      kind: not in its Technical Overview, which is where it publishes its status \
                      families, its throttling rule and its request and response formats; not in \
                      its getting-started guide, which is where it publishes its credential; and \
                      not in the request contract of the endpoint itself, whose whole documented \
                      body is a JSON object of employee field name/value pairs with `firstName` \
                      and `lastName` required. No request header, query parameter, or body \
                      attribute carries a client-supplied request identifier";

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let employees = "/api/v1/employees";
    let one_employee = "/api/v1/employees/{employee_id}";
    let time_off_requests = "/api/v1/time_off/requests";

    // "Get Employee — `GET /api/v1/employees/{id}`." With no `fields` the
    // response carries only `id`, so `fields` is a required declared input
    // rather than an optional one a caller would be surprised by.
    let employee_get = common(Operation::get("employee.get", one_employee))
        .path_param("employee_id", ValueScalar::Int64)
        .query_input("fields", "fields")
        .declared_input("fields", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::Json, Required::Yes)
        .output_pointer("firstName", "/firstName", ValueScalar::String, Required::No)
        .output_pointer("lastName", "/lastName", ValueScalar::String, Required::No)
        .output_pointer("workEmail", "/workEmail", ValueScalar::String, Required::No)
        .output_pointer("jobTitle", "/jobTitle", ValueScalar::String, Required::No)
        .output_pointer(
            "department",
            "/department",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("hireDate", "/hireDate", ValueScalar::String, Required::No)
        .output_pointer(
            "employmentHistoryStatus",
            "/employmentHistoryStatus",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "List Employees — `GET /api/v1/employees`", with the documented `filter`,
    // `sort` and `fields` parameters and the cursor page size fixed here.
    let employee_list = common(Operation::get("employee.list", employees))
        .query_input("fields", "fields")
        .query_input("sort", "sort")
        .query_static("page[limit]", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer("meta", "/meta", ValueScalar::Json, Required::No)
        // The continuation BambooHR publishes, carried as data. Only the
        // declared plan turns it into a request, and only on this origin.
        .output_pointer("_links", "/_links", ValueScalar::Json, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "Create Employee — `POST /api/v1/employees`. New employees must have at
    // least a first name and a last name."
    let employee_create = common(Operation::post("employee.create", employees))
        .body(JsonTemplate::object([
            ("firstName", JsonTemplate::input("firstName")),
            ("lastName", JsonTemplate::input("lastName")),
            ("workEmail", JsonTemplate::input("workEmail")),
            ("jobTitle", JsonTemplate::input("jobTitle")),
            ("department", JsonTemplate::input("department")),
            ("hireDate", JsonTemplate::input("hireDate")),
        ]))
        .declared_input("firstName", ValueScalar::String, Required::Yes)
        .declared_input("lastName", ValueScalar::String, Required::Yes)
        // "The ID of the newly created employee is included in the `Location`
        // header of the response", so the documented success carries no body at
        // all and the declaration says so rather than inventing one.
        .success_statuses([StatusCode::CREATED])
        .no_content_statuses([StatusCode::CREATED])
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            NO_KEY,
            "a second employee record in the HR system of record, with a second internal id in the \
             `Location` header, and every downstream integration that reads the directory seeing \
             the person twice",
        )?))
        .build()?;

    // "Update Employee — `POST /api/v1/employees/{id}`. … Only the fields you
    // include will be updated; omitted fields are left unchanged."
    let employee_update = common(Operation::post("employee.update", one_employee))
        .path_param("employee_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("workEmail", JsonTemplate::input("workEmail")),
            ("jobTitle", JsonTemplate::input("jobTitle")),
            ("department", JsonTemplate::input("department")),
            ("employmentHistoryStatus", JsonTemplate::input("status")),
        ]))
        .success_statuses([StatusCode::OK])
        .no_content_statuses([StatusCode::OK])
        .effect(Effect::inventory_only(PARTIAL_UPDATE_OVER_POST)?)
        .build()?;

    // "List Time Off Requests — `GET /api/v1/time_off/requests`", whose `start`
    // and `end` are required and are also this operation's bound.
    let time_off_request_list = common(Operation::get("time_off_request.list", time_off_requests))
        .query_input("start", "start")
        .query_input("end", "end")
        .query_input("status", "status")
        .declared_input("start", ValueScalar::String, Required::Yes)
        .declared_input("end", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK])
        // The response is a bare array at the document root, so there is no
        // pointer to read: the declaration publishes the whole document, exactly as
        // every other bare-collection connector in this workspace does.
        .declared_output("requests", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        employee_get,
        employee_list,
        employee_create,
        employee_update,
        time_off_request_list,
    ])
}

/// The reason `employee.update` carries: a partial update over a method the gate
/// does not admit, whose repeat the provider never described.
const PARTIAL_UPDATE_OVER_POST: &str = "BambooHR publishes this update over a `POST` — \"Only the fields you include will be \
     updated; omitted fields are left unchanged\" — and publishes nothing about what a second \
     identical send does. Spec 010 §7 admits NaturalMethod for PUT and DELETE only, because HTTP \
     defines repeat-safety for those two, and ADR 063's at-most-once class is admitted on a \
     recorded absence *and* a recorded consequence: a partial update that writes the same values \
     a second time has no consequence to record. So the operation stays declared, typed, tested, \
     and unreachable";
