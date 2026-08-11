//! Salesforce's REST API, on one org's My Domain host.
//!
//! Ground truth is Salesforce's own published REST API Developer Guide, version
//! 67.0 (Summer '26), read on 2026-08-10:
//!
//! * <https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/intro_rest_resources.htm>
//!   — "https://MyDomainName.my.salesforce.com/services/data/vXX.X/resource/",
//!   "Use `https://` to securely access resources", and "HTTP
//!   Authorization—Provides the OAuth 2.0 access token to authorize your client.
//!   REST API supports the Bearer authentication type."
//! * <https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/errorcodes.htm>
//!   — the status table this module's error map is built from, including "403 |
//!   The request has been refused. Verify that the logged-in user has
//!   appropriate permissions. If the error code is `REQUEST_LIMIT_EXCEEDED`,
//!   you've exceeded API request limits in your org."
//! * <https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/resources_query.htm>
//!   — "The response contains the total number of records returned by the Query
//!   request (`totalSize`), a boolean indicating whether there are no more
//!   results (`done`), the URI of the next set of records (`nextRecordsUrl`),
//!   and an array of query result records (`records`)."
//! * The `sobject` retrieve, create, update, delete, and upsert reference pages,
//!   and <https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/resources_search.htm>.
//!
//! # The org is deploy-time configuration
//!
//! `https://{my_domain}.my.salesforce.com` is an `OriginSpec::TemplatedHost`
//! filled only from `config.settings.my_domain`. Salesforce's token response
//! carries an `instance_url` and its own guidance is to "use the returned
//! `instance_url` as the server instance"; this connector deliberately does not,
//! because spec 010 §4 makes the origin a compile-time property that no
//! credential, response, or continuation may change. A deployment that
//! re-homes its org changes one configuration value and restarts.
//!
//! A **sandbox** is not served by this declaration. Salesforce publishes its host
//! as `https://MyDomainName--SandboxName.sandbox.my.salesforce.com`, whose
//! constant suffix differs from production's, and a templated host declares one
//! constant suffix. A sandbox connector is its own module with its own origin,
//! exactly as HubSpot's forms host is.
//!
//! # The version is pinned in the path
//!
//! Salesforce publishes "Versions 31.0 through 66.0 | Supported" beside the
//! current 67.0, and "If you request any resource or use an operation from a
//! retired API version, REST API returns the 410:GONE error code". [`API_VERSION`]
//! is what this contract version was written against; moving it is a change to
//! this module.
//!
//! # `nextRecordsUrl` already carries the version
//!
//! The continuation Salesforce publishes is a server-absolute *path* that
//! already includes `/services/data/vXX.X` — `"nextRecordsUrl":
//! "/services/data/v67.0/query/01gRO0000016PIAYA2-500"` — so it is spent as a
//! destination resolved against the compiled origin
//! ([`Pagination::next_uri_in_body`]) rather than as a token appended to a path
//! this module composes. A continuation that resolved anywhere but this org's
//! origin is refused rather than followed.
//!
//! The walk ends on the *absence* of that field, which is what Salesforce
//! publishes: "If there are still more records to be returned, the response
//! contains a new query locator and `done` is false." A short page is never the
//! end — "to optimize performance, the returned batch can include fewer records
//! than the limit" — which is why no plan here reads a page length.
//!
//! # A quota refusal is a `403`, not a `429`
//!
//! This is the one classification a status-only map gets wrong for Salesforce.
//! `429` does not occur once in the published REST API guide; the rate limit
//! arrives as `403` carrying `REQUEST_LIMIT_EXCEEDED`, and a Process that
//! declares `retry_on: [http_429]` must reach it there. So [`error_map`] reads
//! the published machine-readable `errorCode` first and keeps the bare `403` —
//! "Verify that the logged-in user has appropriate permissions" —
//! `authentication`. Salesforce publishes no `Retry-After` anywhere in the
//! guide, so the retry hint on that failure is whatever the response carried,
//! which is usually nothing.
//!
//! The error body is an *array* of `{message, errorCode, fields}` objects, so
//! the code pointer is `/0/errorCode`. A handful of Salesforce's own examples
//! print a bare object instead; a failure in that shape matches no code rule and
//! is answered by its status, which is the same class for every status in the
//! table except the quota `403`.
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur once in the published 462-page REST API Developer Guide for v67.0 — not
//! in its Headers chapter, which enumerates every custom request header the API
//! reads (`Sforce-Auto-Assign`, `Sforce-Call-Options`,
//! `Sforce-Duplicate-Rule-Header`, `Sforce-Limit-Info`, `Sforce-Mru`,
//! `Sforce-Query-Options`), and not in its status-code chapter. Salesforce
//! *does* publish an `Idempotency-Key`, and publishes exactly where it works:
//! the **User Interface API**, under `/services/data/vXX.X/ui-api/records`, off
//! by default — "Idempotent record writes aren't enabled in your org. Contact
//! Salesforce to enable this feature" — and covering none of the `/sobjects/`
//! resources this connector calls. A documented restriction to another surface
//! is stronger evidence than an absence.
//!
//! `record.create` is therefore `AtMostOnce` (ADR 063): a repeat leaves a second
//! record with a new id.
//!
//! Three operations stay `InventoryOnly`, each for its own recorded reason.
//!
//! * `record.update` is a `PATCH` — the method spec 010 §7 admits for neither
//!   mutating class — and Salesforce documents it as a partial write: "Field
//!   values provided in the request body replace the existing values in the
//!   record", with a one-field example against a many-field record. A repeat
//!   sets the same fields to the same values, which is not a consequence ADR 063
//!   can bound.
//! * `record.upsert` is the operation Salesforce itself calls idempotent — "In
//!   most cases, we recommend that you use `upsert()` instead of `create()` to
//!   avoid creating unwanted duplicate records (idempotent)", with "If the
//!   external ID matches one existing record, then the existing record is
//!   updated" — and it is a `PATCH`. An operation a provider documents as
//!   repeat-safe wants a class that *keeps* the retry, and that class does not
//!   exist; ADR 063's at-most-once is not it, and `NaturalMethod` is `PUT` and
//!   `DELETE` only.
//! * `record.delete` is a `DELETE` against a fixed identity, which is the right
//!   method — and Salesforce publishes no repeat statement for it. That silence
//!   is deliberate rather than incidental: Salesforce *does* publish the
//!   statement where it holds, for Big Objects — "Repeating a successful
//!   `deleteByExample()` operation results in success, even if the data has
//!   already been deleted" — and publishes nothing of the kind for
//!   `DELETE /sobjects/{sObject}/{id}`. Spec 010 §7's `NaturalMethod` evidence
//!   is the provider's own statement, and there is none to cite.

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
pub const NAME: &str = "salesforce";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key that fills the templated host: the org's
/// own My Domain label.
pub const MY_DOMAIN: &str = "my_domain";

/// The pinned API version. Salesforce publishes "Version 67.0, Summer '26" as
/// current and supports 31.0 through 66.0 beside it.
pub const API_VERSION: &str = "v67.0";

/// The OAuth2 scope a deployment must hold to call these resources.
///
/// "`api` — Allows access to the current, logged-in user's account using APIs,
/// such as REST API and Bulk API 2.0", and "`refresh_token`, `offline_access` —
/// Allows a refresh token to be returned when the requesting client is eligible
/// to receive one", which spec 011's stored credential needs.
pub const API_SCOPE: &str = "api";

/// The scope names Salesforce publishes for a refresh token, either of which
/// satisfies the requirement.
pub const REFRESH_SCOPES: [&str; 2] = ["refresh_token", "offline_access"];

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::templated_host("https", "{my_domain}.my.salesforce.com", None)
                    .expect("Salesforce's published host form is valid"),
            )
            // Spec 011: the credential is the source-local store's, written by
            // `donat connector authorize` and handed to one attempt.
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Salesforce declarations are valid"))
            .build()
            .expect("the Salesforce declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map; see the module documentation for why the code rules
/// come first.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/0/errorCode")
            // "The request exceeded either the concurrent request limit or the
            // request rate limit for your org." It arrives as a `403`, and it is
            // the one `403` a Process should wait out rather than fail on.
            .on_code("REQUEST_LIMIT_EXCEEDED", ConnectorErrorClass::Http429)
            // "A deadlock or timeout condition has been detected." A row another
            // transaction holds is the definition of a condition that clears.
            .on_code("UNABLE_TO_LOCK_ROW", ConnectorErrorClass::Http5xx)
            // The published REST timeout code, which is a slow request rather
            // than a wrong one.
            .on_code("QUERY_TIMEOUT", ConnectorErrorClass::Timeout)
            // "The specified sessionId is malformed (incorrect length or format)
            // or has expired."
            .on_code("INVALID_SESSION_ID", ConnectorErrorClass::Authentication)
            // "The request couldn't be understood, usually because the JSON or
            // XML body contains an error", "412 | ... one or more of the
            // preconditions ... wasn't satisfied", "414 | The length of the URI
            // exceeds the 16,384-byte limit", "428", "431".
            .on_statuses([400, 412, 414, 428, 431], ConnectorErrorClass::Validation)
            // "401 | The session ID or OAuth token used has expired or is
            // invalid", and the bare "403 | The request has been refused. Verify
            // that the logged-in user has appropriate permissions."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "300 | The value returned when an external ID exists in more than
            // one record", "404", "405", "409", "410 | The requested resource
            // has been retired or removed", "415".
            .on_statuses(
                [300, 304, 404, 405, 409, 410, 415, 420],
                ConnectorErrorClass::Permanent,
            )
            // "500 | An error has occurred within Lightning Platform", "502 |
            // Salesforce Edge wasn't able to communicate successfully with the
            // Salesforce instance", "503 | The server is unavailable to handle
            // the request."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Salesforce error map is a valid declaration")
    });
    &MAP
}

/// Decode one Salesforce response: the declared success statuses, then the declared
/// contract.
///
/// It is the declaration-driven answer written out per module rather than
/// inherited, so that the serving runtime asks every connector in this batch the
/// same question and each one answers with its own error map.
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

/// The continuation plan of the two query operations.
///
/// See the module documentation: the continuation is a path, so it is a
/// destination checked against the compiled origin, and its absence is the
/// published end of the result set.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static RECORDS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/records", "/nextRecordsUrl")
            .expect("the Salesforce continuation plan is valid")
    });
    match operation_id {
        "record.query" | "record.query_all" => Some(&RECORDS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The path prefix every resource here is served from.
fn data(path: &str) -> String {
    format!("/services/data/{API_VERSION}{path}")
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in Salesforce's published REST API \
                      Developer Guide for version 67.0 — 462 pages including the Headers chapter, \
                      which enumerates every custom request header the API reads, and the \
                      status-code chapter. Salesforce publishes an `Idempotency-Key` for the User \
                      Interface API's `/ui-api/records` resources only, off by default — \
                      \"Idempotent record writes aren't enabled in your org. Contact Salesforce to \
                      enable this feature\" — and publishes it for none of the `/sobjects/` \
                      resources this connector calls";

/// The three fields a write answers with.
///
/// "The response body contains the ID of the new record if the call is
/// successful", and the published body is `{"id", "errors", "success"}`.
fn write_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("success", "/success", ValueScalar::Boolean, Required::Yes)
        .output_pointer("errors", "/errors", ValueScalar::Json, Required::No)
}

/// Every operation this connector publishes.
///
/// The object name is a path parameter rather than deploy-time configuration,
/// because Salesforce's REST surface is object-generic: one declaration serves
/// Account, Contact, Lead, and Opportunity, and which one a Process drives is
/// its own business. Every value binds through the SDK's path renderer, which
/// percent-encodes each segment, so an object or record id can never leave its
/// own segment.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "If you don't use the `fields` parameter, the request retrieves all
    // standard and custom fields from the record", which is what this
    // declaration does: every declared query slot is a value a caller must
    // send, and a record read that demanded a field list would demand one from
    // a Process that wants the record.
    let record_get = common(Operation::get(
        "record.get",
        &data("/sobjects/{sobject}/{record_id}"),
    ))
    .path_param("sobject", ValueScalar::String)
    .path_param("record_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    // The response is the record's own fields, whose names are the org's, so
    // the whole document is the output.
    .declared_output("record", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "`q` — A SOQL query. To create a valid URI, replace spaces in the query
    // string with a plus sign + or with %20." The SDK's query renderer encodes
    // the value, so the declaration passes it through unchanged.
    let query_output = |builder: OperationBuilder| {
        builder
            .output_pointer("records", "/records", ValueScalar::Json, Required::Yes)
            .output_pointer("done", "/done", ValueScalar::Boolean, Required::No)
            .output_pointer("total_size", "/totalSize", ValueScalar::Int64, Required::No)
            .output_pointer(
                "next_records_url",
                "/nextRecordsUrl",
                ValueScalar::String,
                Required::No,
            )
    };

    let record_query = query_output(
        common(Operation::get("record.query", &data("/query")))
            .query_input("q", "q")
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Although the `nextRecordsUrl` has `query` in the URL, it still provides
    // the remaining results from the initial QueryAll request."
    let record_query_all = query_output(
        common(Operation::get("record.query_all", &data("/queryAll")))
            .query_input("q", "q")
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Executes the specified SOSL search. The search string must be
    // URL-encoded." The response carries `searchRecords` and none of the query
    // continuation fields, so it declares neither.
    let record_search = common(Operation::get("record.search", &data("/search")))
        .query_input("q", "q")
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "search_records",
            "/searchRecords",
            ValueScalar::Json,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    // "You must specify values for required fields in the request body." Which
    // fields those are is the org's own configuration, so the body is the
    // caller's record document rather than a fixed key set.
    let record_create = write_output(
        common(Operation::post(
            "record.create",
            &data("/sobjects/{sobject}"),
        ))
        .path_param("sobject", ValueScalar::String)
        .body(JsonTemplate::input("record"))
        .declared_input("record", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        "a second record with a new id. Salesforce publishes a duplicate-rule header — \
         `Sforce-Duplicate-Rule-Header` — but that is an org's own matching policy on business \
         data rather than a request identifier, and this declaration does not send it",
    )?))
    .build()?;

    // "Field values provided in the request body replace the existing values in
    // the record." The documented success carries no body at all.
    let record_update = common(Operation::patch(
        "record.update",
        &data("/sobjects/{sobject}/{record_id}"),
    ))
    .path_param("sobject", ValueScalar::String)
    .path_param("record_id", ValueScalar::String)
    .body(JsonTemplate::input("record"))
    .declared_input("record", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::inventory_only(
        "Salesforce publishes this as a partial write — \"Field values provided in the request \
         body replace the existing values in the record\", with a one-field example against a \
         many-field record — over a `PATCH`, which spec 010 §7 admits for neither mutating class. \
         A repeat sets the same fields to the same values, which is not a consequence ADR 063 can \
         bound",
    )?)
    .build()?;

    // "Based on whether the value of the external ID exists, the request either
    // creates a record or updates an existing one." The status distinguishes the
    // two: "The HTTP status code is 201 (Created)" on insert, and "In API
    // version 46.0 and later, the HTTP status code is 200 (OK)" on update.
    let record_upsert = write_output(
        common(Operation::patch(
            "record.upsert",
            &data("/sobjects/{sobject}/{external_field}/{external_value}"),
        ))
        .path_param("sobject", ValueScalar::String)
        .path_param("external_field", ValueScalar::String)
        .path_param("external_value", ValueScalar::String)
        .body(JsonTemplate::input("record"))
        .declared_input("record", ValueScalar::Json, Required::Yes)
        // "The `created` parameter is present in the response in API version
        // 46.0 and later."
        .declared_output("created", ValueScalar::Boolean, Required::No)
        .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(Effect::inventory_only(
        "Salesforce documents this operation as repeat-safe and says so in those words — \"In most \
         cases, we recommend that you use upsert() instead of create() to avoid creating unwanted \
         duplicate records (idempotent)\", with \"If the external ID matches one existing record, \
         then the existing record is updated\" — and publishes it as a `PATCH`, which spec 010 §7 \
         admits for neither mutating class. An operation a provider documents as repeat-safe wants \
         a class that keeps the retry; ADR 063's at-most-once class trades the retry away and is \
         the wrong contract for it",
    )?)
    .build()?;

    let record_delete = common(Operation::delete(
        "record.delete",
        &data("/sobjects/{sobject}/{record_id}"),
    ))
    .path_param("sobject", ValueScalar::String)
    .path_param("record_id", ValueScalar::String)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::inventory_only(
        "Salesforce publishes no statement about repeating a `DELETE /sobjects/{sObject}/{id}` — \
         its reference publishes only \"Example response body — None returned\" — and spec 010 §7's \
         NaturalMethod evidence is the provider's own repeat statement. The silence is meaningful \
         rather than incidental: Salesforce publishes exactly that statement where it holds, for \
         Big Objects — \"Repeating a successful deleteByExample() operation results in success, \
         even if the data has already been deleted\" — and publishes nothing of the kind here",
    )?)
    .build()?;

    Ok(vec![
        record_get,
        record_query,
        record_query_all,
        record_search,
        record_create,
        record_update,
        record_upsert,
        record_delete,
    ])
}
