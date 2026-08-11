//! PagerDuty's REST API v2 — incidents, their notes, and the alerts under them.
//!
//! Ground truth is PagerDuty's own published API schema and reference, read on
//! 2026-08-10. PagerDuty publishes a machine-readable description of the whole
//! REST API — <https://github.com/PagerDuty/api-schema>, `reference/REST/openapiv3.json`,
//! titled "PagerDuty API" version `2.0.0` — and every quotation below is from it
//! unless another page is named.
//!
//! * The single declared server is `https://api.pagerduty.com`, described as
//!   "PagerDuty V2 API."
//! * The only security scheme is `api_key`, an `apiKey` in the `Authorization`
//!   header, described as "The API Key with format `Token token=<API_KEY>`".
//! * Every operation declares a required `Accept` header, "The `Accept` header
//!   is used as a versioning header", which this connector sends as the
//!   published v2 media type.
//! * Every write declares a required `From` header, "The email address of a
//!   valid user associated with the account making the request."
//! * Collections answer `offset`, `limit` ("The number of results per page.
//!   Maximum of 100."), `more` ("Indicates if there are additional records to
//!   return") and `total`.
//!
//! # The credential is an authentication parameter, not a bare token
//!
//! `Token token=<API_KEY>` is RFC 9110's `auth-param` production rather than its
//! `token68` one, and no existing plan renders it: `AuthPlan::bearer` fixes the
//! scheme, `AuthPlan::api_key_authorization_scheme` renders `Token <API_KEY>`,
//! and `AuthPlan::api_key_header` refuses the `Authorization` name on purpose.
//! `AuthPlan::api_key_authorization_parameter` is the plan PagerDuty forced, and
//! it is the only connector in the workspace that declares it — see
//! `knowledgebase/declarative-saas/decisions/081-*`.
//!
//! # `From` is a deployment's identity, and it is compiled in
//!
//! PagerDuty attributes every write to the account user named by `From`, so a
//! request that could choose it would be a Process choosing whom to act as. It
//! is therefore compiled into the declaration from `config.settings`, exactly as
//! Basecamp's `User-Agent` is
//! ([[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]]),
//! and this module is a `ModuleDeclaration::PerDeployment` rather than a
//! constant. The four reads do not carry it, because PagerDuty does not require
//! it on them.
//!
//! # Pagination
//!
//! PagerDuty publishes a `more` flag beside `offset`/`limit`, and no plan in
//! spec 010 §8's closed set reads a flag. The declared plan is the
//! `offset`/`limit` regime itself, which ends where every plan in that set ends
//! — on a page shorter than the one that was asked for, which is the absence
//! [[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]]
//! requires. The cost is one extra request when the last page happens to be
//! exactly full, and the alternative was a walk that could not terminate.
//!
//! The note collection declares no plan at all: its reference publishes no
//! `limit` and no `offset`, only `Accept`, `Content-Type` and the incident id,
//! so there is no regime to walk.
//!
//! # Effect classification — the deduplication key that is not a retention
//!
//! PagerDuty publishes a deduplication key on the very endpoint this connector
//! declares. `POST /incidents` takes `incident.incident_key`: "A string which
//! identifies the incident. Sending subsequent requests referencing the same
//! service and with the same `incident_key` will result in those requests being
//! rejected if an open incident matches that `incident_key`."
//!
//! That is a binding and a uniqueness scope — the same service and the same key
//! — and it is **not** an `ExplicitKey`, for two reasons that
//! `knowledgebase/declarative-saas/decisions/080-*` records in full:
//!
//! * **No retention.** PagerDuty publishes no window of any kind for the key.
//!   Its lifetime is the incident's, and an incident is resolved by a human or
//!   an automation at a moment nothing here can observe, so there is no minimum
//!   a clock safety margin could sit strictly under.
//! * **A rejection is not an absorption.** `ExplicitKey` tells the activity
//!   worker "send again, the provider will absorb it". PagerDuty publishes the
//!   opposite: the repeat is *rejected* while the incident is open, and once it
//!   is resolved the same request opens a **second** incident.
//!
//! So `incident.create` and `incident_note.create` are `AtMostOnce` (ADR 063),
//! with the whole mechanism quoted in the evidence exactly as monday's is
//! ([[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]]).
//! `incident_key` is still declared as an ordinary optional input, because it is
//! a field of the incident PagerDuty publishes and a deployment that wants
//! PagerDuty's own de-duplication may send one; nothing in the runtime writes it
//! or relies on it.
//!
//! The Events API v2's `dedup_key` is a different thing on a different origin —
//! `https://events.pagerduty.com/v2`, which this connector does not declare —
//! and PagerDuty describes it as "The key used to correlate triggers,
//! acknowledges, and resolves for the same alert" rather than as a request
//! de-duplication mechanism at all.
//!
//! The two `PUT`s stay `InventoryOnly`: PagerDuty publishes them as
//! "Acknowledge, resolve, escalate or reassign an incident" and "Resolve an
//! alert or associate an alert with a new parent incident" — partial state
//! changes rather than writes to a fixed resource identity, and no statement
//! about repeating one, which is neither spec 010 §7's `NaturalMethod` evidence
//! nor a consequence ADR 063 can bound.

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
pub const NAME: &str = "pagerduty";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// PagerDuty's published REST origin.
const ORIGIN: &str = "https://api.pagerduty.com";

/// The deploy-time configuration key carrying the account user every write is
/// attributed to.
pub const FROM_EMAIL: &str = "from_email";

/// "The `Accept` header is used as a versioning header."
const ACCEPT: &str = "application/vnd.pagerduty+json;version=2";

/// "The number of results per page. Maximum of 100."
const PAGE_SIZE: u32 = 100;

/// The `Authorization` scheme PagerDuty publishes, and its one parameter.
const SCHEME: &str = "Token";
const PARAMETER: &str = "token";

/// One deployment's declaration.
///
/// `from_email` is compiled into the four writes as a static header, because
/// PagerDuty attributes the write to that user and an input that could choose it
/// would be a Process choosing an identity.
pub fn connector(from_email: &str) -> Result<Connector, OperationError> {
    validate_from_email(from_email)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN).expect("PagerDuty's published origin is valid"))
        .credential(
            CredentialSpec::for_plan(AuthPlan::api_key_authorization_parameter(
                SCHEME, PARAMETER,
            )?)
            .with_field(FROM_EMAIL, FieldClassification::NonSecret),
        )
        .operations(operations(from_email)?)
        .build()
}

/// The declaration a reviewer and the registry read, with a placeholder address
/// no deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("deployment-configured@example.invalid")
}

/// Whether a configured `From` value is one this connector may send.
///
/// PagerDuty publishes the rule and nothing more precise: "The email address of
/// a valid user associated with the account making the request." What is checked
/// here is therefore what would make the *header* wrong — an empty value, a
/// value that is not a single visible-ASCII address, or one that carries no `@`
/// — rather than an address grammar PagerDuty did not publish.
pub fn validate_from_email(from_email: &str) -> Result<(), OperationError> {
    let invalid = || {
        OperationError::new(
            "the PagerDuty From address must be one visible-ASCII email address of a user on the \
             account",
        )
    };
    if from_email.is_empty() || from_email.len() > 254 {
        return Err(invalid());
    }
    if !from_email
        .chars()
        .all(|character| character.is_ascii_graphic())
    {
        return Err(invalid());
    }
    let Some((local, domain)) = from_email.split_once('@') else {
        return Err(invalid());
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') {
        return Err(invalid());
    }
    Ok(())
}

/// The ordered error map.
///
/// PagerDuty publishes its statuses per operation, each with a description this
/// map is keyed on: `400` "Caller provided invalid arguments … Retrying with the
/// same arguments will *not* work"; `401` "Caller did not supply credentials or
/// did not provide the correct credentials"; `402` "Account does not have the
/// abilities to perform the action"; `403` "Caller is not authorized to view the
/// requested resource"; `404`; `413` "Request Entity Too Large" for a bulk
/// update over its published ceiling; and `429` "Too many requests have been
/// made, the rate limit has been reached."
///
/// PagerDuty publishes a body — `error.message`, `error.code`, and an `errors`
/// map of "field path to list of validation messages" — and this map reads none
/// of it: the classification is the status, and provider prose never crosses the
/// boundary.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_statuses([400, 413], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([402, 404], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the PagerDuty error map is a valid declaration")
    });
    &MAP
}

/// Decode one PagerDuty response: the declared success statuses, then the
/// declared contract.
///
/// PagerDuty reports every failure with a status, so there is no body gate here;
/// the function exists because every module in this batch owns its decoder and
/// the runtime asks each one the same question.
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

/// The continuation plan of each collection.
///
/// The incident and alert collections publish `offset` and `limit`; the note
/// collection publishes neither, so it declares no plan.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static INCIDENTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/incidents", "offset", "limit", PAGE_SIZE)
            .expect("the PagerDuty incident plan is valid")
    });
    static ALERTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/alerts", "offset", "limit", PAGE_SIZE)
            .expect("the PagerDuty alert plan is valid")
    });
    match operation_id {
        "incident.list" => Some(&INCIDENTS),
        "alert.list" => Some(&ALERTS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION).static_header("Accept", ACCEPT)
}

/// The searched documentation behind every at-most-once class here.
const NO_KEY: &str = "PagerDuty publishes a machine-readable description of its whole REST API \
                      (`PagerDuty/api-schema`, `reference/REST/openapiv3.json`, \"PagerDuty API\" \
                      2.0.0) and the term `idempot` occurs four times in it, none of them a \
                      client-supplied request key on this endpoint: twice on a webhook message id \
                      (\"Uniquely identifies this outgoing webhook message; can be used for \
                      idempotency when processing the messages\", `readOnly`), and twice on a \
                      ServiceNow CMDB table endpoint. The one de-duplication mechanism this \
                      endpoint does publish disqualifies itself: `incident_key` is \"A string which \
                      identifies the incident. Sending subsequent requests referencing the same \
                      service and with the same incident_key will result in those requests being \
                      rejected if an open incident matches that incident_key\" — a rejection rather \
                      than an absorption, with no published retention, which lapses the moment the \
                      incident is resolved";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The published incident properties a Process reads.
fn incident_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/incident/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "incident_number",
            "/incident/incident_number",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "title",
            "/incident/title",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "status",
            "/incident/status",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "urgency",
            "/incident/urgency",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "incident_key",
            "/incident/incident_key",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "created_at",
            "/incident/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "html_url",
            "/incident/html_url",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations(from_email: &str) -> Result<Vec<Operation>, OperationError> {
    // "The email address of a valid user associated with the account making the
    // request." It is a constant of the declaration, never an input.
    let attributed = |builder: OperationBuilder| builder.static_header("From", from_email);

    let incident_get = incident_output(
        common(Operation::get("incident.get", "/incidents/{incident_id}"))
            // "Show detailed information about an incident. Accepts either an
            // incident id, or an incident number."
            .path_param("incident_id", ValueScalar::String)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let incident_list = common(Operation::get("incident.list", "/incidents"))
        // "Return only incidents with the given statuses. To query multiple
        // statuses, pass `statuses[]` more than once".
        .query_input("statuses[]", "status")
        // "Returns only the incidents associated with the passed service(s)."
        //
        // Both filters are declared and both are therefore *required* of a
        // caller: a declared query input renders on every request, so a
        // connector that declared PagerDuty's whole seventeen-parameter filter
        // set would make every one of them mandatory. The two a Process
        // actually drives are here and the rest are not declared at all.
        .query_input("service_ids[]", "service_id")
        .success_statuses([StatusCode::OK])
        // The aggregate a walk assembles lands where the plan's item pointer
        // says, so the declared output reads exactly that place.
        .output_pointer("incidents", "/incidents", ValueScalar::Json, Required::Yes)
        // "Indicates if there are additional records to return" — published so
        // a Process can see the walk stopped on a short page rather than on the
        // flag, which no plan in the closed set reads.
        .output_pointer("more", "/more", ValueScalar::Boolean, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "Create an incident synchronously without a corresponding event from a
    // monitoring service." Required: `type`, `title`, `service`.
    let incident_create = incident_output(
        attributed(common(Operation::post("incident.create", "/incidents")))
            .body(JsonTemplate::object([(
                "incident",
                JsonTemplate::object([
                    ("type", JsonTemplate::literal(JsonValue::from("incident"))),
                    ("title", JsonTemplate::input("title")),
                    (
                        "service",
                        JsonTemplate::object([
                            ("id", JsonTemplate::input("service_id")),
                            (
                                "type",
                                JsonTemplate::literal(JsonValue::from("service_reference")),
                            ),
                        ]),
                    ),
                    ("urgency", JsonTemplate::input("urgency")),
                    ("incident_key", JsonTemplate::input("incident_key")),
                    (
                        "body",
                        JsonTemplate::object([
                            (
                                "type",
                                JsonTemplate::literal(JsonValue::from("incident_body")),
                            ),
                            ("details", JsonTemplate::input("details")),
                        ]),
                    ),
                ]),
            )]))
            .declared_input("title", ValueScalar::String, Required::Yes)
            .declared_input("service_id", ValueScalar::String, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second incident on the same service with a new id and a new incident number, and a \
         second round of notifications to whoever is on call for it — unless an incident opened by \
         the first send is still open and both sends carried the same `incident_key`, in which case \
         PagerDuty rejects the second rather than absorbing it, which is a third outcome and not \
         the first one repeated",
    )?)
    .build()?;

    let incident_update = incident_output(
        attributed(common(Operation::put(
            "incident.update",
            "/incidents/{incident_id}",
        )))
        .path_param("incident_id", ValueScalar::String)
        .body(JsonTemplate::object([(
            "incident",
            JsonTemplate::object([
                ("type", JsonTemplate::literal(JsonValue::from("incident"))),
                // "The new status of the incident."
                ("status", JsonTemplate::input("status")),
                // "The resolution for this incident. This field is used only
                // when setting the incident status to resolved."
                ("resolution", JsonTemplate::input("resolution")),
                ("urgency", JsonTemplate::input("urgency")),
            ]),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "PagerDuty publishes this endpoint as \"Acknowledge, resolve, escalate or reassign an \
         incident\" — a partial state change rather than a write to a fixed resource identity, \
         which is not spec 010 §7's NaturalMethod evidence — and publishes no statement about what \
         a second identical send produces, which is what ADR 063 admits a class on",
    )?)
    .build()?;

    let note_list = common(Operation::get(
        "incident_note.list",
        "/incidents/{incident_id}/notes",
    ))
    .path_param("incident_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("notes", "/notes", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Create a new note for the specified incident." Required: `content`.
    let note_create = attributed(common(Operation::post(
        "incident_note.create",
        "/incidents/{incident_id}/notes",
    )))
    .path_param("incident_id", ValueScalar::String)
    .body(JsonTemplate::object([(
        "note",
        JsonTemplate::object([("content", JsonTemplate::input("content"))]),
    )]))
    .declared_input("content", ValueScalar::String, Required::Yes)
    // PagerDuty publishes `200` rather than `201` for this create.
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/note/id", ValueScalar::String, Required::Yes)
    .output_pointer(
        "content",
        "/note/content",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "created_at",
        "/note/created_at",
        ValueScalar::String,
        Required::No,
    )
    .effect(at_most_once(
        "a second note with the same content on the same incident, and a second notification to \
         everyone subscribed to it",
    )?)
    .build()?;

    let alert_get = common(Operation::get(
        "alert.get",
        "/incidents/{incident_id}/alerts/{alert_id}",
    ))
    .path_param("incident_id", ValueScalar::String)
    // "The id of the alert to retrieve."
    .path_param("alert_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/alert/id", ValueScalar::String, Required::Yes)
    .output_pointer(
        "summary",
        "/alert/summary",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("status", "/alert/status", ValueScalar::String, Required::No)
    .output_pointer(
        "alert_key",
        "/alert/alert_key",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "severity",
        "/alert/severity",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "created_at",
        "/alert/created_at",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let alert_list = common(Operation::get(
        "alert.list",
        "/incidents/{incident_id}/alerts",
    ))
    .path_param("incident_id", ValueScalar::String)
    // "Return only alerts with the given statuses."
    .query_input("statuses[]", "status")
    .success_statuses([StatusCode::OK])
    .output_pointer("alerts", "/alerts", ValueScalar::Json, Required::Yes)
    .output_pointer("more", "/more", ValueScalar::Boolean, Required::No)
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        incident_get,
        incident_list,
        incident_create,
        incident_update,
        note_list,
        note_create,
        alert_get,
        alert_list,
    ])
}
