//! Todoist's API v1 — the batch's one provider that publishes an idempotency
//! mechanism, and the one where it does not reach these endpoints.
//!
//! Ground truth is Todoist's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://developer.todoist.com/api/v1/> — "In order to make authorized
//!   calls to the Sync API, your application must provide an authorization
//!   header with the appropriate `Bearer $token`", with the worked example
//!   `curl https://api.todoist.com/api/v1/sync -H "Authorization: Bearer …"`.
//!   Its own OpenAPI names the origin once more: `servers: - url:
//!   https://api.todoist.com/`.
//! * The *Pagination* guide — "Paginated endpoints use **cursor-based
//!   pagination**", "`results`: An array containing the requested objects",
//!   "`next_cursor`: A string token for fetching the next page, or `null` if
//!   there are no more results", "When `next_cursor` is `null`, you've reached
//!   the end of the results", and the `limit` parameter with "**Default**: 50",
//!   "**Maximum**: 200". Also: "Cursors are user-specific and
//!   parameter-dependent … Do not attempt to decode, parse, or modify cursors —
//!   pass them as-is from the previous response."
//! * The *Request limits* guide — "For each user, you can make a maximum of 1000
//!   partial sync requests within a 15 minute period", the 1 MiB request body
//!   limit, and the "Standard Request | 15 seconds" processing timeout.
//! * The published OpenAPI (<https://developer.todoist.com/openapi.json>) for
//!   every path, method, parameter, and request body named below.
//!
//! # The idempotency mechanism, and where it is
//!
//! Todoist publishes command idempotency, and publishes exactly where it
//! applies: on the **Sync** endpoint's command envelope. "Clients should
//! generate a unique string ID for each command and specify it in the `uuid`
//! field. The Command UUID will be used for two purposes: … 2. Command
//! idempotency: Todoist will not execute a command that has same UUID as a
//! previously executed command. This will allow clients to safely retry each
//! command without accidentally performing the action twice."
//!
//! That is not this connector's surface, and it would not qualify if it were.
//! The string `idempot` occurs **exactly once** in Todoist's whole published
//! OpenAPI description — in the sentence above — and nowhere in the contract of
//! `POST /api/v1/tasks`, `POST /api/v1/comments`, or any other endpoint declared
//! here. And Todoist publishes no retention for the `uuid`: no window, no
//! expiry, and no statement of the scope a key is unique within.
//! `ProviderIdempotent::ExplicitKey` is admitted on a binding **plus** a
//! documented minimum retention with a clock safety margin strictly under it
//! (ADR 042), and a mechanism with no published window is the near-miss this
//! programme has already recorded once for Microsoft's `transactionId`. So the
//! writes here are `AtMostOnce` on a *documented exclusion* rather than on an
//! absence, and `providers/INVENTORY.md` records the mechanism rather than
//! dropping it.
//!
//! # Closing a task is not a partial update
//!
//! `task.close` is the one write in this module whose repeat consequence Todoist
//! publishes outright: "Regular tasks are marked complete and moved to history,
//! along with their subtasks. Tasks with recurring due dates will be scheduled
//! to their next occurrence." A second close of a *recurring* task advances the
//! recurrence a second time, which is a different state from the first send's —
//! exactly the consequence ADR 063 asks an operator to accept.
//!
//! # A declared query filter is a filter every call carries
//!
//! Todoist publishes several optional narrowing parameters on each collection,
//! and the SDK renders every declared query input on every request — there is no
//! "omit when absent" leaf, deliberately, because a request whose shape depends
//! on which slots a caller filled is a request nobody can review. So each
//! collection declares the one filter a Process actually drives it by — a
//! project for tasks, a filter query for the search, a task for comments — and
//! the rest are left out rather than forced onto every call. `comment.list`
//! declares `task_id` alone for a second reason: Todoist publishes "Exactly one
//! of `task_id` or `project_id` arguments is required. Providing neither or both
//! will return an error", and a declaration that always sent both would always
//! be that error.
//!
//! # Effect classification
//!
//! `task.update` is `InventoryOnly`: Todoist publishes it as "Updates an
//! existing task" over a `POST`, so a repeat sets the same fields to the same
//! values and no method admits `NaturalMethod`. `task.delete` is `InventoryOnly`
//! for the sharper reason that Todoist *does* publish what the second send does
//! and it is a failure rather than the same one absent task: "Returns
//! `NOT_FOUND` when the task does not exist and `FORBIDDEN` when the
//! authenticated user cannot modify the task."

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
pub const NAME: &str = "todoist";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "`servers: - url: https://api.todoist.com/`", and "we will only focus on
/// `api.todoist.com` as the subdomain".
const ORIGIN: &str = "https://api.todoist.com";

/// "**Maximum**: 200. If you specify a limit greater than 200, the API will
/// return a validation error."
const PAGE_SIZE: u32 = 200;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Todoist's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Todoist declarations are valid"))
            .build()
            .expect("the Todoist declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Todoist's own OpenAPI declares one response shape for a rejected request —
/// FastAPI's `HTTPValidationError`, whose `detail[].type` names a *schema*
/// violation rather than an API error class — so this map reads the status
/// table Todoist publishes beside it and no body pointer at all.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — The request was incorrect."
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "401 Unauthorized — Authentication is required, and has failed or
            // has not yet been provided", "403 Forbidden — The request was valid
            // and authenticated, but the user does not have permission."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found — The request was valid, but the requested resource
            // does not exist."
            .on_status(404, ConnectorErrorClass::Permanent)
            // "429 Too Many Requests — The user has sent too many requests in a
            // given amount of time."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error", "503 Service Unavailable — The server
            // is currently unable to handle the request."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Todoist error map is a valid declaration")
    });
    &MAP
}

/// Decode one Todoist response: the declared success statuses, then the declared
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

/// The continuation plan of each paginated collection.
///
/// One plan serves all four, because Todoist publishes one: `results` in,
/// `next_cursor` out, `cursor` back as a query parameter, and the walk ends
/// where "`next_cursor` is `null`".
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor("/results", "cursor", "/next_cursor", "limit", PAGE_SIZE)
            .expect("the Todoist cursor plan is valid")
    });
    match operation_id {
        "task.list" | "task.search" | "project.list" | "comment.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Accept", "application/json")
        .success_statuses([StatusCode::OK])
}

/// The reason every write in this module carries.
const NO_KEY: &str = "Todoist publishes a client-supplied idempotency mechanism and publishes \
                      exactly where it applies — the Sync endpoint's command envelope, \"Command \
                      idempotency: Todoist will not execute a command that has same UUID as a \
                      previously executed command\" — and the string `idempot` occurs exactly once \
                      in its whole published OpenAPI description, in that sentence. No REST \
                      endpoint declared here takes a `uuid`, an idempotency header, or any other \
                      client-supplied request identifier, and Todoist publishes no retention \
                      window, expiry, or uniqueness scope for the Sync `uuid` either, which is \
                      what ADR 042 admits ExplicitKey on";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The published task attributes a Process reads.
///
/// `url` is deliberately absent: Todoist removed it in this version — "The
/// previous task object included a `url` property … This has been removed" — so
/// a declaration that published it would publish a field the provider does not
/// send.
fn task_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("content", "/content", ValueScalar::String, Required::No)
        .output_pointer(
            "description",
            "/description",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "project_id",
            "/project_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "section_id",
            "/section_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("priority", "/priority", ValueScalar::Int64, Required::No)
        .output_pointer("labels", "/labels", ValueScalar::Json, Required::No)
        .output_pointer("due", "/due", ValueScalar::Json, Required::No)
        .output_pointer("checked", "/checked", ValueScalar::Boolean, Required::No)
        .output_pointer("added_at", "/added_at", ValueScalar::String, Required::No)
        .output_pointer(
            "updated_at",
            "/updated_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The two fields every paginated collection publishes.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_cursor",
            "/next_cursor",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Returns a single active (non-completed) task by ID."
    let task_get = task_output(
        common(Operation::get("task.get", "/api/v1/tasks/{task_id}"))
            .path_param("task_id", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get all active tasks for the user. All provided parameters are used to
    // narrow down the list of tasks."
    let task_list = collection(
        common(Operation::get("task.list", "/api/v1/tasks"))
            .query_input("project_id", "project_id"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get all tasks matching the filter." `query` is Todoist's own filter
    // language and is required.
    let task_search = collection(
        common(Operation::get("task.search", "/api/v1/tasks/filter")).query_input("query", "query"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Create a new task." `content` is the one required body field.
    let task_create = task_output(
        common(Operation::post("task.create", "/api/v1/tasks"))
            .body(JsonTemplate::object([
                ("content", JsonTemplate::input("content")),
                ("description", JsonTemplate::input("description")),
                ("project_id", JsonTemplate::input("project_id")),
                ("section_id", JsonTemplate::input("section_id")),
                ("labels", JsonTemplate::input("labels")),
                ("priority", JsonTemplate::input("priority")),
                ("due_string", JsonTemplate::input("due_string")),
            ]))
            .declared_input("content", ValueScalar::String, Required::Yes)
            .declared_input("labels", ValueScalar::Json, Required::Yes),
    )
    .effect(at_most_once(
        "a second task with a new id in the same project, carrying the same content and due date; \
         Todoist publishes no duplicate detection for tasks and no unique field a second create \
         could collide with",
    )?)
    .build()?;

    let task_update = task_output(
        common(Operation::post("task.update", "/api/v1/tasks/{task_id}"))
            .path_param("task_id", ValueScalar::String)
            .body(JsonTemplate::object([
                ("content", JsonTemplate::input("content")),
                ("description", JsonTemplate::input("description")),
                ("labels", JsonTemplate::input("labels")),
                ("priority", JsonTemplate::input("priority")),
                ("due_string", JsonTemplate::input("due_string")),
            ]))
            .declared_input("labels", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    // "Closes a task. … Regular tasks are marked complete and moved to history,
    // along with their subtasks. Tasks with recurring due dates will be
    // scheduled to their next occurrence."
    let task_close = common(Operation::post(
        "task.close",
        "/api/v1/tasks/{task_id}/close",
    ))
    .path_param("task_id", ValueScalar::String)
    .declared_output("closed", ValueScalar::Json, Required::Yes)
    .effect(at_most_once(
        "for a regular task, nothing beyond the first close — but for a task with a recurring due \
         date, Todoist publishes that a close \"will be scheduled to their next occurrence\", so a \
         second close advances the recurrence a second time and skips an occurrence the Process \
         never completed",
    )?)
    .build()?;

    let task_delete = common(Operation::delete("task.delete", "/api/v1/tasks/{task_id}"))
        .path_param("task_id", ValueScalar::String)
        .declared_output("deleted", ValueScalar::Json, Required::Yes)
        .effect(Effect::inventory_only(PUBLISHED_SECOND_DELETE)?)
        .build()?;

    let project_get = common(Operation::get(
        "project.get",
        "/api/v1/projects/{project_id}",
    ))
    .path_param("project_id", ValueScalar::String)
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("color", "/color", ValueScalar::String, Required::No)
    .output_pointer(
        "is_archived",
        "/is_archived",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer(
        "is_favorite",
        "/is_favorite",
        ValueScalar::Boolean,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get all active user projects, optionally filtered by folder or
    // workspace."
    let project_list = collection(common(Operation::get("project.list", "/api/v1/projects")))
        .effect(Effect::read_only())
        .build()?;

    // "Get all comments for a given task or project. Exactly one of `task_id` or
    // `project_id` arguments is required. Providing neither or both will return
    // an error."
    let comment_list = collection(
        common(Operation::get("comment.list", "/api/v1/comments"))
            .query_input("task_id", "task_id"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Creates a new comment on a project or task and returns it."
    let comment_create = common(Operation::post("comment.create", "/api/v1/comments"))
        .body(JsonTemplate::object([
            ("content", JsonTemplate::input("content")),
            ("task_id", JsonTemplate::input("task_id")),
            ("project_id", JsonTemplate::input("project_id")),
        ]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("content", "/content", ValueScalar::String, Required::No)
        .output_pointer("posted_at", "/posted_at", ValueScalar::String, Required::No)
        .effect(at_most_once(
            "a second comment on the same task, with a new id, and a second notification to every \
             collaborator Todoist publishes `uids_to_notify` for",
        )?)
        .build()?;

    Ok(vec![
        task_get,
        task_list,
        task_search,
        task_create,
        task_update,
        task_close,
        task_delete,
        project_get,
        project_list,
        comment_list,
        comment_create,
    ])
}

/// The reason the update carries.
const PARTIAL_UPDATE: &str = "Todoist publishes its task update as \"Updates an existing task\" \
                              over a `POST` with every body field optional, which is a partial \
                              update: a second identical send leaves exactly the state the first \
                              one did. There is no consequence ADR 063 admits a class on, and spec \
                              010 §7 admits NaturalMethod for PUT and DELETE only";

/// The reason the delete carries.
const PUBLISHED_SECOND_DELETE: &str = "Todoist publishes what a second delete does, and it is a failure rather than the same one \
     absent task: \"Delete a task and all of its subtasks. Returns `NOT_FOUND` when the task does \
     not exist and `FORBIDDEN` when the authenticated user cannot modify the task.\" That is the \
     opposite of the repeat statement spec 010 §7's NaturalMethod is admitted on, and a refusal is \
     not a consequence ADR 063 admits a send on either";
