//! GitLab's REST API v4, served from the instance a deployment operates.
//!
//! Ground truth is GitLab's own published documentation, read on 2026-08-10.
//!
//! * <https://docs.gitlab.com/api/rest/> — "The path must start with `/api/v4`
//!   (`v4` represents the API version)", with every worked example against
//!   `https://gitlab.example.com/api/v4/…`; the offset-pagination table
//!   ("`page` — Page number (default: `1`)", "`per_page` — Number of items to
//!   list per page (default: `20`, max: `100`)"); and "`Link` headers are
//!   returned with each response. They have `rel` set to `prev`, `next`,
//!   `first`, or `last` and contain the relevant URL. Be sure to use these links
//!   instead of generating your own URLs."
//! * <https://docs.gitlab.com/api/rest/authentication/> — the two header forms
//!   for a personal access token: `--header "PRIVATE-TOKEN: <your_access_token>"`
//!   and the "OAuth-compliant" `--header "Authorization: Bearer
//!   <your_access_token>"`.
//! * <https://docs.gitlab.com/api/rest/troubleshooting/> — the status table this
//!   module's error map is built from: `400 Bad Request`, `401 Unauthorized`,
//!   `403 Forbidden`, `404 Not Found`, `405 Method Not Allowed`, `409 Conflict`,
//!   `412 Precondition Failed`, `422 Unprocessable`, `429 Too Many Requests`,
//!   `500 Server Error`, `503 Service Unavailable`.
//! * The endpoint references for
//!   [issues](https://docs.gitlab.com/api/issues/),
//!   [merge requests](https://docs.gitlab.com/api/merge_requests/),
//!   [notes](https://docs.gitlab.com/api/notes/),
//!   [pipelines](https://docs.gitlab.com/api/pipelines/) and
//!   [projects](https://docs.gitlab.com/api/projects/).
//!
//! # The origin is the instance, and the deployment names it
//!
//! GitLab's own examples are `https://gitlab.example.com`, not a label under a
//! vendor suffix: a self-managed instance lives at whatever host its operator
//! owns, and `gitlab.com` is one such value rather than a template this
//! workspace can compile. `OriginSpec::TemplatedHost` fills one lowercase DNS
//! label under a constant suffix and there is no constant suffix here, so this
//! connector declares `OriginSpec::DeploymentOrigin` — WooCommerce's variant,
//! for WooCommerce's reason ([[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]]),
//! and for the extra reason recorded in
//! `knowledgebase/declarative-saas/decisions/082-*`: the instance is
//! infrastructure this deployment operates rather than a tenant it selects.
//!
//! It is not an escape from fixed origins. The value is read once from
//! `config.settings.instance_origin`, validated by [`validate_instance_origin`]
//! before a listener opens, and becomes the same immutable origin every other
//! connector renders against. Two things are refused there:
//!
//! * **A non-`https` instance**, because the declared credential is a bearer
//!   token and an `http://` instance would put it on the wire in clear.
//! * **An instance under a path.** GitLab supports a relative-URL installation,
//!   an origin is a scheme, a host and a port, and composing one by appending a
//!   configured prefix to every declared path would make the compiled path a
//!   function of configuration. Such a deployment is refused with its
//!   configuration key named.
//!
//! # A project id is one path segment
//!
//! "The ID or URL-encoded path of the project": GitLab accepts both, and the
//! SDK percent-encodes each bound path value inside its own segment, so
//! `group/project` renders as `group%2Fproject` — which is exactly the encoding
//! GitLab asks for — and a value carrying `..` or `?` cannot leave its segment.
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur in GitLab's issues, merge-requests, notes or pipelines references, nor
//! in the REST API guide or its troubleshooting page. It occurs twice in the
//! projects reference and both are behavioural notes on endpoints this module
//! does not declare ("This endpoint is idempotent. Archiving an already-archived
//! project does not change the project", and the same for unarchiving). Each
//! endpoint reference here enumerates its complete supported-attribute table and
//! none of them carries a client-supplied request identifier.
//!
//! The four creates and the pipeline trigger are therefore `AtMostOnce`
//! (ADR 063). Nothing here is `NaturalMethod`: this module declares no `PUT` and
//! no `DELETE`.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{
    JsonTemplate, Operation, OperationBuilder, OperationError, Origin, Required,
};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "gitlab";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the instance's whole origin.
pub const INSTANCE_ORIGIN: &str = "instance_origin";

/// "The path must start with `/api/v4`."
const PREFIX: &str = "/api/v4";

/// "Number of items to list per page (default: `20`, max: `100`)."
const PAGE_SIZE: &str = "100";

/// This connector's declaration.
///
/// Unlike Batch G's templated hosts, nothing here is compiled per deployment:
/// the instance is a configured origin the SDK resolves, so the declaration
/// itself is a constant.
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::deployment_origin(INSTANCE_ORIGIN)
                    .expect("the GitLab origin key is valid"),
            )
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the GitLab declarations are valid"))
            .build()
            .expect("the GitLab declaration is valid")
    });
    &CONNECTOR
}

/// Whether a configured instance origin is one this connector may send its
/// declared credential to.
///
/// See the module documentation: `https` because the credential is a bearer
/// token, and no path because an origin is a scheme, a host, and a port.
pub fn validate_instance_origin(value: &str) -> Result<(), OperationError> {
    let origin = Origin::parse(value)?;
    if origin.as_url().scheme() != "https" {
        return Err(OperationError::new(
            "a GitLab instance origin must be https: this connector's credential is a bearer token \
             and an http instance would carry it in clear",
        ));
    }
    Ok(())
}

/// The ordered error map.
///
/// Every row is GitLab's own published status table. `429` is "The user exceeded
/// the application rate limits", which is a throttle rather than a permanent
/// refusal, and `503` is "The server cannot handle the request because the
/// server is temporarily overloaded" — both retryable, and classifying either
/// `permanent` would end a Process for a condition that clears by itself.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — A required attribute of the API request is
            // missing." "422 Unprocessable — The entity couldn't be processed."
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "401 Unauthorized — The user isn't authenticated." "403 Forbidden
            // — The request isn't allowed."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 405, 409, 412], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the GitLab error map is a valid declaration")
    });
    &MAP
}

/// Decode one GitLab response: the declared success statuses, then the declared
/// contract.
///
/// GitLab reports every failure with a status, so there is no body gate here.
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
/// GitLab publishes the `Link` header for offset pagination and asks callers to
/// follow it — "Be sure to use these links instead of generating your own URLs"
/// — and the last page carries no `next` relation, which is the absence the plan
/// ends on. Every collection here is a bare JSON array at the document root, so
/// the items pointer is the empty pointer (RFC 6901's whole document).
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("", "next").expect("the GitLab link plan is valid")
    });
    match operation_id {
        "issue.list" | "merge_request.list" | "pipeline.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The searched documentation behind every at-most-once class here.
const NO_KEY: &str = "the string `idempot` does not occur in GitLab's published references for \
                      issues, merge requests, notes or pipelines, nor in its REST API guide or its \
                      troubleshooting page; each of those references enumerates the complete \
                      supported-attribute table of every endpoint declared here and none carries a \
                      client-supplied request identifier, deduplication key or request-id header. \
                      The two occurrences in the projects reference are behavioural notes on other \
                      endpoints — \"This endpoint is idempotent. Archiving an already-archived \
                      project does not change the project\" and the same sentence for unarchiving";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// "The ID or URL-encoded path of the project."
fn project(builder: OperationBuilder) -> OperationBuilder {
    builder.path_param("project_id", ValueScalar::String)
}

/// The published issue properties a Process reads.
fn issue_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        // "`iid` — internal project-specific ID", which is what every other
        // issue endpoint's path takes.
        .output_pointer("iid", "/iid", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "project_id",
            "/project_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer("title", "/title", ValueScalar::String, Required::No)
        .output_pointer("state", "/state", ValueScalar::String, Required::No)
        .output_pointer("web_url", "/web_url", ValueScalar::String, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The published merge-request properties a Process reads.
fn merge_request_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("iid", "/iid", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "project_id",
            "/project_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer("title", "/title", ValueScalar::String, Required::No)
        // "`state` — `opened`, `closed`, `merged`, or `locked`."
        .output_pointer("state", "/state", ValueScalar::String, Required::No)
        .output_pointer(
            "source_branch",
            "/source_branch",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "target_branch",
            "/target_branch",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("web_url", "/web_url", ValueScalar::String, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The published note properties a Process reads.
fn note_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("body", "/body", ValueScalar::String, Required::No)
        // "`system` — indicating system-generated notes."
        .output_pointer("system", "/system", ValueScalar::Boolean, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
}

/// The published pipeline properties a Process reads.
fn pipeline_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("iid", "/iid", ValueScalar::Int64, Required::No)
        .output_pointer(
            "project_id",
            "/project_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer("sha", "/sha", ValueScalar::String, Required::No)
        .output_pointer("ref", "/ref", ValueScalar::String, Required::No)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("web_url", "/web_url", ValueScalar::String, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
}

/// A bare JSON array at the document root, which is what every GitLab collection
/// answers with.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        // The collection is a bare JSON array, so the whole document is the
        // output and the walk's aggregate lands in exactly that place.
        .declared_output("items", ValueScalar::Json, Required::Yes)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let project_get = project(common(Operation::get(
        "project.get",
        &format!("{PREFIX}/projects/{{project_id}}"),
    )))
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer(
        "path_with_namespace",
        "/path_with_namespace",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "default_branch",
        "/default_branch",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("web_url", "/web_url", ValueScalar::String, Required::No)
    .output_pointer(
        "visibility",
        "/visibility",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    let issue_get = issue_output(
        project(common(Operation::get(
            "issue.get",
            &format!("{PREFIX}/projects/{{project_id}}/issues/{{issue_iid}}"),
        )))
        .path_param("issue_iid", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let issue_list = collection(
        project(common(Operation::get(
            "issue.list",
            &format!("{PREFIX}/projects/{{project_id}}/issues"),
        )))
        // "`state` — Return `all` issues or just those that are `opened` or
        // `closed`." A declared query input renders on *every* request, so a
        // caller must always supply one — which is why this connector declares
        // a filter only where GitLab publishes a value meaning "everything",
        // and declares none at all where it does not.
        .query_input("state", "state"),
    )
    .effect(Effect::read_only())
    .build()?;

    // "`title` (string, required)".
    let issue_create = issue_output(
        project(common(Operation::post(
            "issue.create",
            &format!("{PREFIX}/projects/{{project_id}}/issues"),
        )))
        .body(JsonTemplate::object([
            ("title", JsonTemplate::input("title")),
            ("description", JsonTemplate::input("description")),
            ("labels", JsonTemplate::input("labels")),
            ("assignee_ids", JsonTemplate::input("assignee_ids")),
            // "`issue_type` — one of `issue`, `incident`, `test_case`, `task`."
            ("issue_type", JsonTemplate::input("issue_type")),
            ("confidential", JsonTemplate::input("confidential")),
        ]))
        .declared_input("title", ValueScalar::String, Required::Yes)
        .declared_input("assignee_ids", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second issue in the same project with a new id and a new iid, and a second round of \
         notifications to everyone watching the project",
    )?)
    .build()?;

    // "`body` — The content of a note. Limited to 1,000,000 characters."
    let issue_note_create = note_output(
        project(common(Operation::post(
            "issue_note.create",
            &format!("{PREFIX}/projects/{{project_id}}/issues/{{issue_iid}}/notes"),
        )))
        .path_param("issue_iid", ValueScalar::Int64)
        .body(JsonTemplate::object([(
            "body",
            JsonTemplate::input("body"),
        )]))
        .declared_input("body", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second comment with the same text on the same issue, and a second notification to \
         everyone participating in it",
    )?)
    .build()?;

    let merge_request_get = merge_request_output(
        project(common(Operation::get(
            "merge_request.get",
            &format!("{PREFIX}/projects/{{project_id}}/merge_requests/{{merge_request_iid}}"),
        )))
        .path_param("merge_request_iid", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let merge_request_list = collection(
        project(common(Operation::get(
            "merge_request.list",
            &format!("{PREFIX}/projects/{{project_id}}/merge_requests"),
        )))
        // "`state` — Return `all` merge requests or just those that are
        // `opened`, `closed`, `locked`, or `merged`."
        .query_input("state", "state"),
    )
    .effect(Effect::read_only())
    .build()?;

    // Required: `source_branch`, `target_branch`, `title`.
    let merge_request_create = merge_request_output(
        project(common(Operation::post(
            "merge_request.create",
            &format!("{PREFIX}/projects/{{project_id}}/merge_requests"),
        )))
        .body(JsonTemplate::object([
            ("source_branch", JsonTemplate::input("source_branch")),
            ("target_branch", JsonTemplate::input("target_branch")),
            ("title", JsonTemplate::input("title")),
            ("description", JsonTemplate::input("description")),
            ("labels", JsonTemplate::input("labels")),
            (
                "remove_source_branch",
                JsonTemplate::input("remove_source_branch"),
            ),
        ]))
        .declared_input("source_branch", ValueScalar::String, Required::Yes)
        .declared_input("target_branch", ValueScalar::String, Required::Yes)
        .declared_input("title", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second merge request between the same two branches with a new id and a new iid — unless \
         GitLab refuses the duplicate, which it publishes no `409` contract for, so which of the \
         two happens is not something this connector can promise either way",
    )?)
    .build()?;

    let merge_request_note_create = note_output(
        project(common(Operation::post(
            "merge_request_note.create",
            &format!("{PREFIX}/projects/{{project_id}}/merge_requests/{{merge_request_iid}}/notes"),
        )))
        .path_param("merge_request_iid", ValueScalar::Int64)
        .body(JsonTemplate::object([(
            "body",
            JsonTemplate::input("body"),
        )]))
        .declared_input("body", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second comment with the same text on the same merge request, and a second notification \
         to its author and reviewers",
    )?)
    .build()?;

    let pipeline_get = pipeline_output(
        project(common(Operation::get(
            "pipeline.get",
            &format!("{PREFIX}/projects/{{project_id}}/pipelines/{{pipeline_id}}"),
        )))
        .path_param("pipeline_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // GitLab publishes fourteen filters for this collection — `ref`, `status`,
    // `scope`, `source`, `username`, four date bounds — and no value meaning
    // "everything" for any of them, so none is declared: a declared query input
    // renders on every request and would therefore be mandatory.
    let pipeline_list = collection(project(common(Operation::get(
        "pipeline.list",
        &format!("{PREFIX}/projects/{{project_id}}/pipelines"),
    ))))
    .effect(Effect::read_only())
    .build()?;

    // "`ref` — The branch or tag to run the pipeline on." This is the operation
    // spec 027 §3's `triggering_is_not_a_read` proof exists for: it looks like a
    // small `POST` and it starts a build that deploys things.
    let pipeline_trigger = pipeline_output(
        project(common(Operation::post(
            "pipeline.trigger",
            &format!("{PREFIX}/projects/{{project_id}}/pipeline"),
        )))
        .body(JsonTemplate::object([
            ("ref", JsonTemplate::input("ref")),
            // "An array of hashes containing the variables available in the
            // pipeline."
            ("variables", JsonTemplate::input("variables")),
        ]))
        .declared_input("ref", ValueScalar::String, Required::Yes)
        .declared_input("variables", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second pipeline run on the same ref with a new id, which runs every job in it again — \
         including any deployment, release or external call those jobs make",
    )?)
    .build()?;

    Ok(vec![
        project_get,
        issue_get,
        issue_list,
        issue_create,
        issue_note_create,
        merge_request_get,
        merge_request_list,
        merge_request_create,
        merge_request_note_create,
        pipeline_get,
        pipeline_list,
        pipeline_trigger,
    ])
}
