//! ClickUp's API v2 — the batch's space/folder/list/task hierarchy.
//!
//! Ground truth is ClickUp's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://developer.clickup.com/docs/authentication> — a personal token is
//!   sent as `"Authorization: {personal_token}"`, with **no scheme in front of
//!   it**, beside the OAuth form `"Authorization: Bearer {access_token}"`.
//! * <https://developer.clickup.com/docs/rate-limits> — "100 requests per minute
//!   per token" on the lower plans up to "10,000 requests per minute per token"
//!   on Enterprise, an `HTTP 429` when the limit is passed, and the
//!   `X-RateLimit-Limit`, `X-RateLimit-Remaining`, and `X-RateLimit-Reset`
//!   headers. ClickUp publishes **no** `Retry-After`.
//! * The published OpenAPI
//!   (<https://developer.clickup.com/openapi/clickup-api-v2-reference.json>,
//!   `servers: - url: https://api.clickup.com/api`) for every path, method,
//!   parameter, and request body named below.
//!
//! # The credential is the header value
//!
//! ClickUp publishes both forms side by side and they are not interchangeable.
//! This connector is the personal-token one, so it declares
//! [`AuthPlan::authorization_credential`] — the same plan Linear needed, for the
//! same reason: `Bearer` would authenticate as nobody, and the SDK's API-key
//! header plan refuses the `Authorization` name on purpose.
//!
//! # No walk, because the end of one is a flag
//!
//! ClickUp's task collections publish a page index — "Page to fetch (starts at
//! 0)" — with a fixed size the request cannot name: "Responses are limited to
//! 100 tasks per page", and no `per_page` parameter exists to declare it. The
//! end of the collection is the response's `last_page` **boolean**, and no plan
//! in the SDK's closed set reads a flag; declaring the page regime as a walk
//! would be declaring a walk that cannot end. This is exactly the case
//! `knowledgebase/declarative-saas/decisions/065-*` decided for Zendesk's
//! `meta[has_more]`, so the same answer holds: `page` is a declared input a
//! Process advances, `last_page` is a declared output, and one call is one page.
//!
//! The comment collection is a third regime again — "To fetch the next page of
//! historical comments, use the `start` and `start_id` of the oldest comment" —
//! keyed on values from a page the caller already holds. That is not a
//! continuation an SDK plan can prime *or* end on, and every declared query
//! input is rendered on every request, so declaring the pair would force a first
//! call to invent one. `comment.list` therefore declares neither and answers
//! with what ClickUp publishes as its default: "By default, the request returns
//! the 25 most recent comments."
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! ClickUp's published v2 OpenAPI description — 518 KB covering every endpoint,
//! parameter, request body, and response — nor in its authentication or
//! rate-limit guides. `task.create` and `comment.create` are therefore
//! `AtMostOnce` (ADR 063).
//!
//! `task.update` is `InventoryOnly` on ClickUp's own description of it: "Update
//! a task by including one or more fields in the request body", which is a
//! partial update, and a repeat sets the same fields to the same values.
//! `task.delete` is `InventoryOnly` because ClickUp publishes "Delete a task
//! from your Workspace" — what the first send does — and no sentence about the
//! second, which is the evidence spec 010 §7's `NaturalMethod` asks for.

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
pub const NAME: &str = "clickup";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "`servers: - url: https://api.clickup.com/api`" — the origin, with the
/// `/api/v2` prefix left to each declared path.
const ORIGIN: &str = "https://api.clickup.com";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("ClickUp's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::authorization_credential(),
            ))
            .operations(operations().expect("the ClickUp declarations are valid"))
            .build()
            .expect("the ClickUp declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// ClickUp publishes an error body — "All errors return JSON with `err`
/// (message) and optionally `ECODE` (error code)" — but publishes no closed list
/// of `ECODE` values: its own errors page carries a handful and its feature
/// tracker still asks for the rest. A map keyed on an open vocabulary would be a
/// map with holes in it, so this one reads the status and no body pointer at
/// all — the same choice `hubspot` made, for the same recorded reason.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 405, 409], ConnectorErrorClass::Permanent)
            // "If you exceed the rate limit, you will receive an HTTP 429."
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the ClickUp error map is a valid declaration")
    });
    &MAP
}

/// Decode one ClickUp response: the declared success statuses, then the declared
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

/// No operation of this connector declares a continuation plan, and the module
/// header says why.
///
/// It is named explicitly rather than left out, because a module that declares
/// no lookup at all acquires or loses a walk by omission
/// (`knowledgebase/declarative-saas/decisions/058-*`).
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Accept", "application/json")
        .success_statuses([StatusCode::OK])
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in ClickUp's published v2 OpenAPI \
                      description — 518 KB covering every endpoint, parameter, request body, and \
                      response schema — nor in its authentication or rate-limit guides: no request \
                      header, no query parameter, and no body attribute carries a client-supplied \
                      request identifier or a deduplication behaviour";

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
/// `date_created` and the other timestamps are strings of epoch milliseconds in
/// ClickUp's own examples — `"date_created": "1567780450202"` — so they are
/// declared as strings rather than as the numbers they look like.
fn task_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer(
            "description",
            "/description",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("status", "/status", ValueScalar::Json, Required::No)
        .output_pointer("url", "/url", ValueScalar::String, Required::No)
        .output_pointer("list", "/list", ValueScalar::Json, Required::No)
        .output_pointer("assignees", "/assignees", ValueScalar::Json, Required::No)
        .output_pointer("due_date", "/due_date", ValueScalar::Json, Required::No)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "date_updated",
            "/date_updated",
            ValueScalar::String,
            Required::No,
        )
}

/// The two fields every task collection publishes: the page, and the flag no
/// pagination plan can read.
fn task_page(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("tasks", "/tasks", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "last_page",
            "/last_page",
            ValueScalar::Boolean,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "View information about a task."
    let task_get = task_output(
        common(Operation::get("task.get", "/api/v2/task/{task_id}"))
            .path_param("task_id", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    // "View the tasks in a List. Responses are limited to 100 tasks per page."
    let task_list = task_page(
        common(Operation::get("task.list", "/api/v2/list/{list_id}/task"))
            .path_param("list_id", ValueScalar::String)
            .query_input("page", "page")
            .query_input("include_closed", "include_closed")
            .query_input("subtasks", "subtasks"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "View the tasks that meet specific criteria from a Workspace." ClickUp
    // spells the Workspace path segment `{team_Id}` in its own description; the
    // declared input name is the ordinary one and the template is ClickUp's.
    let task_search = task_page(
        common(Operation::get("task.search", "/api/v2/team/{team_Id}/task"))
            .path_param("team_Id", ValueScalar::String)
            .query_input("page", "page")
            .query_input("include_closed", "include_closed"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Create a new task."  `name` is the one required body field.
    let task_create = task_output(
        common(Operation::post(
            "task.create",
            "/api/v2/list/{list_id}/task",
        ))
        .path_param("list_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("name", JsonTemplate::input("name")),
            ("description", JsonTemplate::input("description")),
            ("assignees", JsonTemplate::input("assignees")),
            ("tags", JsonTemplate::input("tags")),
            ("status", JsonTemplate::input("status")),
            ("priority", JsonTemplate::input("priority")),
            ("due_date", JsonTemplate::input("due_date")),
        ]))
        .declared_input("name", ValueScalar::String, Required::Yes)
        .declared_input("assignees", ValueScalar::Json, Required::Yes)
        .declared_input("tags", ValueScalar::Json, Required::Yes),
    )
    .effect(at_most_once(
        "a second task with a new id in the same List, at the same status and due date; ClickUp \
         publishes no duplicate detection for tasks and no unique field a second create could \
         collide with",
    )?)
    .build()?;

    let task_update = task_output(
        common(Operation::put("task.update", "/api/v2/task/{task_id}"))
            .path_param("task_id", ValueScalar::String)
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("description", JsonTemplate::input("description")),
                ("status", JsonTemplate::input("status")),
                ("priority", JsonTemplate::input("priority")),
                ("due_date", JsonTemplate::input("due_date")),
                ("archived", JsonTemplate::input("archived")),
            ])),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    // "Delete a task from your Workspace." ClickUp declares the `Content-Type`
    // header **required** on this endpoint, and it is the only `DELETE` in this
    // batch that does, so the declaration sends it. The documented success is a
    // `204` with no body at all.
    let task_delete = common(Operation::delete("task.delete", "/api/v2/task/{task_id}"))
        .path_param("task_id", ValueScalar::String)
        .static_header("Content-Type", "application/json")
        .success_statuses([StatusCode::NO_CONTENT])
        .no_content_statuses([StatusCode::NO_CONTENT])
        .effect(Effect::inventory_only(SILENT_DELETE)?)
        .build()?;

    // "Retrieve comments from a task. Results are returned in reverse
    // chronological order (newest to oldest). By default, the request returns
    // the 25 most recent comments."
    let comment_list = common(Operation::get(
        "comment.list",
        "/api/v2/task/{task_id}/comment",
    ))
    .path_param("task_id", ValueScalar::String)
    .output_pointer("comments", "/comments", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Add a new comment to a task." `comment_text` and `notify_all` are the two
    // required body fields.
    let comment_create = common(Operation::post(
        "comment.create",
        "/api/v2/task/{task_id}/comment",
    ))
    .path_param("task_id", ValueScalar::String)
    .body(JsonTemplate::object([
        ("comment_text", JsonTemplate::input("comment_text")),
        ("notify_all", JsonTemplate::input("notify_all")),
        ("assignee", JsonTemplate::input("assignee")),
    ]))
    .declared_input("comment_text", ValueScalar::String, Required::Yes)
    .declared_input("notify_all", ValueScalar::Boolean, Required::Yes)
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("hist_id", "/hist_id", ValueScalar::String, Required::No)
    .output_pointer("date", "/date", ValueScalar::Int64, Required::No)
    .effect(at_most_once(
        "a second comment on the same task, with a new id — and, because ClickUp publishes that \
         \"other assignees and watchers on the task are always notified regardless of this \
         setting\", a second notification to every one of them",
    )?)
    .build()?;

    let list_get = common(Operation::get("list.get", "/api/v2/list/{list_id}"))
        .path_param("list_id", ValueScalar::String)
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("content", "/content", ValueScalar::String, Required::No)
        .output_pointer("folder", "/folder", ValueScalar::Json, Required::No)
        .output_pointer("space", "/space", ValueScalar::Json, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "View the Lists within a Folder."
    let list_list = common(Operation::get(
        "list.list",
        "/api/v2/folder/{folder_id}/list",
    ))
    .path_param("folder_id", ValueScalar::String)
    .query_input("archived", "archived")
    .output_pointer("lists", "/lists", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "View the Spaces avialable in a Workspace" — ClickUp's own spelling.
    let space_list = common(Operation::get("space.list", "/api/v2/team/{team_id}/space"))
        .path_param("team_id", ValueScalar::String)
        .query_input("archived", "archived")
        .output_pointer("spaces", "/spaces", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        task_get,
        task_list,
        task_search,
        task_create,
        task_update,
        task_delete,
        comment_list,
        comment_create,
        list_get,
        list_list,
        space_list,
    ])
}

/// The reason the update carries.
const PARTIAL_UPDATE: &str = "ClickUp documents its task `PUT` as a partial update — \"Update a \
                              task by including one or more fields in the request body\" — so a \
                              second identical send leaves exactly the state the first one did. \
                              There is no consequence ADR 063 admits a class on, and no repeat \
                              statement for spec 010 §7's NaturalMethod either";

/// The reason the delete carries.
const SILENT_DELETE: &str = "ClickUp publishes \"Delete a task from your Workspace\" and a `204`, \
                             which is what the first send does, and nothing at all about a second \
                             one. Spec 010 §7 admits NaturalMethod on the provider's own repeat \
                             statement, and the shape of the request is not that statement";
