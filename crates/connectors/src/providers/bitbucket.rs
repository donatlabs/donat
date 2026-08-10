//! Bitbucket Cloud's REST API v2 — repositories, issues and pull requests.
//!
//! Ground truth is Atlassian's own published documentation and the Swagger
//! description Bitbucket serves for itself, read on 2026-08-10:
//!
//! * <https://api.bitbucket.org/swagger.json> — Bitbucket's own machine-readable
//!   description, `host: api.bitbucket.org`, `basePath: /2.0`, `schemes:
//!   [https]`, with `securityDefinitions.basic` described as "Basic HTTP
//!   Authentication as per [RFC-2617](https://tools.ietf.org/html/rfc2617)
//!   (Digest not supported)".
//! * <https://developer.atlassian.com/cloud/bitbucket/rest/intro/> — the
//!   authentication section: "API Tokens are personal access tokens that users
//!   can create to authenticate with Bitbucket's REST APIs … They are designed
//!   as a long term replacement for app passwords", and "To authenticate with an
//!   API token, use Basic HTTP Authentication as per RFC-2617, where the
//!   username is your Atlassian email and password is the API token."
//! * The same page's pagination section: "Paginated collections are always
//!   wrapped in the following wrapper object" — `size`, `page`, `pagelen`,
//!   `next`, `previous`, `values` — with "`pagelen` … Globally, the minimum
//!   length is 10 and the maximum is 100", "`next` Link to the next page if it
//!   exists. The last page of a collection does not have this value. Use this
//!   link to navigate the result set and refrain from constructing your own
//!   URLs", and "clients are not expected to construct URLs themselves by
//!   manipulating the page number query parameter".
//!
//! # The credential is a Basic pair whose username is the deployment's address
//!
//! Bitbucket's published Basic form takes the Atlassian account email as the
//! username and the API token as the password, so the username is declaration
//! material and the token is the secret — the Jira shape of
//! [[064-a-credentials-scheme-and-its-username-are-the-providers]] and a
//! declaration one deployment completes
//! ([[048-a-declaration-a-deployment-completes]]). The email is not a secret and
//! is refused at deploy time rather than at the first `401`.
//!
//! # Pagination
//!
//! `next` is a whole URL Bitbucket asks callers to follow rather than rebuild,
//! so the declared plan is `Pagination::next_uri_in_body` — the continuation is
//! a *destination*, resolved against the compiled origin and refused when it
//! lands anywhere else, exactly as a `Link` continuation is
//! ([[057-rotation-is-a-write-and-a-continuation-is-a-destination]]). It ends on
//! the absence Bitbucket publishes: the last page carries no `next`.
//!
//! # Effect classification
//!
//! **Machine-readable description, no key in it.** The string `idempot` occurs
//! twice in Bitbucket's whole published Swagger and both are on default-reviewer
//! endpoints this module does not declare: "Adds the specified user to the
//! repository's list of default reviewers. This method is idempotent. Adding a
//! user a second time has no effect", and the same statement for a project's
//! list. No endpoint declared here publishes a client-supplied request
//! identifier, a deduplication key, or a repeat statement.
//!
//! The four creates are therefore `AtMostOnce` (ADR 063). Nothing here is
//! `NaturalMethod`: this module declares no `PUT` and no `DELETE`.

use std::sync::LazyLock;

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
pub const NAME: &str = "bitbucket";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Bitbucket's published API host.
const ORIGIN: &str = "https://api.bitbucket.org";

/// `basePath: /2.0`.
const PREFIX: &str = "/2.0";

/// The deploy-time configuration key carrying the Atlassian account address that
/// is the HTTP Basic *username*.
pub const ACCOUNT_EMAIL: &str = "account_email";

/// "Globally, the minimum length is 10 and the maximum is 100."
const PAGE_SIZE: &str = "100";

/// One deployment's declaration.
///
/// `account_email` is the Basic username, which `AuthPlan::basic` takes where the
/// plan is built — so this declaration is completed by one deployment, exactly
/// as Jira's and WooCommerce's are.
pub fn connector(account_email: &str) -> Result<Connector, OperationError> {
    validate_account_email(account_email)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::fixed(ORIGIN).expect("Bitbucket's published origin is valid"))
        .credential(
            CredentialSpec::for_plan(AuthPlan::basic(account_email)?)
                .with_field(ACCOUNT_EMAIL, FieldClassification::NonSecret),
        )
        .operations(operations()?)
        .build()
}

/// The declaration a reviewer and the registry read, with a placeholder address
/// no deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("deployment-configured@example.invalid")
}

/// The Basic username's grammar.
///
/// Bitbucket publishes the rule — "the username is your Atlassian email" — and
/// no address format beyond it, so what is checked here is what would make the
/// *request* wrong: an empty value, a value carrying a colon (which would forge
/// the Basic separator), a value that is not printable ASCII, or one that is not
/// an address at all.
pub fn validate_account_email(account_email: &str) -> Result<(), OperationError> {
    let invalid = || {
        OperationError::new(
            "the Bitbucket account email must be one printable-ASCII Atlassian account address",
        )
    };
    if account_email.len() > 254 {
        return Err(invalid());
    }
    let Some((local, domain)) = account_email.split_once('@') else {
        return Err(invalid());
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !domain.contains('.') {
        return Err(invalid());
    }
    AuthPlan::basic(account_email).map(|_| ())
}

/// The ordered error map.
///
/// Bitbucket publishes its statuses per endpoint — `400`, `401`, `403`, `404`,
/// and `410` for a deleted issue — and an error body of `type`, `error.message`
/// and `error.detail`. This map reads none of that body: the classification is
/// the status, and provider prose never crosses the boundary.
///
/// `429` is mapped even though Bitbucket's REST intro does not name a status for
/// its rate limiting: `429` is the one HTTP defines for it, and classifying a
/// throttle `permanent` would end a Process for a condition that clears by
/// itself.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 405, 409, 410], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Bitbucket error map is a valid declaration")
    });
    &MAP
}

/// Decode one Bitbucket response: the declared success statuses, then the
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

/// The continuation plan of each collection.
///
/// Every paginated collection is the same wrapper object, so every walked
/// operation shares one plan: the items live at `/values` and the continuation
/// is the whole URL at `/next`.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::next_uri_in_body("/values", "/next")
            .expect("the Bitbucket continuation plan is valid")
    });
    match operation_id {
        "issue.list" | "pull_request.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The searched documentation behind every at-most-once class here.
const NO_KEY: &str = "the string `idempot` occurs twice in Bitbucket's own published Swagger \
                      description of the whole v2 REST API (`https://api.bitbucket.org/swagger.json`) \
                      and both are on default-reviewer endpoints this module does not declare — \
                      \"Adds the specified user to the repository's list of default reviewers. This \
                      method is idempotent. Adding a user a second time has no effect\", and the \
                      same statement for a project's list. No endpoint declared here publishes a \
                      client-supplied request identifier, a deduplication key, or any statement \
                      about the effect of repeating a create";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// Every repository-scoped path takes the workspace and the repository slug.
fn repository(builder: OperationBuilder) -> OperationBuilder {
    builder
        .path_param("workspace", ValueScalar::String)
        .path_param("repo_slug", ValueScalar::String)
}

/// The published issue properties a Process reads.
fn issue_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("title", "/title", ValueScalar::String, Required::No)
        .output_pointer("state", "/state", ValueScalar::String, Required::No)
        .output_pointer("kind", "/kind", ValueScalar::String, Required::No)
        .output_pointer("priority", "/priority", ValueScalar::String, Required::No)
        .output_pointer(
            "created_on",
            "/created_on",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "html_url",
            "/links/html/href",
            ValueScalar::String,
            Required::No,
        )
}

/// The published pull-request properties a Process reads.
fn pull_request_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("title", "/title", ValueScalar::String, Required::No)
        .output_pointer("state", "/state", ValueScalar::String, Required::No)
        .output_pointer(
            "source_branch",
            "/source/branch/name",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "destination_branch",
            "/destination/branch/name",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "created_on",
            "/created_on",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "html_url",
            "/links/html/href",
            ValueScalar::String,
            Required::No,
        )
}

/// The published comment properties a Process reads.
fn comment_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "content_raw",
            "/content/raw",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "created_on",
            "/created_on",
            ValueScalar::String,
            Required::No,
        )
}

/// One paginated collection: the wrapper object Bitbucket answers every list
/// with, read through the pointers the plan's aggregate lands on.
fn collection(builder: OperationBuilder) -> OperationBuilder {
    builder
        .query_static("pagelen", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer("values", "/values", ValueScalar::Json, Required::Yes)
        // "`size` … This is an optional element that is not provided in all
        // responses, as it can be expensive to compute."
        .output_pointer("size", "/size", ValueScalar::Int64, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Returns the object describing this repository."
    let repository_get = repository(common(Operation::get(
        "repository.get",
        &format!("{PREFIX}/repositories/{{workspace}}/{{repo_slug}}"),
    )))
    .success_statuses([StatusCode::OK])
    .output_pointer("uuid", "/uuid", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("full_name", "/full_name", ValueScalar::String, Required::No)
    .output_pointer(
        "mainbranch",
        "/mainbranch/name",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "is_private",
        "/is_private",
        ValueScalar::Boolean,
        Required::No,
    )
    .output_pointer(
        "html_url",
        "/links/html/href",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Returns the specified issue."
    let issue_get = issue_output(
        repository(common(Operation::get(
            "issue.get",
            &format!("{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/issues/{{issue_id}}"),
        )))
        .path_param("issue_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Returns the issues in the issue tracker."
    //
    // Bitbucket publishes `q` and `sort` for this collection and no value
    // meaning "everything" for either. A declared query input renders on every
    // request, so declaring one would make it mandatory; the unfiltered list is
    // what Bitbucket answers by default and is what this operation asks for.
    let issue_list = collection(repository(common(Operation::get(
        "issue.list",
        &format!("{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/issues"),
    ))))
    .effect(Effect::read_only())
    .build()?;

    // "Creates a new issue. … The authenticated user is used for the issue's
    // `reporter` field."
    let issue_create = issue_output(
        repository(common(Operation::post(
            "issue.create",
            &format!("{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/issues"),
        )))
        .body(JsonTemplate::object([
            ("title", JsonTemplate::input("title")),
            (
                "content",
                JsonTemplate::object([("raw", JsonTemplate::input("content"))]),
            ),
            ("kind", JsonTemplate::input("kind")),
            ("priority", JsonTemplate::input("priority")),
        ]))
        .declared_input("title", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second issue in the same tracker with a new id, and a second notification to everyone \
         watching the repository",
    )?)
    .build()?;

    // "Creates a new issue comment", with the published body
    // `{"content": {"raw": "Lorem ipsum."}}`.
    let issue_comment_create = comment_output(
        repository(common(Operation::post(
            "issue_comment.create",
            &format!(
                "{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/issues/{{issue_id}}/comments"
            ),
        )))
        .path_param("issue_id", ValueScalar::Int64)
        .body(JsonTemplate::object([(
            "content",
            JsonTemplate::object([("raw", JsonTemplate::input("content"))]),
        )]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        // Bitbucket's reference publishes no response schema for this `201` and
        // publishes one for the pull-request comment beside it ("Returns the
        // newly created pull request comment"), so both are declared to answer
        // the created comment and a body that does not carry one is a contract
        // violation rather than a silent empty success.
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second comment with the same text on the same issue, and a second notification to \
         everyone following it",
    )?)
    .build()?;

    // "Returns the specified pull request."
    let pull_request_get = pull_request_output(
        repository(common(Operation::get(
            "pull_request.get",
            &format!(
                "{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/pullrequests/\
                 {{pull_request_id}}"
            ),
        )))
        .path_param("pull_request_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Returns all pull requests on the specified repository. By default only
    // open pull requests are returned. This can be controlled using the `state`
    // query parameter" — whose published values are the four states and no
    // value meaning "all", so this operation takes Bitbucket's own default
    // rather than making a filter mandatory.
    let pull_request_list = collection(repository(common(Operation::get(
        "pull_request.list",
        &format!("{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/pullrequests"),
    ))))
    .effect(Effect::read_only())
    .build()?;

    // "The minimum required fields to create a pull request are `title` and
    // `source`, specified by a branch name."
    let pull_request_create = pull_request_output(
        repository(common(Operation::post(
            "pull_request.create",
            &format!("{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/pullrequests"),
        )))
        .body(JsonTemplate::object([
            ("title", JsonTemplate::input("title")),
            (
                "source",
                JsonTemplate::object([(
                    "branch",
                    JsonTemplate::object([("name", JsonTemplate::input("source_branch"))]),
                )]),
            ),
            (
                "destination",
                JsonTemplate::object([(
                    "branch",
                    JsonTemplate::object([("name", JsonTemplate::input("destination_branch"))]),
                )]),
            ),
            (
                "summary",
                JsonTemplate::object([("raw", JsonTemplate::input("summary"))]),
            ),
            (
                "close_source_branch",
                JsonTemplate::input("close_source_branch"),
            ),
        ]))
        .declared_input("title", ValueScalar::String, Required::Yes)
        .declared_input("source_branch", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second pull request between the same two branches with a new id, and a second review \
         request to everyone Bitbucket adds as a default reviewer",
    )?)
    .build()?;

    // "Creates a new pull request comment. Returns the newly created pull
    // request comment."
    let pull_request_comment_create = comment_output(
        repository(common(Operation::post(
            "pull_request_comment.create",
            &format!(
                "{PREFIX}/repositories/{{workspace}}/{{repo_slug}}/pullrequests/\
                 {{pull_request_id}}/comments"
            ),
        )))
        .path_param("pull_request_id", ValueScalar::Int64)
        .body(JsonTemplate::object([(
            "content",
            JsonTemplate::object([("raw", JsonTemplate::input("content"))]),
        )]))
        .declared_input("content", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(at_most_once(
        "a second comment with the same text on the same pull request, and a second notification \
         to its author and reviewers",
    )?)
    .build()?;

    Ok(vec![
        repository_get,
        issue_get,
        issue_list,
        issue_create,
        issue_comment_create,
        pull_request_get,
        pull_request_list,
        pull_request_create,
        pull_request_comment_create,
    ])
}
