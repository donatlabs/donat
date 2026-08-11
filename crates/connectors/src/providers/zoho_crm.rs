//! Zoho CRM's REST API v8, in one deployment's own data centre.
//!
//! Ground truth is Zoho's own published documentation, read on 2026-08-10:
//!
//! * <https://www.zoho.com/crm/developer/docs/api/v8/access-refresh.html> —
//!   "This completes the authentication. Once your app receives the access
//!   token, send the token in your HTTP authorization header to Zoho CRM API
//!   with the value "Zoho-oauthtoken {access_token}" for each endpoint (for each
//!   request)", "{accounts_URL}/oauth/v2/token", and "A refresh token does not
//!   expire."
//! * <https://www.zoho.com/crm/developer/docs/api/v8/multi-dc.html> and
//!   <https://www.zoho.com/accounts/protocol/oauth/multi-dc.html> — the data
//!   centres and their accounts hosts.
//! * <https://www.zoho.com/crm/developer/docs/api/v8/get-records.html>,
//!   `/search-records.html`, `/insert-records.html`, `/update-records.html`,
//!   `/upsert-records.html`, `/create-notes.html`, `/get-notes.html` — every
//!   path, parameter, and response field below.
//! * <https://www.zoho.com/crm/developer/docs/api/v8/status-codes.html> and
//!   `/api-limits.html` — the status table and the rate limit.
//! * <https://www.zoho.com/crm/developer/docs/api/v8/scopes.html> — "The format
//!   to define a scope is scope=service_name.scope_name.operation_type".
//!
//! # The authorization scheme is Zoho's, not RFC 6750's
//!
//! Every other stored-OAuth2 connector in this workspace sends `Authorization:
//! Bearer <token>`. Zoho CRM publishes `Zoho-oauthtoken` and uses it in every
//! example on every endpoint page; `Bearer` appears in its CRM documentation
//! only as the `token_type` *value* in a token response and in one line of
//! generic OAuth preamble that its own instruction and examples contradict.
//! Sending `Bearer` here would be sending a credential the provider's own
//! reference never publishes, so the scheme is part of the declaration and the
//! credential lifecycle formats the applied header with it. See
//! [`AuthPlan::oauth2_authorization_code_scheme`].
//!
//! # The data centre is deploy-time configuration, and it is a closed set
//!
//! Zoho serves one org from one data centre, and the API host differs per centre
//! — `https://www.zohoapis.com`, `.eu`, `.in`, `.com.au`, `.jp`, `.ca`,
//! `.com.cn`, `.sa`. Two of those spell their suffix with a dot in it, which a
//! single templated host label cannot produce, so this connector's declaration
//! is built per deployment from a **closed compiled table** of the origins Zoho
//! publishes ([`Region`]): the deployment names a region, not a host, and a
//! region Zoho does not publish does not resolve. That is strictly narrower than
//! a templated host — a template admits any label, a table admits eight — and it
//! is what lets one connector serve every data centre without letting
//! configuration name an authority.
//!
//! Zoho's token response carries an `api_domain` and its own guidance is to "use
//! this domain in your requests"; this connector deliberately does not, for the
//! reason spec 010 §4 gives: an origin a provider response can move is not a
//! fixed origin. The region's accounts host is published beside it so that a
//! deployment whose `config.oauth2.token_endpoint` names a *different* data
//! centre is refused before a listener opens rather than authenticating into an
//! org it cannot then reach.
//!
//! # No continuation plan, and the reason is a documented status
//!
//! Zoho publishes a walkable cursor — "This param takes the value from the key
//! `next_page_token` in the response of the first Get Records call" — and it
//! also publishes "No Content **HTTP 204** — There is no content available for
//! the request" as the answer to a collection with nothing in it. A walk in this
//! SDK reads the declared item list out of every page it receives, so a walk
//! whose first page is a documented empty body fails an attempt that should have
//! returned nothing. The provider's own contract therefore contains a page no
//! plan in the closed set can spend, and this module declares none
//! (`knowledgebase/declarative-saas/decisions/055-*`).
//!
//! What it declares instead is the regime Zoho publishes for a caller: "If your
//! requirement is to fetch under 2000 records, use the "page" and "per_page"
//! parameters (page=1 to 10, per_page=200)." The page number is an ordinary
//! declared input a Process advances, and the `next_page_token` regime is not
//! bound at all — Zoho publishes it as mutually exclusive with `page` ("Note
//! that you cannot use this param with the "page" param"), its token "is valid
//! only for 24 hours", and it is the regime whose empty answer breaks a walk.
//! `next_page_token` and `more_records` are published as *outputs* so a Process
//! can see that a collection is longer than the regime reaches.
//!
//! # A `2xx` can carry a per-record failure
//!
//! "In case of partial success or failure, the API returns an HTTP status - 207
//! (Multi-Status), where individual record-level success or error details are
//! provided within the response array in the same order." A single-record write
//! that failed therefore arrives as a `207` whose one entry is
//! `"status": "error"`, and Zoho's success bodies carry the same per-record
//! `status` on `200` and `201`. [`decode`] refuses any `2xx` whose first record
//! entry is not `"success"` before the declared output pointers are read, so
//! there is no spelling in which a rejected record is reported as an activity
//! success. `207` is not a declared success status at all, which is the same
//! answer one layer earlier. See
//! `knowledgebase/declarative-saas/decisions/056-*`.
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur in any of Zoho CRM's v8 pages for authentication, scopes, multi-DC,
//! records, notes, search, API limits, or status codes: no request header, no
//! body property, no response field.
//!
//! `record.create` and `note.create` are `AtMostOnce` (ADR 063). `record.update`
//! stays `InventoryOnly` — Zoho publishes a `PUT` whose `_delete` key and
//! `$append_values` option only make sense if omitted fields are left untouched,
//! which is a partial update rather than the write to a fixed resource identity
//! `NaturalMethod` needs, and it publishes no consequence for a repeat.
//!
//! `record.upsert` stays `InventoryOnly` for the opposite reason, and it is the
//! sharpest case in this batch: Zoho documents it as repeat-safe — "If a
//! matching record exists, it gets updated. If no matching record is found, a
//! new record is inserted", with the response distinguishing the two in an
//! `action` field — and publishes it as a `POST`, which spec 010 §7's
//! `NaturalMethod` does not admit. An operation a provider documents as
//! repeat-safe wants a class that keeps the retry, and ADR 063's is not it.

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
pub const NAME: &str = "zoho_crm";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key naming the data centre.
pub const REGION: &str = "region";

/// The published `Authorization` scheme: "send the token in your HTTP
/// authorization header to Zoho CRM API with the value "Zoho-oauthtoken
/// {access_token}"".
pub const AUTHORIZATION_SCHEME: &str = "Zoho-oauthtoken";

/// The pinned API version. Zoho's current published version is v8.
pub const API_VERSION: &str = "v8";

/// "Specify how many records to return per page. The default and the maximum
/// possible value is 200."
const PAGE_SIZE: &str = "200";

/// The scope prefix every CRM scope carries, which is what a deployment's
/// declared scopes are checked against.
///
/// "The format to define a scope is `scope=service_name.scope_name.operation_type`",
/// with the service name `ZohoCRM` and values such as
/// `ZohoCRM.modules.deals.READ` and `ZohoCRM.modules.ALL`.
pub const SCOPE_PREFIX: &str = "ZohoCRM.";

/// One Zoho data centre: the API origin this connector renders against and the
/// accounts origin its OAuth2 exchange must use.
///
/// The set is closed and compiled. Zoho's CRM multi-DC page publishes the
/// accounts host of every centre and the API host of two of them
/// (`https://www.zohoapis.eu`, `https://www.zohoapis.com.cn`), its token
/// response sample publishes `https://www.zohoapis.com`, and its own
/// domain-specific URL table publishes the remaining five. Canada is the one to
/// read twice: its accounts host is `zohocloud.ca` while its API host is
/// `zohoapis.ca`, and neither is derivable from the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    name: &'static str,
    api_origin: &'static str,
    accounts_origin: &'static str,
}

impl Region {
    /// Every data centre this connector serves, in the order Zoho lists them.
    pub const ALL: [Self; 8] = [
        Self::new(
            "us",
            "https://www.zohoapis.com",
            "https://accounts.zoho.com",
        ),
        Self::new("eu", "https://www.zohoapis.eu", "https://accounts.zoho.eu"),
        Self::new("in", "https://www.zohoapis.in", "https://accounts.zoho.in"),
        Self::new(
            "au",
            "https://www.zohoapis.com.au",
            "https://accounts.zoho.com.au",
        ),
        Self::new("jp", "https://www.zohoapis.jp", "https://accounts.zoho.jp"),
        Self::new(
            "ca",
            "https://www.zohoapis.ca",
            "https://accounts.zohocloud.ca",
        ),
        Self::new(
            "cn",
            "https://www.zohoapis.com.cn",
            "https://accounts.zoho.com.cn",
        ),
        Self::new("sa", "https://www.zohoapis.sa", "https://accounts.zoho.sa"),
    ];

    const fn new(
        name: &'static str,
        api_origin: &'static str,
        accounts_origin: &'static str,
    ) -> Self {
        Self {
            name,
            api_origin,
            accounts_origin,
        }
    }

    /// The one region a deployment named, or a refusal listing what it could
    /// have named.
    pub fn parse(name: &str) -> Result<Self, OperationError> {
        Self::ALL
            .into_iter()
            .find(|region| region.name == name)
            .ok_or_else(|| {
                OperationError::new(
                    "the Zoho CRM region must be one Zoho publishes a data centre for: us, eu, \
                     in, au, jp, ca, cn, sa",
                )
            })
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn api_origin(&self) -> &'static str {
        self.api_origin
    }

    /// The accounts origin a code or a refresh token is exchanged at.
    pub const fn accounts_origin(&self) -> &'static str {
        self.accounts_origin
    }

    /// Whether a deployment's declared token endpoint belongs to this region.
    ///
    /// Zoho serves one org from one data centre, and a token minted at another
    /// centre's accounts host does not authenticate here. The check is a startup
    /// one: it compares origins, and it never rewrites what the deployment
    /// declared.
    pub fn admits_token_endpoint(&self, endpoint: &str) -> bool {
        endpoint
            .strip_prefix(self.accounts_origin)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

/// One deployment's declaration.
///
/// The region decides the origin, so the declaration is built per deployment
/// exactly as Twilio's and Jira's are — and, unlike a templated host, what a
/// deployment may name is a compiled set rather than a grammar.
pub fn connector(region: Region) -> Result<Connector, OperationError> {
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(region.api_origin())?)
        .credential(CredentialSpec::for_plan(
            AuthPlan::oauth2_authorization_code_scheme(AUTHORIZATION_SCHEME)?,
        ))
        .operations(operations()?)
        .build()
}

/// The declaration a reviewer and the registry read, on Zoho's own default data
/// centre. A deployment is always compiled against its own configured region.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector(Region::ALL[0])
}

/// The ordered error map.
///
/// Zoho publishes `code` as the machine-readable half of every failure, with a
/// SCREAMING_SNAKE value and a published resolution for each, so the code rules
/// come first and the documented status table answers everything else.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/code")
            // "When the sub-concurrency limit for any of the two requests is
            // reached, the system throws the TOO_MANY_REQUESTS error", and "TOO
            // MANY REQUESTS HTTP 429 — Number of API requests for the 24 hour
            // period is exceeded or the concurrency limit of the user for the
            // app is exceeded."
            .on_code("TOO_MANY_REQUESTS", ConnectorErrorClass::Http429)
            // "Client does not have ZohoCRM.modules.{module_name}.UPDATE scope.
            // Create a new client with valid scope." A grant, not a request.
            .on_code("OAUTH_SCOPE_MISMATCH", ConnectorErrorClass::Authentication)
            .on_code("INVALID_TOKEN", ConnectorErrorClass::Authentication)
            // "The user does not have permission to read records."
            .on_code("NO_PERMISSION", ConnectorErrorClass::Authentication)
            // "The 'INVALID_DATA' error is thrown if the field value length is
            // more than the maximum length defined for that field", "required
            // field not found", "The module name given seems to be invalid",
            // "You have specified an invalid HTTP method to access the API URL."
            .on_code("INVALID_DATA", ConnectorErrorClass::Validation)
            .on_code("MANDATORY_NOT_FOUND", ConnectorErrorClass::Validation)
            .on_code("INVALID_MODULE", ConnectorErrorClass::Validation)
            .on_code("INVALID_REQUEST_METHOD", ConnectorErrorClass::Validation)
            .on_code("LIMIT_EXCEEDED", ConnectorErrorClass::Validation)
            // "You have specified a duplicate value for one or more unique
            // fields", and "Sorry, you cannot perform this operation as the
            // record is locked." Neither is fixed by repeating the request.
            .on_code("DUPLICATE_DATA", ConnectorErrorClass::Permanent)
            .on_code("RECORD_LOCKED", ConnectorErrorClass::Permanent)
            .on_code("INTERNAL_ERROR", ConnectorErrorClass::Http5xx)
            // "BAD REQUEST HTTP 400 — The request or the authentication
            // considered is invalid", "REQUEST ENTITY TOO LARGE HTTP 413",
            // "UNSUPPORTED MEDIA TYPE HTTP 415".
            .on_statuses([400, 413, 415], ConnectorErrorClass::Validation)
            // "AUTHORIZATION ERROR HTTP 401 — Invalid API key provided",
            // "FORBIDDEN HTTP 403 — No permission to do the operation."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "NOT FOUND HTTP 404 — Invalid request", "METHOD NOT ALLOWED HTTP
            // 405". Zoho's "Multi-Status HTTP 207" is deliberately absent: a
            // `2xx` is classified by the operation rather than by an error map,
            // and no operation here declares `207` a success, so it reaches this
            // map's declared fallback as the permanent refusal of the one record
            // the request carried.
            .on_statuses([404, 405], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            // "INTERNAL SERVER ERROR HTTP 500 — Generic error that is
            // encountered due to an unexpected server error."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Zoho CRM error map is a valid declaration")
    });
    &MAP
}

/// This connector declares no continuation plan; see the module documentation.
///
/// It is spelled out rather than defaulted so that the decision is visible where
/// the module is wired ([[058-a-declared-walk-is-the-executors-walk]]).
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

/// Whether a Zoho write body reports a per-record failure.
///
/// `Some(true)` and `Some(false)` are the two answers the published contract
/// admits for `data[0].status`; `None` is a body that carries no such entry,
/// which is every read.
fn first_record_succeeded(body: &[u8]) -> Option<bool> {
    let status = serde_json::from_slice::<JsonValue>(body)
        .ok()?
        .pointer("/data/0/status")?
        .as_str()?
        .to_owned();
    Some(status == "success")
}

/// Decode one Zoho response: the status, then the per-record status, then the
/// declared contract.
///
/// The order is the contract. A `2xx` whose one record entry says `"error"` can
/// never reach [`Operation::decode_response`], so there is no path by which a
/// rejected record is reported as an activity success.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    match first_record_succeeded(body) {
        Some(false) => Err(record_failure(headers, body)),
        _ => operation.decode_response(status, body),
    }
}

/// Classify a per-record failure through the same ordered map a status-level one
/// goes through, by lifting the record's own `code` to where the map reads one.
fn record_failure(headers: &HeaderMap, body: &[u8]) -> ConnectorFailure {
    let lifted = serde_json::from_slice::<JsonValue>(body)
        .ok()
        .and_then(|document| document.pointer("/data/0").cloned())
        .and_then(|record| serde_json::to_vec(&record).ok());
    match lifted {
        // The record entry has the same `{code, message, status, details}` shape
        // the status-level error body has, so the module's own map classifies it
        // without a second set of rules.
        Some(record) => error_map().classify(400, headers, &record),
        None => {
            ConnectorFailure::invariant("connector provider answered outside its declared contract")
        }
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The path prefix every resource here is served from.
fn crm(path: &str) -> String {
    format!("/crm/{API_VERSION}{path}")
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur in any of Zoho CRM's published v8 pages \
                      for authentication, scopes, multi-datacentre routing, get/insert/update/ \
                      upsert/delete records, notes, search, API limits, or status codes: no \
                      request header, no body property, and no response field carries a \
                      client-supplied request identifier or a deduplication behaviour";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The two fields every write's per-record result publishes.
fn write_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer(
            "id",
            "/data/0/details/id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("code", "/data/0/code", ValueScalar::String, Required::No)
        .output_pointer(
            "status",
            "/data/0/status",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
///
/// Zoho's record API is module-generic — one path shape serves Leads, Contacts,
/// Accounts, and Deals — so the CRM module name is a path parameter and one
/// declaration serves them all. It binds through the SDK's path renderer, which
/// percent-encodes each segment, so a module name can never leave its own.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "`fields` — string, mandatory when fetching all records."
    let record_list = common(Operation::get("record.list", &crm("/{module_api_name}")))
        .path_param("module_api_name", ValueScalar::String)
        .query_input("fields", "fields")
        // "If your requirement is to fetch under 2000 records, use the "page" and
        // "per_page" parameters (page=1 to 10, per_page=200)." The page is the
        // caller's; see the module documentation for why this connector declares no
        // walk and does not bind the `page_token` regime.
        .query_input("page", "page")
        .query_static("per_page", PAGE_SIZE)
        // "No Content HTTP 204 — There is no content available for the request",
        // which is Zoho's answer to a module with nothing in it.
        .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
        .no_content_statuses([StatusCode::NO_CONTENT])
        .output_pointer("data", "/data", ValueScalar::Json, Required::No)
        .output_pointer(
            "next_page_token",
            "/info/next_page_token",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "more_records",
            "/info/more_records",
            ValueScalar::Boolean,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let record_get = common(Operation::get(
        "record.get",
        &crm("/{module_api_name}/{record_id}"),
    ))
    .path_param("module_api_name", ValueScalar::String)
    .path_param("record_id", ValueScalar::String)
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .output_pointer("data", "/data", ValueScalar::Json, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "`GET /{module_api_name}/search?criteria={{criteria_here}}`", with `email`,
    // `phone`, and `word` as the other published selectors. "A single API call
    // can fetch a maximum of 200 records."
    let record_search = common(Operation::get(
        "record.search",
        &crm("/{module_api_name}/search"),
    ))
    .path_param("module_api_name", ValueScalar::String)
    // Zoho publishes four alternative selectors — `criteria`, `email`, `phone`,
    // and `word` — and every declared query slot is a value a caller must send,
    // so this declaration binds the general one. `criteria` expresses the other
    // three: "(Email:equals:value)".
    .query_input("criteria", "criteria")
    .query_input("page", "page")
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .output_pointer("data", "/data", ValueScalar::Json, Required::No)
    .output_pointer(
        "more_records",
        "/info/more_records",
        ValueScalar::Boolean,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "You can insert up to 100 records in a single API call." This declaration
    // sends the caller's own `data` array and nothing else.
    let record_create = write_output(
        common(Operation::post("record.create", &crm("/{module_api_name}")))
            .path_param("module_api_name", ValueScalar::String)
            .body(JsonTemplate::object([
                ("data", JsonTemplate::input("data")),
                ("trigger", JsonTemplate::input("trigger")),
            ]))
            .declared_input("data", ValueScalar::Json, Required::Yes)
            .declared_input("trigger", ValueScalar::Json, Required::Yes)
            // "Created HTTP 201 — Message: record added."
            .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second record with a new id — unless the module has a unique field the payload fills, \
         where Zoho answers `DUPLICATE_DATA` instead and publishes the existing record's id in the \
         error's `details`. Neither outcome is the first send's",
    )?)
    .build()?;

    let record_update = write_output(
        common(Operation::put(
            "record.update",
            &crm("/{module_api_name}/{record_id}"),
        ))
        .path_param("module_api_name", ValueScalar::String)
        .path_param("record_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("data", JsonTemplate::input("data")),
            ("trigger", JsonTemplate::input("trigger")),
        ]))
        .declared_input("data", ValueScalar::Json, Required::Yes)
        .declared_input("trigger", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Zoho publishes no statement that this `PUT` replaces the record, and publishes two \
         features that only make sense if it does not: a `_delete` key \"to delete data from \
         multi-select lookup, multi-user lookup, and image upload fields\", and `$append_values` \
         to control append-versus-replace on multi-selects. That is a partial update rather than \
         the write to a fixed resource identity spec 010 §7's NaturalMethod evidence needs, and \
         Zoho publishes no consequence for a repeat of it",
    )?)
    .build()?;

    let record_upsert = write_output(
        common(Operation::post(
            "record.upsert",
            &crm("/{module_api_name}/upsert"),
        ))
        .path_param("module_api_name", ValueScalar::String)
        .body(JsonTemplate::object([
            ("data", JsonTemplate::input("data")),
            (
                "duplicate_check_fields",
                JsonTemplate::input("duplicate_check_fields"),
            ),
        ]))
        .declared_input("data", ValueScalar::Json, Required::Yes)
        .declared_input("duplicate_check_fields", ValueScalar::Json, Required::Yes)
        // "`action`" is `"insert"` or `"update"`, and `duplicate_field` names
        // the field that matched.
        .declared_output("action", ValueScalar::String, Required::No)
        .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(Effect::inventory_only(
        "Zoho documents this operation as repeat-safe: \"The system checks for duplicate records \
         using the values of the duplicate check fields. If a matching record exists, it gets \
         updated. If no matching record is found, a new record is inserted\", and its response \
         distinguishes the two in an `action` field. It publishes it as a `POST`, and spec 010 §7 \
         admits NaturalMethod for PUT and DELETE only. An operation a provider documents as \
         repeat-safe wants a class that keeps the retry; ADR 063's at-most-once class trades the \
         retry away and is the wrong contract for it",
    )?)
    .build()?;

    // "`POST /Notes`" with "`Parent_Id` — string, mandatory".
    let note_create = write_output(
        common(Operation::post("note.create", &crm("/Notes")))
            .body(JsonTemplate::object([(
                "data",
                JsonTemplate::input("data"),
            )]))
            .declared_input("data", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second note with a new id under the same parent record; Zoho publishes no uniqueness \
         constraint on a note's content or title",
    )?)
    .build()?;

    let note_list = common(Operation::get(
        "note.list",
        &crm("/{module_api_name}/{record_id}/Notes"),
    ))
    .path_param("module_api_name", ValueScalar::String)
    .path_param("record_id", ValueScalar::String)
    .query_input("page", "page")
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .output_pointer("data", "/data", ValueScalar::Json, Required::No)
    .output_pointer(
        "more_records",
        "/info/more_records",
        ValueScalar::Boolean,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        record_get,
        record_list,
        record_search,
        record_create,
        record_update,
        record_upsert,
        note_create,
        note_list,
    ])
}
