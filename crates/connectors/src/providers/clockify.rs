//! Clockify's REST API v1 — the time-entry, project and client surface.
//!
//! Ground truth is Clockify's own published API documentation
//! (<https://docs.clockify.me/>), read on 2026-08-10:
//!
//! * The base URL `https://api.clockify.me/api/v1/`, and the credential: "make
//!   sure to include either the `X-Api-Key` or the `X-Addon-Token` in the
//!   request header".
//! * Pagination on every `GET` collection: `page` ("integer, 1-indexed, defaults
//!   to 1") and `page-size`, with a `Last-Page` response header indicating
//!   whether further pages exist.
//! * The endpoints this connector declares:
//!   `GET /v1/user`;
//!   `GET /v1/workspaces/{workspaceId}/time-entries/{timeEntryId}`;
//!   `GET /v1/workspaces/{workspaceId}/user/{userId}/time-entries`;
//!   `POST /v1/workspaces/{workspaceId}/time-entries` (`201`);
//!   `PUT /v1/workspaces/{workspaceId}/time-entries/{timeEntryId}` (`200`);
//!   `GET /v1/workspaces/{workspaceId}/projects` and
//!   `GET /v1/workspaces/{workspaceId}/projects/{projectId}`;
//!   `GET /v1/workspaces/{workspaceId}/clients`.
//! * The status codes: `200 OK`, `201 Created`, `204 No Content` for a delete
//!   with no body, and "Too many requests" for the rate limit, which Clockify
//!   publishes as 50 requests per second on one workspace.
//!
//! # The workspace is a compiled path prefix
//!
//! Every endpoint here but `/v1/user` is scoped to one workspace, and the
//! workspace is a **path segment**: a Clockify API key belongs to a person and
//! that person may be in several workspaces. Which one this deployment talks to
//! is therefore deploy-time configuration, and it is compiled into every path by
//! [`connector`] rather than filled from operation input — the Basecamp shape of
//! `knowledgebase/declarative-saas/decisions/066-*`. A `{workspace_id}` path
//! *binding* would have been a slot a Process fills, which is a Process choosing
//! a tenant.
//!
//! The `userId` in the time-entry listing is deliberately not that: it selects a
//! person *inside* the configured workspace, the API key's own permissions
//! govern what it may read, and `user.me` is the operation a Process reads it
//! from. It is an ordinary declared input.
//!
//! # Pagination
//!
//! Clockify publishes a page-number regime — `page` from 1, `page-size` — and
//! answers a bare JSON array, so every walked collection declares
//! [`Pagination::page_number`] over the whole document (RFC 6901's empty
//! pointer) and ends on a short page. `Last-Page` is a header no plan in the
//! SDK's closed set reads, and it is not needed: a page shorter than the
//! declared size is the same absence, and the declared size is what makes it
//! observable.
//!
//! # Effect classification
//!
//! Clockify publishes no idempotency mechanism: neither the API documentation's
//! cross-cutting sections — authentication, pagination, rate limits, status
//! codes — nor the request contract of either write names a client-supplied
//! request identifier, a deduplication behaviour, or a replay.
//!
//! * `time_entry.create` is `AtMostOnce` (ADR 063): a repeat is a second time
//!   entry with a new id on the same project, and a second block of hours
//!   against whatever that workspace bills from.
//! * `time_entry.update` stays `InventoryOnly`, and it is the sharper of the two
//!   calls. It is a `PUT` against a fixed resource identity, which is the
//!   *method* half of spec 010 §7's `NaturalMethod` — and that class is admitted
//!   on the provider's own repeat statement, not on the method. Clockify
//!   publishes no sentence about what a second identical `PUT` does, so there is
//!   nothing to quote; it is the `grafana.alert_rule.update` finding one
//!   provider over. `AtMostOnce` is not the answer either, because a replacement
//!   that writes the same representation twice has no consequence to record.
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
pub const NAME: &str = "clockify";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the workspace every scoped path is
/// built from.
///
/// It is not a secret: Clockify prints the workspace id in its own web
/// application URLs, and holding it authorizes nothing.
pub const WORKSPACE_ID: &str = "workspace_id";

/// "https://api.clockify.me/api/v1/".
const ORIGIN: &str = "https://api.clockify.me";

/// "make sure to include either the `X-Api-Key` or the `X-Addon-Token` in the
/// request header". This connector declares the first: it is the credential a
/// deployment can create for itself.
const API_KEY_HEADER: &str = "X-Api-Key";

/// The page the declaration asks for. Clockify's own default is 50 and it
/// admits far more; a fixed size is what makes a short page an observable end of
/// the walk.
const PAGE_SIZE: u32 = 50;

/// Clockify publishes no per-operation deadline, so this is the module's own
/// bound on one attempt.
const OPERATION_DEADLINE: Duration = Duration::from_secs(30);

/// One deployment's declaration.
///
/// `workspace_id` is compiled into every scoped path, so nothing in operation
/// input, a provider response, or a continuation can move it.
pub fn connector(workspace_id: &str) -> Result<Connector, OperationError> {
    validate_workspace_id(workspace_id)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN)?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::api_key_header(API_KEY_HEADER)?)
                // Not a secret: the workspace id is in every Clockify web URL,
                // and it names a workspace rather than authorizing one.
                .with_field(WORKSPACE_ID, FieldClassification::NonSecret),
        )
        .operations(operations(workspace_id)?)
        .build()
}

/// The declaration a reviewer and the registry read, with a placeholder
/// workspace no deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("64a687e29ae1f428e7ebe303")
}

/// Clockify's own grammar for the value: its identifiers are 24-character
/// lowercase hexadecimal object ids, and this one is a path segment.
///
/// The check is the narrow one, because a path segment a deployment types is the
/// one value here that could reach outside its own segment.
pub fn validate_workspace_id(workspace_id: &str) -> Result<(), OperationError> {
    let valid = workspace_id.len() == 24
        && workspace_id
            .chars()
            .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character));
    if !valid {
        return Err(OperationError::new(
            "the Clockify workspace id must be the 24-character hexadecimal identifier Clockify \
             publishes in every workspace-scoped URL",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Clockify publishes a numeric `code` beside a human `message` in its failure
/// bodies, and the classification this connector needs is settled by the status
/// alone, so this map reads no body pointer.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_status(400, ConnectorErrorClass::Validation)
            // A missing or rejected `X-Api-Key`, and a workspace this key is not
            // a member of.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_status(404, ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Clockify error map is a valid declaration")
    });
    &MAP
}

/// Decode one Clockify response: the declared success statuses, then the
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
/// Every Clockify collection is a bare JSON array at the document root, so the
/// items pointer is the empty pointer, and the walk ends on a page shorter than
/// the declared size. Clockify numbers its pages from 1.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("", "page", "page-size", PAGE_SIZE)
            .expect("the Clockify page plan is valid")
    });
    match operation_id {
        "time_entry.list" | "project.list" | "client.list" => Some(&COLLECTION),
        _ => None,
    }
}

/// One workspace-scoped path, under this deployment's workspace.
fn path(workspace_id: &str, suffix: &str) -> String {
    format!("/api/v1/workspaces/{workspace_id}{suffix}")
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .deadline(OPERATION_DEADLINE)
        .static_header("Accept", "application/json")
}

/// What every walked collection declares.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .query_static("page-size", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
}

/// The reason this connector's keyless write carries.
const NO_KEY: &str = "Clockify's published API documentation names no idempotency mechanism \
                      anywhere: not in the cross-cutting sections that carry its credential, its \
                      pagination regime, its rate limit and its status codes, and not in the \
                      request contract of this endpoint, whose documented body is the time \
                      entry's own fields — `start`, `end`, `billable`, `description`, `projectId`, \
                      `taskId`, `tagIds`. No request header, query parameter, or body attribute \
                      carries a client-supplied request identifier, and no response field or \
                      header reports a replay";

/// The published time-entry attributes a Process reads.
fn time_entry_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "description",
            "/description",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("billable", "/billable", ValueScalar::Boolean, Required::No)
        .output_pointer("projectId", "/projectId", ValueScalar::String, Required::No)
        .output_pointer("taskId", "/taskId", ValueScalar::String, Required::No)
        .output_pointer("userId", "/userId", ValueScalar::String, Required::No)
        .output_pointer(
            "workspaceId",
            "/workspaceId",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("tagIds", "/tagIds", ValueScalar::Json, Required::No)
        // Clockify publishes the interval as an object of ISO-8601 instants and
        // an ISO-8601 duration, so the contract carries the provider's own
        // shape (`knowledgebase/declarative-saas/decisions/071-*`).
        .output_pointer(
            "timeInterval",
            "/timeInterval",
            ValueScalar::Json,
            Required::No,
        )
}

/// Every operation this connector publishes, under one deployment's workspace.
fn operations(workspace_id: &str) -> Result<Vec<Operation>, OperationError> {
    let time_entries = path(workspace_id, "/time-entries");
    let one_time_entry = path(workspace_id, "/time-entries/{time_entry_id}");
    let user_time_entries = path(workspace_id, "/user/{user_id}/time-entries");
    let projects = path(workspace_id, "/projects");
    let one_project = path(workspace_id, "/projects/{project_id}");
    let clients = path(workspace_id, "/clients");

    // "Get currently logged-in user's info — `GET /v1/user`." It is the one
    // endpoint here that is not workspace-scoped, and it is how a Process learns
    // the `userId` the listing below takes.
    let user_me = common(Operation::get("user.me", "/api/v1/user"))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("email", "/email", ValueScalar::String, Required::No)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer(
            "activeWorkspace",
            "/activeWorkspace",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Get a specific time entry on a workspace."
    let time_entry_get = time_entry_output(
        common(Operation::get("time_entry.get", &one_time_entry))
            .path_param("time_entry_id", ValueScalar::String),
    )
    .success_statuses([StatusCode::OK])
    .effect(Effect::read_only())
    .build()?;

    // "Get time entries for a user on workspace."
    let time_entry_list = collection(
        common(Operation::get("time_entry.list", &user_time_entries))
            .path_param("user_id", ValueScalar::String)
            .query_input("start", "start")
            .query_input("end", "end"),
    )
    .build()?;

    // "Add a new time entry to workspace."
    let time_entry_create = time_entry_output(
        common(Operation::post("time_entry.create", &time_entries)).body(JsonTemplate::object([
            ("start", JsonTemplate::input("start")),
            ("end", JsonTemplate::input("end")),
            ("billable", JsonTemplate::input("billable")),
            ("description", JsonTemplate::input("description")),
            ("projectId", JsonTemplate::input("projectId")),
            ("taskId", JsonTemplate::input("taskId")),
            ("tagIds", JsonTemplate::input("tagIds")),
        ])),
    )
    .declared_input("start", ValueScalar::String, Required::Yes)
    .success_statuses([StatusCode::CREATED])
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        "a second time entry with a new id on the same project for the same person, and a second \
         block of hours against whatever that workspace bills from",
    )?))
    .build()?;

    // "Update time entry on workspace."
    let time_entry_update = time_entry_output(
        common(Operation::put("time_entry.update", &one_time_entry))
            .path_param("time_entry_id", ValueScalar::String)
            .body(JsonTemplate::object([
                ("start", JsonTemplate::input("start")),
                ("end", JsonTemplate::input("end")),
                ("billable", JsonTemplate::input("billable")),
                ("description", JsonTemplate::input("description")),
                ("projectId", JsonTemplate::input("projectId")),
                ("taskId", JsonTemplate::input("taskId")),
                ("tagIds", JsonTemplate::input("tagIds")),
            ]))
            .declared_input("start", ValueScalar::String, Required::Yes),
    )
    .success_statuses([StatusCode::OK])
    .effect(Effect::inventory_only(PUT_WITH_NO_REPEAT_STATEMENT)?)
    .build()?;

    // "Find a project by ID."
    let project_get = common(Operation::get("project.get", &one_project))
        .path_param("project_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("clientId", "/clientId", ValueScalar::String, Required::No)
        .output_pointer("archived", "/archived", ValueScalar::Boolean, Required::No)
        .output_pointer("billable", "/billable", ValueScalar::Boolean, Required::No)
        .output_pointer("color", "/color", ValueScalar::String, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "Get all projects on workspace."
    let project_list = collection(
        common(Operation::get("project.list", &projects))
            .query_input("name", "name")
            .query_input("archived", "archived"),
    )
    .build()?;

    // "Get all clients on workspace."
    let client_list = collection(
        common(Operation::get("client.list", &clients))
            .query_input("name", "name")
            .query_input("archived", "archived"),
    )
    .build()?;

    Ok(vec![
        user_me,
        time_entry_get,
        time_entry_list,
        time_entry_create,
        time_entry_update,
        project_get,
        project_list,
        client_list,
    ])
}

/// The reason `time_entry.update` carries: a `PUT` against a fixed identity for
/// which the provider publishes no repeat statement at all.
const PUT_WITH_NO_REPEAT_STATEMENT: &str = "Clockify publishes this as a `PUT` against one fixed resource identity, which is the \
     *method* half of spec 010 §7's NaturalMethod — and that class is admitted on the provider's \
     own repeat statement rather than on the method (ADR 042). Clockify publishes no sentence \
     about what a second identical send does: no repeat-safety note, no statement about the \
     response of a replaced entry, and no marked idempotency in its documentation. ADR 063's \
     at-most-once class is not the answer either, because a replacement that writes the same \
     representation a second time has no consequence to record. So the operation stays declared, \
     typed, tested, and unreachable";
