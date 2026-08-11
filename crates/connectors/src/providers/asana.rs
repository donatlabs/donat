//! Asana's REST API 1.0 — the batch's task-and-project surface.
//!
//! Ground truth is Asana's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://developers.asana.com/docs/personal-access-token> — the worked
//!   example `curl https://app.asana.com/api/1.0/users/me -H "Authorization:
//!   Bearer ACCESS_TOKEN"`, which is the origin and the credential form in one
//!   line. Asana's published OpenAPI names the same origin: `servers: - url:
//!   https://app.asana.com/api/1.0`.
//! * <https://developers.asana.com/docs/pagination> — "The number of objects to
//!   return per page. The value must be between 1 and 100", "An offset to the
//!   next page returned by the API", "You can only pass in an offset that was
//!   returned to you via a previously paginated request", and the end of the
//!   walk: "If there are no more pages available, `next_page` will be null and
//!   no offset will be provided."
//! * <https://developers.asana.com/docs/rate-limits> — `429 Too Many Requests`
//!   with a `Retry-After` header "indicating how many seconds the client should
//!   wait before retrying", and "requests rejected by this limiter still count
//!   against your quota".
//! * <https://developers.asana.com/docs/errors> — the status table this module's
//!   error map is built from, and the error body: an `errors` array whose
//!   entries carry `message`, `help`, and (on a `500`) `phrase`.
//! * The published OpenAPI
//!   (<https://github.com/Asana/openapi>, `defs/asana_oas.yaml`) for every path,
//!   method, and parameter named below.
//!
//! # Everything is wrapped in `data`
//!
//! Asana publishes one envelope for both directions: a request body is
//! `{"data": {…}}` and a response is `{"data": …}` beside an optional
//! `next_page`. So every template here nests the declared slots under one
//! literal `data` key, and every output pointer starts `/data`.
//!
//! # The walk is Asana's own offset token
//!
//! `next_page.offset` is an opaque continuation Asana hands back and takes as
//! the `offset` query parameter, and it is *absent* on the last page — which is
//! exactly what [`Pagination::cursor`] ends on. The page size is the
//! declaration's, primed by the plan at Asana's published maximum of 100.
//!
//! The **search** endpoint deliberately declares no plan. Asana publishes why:
//! "Search results are not stable; repeating the same query multiple times may
//! return the data in a different order, even if the data do not change. Because
//! of this, the traditional pagination available elsewhere in the Asana API is
//! not available here." An aggregate assembled from pages the provider says may
//! reorder is not an aggregate, so the search is one request of at most 100
//! items and a Process that wants more narrows its query
//! (`knowledgebase/declarative-saas/decisions/065-*`).
//!
//! # Completing a task is an update
//!
//! Asana publishes no `complete` endpoint: `completed` is a writable field of
//! the task, so `task.update` is where a Process completes one. That operation
//! is inventory-only for the reason below, which means completing a task from a
//! Process is not reachable yet — recorded here rather than worked around.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Asana's published OpenAPI description — 3.0 MB covering every endpoint, its
//! parameters, and its request and response schemas — nor in the authentication,
//! pagination, rate-limit, or errors guides. `task.create` and `story.create`
//! are therefore `AtMostOnce` (ADR 063).
//!
//! `task.update` is `InventoryOnly` on Asana's own description of it: "Only the
//! fields provided in the `data` block will be updated; any unspecified fields
//! will remain unchanged", which is a partial update. A repeat sets the same
//! fields to the same values, so there is no consequence ADR 063 could admit it
//! on, and the `PUT` carries no repeat statement for spec 010 §7's
//! `NaturalMethod` either.
//!
//! `task.delete` is `InventoryOnly` for the reason `salesforce.record.delete`
//! is: a `DELETE` against a fixed identity is the right *shape*, but the
//! evidence `NaturalMethod` needs is the provider's own repeat statement and
//! Asana publishes none. What it publishes is the first send's effect —
//! "Deleted tasks go into the 'trash' of the user making the delete request.
//! Tasks can be recovered from the trash within a period of 30 days" — and
//! nothing about the second.

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
pub const NAME: &str = "asana";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "`curl https://app.asana.com/api/1.0/users/me`" — the origin, with the
/// version prefix left to each declared path.
const ORIGIN: &str = "https://app.asana.com";

/// "The value must be between 1 and 100."
const PAGE_SIZE: u32 = 100;

/// "Page sizes are limited to a maximum of 100 items, and can be specified by
/// the `limit` query parameter" — the search's own ceiling, declared as a
/// literal because the search is one request rather than a walk.
const SEARCH_PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Asana's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Asana declarations are valid"))
            .build()
            .expect("the Asana declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// It reads no body pointer at all, because Asana's error entries publish
/// `message`, `help`, and `phrase` and nothing machine-readable: "phrase — A
/// unique identifier (500 errors only)" is a support handle rather than a class.
/// The status table is the whole contract, and each row below is Asana's own
/// sentence.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — This usually occurs because of a missing or
            // malformed parameter."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized — A valid authentication token was not provided
            // with the request", "403 Forbidden — The server is refusing to
            // complete the request."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "402 Payment Required — The queried object or object mutation
            // specified in the request is only available to premium
            // organizations", "404 Not Found", "451 Unavailable For Legal
            // Reasons — This request was blocked for legal reasons." None of the
            // three is fixed by sending the same request again.
            .on_statuses([402, 404, 451], ConnectorErrorClass::Permanent)
            // "429 Too Many Requests — You have exceeded one of the enforced
            // rate limits in the API."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error — There was a problem on Asana's end."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Asana error map is a valid declaration")
    });
    &MAP
}

/// Decode one Asana response: the declared success statuses, then the declared
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
/// The items are always `data`, the continuation is always `next_page.offset`,
/// and the walk ends where Asana publishes that it ends: "If there are no more
/// pages available, `next_page` will be null and no offset will be provided."
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor("/data", "offset", "/next_page/offset", "limit", PAGE_SIZE)
            .expect("the Asana cursor plan is valid")
    });
    match operation_id {
        "task.list" | "project.list" | "story.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Accept", "application/json")
        .success_statuses([StatusCode::OK, StatusCode::CREATED])
}

/// One request body, under Asana's own envelope.
fn data<const N: usize>(fields: [(&str, JsonTemplate); N]) -> JsonTemplate {
    JsonTemplate::object([("data", JsonTemplate::object(fields))])
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in Asana's published OpenAPI \
                      description — 3.0 MB covering every endpoint, parameter, request schema, and \
                      response schema — nor in its authentication, pagination, rate-limit, or \
                      errors guides: no request header, no body attribute, and no response field \
                      carries a client-supplied request identifier or a deduplication behaviour";

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
/// Everything but the identity is optional, because Asana publishes that "This
/// endpoint returns a resource which excludes some properties by default" and a
/// declaration that demanded one of them would fail an attempt the provider
/// answered correctly.
fn task_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("gid", "/data/gid", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/data/name", ValueScalar::String, Required::No)
        .output_pointer("notes", "/data/notes", ValueScalar::String, Required::No)
        .output_pointer(
            "completed",
            "/data/completed",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer("due_on", "/data/due_on", ValueScalar::String, Required::No)
        .output_pointer(
            "assignee",
            "/data/assignee",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "projects",
            "/data/projects",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "permalink_url",
            "/data/permalink_url",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "created_at",
            "/data/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "modified_at",
            "/data/modified_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The two fields every walked collection publishes: the page, and the
/// continuation a completed walk has already spent.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_offset",
            "/next_page/offset",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Returns the complete task record for a single task."
    let task_get = task_output(
        common(Operation::get("task.get", "/api/1.0/tasks/{task_gid}"))
            .path_param("task_gid", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Returns the compact task records for some filtered set of tasks. … You
    // must specify a `project` or `tag` if you do not specify `assignee` and
    // `workspace`." The declared filter is the project, which satisfies that
    // rule on its own; every declared query input has to carry a value on every
    // call, so the narrow pair is a better contract than the whole filter set.
    let task_list = collection(
        common(Operation::get("task.list", "/api/1.0/tasks"))
            .query_input("project", "project")
            .query_input("completed_since", "completed_since"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "the Asana API has a task search endpoint that allows you to build complex
    // filters to find and retrieve the exact data you need."
    let task_search = common(Operation::get(
        "task.search",
        "/api/1.0/workspaces/{workspace_gid}/tasks/search",
    ))
    .path_param("workspace_gid", ValueScalar::String)
    .query_input("text", "text")
    .query_input("sort_by", "sort_by")
    .query_static("limit", SEARCH_PAGE_SIZE)
    .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Creating a new task is as easy as POSTing to the `/tasks` endpoint with a
    // data block containing the fields you'd like to set on the task. Any
    // unspecified fields will take on default values."
    let task_create = task_output(
        common(Operation::post("task.create", "/api/1.0/tasks"))
            .body(data([
                ("name", JsonTemplate::input("name")),
                ("notes", JsonTemplate::input("notes")),
                ("workspace", JsonTemplate::input("workspace")),
                ("projects", JsonTemplate::input("projects")),
                ("assignee", JsonTemplate::input("assignee")),
                ("due_on", JsonTemplate::input("due_on")),
            ]))
            .declared_input("name", ValueScalar::String, Required::Yes)
            .declared_input("projects", ValueScalar::Json, Required::Yes),
    )
    .effect(at_most_once(
        "a second task with a new gid in the same project, assigned to the same person and due on \
         the same day; Asana publishes no duplicate detection for tasks and no unique field a \
         second create could collide with",
    )?)
    .build()?;

    let task_update = task_output(
        common(Operation::put("task.update", "/api/1.0/tasks/{task_gid}"))
            .path_param("task_gid", ValueScalar::String)
            .body(data([
                ("name", JsonTemplate::input("name")),
                ("notes", JsonTemplate::input("notes")),
                ("completed", JsonTemplate::input("completed")),
                ("assignee", JsonTemplate::input("assignee")),
                ("due_on", JsonTemplate::input("due_on")),
            ])),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    // "Returns an empty data record", which is why the success is declared with
    // no readable field: the identity a Process needs is the one it sent.
    let task_delete = common(Operation::delete(
        "task.delete",
        "/api/1.0/tasks/{task_gid}",
    ))
    .path_param("task_gid", ValueScalar::String)
    .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
    .effect(Effect::inventory_only(SILENT_DELETE)?)
    .build()?;

    // "Returns the compact records for all stories on the task."
    let story_list = collection(
        common(Operation::get(
            "story.list",
            "/api/1.0/tasks/{task_gid}/stories",
        ))
        .path_param("task_gid", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Adds a story to a task. This endpoint currently only allows for comment
    // stories to be created."
    let story_create = common(Operation::post(
        "story.create",
        "/api/1.0/tasks/{task_gid}/stories",
    ))
    .path_param("task_gid", ValueScalar::String)
    .body(data([("text", JsonTemplate::input("text"))]))
    .declared_input("text", ValueScalar::String, Required::Yes)
    .output_pointer("gid", "/data/gid", ValueScalar::String, Required::Yes)
    .output_pointer("text", "/data/text", ValueScalar::String, Required::No)
    .output_pointer(
        "created_at",
        "/data/created_at",
        ValueScalar::String,
        Required::No,
    )
    .effect(at_most_once(
        "a second comment on the same task, with a new gid, and a second notification to everyone \
         following it",
    )?)
    .build()?;

    let project_get = common(Operation::get(
        "project.get",
        "/api/1.0/projects/{project_gid}",
    ))
    .path_param("project_gid", ValueScalar::String)
    .output_pointer("gid", "/data/gid", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/data/name", ValueScalar::String, Required::No)
    .output_pointer(
        "archived",
        "/data/archived",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer(
        "permalink_url",
        "/data/permalink_url",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "workspace",
        "/data/workspace",
        ValueScalar::Json,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let project_list = collection(
        common(Operation::get("project.list", "/api/1.0/projects"))
            .query_input("workspace", "workspace")
            .query_input("archived", "archived"),
    )
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        task_get,
        task_list,
        task_search,
        task_create,
        task_update,
        task_delete,
        story_list,
        story_create,
        project_get,
        project_list,
    ])
}

/// The reason the update carries.
const PARTIAL_UPDATE: &str = "Asana documents its task `PUT` as a partial update — \"Only the \
                              fields provided in the `data` block will be updated; any unspecified \
                              fields will remain unchanged\" — so a second identical send leaves \
                              exactly the state the first one did. There is no consequence ADR 063 \
                              admits a class on, and no repeat statement for spec 010 §7's \
                              NaturalMethod either";

/// The reason the delete carries.
const SILENT_DELETE: &str = "Asana publishes what the first delete does — \"Deleted tasks go into \
                             the 'trash' of the user making the delete request. Tasks can be \
                             recovered from the trash within a period of 30 days\" — and nothing at \
                             all about a second one. Spec 010 §7 admits NaturalMethod on the \
                             provider's own repeat statement, and a shape that looks repeat-safe is \
                             not that statement";
