//! Twilio's 2010-04-01 REST API (Messages and Calls).
//!
//! Ground truth is Twilio's own published API reference, read on 2026-08-10:
//!
//! * <https://www.twilio.com/docs/messaging/api/message-resource> — `POST
//!   https://api.twilio.com/2010-04-01/Accounts/{AccountSid}/Messages.json`,
//!   content type `application/x-www-form-urlencoded`, required `To` plus one
//!   of `From`/`MessagingServiceSid` plus one of `Body`/`MediaUrl`/`ContentSid`.
//! * <https://www.twilio.com/docs/voice/api/call-resource> — `POST …/Calls.json`
//!   with required `To`, `From`, and exactly one of `Url`, `Twiml`, or
//!   `ApplicationSid`.
//! * <https://www.twilio.com/docs/usage/twilios-response> — the status table
//!   ("200 Successfully processed", "201 Resource created", "401 Invalid
//!   credentials", "404 Resource not found", "429 Rate limit exceeded", "500
//!   Server error", "503 Service unavailable"), the error body
//!   `{"status": 400, "message": …, "code": 21201, "more_info": …}`, and the
//!   list envelope (`page`, `page_size`, `uri`, `first_page_uri`,
//!   `next_page_uri`, `previous_page_uri`).
//!
//! # The account SID completes the declaration
//!
//! Twilio authenticates with HTTP Basic where the username is the Account SID
//! and the password is the Auth Token, and the same Account SID is a path
//! segment of every resource. The SID is deploy-time material, so — unlike
//! every other connector in this batch — the declaration itself is built per
//! deployment by [`connector`] rather than held in a `static`. The SDK's
//! `AuthPlan::basic` takes its username where the plan is built, and inventing
//! a placeholder to keep a `static` would be a credential contract that does
//! not describe what reaches the wire.
//!
//! # The body is form-encoded
//!
//! Twilio's 2010-04-01 API takes `application/x-www-form-urlencoded` bodies,
//! which no static JSON template expresses, so the two write operations declare
//! a processor body and [`message_send_body`] / [`call_create_body`] assemble
//! the bytes. The processors choose nothing else: the method, origin, path,
//! query, and every header name stay in the declaration, and each renders only
//! the documented parameter names.
//!
//! # Pagination
//!
//! Twilio publishes its continuation as `next_page_uri`, a URI inside the
//! response body, and its `page` is zero-indexed — two protocols, and this
//! connector declares one plan for each. `Cursor` and `TokenInBody` would send
//! that URI back as a `PageToken` *value*, which Twilio does not accept, and
//! `LinkHeader` reads a header Twilio does not send; the plan that fits is the
//! SDK's body-carried next URI, resolved and origin-checked exactly as
//! `LinkHeader` is. [`pagination`] declares it, and it is the plan the serving
//! executor walks, because only the continuation carries the `PageToken` the
//! API needs past the first page. [`page_number_pagination`] declares the other
//! protocol, and its first page is 0: a walk that began at 1 would return the
//! collection without its first page. Nothing walks that second plan — it
//! records what Twilio publishes and is proven by its own test
//! (`knowledgebase/declarative-saas/decisions/058-*` names it as the one
//! declared-but-unwired plan in this workspace).
//!
//! # Effect classification
//!
//! Twilio documents an idempotency header in exactly one place, and it is not
//! this one: the Monitor Alarms API takes `Idempotency-Token`, and inbound
//! webhooks carry `I-Twilio-Idempotency-Token`. The Message and Call resources
//! document neither, and their documented request parameter sets contain no
//! client-supplied key, so `message.send` and `call.create` are `AtMostOnce`
//! (ADR 063): a repeat is a second message or a second call, each with a second
//! charge, and a Process reaches them only by declaring `at_most_once` and a
//! route for an outcome nobody can know (see `INVENTORY.md`). Everything else
//! here is a `GET`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::StatusCode;
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{
    Connector, ConnectorConfiguration, CredentialSpec, FieldClassification, OriginSpec,
};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "twilio";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Twilio's one published REST API origin.
const ORIGIN: &str = "https://api.twilio.com";

/// The deploy-time configuration key holding the account this instance uses.
/// It is both the HTTP Basic username and a path segment of every resource.
pub const ACCOUNT_SID: &str = "account_sid";

/// "PageSize: Results per page (default 50, max 1000)". The declaration fixes
/// it, so a caller cannot ask for an unbounded page.
const PAGE_SIZE: u32 = 50;

/// Twilio's list envelope documents `page` as the "current page number", and it
/// is zero-indexed: page 0 is the first page.
const FIRST_PAGE: u32 = 0;

/// The media type Twilio documents for its 2010-04-01 request bodies.
const FORM_MEDIA_TYPE: &str = "application/x-www-form-urlencoded";

/// This connector's declaration for one deployment.
///
/// `account_sid` is the deployment's Twilio Account SID; it becomes the Basic
/// username and the `account_sid` path value, and nothing else can supply
/// either.
pub fn connector(account_sid: &str) -> Result<Connector, OperationError> {
    validate_account_sid(account_sid)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN)?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::basic(account_sid)?)
                // Not a secret: Twilio prints the Account SID in its own
                // console and in every resource URI. It is still deploy-time
                // material, which is why it is declared here.
                .with_field(ACCOUNT_SID, FieldClassification::NonSecret),
        )
        .operations(operations()?)
        .build()
}

/// The ordered error map.
///
/// Twilio publishes a numeric `code` beside every failure, and the two rules
/// below are the ones whose class the status alone would not settle. Everything
/// else is decided by the documented status table.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/code")
            // "20003: Permission Denied — This error usually means the
            // credentials on your request are incorrect, expired, deleted,
            // scoped to the wrong account, or not valid for the resource you
            // are trying to access."
            .on_code("20003", ConnectorErrorClass::Authentication)
            // "20429: Too many requests — your account exceeds allowed
            // concurrency to Twilio's REST API (HTTP 429 Too Many Requests)."
            .on_code("20429", ConnectorErrorClass::Http429)
            // The error dictionary's own HTTP statuses: 400, 403, 404, 410,
            // 503, plus the response table's 401, 429, 500.
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 410], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Twilio error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each list operation.
///
/// Twilio publishes `next_page_uri`, a *relative* URI inside the response
/// body, which the SDK's body-carried next-URI plan resolves against the
/// compiled origin and then checks against it, so a continuation naming
/// anywhere else is refused rather than followed. This is the plan a walk
/// should use: Twilio's own `Page` is client state, and only the continuation
/// carries the `PageToken` the API needs past the first page.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static MESSAGES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/messages", "/next_page_uri")
            .expect("the Twilio message continuation plan is valid")
    });
    static CALLS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/calls", "/next_page_uri")
            .expect("the Twilio call continuation plan is valid")
    });
    match operation_id {
        "message.list" => Some(&MESSAGES),
        "call.list" => Some(&CALLS),
        _ => None,
    }
}

/// Twilio's other documented paging protocol, for the same two operations.
///
/// The serving executor walks [`pagination`], not this: a deployment cannot
/// select between them, and a walk that spent `Page` would stop carrying the
/// `PageToken` Twilio needs past its first page.
///
/// It is declared separately rather than instead of [`pagination`] because
/// Twilio publishes both, and the difference between them is the whole reason
/// the SDK's page walk carries its first page number: Twilio's list envelope
/// documents `page` as the "current page number ... zero-indexed", so a walk
/// that began at page 1 would silently return the collection *without its first
/// page* — a wrong answer rather than a failure.
pub fn page_number_pagination(operation_id: &str) -> Option<&'static Pagination> {
    static MESSAGES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number_from("/messages", "Page", "PageSize", PAGE_SIZE, FIRST_PAGE)
            .expect("the Twilio message page plan is valid")
    });
    static CALLS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number_from("/calls", "Page", "PageSize", PAGE_SIZE, FIRST_PAGE)
            .expect("the Twilio call page plan is valid")
    });
    match operation_id {
        "message.list" => Some(&MESSAGES),
        "call.list" => Some(&CALLS),
        _ => None,
    }
}

/// Bind the deployment's configured account to one operation input.
///
/// The account SID is the account-scoped identifier of spec 012 §1: it comes
/// from deploy-time configuration and never from operation input.
pub fn account_scoped_input(
    configuration: &ConnectorConfiguration,
    input: &JsonValue,
) -> Result<JsonValue, ConnectorFailure> {
    let Some(fields) = input.as_object() else {
        return Err(ConnectorFailure::invariant(
            "a twilio operation input is a JSON object",
        ));
    };
    if fields.contains_key(ACCOUNT_SID) {
        return Err(ConnectorFailure::invariant(
            "the twilio account is deploy-time configuration and cannot be chosen by input",
        ));
    }
    let account_sid = configuration.get(ACCOUNT_SID).ok_or_else(|| {
        ConnectorFailure::invariant("the twilio account is not configured for this instance")
    })?;
    if validate_account_sid(account_sid).is_err() {
        return Err(ConnectorFailure::invariant(
            "a twilio account SID is 34 alphanumeric characters beginning with AC",
        ));
    }
    let mut bound = JsonMap::clone(fields);
    bound.insert(
        ACCOUNT_SID.to_owned(),
        JsonValue::String(account_sid.to_owned()),
    );
    Ok(JsonValue::Object(bound))
}

/// Twilio documents the Account SID as a 34-character identifier beginning
/// `AC`. Refusing anything else here means a mistyped deployment fails at the
/// boundary rather than authenticating as nobody.
fn validate_account_sid(account_sid: &str) -> Result<(), OperationError> {
    let valid = account_sid.len() == 34
        && account_sid.starts_with("AC")
        && account_sid.chars().all(|c| c.is_ascii_alphanumeric());
    if !valid {
        return Err(OperationError::new(
            "a twilio account SID is 34 alphanumeric characters beginning with AC",
        ));
    }
    Ok(())
}

/// The form body of `message.send`: the documented `To`, `From`, and `Body`
/// parameters and nothing else.
pub fn message_send_body(input: &JsonValue) -> Result<Vec<u8>, ConnectorFailure> {
    form_body(input, &["To", "From", "Body"], &["to", "from", "body"])
}

/// The form body of `call.create`: the documented `To`, `From`, and `Url`
/// parameters and nothing else.
pub fn call_create_body(input: &JsonValue) -> Result<Vec<u8>, ConnectorFailure> {
    form_body(input, &["To", "From", "Url"], &["to", "from", "url"])
}

/// Render a fixed list of documented parameter names from declared input slots.
///
/// The parameter names are `&'static` and come from the caller above; input
/// supplies values only, exactly as a JSON body template would allow. Every
/// byte outside `[A-Za-z0-9]` is percent-encoded, so a value carrying `&`, `=`,
/// or a newline cannot add a parameter of its own.
fn form_body(
    input: &JsonValue,
    parameters: &[&str],
    slots: &[&str],
) -> Result<Vec<u8>, ConnectorFailure> {
    let mut pairs = Vec::with_capacity(parameters.len());
    for (parameter, slot) in parameters.iter().zip(slots) {
        let value = input.get(*slot).ok_or_else(|| {
            ConnectorFailure::invariant("a declared connector input value is missing")
        })?;
        let value = match value {
            JsonValue::String(value) => value.clone(),
            JsonValue::Number(value) => value.to_string(),
            JsonValue::Bool(value) => value.to_string(),
            JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => {
                return Err(ConnectorFailure::invariant(
                    "a declared connector input value must be scalar",
                ));
            }
        };
        pairs.push(format!(
            "{parameter}={}",
            utf8_percent_encode(&value, NON_ALPHANUMERIC)
        ));
    }
    Ok(pairs.join("&").into_bytes())
}

fn resource(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .path_param(ACCOUNT_SID, ValueScalar::String)
        // The Account SID is the deployment's, in the path exactly as it is in
        // the Basic username: a Process binds neither.
        .supplied_input(ACCOUNT_SID)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let messages = "/2010-04-01/Accounts/{account_sid}/Messages.json";
    let one_message = "/2010-04-01/Accounts/{account_sid}/Messages/{sid}.json";
    let calls = "/2010-04-01/Accounts/{account_sid}/Calls.json";
    let one_call = "/2010-04-01/Accounts/{account_sid}/Calls/{sid}.json";

    let message_send = resource(Operation::post("message.send", messages))
        .processor_body(FORM_MEDIA_TYPE)
        .success_statuses([StatusCode::CREATED])
        .output_pointer("sid", "/sid", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
        .output_pointer("to", "/to", ValueScalar::String, Required::Yes)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::Yes,
        )
        // Twilio publishes the failure of a *sent* message here rather than as
        // a status, so the declared contract carries it as an explicit null
        // when the message has not failed.
        .output_pointer(
            "error_code",
            "/error_code",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "Twilio's Message resource documents the complete parameter set for the create and \
             publishes no idempotency key for it; the one idempotency header Twilio documents, \
             Idempotency-Token, belongs to the Monitor Alarms API, and \
             I-Twilio-Idempotency-Token belongs to deliveries Twilio itself makes",
            "a second SMS or MMS, a second `sid`, and a second charge",
        )?))
        .build()?;

    let message_get = resource(Operation::get("message.get", one_message))
        .path_param("sid", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("sid", "/sid", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
        .output_pointer("to", "/to", ValueScalar::String, Required::Yes)
        .output_pointer("body", "/body", ValueScalar::String, Required::No)
        .output_pointer(
            "error_code",
            "/error_code",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let message_list = resource(Operation::get("message.list", messages))
        .query_static("PageSize", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("messages", "/messages", ValueScalar::Json, Required::Yes)
        .output_pointer("page", "/page", ValueScalar::Int64, Required::Yes)
        // The continuation Twilio publishes, carried as data. Nothing in this
        // connector turns it into a request.
        .output_pointer(
            "next_page_uri",
            "/next_page_uri",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let call_create = resource(Operation::post("call.create", calls))
        .processor_body(FORM_MEDIA_TYPE)
        .success_statuses([StatusCode::CREATED])
        .output_pointer("sid", "/sid", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
        .output_pointer("to", "/to", ValueScalar::String, Required::Yes)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "Twilio's Call resource documents the complete parameter set for the create and \
             publishes no idempotency key for it, and the same Twilio observation applies: the \
             concept is published where it applies and not here",
            "a second outbound call to the same number, and a second charge",
        )?))
        .build()?;

    let call_get = resource(Operation::get("call.get", one_call))
        .path_param("sid", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("sid", "/sid", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
        .output_pointer("to", "/to", ValueScalar::String, Required::Yes)
        .output_pointer(
            "direction",
            "/direction",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    let call_list = resource(Operation::get("call.list", calls))
        .query_static("PageSize", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("calls", "/calls", ValueScalar::Json, Required::Yes)
        .output_pointer("page", "/page", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "next_page_uri",
            "/next_page_uri",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        message_send,
        message_get,
        message_list,
        call_create,
        call_get,
        call_list,
    ])
}
