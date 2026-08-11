//! Basecamp's API — a to-do surface, and the batch's one provider that
//! publishes which of its writes are repeat-safe.
//!
//! Ground truth is Basecamp's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://github.com/basecamp/bc3-api> (`README.md`) — "All URLs start with
//!   `https://3.basecampapi.com/999999999/`. URLs are HTTPS only. The path is
//!   prefixed with the account ID, but no `/api/v1` API prefix", the credential
//!   `curl -H "Authorization: Bearer $ACCESS_TOKEN"`, "You must include a
//!   `User-Agent` header with **both**: The name of your application [and] A
//!   link to your application or your email address … If you don't include a
//!   `User-Agent` header, you'll get a `400 Bad Request` response", and "you
//!   have to send the `Content-Type` header `Content-Type: application/json;
//!   charset=utf-8` when you're POSTing or PUTing data into Basecamp. …
//!   Alternatively you can send `Accept: application/json`."
//! * The same README's *Pagination* section — "The Basecamp API follows the
//!   RFC5988 convention of using the `Link` header to provide URLs for the
//!   `next` page. Follow this convention to retrieve the next page of data —
//!   please don't build the pagination URLs yourself!", and the end of the walk:
//!   "If the `Link` header is blank, that's the last page."
//! * The same README's *Rate limiting* section — "We return a 429 Too Many
//!   Requests response when you've exceeded a rate limit. Consult the
//!   `Retry-After` response header to determine how long to wait (in seconds)
//!   before retrying the request", with "the first rate limit you'll commonly
//!   encounter is currently 50 requests per 10 second period per IP address".
//! * The *flat routes* section — "These **flat routes** are the canonical form
//!   for all new integrations. The project context is derived server-side from
//!   the resource itself" — which is why no path here carries a `bucket_id`.
//! * Basecamp's published OpenAPI description
//!   (<https://github.com/basecamp/basecamp-sdk>, `openapi.json`, `"title":
//!   "Basecamp"`, `"version": "2026-08-05"`), linked from the README as the
//!   machine-readable form of the same reference.
//!
//! # The account id is a compiled path prefix, not a host
//!
//! Every Basecamp URL is `https://3.basecampapi.com/{account_id}/…`: the host is
//! constant and the account is the **first path segment**. So this connector's
//! origin is fixed and its declaration is built per deployment with the account
//! id rendered into every path as a literal — the "declaration a deployment
//! completes" shape of
//! `knowledgebase/declarative-saas/decisions/048-*`, applied to a path rather
//! than to a credential. Nothing in operation input, a provider response, or a
//! continuation can move it, and the account id's grammar — "Basecamp account ID
//! (numeric string)" — is checked where the declaration is built. See
//! `knowledgebase/declarative-saas/decisions/066-*`.
//!
//! # `User-Agent` is deploy-time material
//!
//! Basecamp refuses a request without one, and requires it to name *this
//! deployment* and a way to reach it. It is therefore a configured value on the
//! declaration exactly as the account id is, and never an operation input: a
//! request that could choose its own `User-Agent` would let a Process
//! impersonate another integration to the provider.
//!
//! # Effect classification, from the provider's own marks
//!
//! Basecamp is the only provider in this batch that publishes, per operation,
//! whether repeating it is safe: its OpenAPI carries `x-basecamp-idempotent:
//! {"natural": true}` on 83 of its 250 operations and nothing on the rest.
//!
//! * `todo.replace` is `NaturalMethod`. It is a `PUT` against a fixed resource
//!   identity, it carries Basecamp's own mark, and its description is the
//!   repeat statement spec 010 §7 asks for: "Replace a todo with a new complete
//!   representation. The request body is the todo's full writable state: any
//!   writable field omitted from the request is cleared server-side."
//! * `todo.uncomplete` is `NaturalMethod`: a `DELETE` against a fixed identity —
//!   `DELETE /todos/{id}/completion.json` — carrying the same mark, for "Mark a
//!   todo as incomplete".
//! * **`todo.complete` stays `InventoryOnly`, and it is the sharpest entry in
//!   this batch.** Basecamp marks it `natural: true` as well, and it is over a
//!   `POST`. Spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only,
//!   because HTTP defines repeat-safety for those two; a class keyed on a
//!   provider sentence over an arbitrary method is the widening ADR 042 exists
//!   to refuse. ADR 063's at-most-once class is not the answer either, because
//!   it *trades the retry away* and this operation wants a class that keeps it.
//!   It joins the population `INVENTORY.md` records as still waiting.
//! * `todo.create` and `comment.create` carry **no** mark and no idempotency key
//!   anywhere in Basecamp's published contract, so they are `AtMostOnce`
//!   (ADR 063). The 88 occurrences of `idempot` in that description are all the
//!   `x-basecamp-idempotent` extension itself; no request header, body
//!   attribute, or response field carries a client-supplied request identifier.

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
pub const NAME: &str = "basecamp";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the account id every path is
/// prefixed with.
pub const ACCOUNT_ID: &str = "account_id";

/// The deploy-time configuration key carrying the `User-Agent` Basecamp demands.
pub const USER_AGENT: &str = "user_agent";

/// "All URLs start with `https://3.basecampapi.com/999999999/`."
const ORIGIN: &str = "https://3.basecampapi.com";

/// The OAuth2 scopes Basecamp issues; the module needs none of its own, so the
/// deadline below is the only per-operation budget this connector declares.
const OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// One deployment's declaration.
///
/// `account_id` becomes the first path segment of every operation and
/// `user_agent` becomes a static header on every request. Both are checked here,
/// where the declaration is built, so a mistyped value is a startup refusal
/// rather than a `400` on the first activity attempt.
pub fn connector(account_id: &str, user_agent: &str) -> Result<Connector, OperationError> {
    validate_account_id(account_id)?;
    validate_user_agent(user_agent)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN)?)
        .credential(CredentialSpec::for_plan(
            AuthPlan::oauth2_authorization_code(),
        ))
        .operations(operations(account_id, user_agent)?)
        .build()
}

/// The declaration a reviewer and the registry read, with placeholder values no
/// deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("999999999", "Donat (deployment.configured@example.invalid)")
}

/// Basecamp's own grammar for the value: "Basecamp account ID (numeric string)".
///
/// A path prefix a deployment types is the one value in this connector that
/// could reach outside its own segment, so the check is the narrow one: ASCII
/// digits, nothing else, and a bounded length.
pub fn validate_account_id(account_id: &str) -> Result<(), OperationError> {
    if account_id.is_empty()
        || account_id.len() > 20
        || !account_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return Err(OperationError::new(
            "the Basecamp account id must be the numeric string Basecamp publishes as the first \
             path segment of every URL",
        ));
    }
    Ok(())
}

/// Basecamp's own rule for the header: "You must include a `User-Agent` header
/// with **both**: The name of your application [and] A link to your application
/// or your email address", with the published examples `Freshbooks
/// (http://freshbooks.com/contact.php)` and `Fabian's Ingenious Integration
/// (fabian@example.com)`.
///
/// What is checked is what a machine can check: a non-empty name, a bracketed
/// contact, printable ASCII, and a bounded length. Whether the contact is real
/// is Basecamp's business with the deployment.
pub fn validate_user_agent(user_agent: &str) -> Result<(), OperationError> {
    let Some((name, contact)) = user_agent.split_once('(') else {
        return Err(OperationError::new(
            "the Basecamp user agent must name the application and a contact, as \
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
            "the Basecamp user agent must name the application and a contact, as \
             `MyApp (you@example.com)`",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Basecamp publishes a status table and no machine-readable error code — its
/// error responses carry a human `error` string — so this map reads the status
/// and no body pointer.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "If you don't include a `User-Agent` header, you'll get a
            // `400 Bad Request` response", and a malformed body is the same
            // status.
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found", and "You'll receive a `415 Unsupported Media
            // Type` response code if you don't include the `Content-Type`
            // header."
            .on_statuses([404, 415], ConnectorErrorClass::Permanent)
            // "422 Unprocessable Entity" for a body Basecamp parsed and refused.
            .on_status(422, ConnectorErrorClass::Validation)
            // "We return a 429 Too Many Requests response when you've exceeded a
            // rate limit."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 (Internal Server Error), 502 (Bad Gateway), 503 (Service
            // Unavailable), and 504 (Gateway Timeout) may be retried with
            // exponential backoff."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Basecamp error map is a valid declaration")
    });
    &MAP
}

/// Decode one Basecamp response: the declared success statuses, then the
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

/// The continuation plan of each walked collection.
///
/// Every collection is a bare JSON array at the document root, so the items
/// pointer is the empty pointer (RFC 6901's whole document), and the
/// continuation is Basecamp's own `Link` header — with the end of the walk
/// published as an absence: "If the `Link` header is blank, that's the last
/// page." Basecamp asks for exactly this: "Follow this convention to retrieve
/// the next page of data — please don't build the pagination URLs yourself!"
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("", "next").expect("the Basecamp link plan is valid")
    });
    match operation_id {
        "todo.list" | "todolist.list" | "comment.list" | "project.list" => Some(&COLLECTION),
        _ => None,
    }
}

/// One operation's path, under this deployment's account prefix.
fn path(account_id: &str, suffix: &str) -> String {
    format!("/{account_id}{suffix}")
}

fn common(builder: OperationBuilder, user_agent: &str) -> OperationBuilder {
    builder
        .version(VERSION)
        .deadline(OPERATION_DEADLINE)
        // "Alternatively you can send `Accept: application/json`."
        .static_header("Accept", "application/json")
        // "You must include a `User-Agent` header."
        .static_header("User-Agent", user_agent)
        .success_statuses([StatusCode::OK, StatusCode::CREATED])
}

/// The reason every keyless write in this module carries.
const NO_KEY: &str = "Basecamp's published OpenAPI description marks, per operation, whether \
                      repeating it is safe — `x-basecamp-idempotent: {\"natural\": true}` on 83 of \
                      its operations — and it carries no mark at all on this one. Nowhere in that \
                      description, nor in the published REST reference, is there a request header, \
                      query parameter, or body attribute carrying a client-supplied request \
                      identifier or a deduplication behaviour: every one of the 88 occurrences of \
                      `idempot` is the extension itself";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The published to-do attributes a Process reads.
fn todo_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("title", "/title", ValueScalar::String, Required::No)
        .output_pointer("content", "/content", ValueScalar::String, Required::No)
        .output_pointer(
            "description",
            "/description",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "completed",
            "/completed",
            ValueScalar::Boolean,
            Required::No,
        )
        .output_pointer("due_on", "/due_on", ValueScalar::String, Required::No)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("app_url", "/app_url", ValueScalar::String, Required::No)
        .output_pointer("assignees", "/assignees", ValueScalar::Json, Required::No)
        .output_pointer("bucket", "/bucket", ValueScalar::Json, Required::No)
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

/// Every operation this connector publishes, under one deployment's account.
fn operations(account_id: &str, user_agent: &str) -> Result<Vec<Operation>, OperationError> {
    // "`GET /todos/2.json` will return the to-do with an ID of `2`."
    let todo_get = todo_output(
        common(
            Operation::get("todo.get", &path(account_id, "/todos/{todo_id}.json")),
            user_agent,
        )
        .path_param("todo_id", ValueScalar::Int64),
    )
    .effect(Effect::read_only())
    .build()?;

    // "`GET /todolists/3/todos.json` will return a paginated list of active
    // to-dos in the to-do list with ID of `3`."
    let todo_list = common(
        Operation::get(
            "todo.list",
            &path(account_id, "/todolists/{todolist_id}/todos.json"),
        ),
        user_agent,
    )
    .path_param("todolist_id", ValueScalar::Int64)
    .query_input("status", "status")
    .query_input("completed", "completed")
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "`POST /todolists/3/todos.json` creates a to-do under the to-do list with
    // an ID of `3`." `content` is the one required parameter.
    let todo_create = todo_output(
        common(
            Operation::post(
                "todo.create",
                &path(account_id, "/todolists/{todolist_id}/todos.json"),
            ),
            user_agent,
        )
        .path_param("todolist_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("content", JsonTemplate::input("content")),
            ("description", JsonTemplate::input("description")),
            ("assignee_ids", JsonTemplate::input("assignee_ids")),
            ("notify", JsonTemplate::input("notify")),
            ("due_on", JsonTemplate::input("due_on")),
        ]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        .declared_input("assignee_ids", ValueScalar::Json, Required::Yes),
    )
    .effect(at_most_once(
        "a second to-do with a new id in the same list — and, where `notify` is true, a second \
         notification to every assignee",
    )?)
    .build()?;

    // "Replace a todo with a new complete representation. The request body is
    // the todo's full writable state: any writable field omitted from the
    // request is cleared server-side."
    let todo_replace = todo_output(
        common(
            Operation::put("todo.replace", &path(account_id, "/todos/{todo_id}.json")),
            user_agent,
        )
        .path_param("todo_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("content", JsonTemplate::input("content")),
            ("description", JsonTemplate::input("description")),
            ("assignee_ids", JsonTemplate::input("assignee_ids")),
            ("notify", JsonTemplate::input("notify")),
            ("due_on", JsonTemplate::input("due_on")),
        ]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        .declared_input("assignee_ids", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::provider_idempotent_natural_method(
        "Basecamp publishes this write as a replacement of one fixed resource identity — \"Replace \
         a todo with a new complete representation. The request body is the todo's full writable \
         state: any writable field omitted from the request is cleared server-side (empty/missing \
         assignee_ids clears assignees, missing description clears it, and so on)\" — and marks the \
         operation `x-basecamp-idempotent: {\"natural\": true}` in its own published API \
         description. A second identical `PUT` writes the same complete representation to the same \
         `/todos/{id}.json`",
    )?)
    .build()?;

    // "`POST /todos/2/completion.json` will mark the to-do with an ID of `2` as
    // completed." Marked `natural: true` — over a POST, which the gate does not
    // admit.
    let todo_complete = common(
        Operation::post(
            "todo.complete",
            &path(account_id, "/todos/{todo_id}/completion.json"),
        ),
        user_agent,
    )
    .path_param("todo_id", ValueScalar::Int64)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::inventory_only(REPEAT_SAFE_OVER_POST)?)
    .build()?;

    // "`DELETE /todos/2/completion.json` will mark the to-do with an ID of `2`
    // as uncompleted."
    let todo_uncomplete = common(
        Operation::delete(
            "todo.uncomplete",
            &path(account_id, "/todos/{todo_id}/completion.json"),
        ),
        user_agent,
    )
    .path_param("todo_id", ValueScalar::Int64)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Basecamp publishes \"Mark a todo as incomplete\" as a `DELETE` against one fixed resource \
         identity — `DELETE /todos/{id}/completion.json` — and marks the operation \
         `x-basecamp-idempotent: {\"natural\": true}` in its own published API description. A \
         second identical send leaves the same one incomplete to-do",
    )?)
    .build()?;

    // "`GET /recordings/3/comments.json` will return a paginated list of active
    // comments for the recording with ID of `3`."
    let comment_list = common(
        Operation::get(
            "comment.list",
            &path(account_id, "/recordings/{recording_id}/comments.json"),
        ),
        user_agent,
    )
    .path_param("recording_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "`POST /recordings/3/comments.json` publishes a comment under the
    // recording with an ID of `3`." `content` is required.
    let comment_create = common(
        Operation::post(
            "comment.create",
            &path(account_id, "/recordings/{recording_id}/comments.json"),
        ),
        user_agent,
    )
    .path_param("recording_id", ValueScalar::Int64)
    .body(JsonTemplate::object([(
        "content",
        JsonTemplate::input("content"),
    )]))
    .declared_input("content", ValueScalar::String, Required::Yes)
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("content", "/content", ValueScalar::String, Required::No)
    .output_pointer("app_url", "/app_url", ValueScalar::String, Required::No)
    .output_pointer(
        "created_at",
        "/created_at",
        ValueScalar::String,
        Required::No,
    )
    .effect(at_most_once(
        "a second comment on the same recording, with a new id, and a second notification to \
         everyone subscribed to it",
    )?)
    .build()?;

    // "List todolists in a todoset."
    let todolist_list = common(
        Operation::get(
            "todolist.list",
            &path(account_id, "/todosets/{todoset_id}/todolists.json"),
        ),
        user_agent,
    )
    .path_param("todoset_id", ValueScalar::Int64)
    .query_input("status", "status")
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Get a single project by id."
    let project_get = common(
        Operation::get(
            "project.get",
            &path(account_id, "/projects/{project_id}.json"),
        ),
        user_agent,
    )
    .path_param("project_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer(
        "description",
        "/description",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("status", "/status", ValueScalar::String, Required::No)
    .output_pointer("app_url", "/app_url", ValueScalar::String, Required::No)
    // "Response: Single project object with `dock` array containing enabled
    // tools" — which is where a Process finds the to-do set it then lists.
    .output_pointer("dock", "/dock", ValueScalar::Json, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "List projects (active by default; optionally archived/trashed)."
    let project_list = common(
        Operation::get("project.list", &path(account_id, "/projects.json")),
        user_agent,
    )
    .query_input("status", "status")
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        todo_get,
        todo_list,
        todo_create,
        todo_replace,
        todo_complete,
        todo_uncomplete,
        comment_list,
        comment_create,
        todolist_list,
        project_get,
        project_list,
    ])
}

/// The reason `todo.complete` carries: a write the provider documents as
/// repeat-safe, over a method the gate does not admit.
const REPEAT_SAFE_OVER_POST: &str = "Basecamp marks this operation `x-basecamp-idempotent: {\"natural\": true}` in its own \
     published API description, for \"Mark a todo as complete\" — a genuine repeat-safe write, over \
     a `POST`. Spec 010 §7 admits NaturalMethod for PUT and DELETE only, because HTTP defines \
     repeat-safety for those two, and a class keyed on a provider sentence over an arbitrary method \
     is the widening ADR 042 exists to refuse. ADR 063's at-most-once class is not the answer \
     either: it trades the retry away, and an operation that is safe to send twice wants a class \
     that keeps it. That class does not exist yet";
