//! Harvest's API v2 — time entries, projects, and clients.
//!
//! Ground truth is Harvest's own published API v2 reference, read on
//! 2026-08-10:
//!
//! * <https://help.getharvest.com/api-v2/authentication-api/authentication/authentication/>
//!   — "The API can be accessed by creating a Personal Access Token from the
//!   **Developers** section of Harvest ID", "Each request will require your
//!   account ID as well, since you can use this Personal Access Token to access
//!   any of your Harvest or Forecast accounts", and the worked request:
//!   `-H "Authorization: Bearer $ACCESS_TOKEN"`,
//!   `-H "Harvest-Account-Id: $ACCOUNT_ID"`,
//!   `-H "User-Agent: MyApp (yourname@example.com)"`.
//! * <https://help.getharvest.com/api-v2/introduction/overview/general/> — the
//!   base URL `https://api.harvestapp.com/v2/`; "We also require that each
//!   request include a `User-Agent` header with both: The name of your
//!   application and A link to your application or email address"; "If you don't
//!   include a `User-Agent` header, you'll get a `400 Bad Request` response";
//!   "When submitting request parameters as JSON, you must pass
//!   `application/json` in the `Content-Type` header"; the status table
//!   ("200 Your request was successful", "201 A new object has been created",
//!   "403 Found but you lack authorization", "404 The object you requested can't
//!   be found", "422 Errors processing your request", "429 Your request has been
//!   throttled", "500 Server error"); and "The rate limit for general API
//!   requests is 100 requests per 15 seconds. … When the rate limit is exceeded
//!   Harvest will send an HTTP 429 status code. The number of seconds until the
//!   throttle is lifted is sent via the `Retry-After` HTTP header."
//! * <https://help.getharvest.com/api-v2/introduction/overview/pagination/> —
//!   "The default and maximum `per_page` value for API requests is `2000`"; the
//!   response envelope's `per_page`, `total_pages`, `total_entries`,
//!   `next_page`, `previous_page`, `page`, and `links` (`first`, `next`,
//!   `previous`, `last`); "The `page` and `cursor` parameters are mutually
//!   exclusive and should not be used together"; and the instruction to "always
//!   use the URLs in the `links` section" rather than building them.
//! * <https://help.getharvest.com/api-v2/timesheets-api/timesheets/time-entries/>
//!   — `GET /v2/time_entries` (`200`, envelope key `time_entries`),
//!   `GET /v2/time_entries/{TIME_ENTRY_ID}` (`200`),
//!   `POST /v2/time_entries` (`201`, required `project_id`, `task_id`,
//!   `spent_date`), `PATCH /v2/time_entries/{TIME_ENTRY_ID}` (`200`).
//! * <https://help.getharvest.com/api-v2/projects-api/projects/projects/> —
//!   `GET /v2/projects` (`200`, envelope key `projects`),
//!   `GET /v2/projects/{PROJECT_ID}` (`200`).
//! * <https://help.getharvest.com/api-v2/clients-api/clients/clients/> —
//!   `GET /v2/clients` (`200`, envelope key `clients`),
//!   `GET /v2/clients/{CLIENT_ID}` (`200`).
//!
//! # Two credential-adjacent values, and only one of them is a secret
//!
//! Harvest sends **two** values on every request: a Personal Access Token in
//! `Authorization: Bearer`, and an account identifier in `Harvest-Account-Id`.
//! They are not the same kind of value and this connector does not treat them as
//! one.
//!
//! * The **token** is the secret. It is resolved from a `SecretRef`, applied
//!   only by [`AuthPlan::bearer`], marked sensitive on the header, and it
//!   appears in no `Debug`, no diagnostic, no error, and no configuration
//!   fingerprint — a fingerprint carries the *name* of the environment variable
//!   and never its value.
//! * The **account id** is not a secret. Harvest prints it in its own Developers
//!   page beside the token it issues, and it selects which of the deployment's
//!   accounts a request reaches; publishing it discloses nothing an operator
//!   would not put in `connectors.yaml`. So it is `config.settings` material,
//!   declared `FieldClassification::NonSecret`, and it *does* enter the
//!   configuration fingerprint, because changing it changes what a pinned
//!   operation reaches. This is Twilio's Account SID one provider over
//!   (`knowledgebase/declarative-saas/decisions/048-*`), applied to a header
//!   rather than to an HTTP Basic username.
//!
//! Because the account id is a **static header on every operation** rather than
//! a value some render step fills, the declaration is built per deployment by
//! [`connector`], exactly as Basecamp's path prefix and Twilio's Basic username
//! are (ADR 066). A `header_input` binding would have made the account a slot a
//! Process could fill, which is a Process choosing a tenant.
//!
//! `User-Agent` is the same shape of value for Basecamp's reason: Harvest
//! answers a request without one with a `400`, and demands that it name the
//! application and a way to reach its author. It identifies this deployment to
//! the provider, so it is deploy-time configuration and never an input.
//!
//! # Pagination
//!
//! Harvest publishes its continuation as `links.next`, a URI inside the response
//! body, and asks callers to spend it rather than to build one: "always use the
//! URLs in the `links` section". The walk ends on an absence — `links.next` is
//! `null` on the last page — which is what the SDK's closed plan set reads
//! (ADR 065). `TokenInBody` would send that URI back as a query *value*, which
//! Harvest does not accept, and `page` is the regime Harvest publishes as
//! mutually exclusive with the cursor its own links carry, so the plan that fits
//! is the body-carried next URI, resolved and origin-checked exactly as a `Link`
//! continuation is.
//!
//! `per_page` is fixed by the declaration at [`PAGE_SIZE`] rather than left at
//! Harvest's maximum of 2000, so a caller cannot ask for an unbounded page.
//!
//! # `hours` is a JSON number
//!
//! Harvest publishes a time entry's `hours` as a JSON number (`2.0`), so the
//! declared pointer is `ValueScalar::Json` and not `Decimal`: `Decimal` in this
//! SDK reads a JSON *string*, and a connector that retyped the field would be
//! converting a quantity the provider published
//! (`knowledgebase/declarative-saas/decisions/071-*`).
//!
//! # Effect classification
//!
//! Harvest publishes no idempotency mechanism anywhere in its v2 reference: the
//! authentication, overview, and pagination guides do not mention one, and the
//! Time Entries page enumerates the complete parameter set of
//! `POST /v2/time_entries` — `project_id`, `task_id`, `spent_date`, `user_id`,
//! `hours`, `started_time`, `ended_time`, `notes`, `external_reference` — with
//! no client-supplied request identifier among them. `external_reference` is not
//! one either: Harvest documents it as a link to an object in another system
//! (`id`, `group_id`, `permalink`, `service`, `service_icon_url`) and publishes
//! no uniqueness, no rejection, and no replay behaviour for it, so it is a
//! declared input and reaches no effect class (the `pagerduty.incident_key`
//! treatment, ADR 080).
//!
//! * `time_entry.create` is therefore `AtMostOnce` (ADR 063): a repeat is a
//!   second time entry, with a new id, and a second block of billable hours on
//!   the same project.
//! * `time_entry.update` stays `InventoryOnly`. It is a `PATCH` — a method the
//!   gate does not admit for `NaturalMethod`, because HTTP defines repeat-safety
//!   for `PUT` and `DELETE` — and Harvest publishes nothing about what a second
//!   send does, so there is no consequence to record and ADR 063's evidence bar
//!   is not met either. It joins the population `INVENTORY.md` records as
//!   partial updates with no published repeat consequence.
//! * Everything else here is a `GET`.

use std::sync::LazyLock;
use std::time::Duration;

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
pub const NAME: &str = "harvest";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the account every request names.
///
/// It is **not** a secret: Harvest prints it beside the Personal Access Token it
/// issues, and it selects an account rather than authorizing one.
pub const ACCOUNT_ID: &str = "account_id";

/// The deploy-time configuration key carrying the `User-Agent` Harvest demands.
pub const USER_AGENT: &str = "user_agent";

/// "https://api.harvestapp.com/v2/".
const ORIGIN: &str = "https://api.harvestapp.com";

/// `-H "Harvest-Account-Id: $ACCOUNT_ID"`.
const ACCOUNT_HEADER: &str = "Harvest-Account-Id";

/// "The default and maximum `per_page` value for API requests is `2000`." The
/// declaration fixes a much smaller page, so a caller cannot ask for an
/// unbounded one and a walk's per-page budget means something.
const PAGE_SIZE: u32 = 100;

/// Harvest publishes no per-operation deadline, so this is the module's own
/// bound on one attempt.
const OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// One deployment's declaration.
///
/// `account_id` becomes a static `Harvest-Account-Id` header on every operation
/// and `user_agent` becomes a static `User-Agent`. Both are checked here, where
/// the declaration is built, so a mistyped value is a startup refusal rather
/// than a `400` or a request against another account on the first activity
/// attempt.
pub fn connector(account_id: &str, user_agent: &str) -> Result<Connector, OperationError> {
    validate_account_id(account_id)?;
    validate_user_agent(user_agent)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN)?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::bearer())
                // Not a secret: Harvest prints the account id in its own
                // Developers page. It is still deploy-time material, which is
                // why it is declared here rather than left implicit.
                .with_field(ACCOUNT_ID, FieldClassification::NonSecret),
        )
        .operations(operations(account_id, user_agent)?)
        .build()
}

/// The declaration a reviewer and the registry read, with placeholder values no
/// deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("1234567", "Donat (deployment.configured@example.invalid)")
}

/// Harvest's own grammar for the value: the Account ID its Developers page
/// prints beside a Personal Access Token is a numeric string, and it travels in
/// a header.
///
/// The check is the narrow one — ASCII digits, non-empty, bounded — because a
/// header value a deployment types is the one value here that could carry
/// something other than an account.
pub fn validate_account_id(account_id: &str) -> Result<(), OperationError> {
    if account_id.is_empty()
        || account_id.len() > 20
        || !account_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err(OperationError::new(
            "the Harvest account id must be the numeric account identifier Harvest issues beside \
             a Personal Access Token",
        ));
    }
    Ok(())
}

/// Harvest's own rule for the header: "We also require that each request include
/// a `User-Agent` header with both: The name of your application and A link to
/// your application or email address", with the published example
/// `User-Agent: MyApp (yourname@example.com)`.
///
/// What is checked is what a machine can check: a non-empty name, a bracketed
/// contact, printable ASCII, and a bounded length. Whether the contact is real
/// is Harvest's business with the deployment.
pub fn validate_user_agent(user_agent: &str) -> Result<(), OperationError> {
    let Some((name, contact)) = user_agent.split_once('(') else {
        return Err(OperationError::new(
            "the Harvest user agent must name the application and a contact, as \
             `MyApp (you@example.com)`",
        ));
    };
    let contact = contact.strip_suffix(')').unwrap_or_default();
    if name.trim().is_empty()
        || contact.trim().is_empty()
        || user_agent.len() > 200
        || !user_agent
            .chars()
            .all(|character| character.is_ascii_graphic() || character == ' ')
    {
        return Err(OperationError::new(
            "the Harvest user agent must name the application and a contact, as \
             `MyApp (you@example.com)`",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Harvest publishes a status table and no machine-readable error code — its
/// failures carry a human `message` — so this map reads the status and no body
/// pointer.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "If you don't include a `User-Agent` header, you'll get a `400 Bad
            // Request` response", and a malformed body is the same status.
            .on_status(400, ConnectorErrorClass::Validation)
            // The table's "403 Found but you lack authorization", beside HTTP's
            // own unauthenticated status, which a rejected Personal Access Token
            // or a `Harvest-Account-Id` this token cannot reach answers with.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 The object you requested can't be found."
            .on_status(404, ConnectorErrorClass::Permanent)
            // "422 Errors processing your request", which is also the answer to
            // an out-of-range `per_page`.
            .on_status(422, ConnectorErrorClass::Validation)
            // "429 Your request has been throttled."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Server error", with the gateway statuses its edge answers.
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Harvest error map is a valid declaration")
    });
    &MAP
}

/// Decode one Harvest response: the declared success statuses, then the declared
/// contract.
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

/// The continuation plan of each walked collection.
///
/// Harvest's own instruction is to spend the URL it publishes rather than to
/// build one, and the walk ends where that URL is absent: `links.next` is `null`
/// on the last page.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static TIME_ENTRIES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/time_entries", "/links/next")
            .expect("the Harvest time entry continuation plan is valid")
    });
    static PROJECTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/projects", "/links/next")
            .expect("the Harvest project continuation plan is valid")
    });
    static CLIENTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/clients", "/links/next")
            .expect("the Harvest client continuation plan is valid")
    });
    match operation_id {
        "time_entry.list" => Some(&TIME_ENTRIES),
        "project.list" => Some(&PROJECTS),
        "client.list" => Some(&CLIENTS),
        _ => None,
    }
}

/// What every operation of this deployment carries: its own deadline, the
/// account this instance was configured for, and the identity Harvest demands.
fn common(builder: OperationBuilder, account_id: &str, user_agent: &str) -> OperationBuilder {
    builder
        .version(VERSION)
        .deadline(OPERATION_DEADLINE)
        // "Each request will require your account ID as well." It is the
        // deployment's, in a header, and no operation input can reach it.
        .static_header(ACCOUNT_HEADER, account_id)
        // "If you don't include a `User-Agent` header, you'll get a `400 Bad
        // Request` response."
        .static_header("User-Agent", user_agent)
        .static_header("Accept", "application/json")
}

/// The one reason this connector's keyless write carries.
const NO_KEY: &str = "Harvest's published API v2 reference enumerates the complete parameter set of \
                      `POST /v2/time_entries` — `project_id`, `task_id`, `spent_date`, `user_id`, \
                      `hours`, `started_time`, `ended_time`, `notes`, `external_reference` — and \
                      none of them is a client-supplied request identifier. Neither the \
                      authentication guide, the overview (which is where Harvest publishes its \
                      status table, its rate limit, and its required headers), nor the pagination \
                      guide documents an idempotency header, parameter, or replay behaviour, and \
                      `external_reference` is published as a link to an object in another system \
                      with no uniqueness, rejection, or replay statement attached to it";

/// The published time-entry attributes a Process reads.
fn time_entry_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "spent_date",
            "/spent_date",
            ValueScalar::String,
            Required::No,
        )
        // Harvest publishes `hours` as a JSON number, so the contract carries
        // the provider's own shape (ADR 071).
        .output_pointer("hours", "/hours", ValueScalar::Json, Required::No)
        .output_pointer(
            "rounded_hours",
            "/rounded_hours",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer("notes", "/notes", ValueScalar::String, Required::No)
        .output_pointer(
            "is_running",
            "/is_running",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer(
            "is_billed",
            "/is_billed",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer("billable", "/billable", ValueScalar::Boolean, Required::No)
        .output_pointer("user", "/user", ValueScalar::Json, Required::No)
        .output_pointer("client", "/client", ValueScalar::Json, Required::No)
        .output_pointer("project", "/project", ValueScalar::Json, Required::No)
        .output_pointer("task", "/task", ValueScalar::Json, Required::No)
        .output_pointer(
            "external_reference",
            "/external_reference",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "updated_at",
            "/updated_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The published envelope of a paginated collection, carried as data.
fn collection_output(builder: OperationBuilder, items: &str, pointer: &str) -> OperationBuilder {
    builder
        .output_pointer(items, pointer, ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_entries",
            "/total_entries",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "total_pages",
            "/total_pages",
            ValueScalar::Int64,
            Required::No,
        )
        // The continuation Harvest publishes. Nothing here turns it into a
        // request: the declared plan does that, on this origin.
        .output_pointer("links", "/links", ValueScalar::Json, Required::No)
        .success_statuses([StatusCode::OK])
        .effect(Effect::read_only())
}

/// Every operation this connector publishes, under one deployment's account.
fn operations(account_id: &str, user_agent: &str) -> Result<Vec<Operation>, OperationError> {
    let time_entries = "/v2/time_entries";
    let one_time_entry = "/v2/time_entries/{time_entry_id}";
    let projects = "/v2/projects";
    let one_project = "/v2/projects/{project_id}";
    let clients = "/v2/clients";
    let one_client = "/v2/clients/{client_id}";

    // "Retrieve a time entry — `GET /v2/time_entries/{TIME_ENTRY_ID}`."
    let time_entry_get = time_entry_output(
        common(
            Operation::get("time_entry.get", one_time_entry),
            account_id,
            user_agent,
        )
        .path_param("time_entry_id", ValueScalar::Int64),
    )
    .success_statuses([StatusCode::OK])
    .effect(Effect::read_only())
    .build()?;

    // "List all time entries — `GET /v2/time_entries`", with the documented
    // filters a Process actually drives.
    let time_entry_list = collection_output(
        common(
            Operation::get("time_entry.list", time_entries),
            account_id,
            user_agent,
        )
        .query_input("user_id", "user_id")
        .query_input("project_id", "project_id")
        .query_input("client_id", "client_id")
        .query_input("from", "from")
        .query_input("to", "to")
        .query_input("is_billed", "is_billed")
        .query_input("updated_since", "updated_since")
        .query_static("per_page", &PAGE_SIZE.to_string()),
        "time_entries",
        "/time_entries",
    )
    .build()?;

    // "Create a time entry — `POST /v2/time_entries`. … `project_id`, `task_id`
    // and `spent_date` are required."
    let time_entry_create = time_entry_output(
        common(
            Operation::post("time_entry.create", time_entries),
            account_id,
            user_agent,
        )
        .body(JsonTemplate::object([
            ("project_id", JsonTemplate::input("project_id")),
            ("task_id", JsonTemplate::input("task_id")),
            ("spent_date", JsonTemplate::input("spent_date")),
            ("user_id", JsonTemplate::input("user_id")),
            ("hours", JsonTemplate::input("hours")),
            ("notes", JsonTemplate::input("notes")),
            (
                "external_reference",
                JsonTemplate::input("external_reference"),
            ),
        ]))
        .declared_input("project_id", ValueScalar::Int64, Required::Yes)
        .declared_input("task_id", ValueScalar::Int64, Required::Yes)
        .declared_input("spent_date", ValueScalar::String, Required::Yes),
    )
    .success_statuses([StatusCode::CREATED])
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        "a second time entry with a new id on the same project and task, and a second block of \
         billable hours against that project's budget",
    )?))
    .build()?;

    // "Update a time entry — `PATCH /v2/time_entries/{TIME_ENTRY_ID}`. … Any
    // parameters not provided will be left unchanged."
    let time_entry_update = time_entry_output(
        common(
            Operation::patch("time_entry.update", one_time_entry),
            account_id,
            user_agent,
        )
        .path_param("time_entry_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("project_id", JsonTemplate::input("project_id")),
            ("task_id", JsonTemplate::input("task_id")),
            ("spent_date", JsonTemplate::input("spent_date")),
            ("hours", JsonTemplate::input("hours")),
            ("notes", JsonTemplate::input("notes")),
        ])),
    )
    .success_statuses([StatusCode::OK])
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    // "Retrieve a project — `GET /v2/projects/{PROJECT_ID}`."
    let project_get = common(
        Operation::get("project.get", one_project),
        account_id,
        user_agent,
    )
    .path_param("project_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("code", "/code", ValueScalar::String, Required::No)
    .output_pointer(
        "is_active",
        "/is_active",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer(
        "is_billable",
        "/is_billable",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer("bill_by", "/bill_by", ValueScalar::String, Required::No)
    .output_pointer("budget_by", "/budget_by", ValueScalar::String, Required::No)
    .output_pointer("budget", "/budget", ValueScalar::Json, Required::No)
    .output_pointer("client", "/client", ValueScalar::Json, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "List all projects — `GET /v2/projects`."
    let project_list = collection_output(
        common(
            Operation::get("project.list", projects),
            account_id,
            user_agent,
        )
        .query_input("is_active", "is_active")
        .query_input("client_id", "client_id")
        .query_input("updated_since", "updated_since")
        .query_static("per_page", &PAGE_SIZE.to_string()),
        "projects",
        "/projects",
    )
    .build()?;

    // "Retrieve a client — `GET /v2/clients/{CLIENT_ID}`."
    let client_get = common(
        Operation::get("client.get", one_client),
        account_id,
        user_agent,
    )
    .path_param("client_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer(
        "is_active",
        "/is_active",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer("address", "/address", ValueScalar::String, Required::No)
    .output_pointer("currency", "/currency", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "List all clients — `GET /v2/clients`."
    let client_list = collection_output(
        common(
            Operation::get("client.list", clients),
            account_id,
            user_agent,
        )
        .query_input("is_active", "is_active")
        .query_input("updated_since", "updated_since")
        .query_static("per_page", &PAGE_SIZE.to_string()),
        "clients",
        "/clients",
    )
    .build()?;

    Ok(vec![
        time_entry_get,
        time_entry_list,
        time_entry_create,
        time_entry_update,
        project_get,
        project_list,
        client_get,
        client_list,
    ])
}

/// The reason `time_entry.update` carries: a partial update over a method the
/// gate does not admit, whose repeat the provider never described.
const PARTIAL_UPDATE: &str = "Harvest publishes this as a `PATCH` whose unset parameters are left \
     unchanged, and publishes nothing at all about what a second identical send does. Spec 010 §7 \
     admits NaturalMethod for PUT and DELETE only, because HTTP defines repeat-safety for those \
     two, and ADR 063's at-most-once class is admitted on a recorded absence *and* a recorded \
     consequence — there is no consequence to record for a partial update that writes the same \
     values a second time, so the operation stays declared, typed, tested, and unreachable";
