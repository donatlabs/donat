//! Acuity Scheduling's API v1.
//!
//! Ground truth is Acuity's own published developer documentation, read on
//! 2026-08-10 at <https://developers.acuityscheduling.com>. Each endpoint page
//! embeds the OpenAPI 3.1 definition of that endpoint, and the quotations below
//! are those pages and those definitions:
//!
//! * `servers: [{ "url": "https://acuityscheduling.com/api/v1" }]`, on every
//!   endpoint page.
//! * "Authentication to the API is done over SSL with HTTP Basic Auth, using
//!   your numeric **User ID** for the username and your **API Key** for the
//!   password. 401 Unauthorized will be returned on authentication failure.",
//!   with the published example
//!   `curl -u ACUITY_USER_ID:ACUITY_API_KEY "https://acuityscheduling.com/api/v1/appointments"`.
//! * `GET /appointments` — "Get a list of appointments currently scheduled for
//!   the authenticated user."
//! * `GET /appointments/{id}` — "Get a single appointment by ID."
//! * `POST /appointments` — "Create an appointment.", whose OpenAPI body
//!   requires `["datetime", "appointmentTypeID", "firstName", "lastName",
//!   "email"]`.
//! * `PUT /appointments/{id}/cancel` — "Cancel an appointment."
//! * `GET /appointment-types` — "Return appointment types."
//! * `GET /availability/times` — "Return available times for a date and
//!   appointment type.", whose `date` and `appointmentTypeID` are both
//!   `required: true`.
//! * The "API Errors" page's two status tables, quoted in [`error_map`].
//!
//! # Two deploy-time values, and only one of them is a secret
//!
//! Acuity's HTTP Basic username **is** the account's numeric User ID and its
//! password is the API key. The username is therefore declaration material —
//! `AuthPlan::basic` compiles it — so this connector's declaration is built per
//! deployment exactly as Twilio's, Jira's and Bitbucket's are
//! ([[064-a-credentials-scheme-and-its-username-are-the-providers]],
//! [[048-a-declaration-a-deployment-completes]]).
//!
//! The split is the one spec 028 §3 asks for. The User ID is **not** a secret:
//! Acuity prints it in its own settings screen beside the key, it identifies an
//! account rather than authenticating one, and it belongs in `config.settings`
//! and in the configuration fingerprint — a pinned operation against one account
//! is not the same deployment as the same operation against another. The API key
//! is the secret: it lives in `config.secret_key`, it is applied only by the
//! declared auth plan, and it reaches no log line, diagnostic, error or
//! fingerprint. [`validate_user_id`] holds the non-secret half to Acuity's own
//! grammar — "your numeric User ID" — at deploy time, so a mistyped account is a
//! startup refusal rather than a `401` on the first activity.
//!
//! # There is no continuation, so the request is bounded by a date window
//!
//! Acuity publishes no pagination of any kind: `GET /appointments` takes `max` —
//! "maximum number of results", default 100 — and no offset, page, cursor or
//! link, and no other collection here takes even that. No plan in the closed set
//! can express a walk the provider does not publish, so this connector declares
//! none and the collection is bounded the way Acuity's own reference bounds it:
//! `minDate` — "only get appointments this date and after" — and `maxDate` —
//! "only get appointments this date and before" — are **required** inputs here
//! even though Acuity marks them optional. A declared query input renders on
//! every request, so declaring them is declaring that a Process must say which
//! window it wants; the alternative is one unbounded, silently truncated page.
//!
//! # Failures arrive with their own status
//!
//! Every failure body Acuity publishes carries `status_code`, and every row of
//! its error table pairs that value with the HTTP status of the response
//! carrying it — `{"status_code": 401, "message": "Unauthorized", "error":
//! "unauthorized"}` for a `401`, and so on for `400`, `403`, `404`, `405`, `422`,
//! `429` and `500`. There is therefore no body gate here: unlike Jotform,
//! SurveyMonkey and Cal.com, this provider never publishes a failure inside a
//! success status.
//!
//! # Effect classification
//!
//! **Machine-readable description, and nothing in it.** The term `idempot` does
//! not occur in any of the OpenAPI definitions Acuity publishes for the
//! endpoints declared here, nor anywhere in its Quick Start, its API Errors
//! page, or its webhook guide. Each of those definitions enumerates the complete
//! parameter list and request-body schema of its endpoint, and none carries a
//! client-supplied request identifier, an idempotency key, or a deduplication
//! behaviour.
//!
//! `appointment.create` is `AtMostOnce` (ADR 063): a second send books a second
//! appointment wherever the type still has a slot, and Acuity sends the
//! confirmation itself unless the caller suppresses it.
//!
//! `appointment.cancel` stays **`InventoryOnly`**, and it is the sharpest case
//! in this half because it is *method-eligible*: `PUT /appointments/{id}/cancel`
//! is a `PUT` against a fixed resource identity, which is exactly what spec 010
//! §7's `NaturalMethod` admits — on the provider's own repeat statement. Acuity
//! publishes no such statement. What it publishes about repetition is about a
//! different operation: "Once canceled, appointments will have a `noShow`
//! attribute. This attribute may be updated, but it isn't possible to un-cancel
//! the appointment." That says the state is terminal; it does not say a second
//! cancel is absorbed, and it does not say whether the cancellation e-mail and
//! SMS Acuity documents — "Skip sending the cancellation e-mail and SMS by
//! canceling the appointment with the `noEmail=true` query parameter" — are sent
//! again. ADR 042's rule is that the gate admits evidence and not methods, so
//! the method alone does not carry it, and ADR 063 is admitted on a recorded
//! consequence, which an unstated outcome is not.

use std::sync::LazyLock;

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
pub const NAME: &str = "acuity";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Acuity's one published API origin.
const ORIGIN: &str = "https://acuityscheduling.com";

/// The version prefix of every path.
const PREFIX: &str = "/api/v1";

/// The deploy-time configuration key naming the account's numeric User ID, which
/// is this connector's HTTP Basic username and is not a secret.
pub const USER_ID: &str = "user_id";

/// "`max` — maximum number of results", `default: 100`.
const MAX_RESULTS: &str = "100";

/// One deployment's declaration.
///
/// `user_id` is Acuity's Basic username, so it is compiled into the auth plan
/// and no request may choose it. It is also declared as a **non-secret**
/// credential field, so startup can answer "is this instance complete" by name.
pub fn connector(user_id: &str) -> Result<Connector, OperationError> {
    validate_user_id(user_id)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN)?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::basic(user_id)?)
                .with_field(USER_ID, FieldClassification::NonSecret),
        )
        .operations(operations()?)
        .build()
}

/// The declaration a reviewer and the registry read. A deployment is always
/// compiled against its own configured User ID.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("1")
}

/// Acuity's own grammar for the Basic username: "using your **numeric** User ID
/// for the username".
///
/// It is checked at deploy time rather than at the first request, and the
/// refusal names the grammar and never a value.
pub fn validate_user_id(user_id: &str) -> Result<(), OperationError> {
    if user_id.is_empty()
        || user_id.len() > 20
        || !user_id.chars().all(|character| character.is_ascii_digit())
    {
        return Err(OperationError::new(
            "the Acuity user id is the account's numeric User ID: 1 to 20 ASCII digits",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Acuity publishes both halves of every failure — the HTTP status and a stable
/// `error` key — in one table, so the statuses decide and the keys refine. Its
/// `message` is prose and is never matched on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error")
            // "400 Bad Request — The request requires a JSON body, but we
            // couldn't parse it", and "General validation errors such as
            // unexpected values or missing required fields will generate 400
            // errors."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized — We don't know who you are! … Double check the
            // user ID and API key" and "403 Forbidden — The request included
            // authentication and we know who you are, but you don't have access
            // to the resource."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found — The requested resource could not be found" and
            // "405 Method Not Allowed — The resource exists but the HTTP method
            // is not supported."
            .on_statuses([404, 405], ConnectorErrorClass::Permanent)
            // "422 Unprocessable Entity — … our POST /blocks endpoint returns
            // 422 error on time validation errors."
            .on_status(422, ConnectorErrorClass::Validation)
            // "429 Too Many Requests — Woah there! Our API is currently rate
            // limited to 10 requests a second and 20 concurrent connections from
            // an IP." Acuity publishes no `Retry-After`, so a delay only ever
            // arrives if it sends the standard header.
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error — Something unexpected happened."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // The published `error` keys, for any status the table above does
            // not name.
            .on_code("unauthorized", ConnectorErrorClass::Authentication)
            .on_code("forbidden", ConnectorErrorClass::Authentication)
            .on_code("not_found", ConnectorErrorClass::Permanent)
            .on_code("method_not_allowed", ConnectorErrorClass::Permanent)
            .on_code("too_many_requests", ConnectorErrorClass::Http429)
            .on_code("internal_server_error", ConnectorErrorClass::Http5xx)
            .on_code("bad_request", ConnectorErrorClass::Validation)
            .on_code("invalid_name", ConnectorErrorClass::Validation)
            .on_code("invalid_time", ConnectorErrorClass::Validation)
            // The cancel's own two published errors.
            .on_code("cancel_not_allowed", ConnectorErrorClass::Validation)
            .on_code("cancel_too_close", ConnectorErrorClass::Validation)
            .build()
            .expect("the Acuity error map is a valid declaration")
    });
    &MAP
}

/// Decode one Acuity response: the declared success statuses, then the declared
/// contract.
///
/// There is no body gate here, and the module documentation says why: every
/// failure Acuity publishes carries the same status inside the body as the
/// response it arrives on.
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

/// Acuity publishes no continuation of any kind, so no operation declares a
/// plan; see the module documentation.
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// A bare JSON array at the document root, which is what every Acuity collection
/// answers with.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder.success_statuses([StatusCode::OK]).declared_output(
        "items",
        ValueScalar::Json,
        Required::Yes,
    )
}

/// The declared fields of one appointment, from Acuity's own published example.
fn appointment(builder: OperationBuilder) -> OperationBuilder {
    builder
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("datetime", "/datetime", ValueScalar::String, Required::Yes)
        .output_pointer(
            "appointment_type_id",
            "/appointmentTypeID",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer(
            "calendar_id",
            "/calendarID",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "first_name",
            "/firstName",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("last_name", "/lastName", ValueScalar::String, Required::No)
        .output_pointer("email", "/email", ValueScalar::String, Required::No)
        .output_pointer("canceled", "/canceled", ValueScalar::Boolean, Required::No)
        .output_pointer("no_show", "/noShow", ValueScalar::Boolean, Required::No)
        .output_pointer(
            "confirmation_page",
            "/confirmationPage",
            ValueScalar::String,
            Required::No,
        )
}

/// The searched documentation behind this connector's one at-most-once class.
const NO_KEY: &str = "the term `idempot` does not occur in any of the OpenAPI 3.1 definitions \
                      Acuity publishes on the endpoint pages declared here \
                      (`developers.acuityscheduling.com/reference/*`), nor in its Quick Start, its \
                      API Errors page, or its webhook guide. Each of those definitions enumerates \
                      the complete parameter list and request-body schema of its endpoint — the \
                      create's is `required: [\"datetime\", \"appointmentTypeID\", \"firstName\", \
                      \"lastName\", \"email\"]` with eleven further optional properties — and none \
                      of them is a client-supplied request identifier or a deduplication \
                      behaviour";

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Also include deleted appointment types in the response." is the one
    // filter, and Acuity publishes no value of it meaning "everything and
    // nothing", so it is not declared: a declared query input renders on every
    // request.
    let appointment_type_list = collection(common(Operation::get(
        "appointment_type.list",
        &format!("{PREFIX}/appointment-types"),
    )))
    .effect(Effect::read_only())
    .build()?;

    // Declared because Acuity's own reference makes it the step before a create:
    // "Use /availability/times together with /availability/dates to find
    // available slots for creating an appointment." Both of its inputs are
    // `required: true` in the provider's own definition.
    let availability_times = collection(
        common(Operation::get(
            "availability.times",
            &format!("{PREFIX}/availability/times"),
        ))
        .query_input("date", "date")
        .query_input("appointmentTypeID", "appointment_type_id"),
    )
    .effect(Effect::read_only())
    .build()?;

    let appointment_list = collection(
        common(Operation::get(
            "appointment.list",
            &format!("{PREFIX}/appointments"),
        ))
        .query_static("max", MAX_RESULTS)
        .query_input("minDate", "min_date")
        .query_input("maxDate", "max_date"),
    )
    .effect(Effect::read_only())
    .build()?;

    let appointment_get = appointment(
        common(Operation::get(
            "appointment.get",
            &format!("{PREFIX}/appointments/{{appointment_id}}"),
        ))
        .path_param("appointment_id", ValueScalar::Int64),
    )
    .effect(Effect::read_only())
    .build()?;

    // The five properties Acuity's own OpenAPI marks required, and no more. The
    // optional eleven are deliberately not declared: a body leaf here is a
    // mandatory input, and a create that demanded a certificate code or an addon
    // id from every Process would be a contract this provider does not publish.
    let appointment_create = appointment(
        common(Operation::post(
            "appointment.create",
            &format!("{PREFIX}/appointments"),
        ))
        .body(JsonTemplate::object([
            ("datetime", JsonTemplate::input("datetime")),
            (
                "appointmentTypeID",
                JsonTemplate::input("appointment_type_id"),
            ),
            ("firstName", JsonTemplate::input("first_name")),
            ("lastName", JsonTemplate::input("last_name")),
            ("email", JsonTemplate::input("email")),
        ]))
        // "Required date and time for the appointment, parsed by strtotime in
        // the business or calendar timezone."
        .declared_input("datetime", ValueScalar::String, Required::Yes)
        .declared_input("appointment_type_id", ValueScalar::Int64, Required::Yes)
        .declared_input("first_name", ValueScalar::String, Required::Yes)
        .declared_input("last_name", ValueScalar::String, Required::Yes)
        .declared_input("email", ValueScalar::String, Required::Yes),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        "a second appointment at the same time wherever the appointment type still has a slot for \
         one — a second entry on the calendar and a second confirmation to the client, which \
         Acuity sends itself unless the caller suppresses it",
    )?))
    .build()?;

    // "A message to send with cancellation notifications." Declared rather than
    // omitted because it is what the client is told; a Process with nothing to
    // say sends an empty string.
    let appointment_cancel = appointment(
        common(Operation::put(
            "appointment.cancel",
            &format!("{PREFIX}/appointments/{{appointment_id}}/cancel"),
        ))
        .path_param("appointment_id", ValueScalar::Int64)
        .body(JsonTemplate::object([(
            "cancelNote",
            JsonTemplate::input("cancel_note"),
        )]))
        .declared_input("cancel_note", ValueScalar::String, Required::Yes),
    )
    .effect(Effect::inventory_only(
        "Acuity publishes the cancel as a PUT against the fixed appointment identity in the path — \
         \"Cancel an appointment.\" — which is the method half of spec 010 §7's NaturalMethod and \
         not its evidence half. The provider publishes no repeat statement: what it says about \
         repetition is about a different operation, \"it isn't possible to un-cancel the \
         appointment\", and it never says whether a second cancel is absorbed or whether the \
         cancellation e-mail and SMS it documents are sent again. ADR 042 admits evidence rather \
         than methods, and ADR 063's AtMostOnce is admitted on a recorded consequence, which an \
         unstated outcome is not.",
    )?)
    .build()?;

    Ok(vec![
        appointment_type_list,
        availability_times,
        appointment_list,
        appointment_get,
        appointment_create,
        appointment_cancel,
    ])
}
