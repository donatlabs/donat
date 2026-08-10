//! Jira Cloud's platform REST API v3 — the batch's templated Atlassian site.
//!
//! Ground truth is Atlassian's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://developer.atlassian.com/cloud/jira/platform/basic-auth-for-rest-apis/>
//!   — "Build a string of the form `useremail:api_token`", "BASE64 encode the
//!   string", and "Supply an `Authorization` header with content `Basic`
//!   followed by the encoded string."
//! * <https://developer.atlassian.com/cloud/jira/platform/rate-limiting/> — "When
//!   any limit is exceeded, Jira returns an HTTP `429 Too Many Requests`
//!   response", `Retry-After` is "Only returned with 429 responses. Indicates how
//!   many seconds to wait before retrying", and "Some transient 5xx responses
//!   (such as 503) may also include a `Retry-After` header."
//! * The published OpenAPI description
//!   (`https://developer.atlassian.com/cloud/jira/platform/swagger-v3.v3.json`),
//!   whose `servers` is `https://your-domain.atlassian.net`, for every path,
//!   parameter, and schema below — including `ErrorCollection`, whose only
//!   fields are `errorMessages` ("The list of error messages produced by this
//!   operation"), `errors` (a map of parameter to message), and `status`.
//!
//! # The site is deploy-time configuration, and so is the email
//!
//! Two values complete this declaration and neither can come from a request.
//!
//! The **site** is the host: `https://{site}.atlassian.net`. It is an
//! `OriginSpec::TemplatedHost` filled only from `config.settings.site`, exactly
//! as Shopify's shop label is, because an operation input that could reach it
//! would be an operation input choosing an authority.
//!
//! The **email** is the HTTP Basic username, and `AuthPlan::basic` takes its
//! username where the plan is built. This connector's declaration is therefore
//! built per deployment — [`connector`] takes the address — for the same reason
//! Twilio's is ([[048-a-declaration-a-deployment-completes]]): a `&'static`
//! declaration would have to carry a placeholder username, which is a credential
//! contract that does not describe what reaches the wire.
//!
//! # `issue.search` is a cursor, not an offset
//!
//! Spec 016 §1 records Jira's pagination as `OffsetLimit`, and that is true of
//! `comment.list` — `startAt` is "The index of the first item to return in a page
//! of results (page offset)" — and no longer true of the issue search. Atlassian
//! marks `GET /rest/api/3/search` "Currently being removed" and `deprecated:
//! true`; its replacement, `GET /rest/api/3/search/jql`, paginates with
//! `nextPageToken`: "The token for a page to fetch that is not the first page.
//! The first page has a `nextPageToken` of `null`." So this module declares both
//! plans, each for the endpoint that publishes it.
//!
//! # An issue key is one percent-encoded segment
//!
//! `{issueIdOrKey}` binds through the SDK's path renderer, which percent-encodes
//! every non-alphanumeric byte, so `ACM-42` arrives as `ACM%2D42`. That is
//! equivalent under RFC 3986 §2.3 and is the same encoding `github.file.get`
//! sends a path in; it is what keeps a value that spelled `../` inside its own
//! segment.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Atlassian's whole published Jira platform OpenAPI description: no request
//! header, no body property, no response field. `issue.create`,
//! `issue.transition` and `comment.add` are therefore `AtMostOnce` (ADR 063):
//! each answers with a fresh identifier or a fresh comment, which is the
//! provider's own evidence that a repeat produces a second one, and a Process
//! reaches them only by declaring `at_most_once` and a route for an outcome
//! nobody can know.
//!
//! `issue.update` is a `PUT` and is still `InventoryOnly`: Atlassian documents it
//! as "Edits an issue", whose "edits to the issue's fields are defined using
//! `update` and `fields`" — a partial edit against an issue, not a replacement of
//! it. Spec 010 §7's `NaturalMethod` evidence is a statement that the endpoint
//! writes a fixed resource identity, and Atlassian publishes the opposite. ADR
//! 063 does not reach it either: what a repeated partial edit produces is what
//! Atlassian does not publish.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "jira";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key that fills the templated host: the site's
/// own `atlassian.net` label.
pub const SITE: &str = "site";

/// The deploy-time configuration key carrying the Basic username.
///
/// It is not a secret — Atlassian pairs it with the API token, which is — so it
/// lives in `config.settings` and is published as a non-secret credential field.
pub const EMAIL: &str = "email";

/// "`maxResults` — The maximum number of items to return per page."
const PAGE_SIZE: u32 = 100;

/// One deployment's declaration.
///
/// `email` is the Basic username, which Atlassian documents as the account's own
/// address: "Build a string of the form `useremail:api_token`".
pub fn connector(email: &str) -> Result<Connector, OperationError> {
    validate_email(email)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::templated_host(
            "https",
            "{site}.atlassian.net",
            None,
        )?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::basic(email)?)
                .with_field(EMAIL, crate::sdk::connector::FieldClassification::NonSecret),
        )
        .operations(operations()?)
        .build()
}

/// The Basic username's grammar, checked where the declaration is built so a
/// mistyped address is a startup refusal rather than a `401` on the first
/// activity attempt.
///
/// `AuthPlan::basic` already refuses an empty username, a colon, and anything
/// outside printable ASCII. What is added here is Atlassian's own requirement
/// that the value is an email address rather than an account name.
fn validate_email(email: &str) -> Result<(), OperationError> {
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or_default();
    let domain = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || local.is_empty()
        || domain.is_empty()
        || !domain.contains('.')
        || email.chars().any(char::is_whitespace)
    {
        return Err(OperationError::new(
            "the Jira Basic username must be the account's email address, as \
             `useremail:api_token`",
        ));
    }
    Ok(())
}

/// The declaration a reviewer and the registry read, with a placeholder
/// username no deployment uses.
///
/// It exists so the module table can carry this connector's identity and
/// operation set; a deployment is always compiled against
/// [`connector`] with its own configured address.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("deployment.configured@example.invalid")
}

/// The ordered error map.
///
/// Jira's `ErrorCollection` carries `errorMessages` and a per-parameter `errors`
/// map, both of them prose, and no stable machine-readable code anywhere — so
/// the map is keyed on the documented statuses only and the body is never read.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // A malformed JQL, an unknown field, a body Jira cannot apply.
            .on_statuses([400, 413, 422], ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404" for an issue this account cannot browse — Jira answers the
            // same status for absent and forbidden — "409" for a concurrent
            // edit, "410" for a resource that is gone.
            .on_statuses([404, 405, 409, 410], ConnectorErrorClass::Permanent)
            // "When any limit is exceeded, Jira returns an HTTP `429 Too Many
            // Requests` response."
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Jira error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection.
///
/// Two plans, because Jira publishes two: the enhanced search walks a
/// `nextPageToken` cursor, and the comment collection walks `startAt`, "The
/// index of the first item to return in a page of results (page offset)."
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static ISSUES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/issues",
            "nextPageToken",
            "/nextPageToken",
            "maxResults",
            PAGE_SIZE,
        )
        .expect("the Jira search cursor plan is valid")
    });
    static COMMENTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/comments", "startAt", "maxResults", PAGE_SIZE)
            .expect("the Jira comment offset plan is valid")
    });
    match operation_id {
        "issue.search" => Some(&ISSUES),
        "comment.list" => Some(&COMMENTS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        // Jira answers `application/json`; asking for it explicitly is what the
        // platform documentation's own examples do.
        .static_header("Accept", "application/json")
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur once in Atlassian's whole published Jira \
                      platform OpenAPI description: no request header, no body property, and no \
                      response field carries a client-supplied request identifier or a \
                      deduplication behaviour";

/// One write whose repeat would leave a second thing behind (ADR 063).
///
/// The search is the module's and the consequence is the operation's: both are
/// what a Process author accepts when they declare `at_most_once`.
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        repeat_produces,
    )?))
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let issue_get = common(Operation::get(
        "issue.get",
        "/rest/api/3/issue/{issueIdOrKey}",
    ))
    .path_param("issueIdOrKey", ValueScalar::String)
    // "A list of fields to return for the issue ... Use it to retrieve a subset
    // of fields." Which subset a caller wants is the caller's.
    .query_input("fields", "fields")
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("key", "/key", ValueScalar::String, Required::Yes)
    .output_pointer("self", "/self", ValueScalar::String, Required::Yes)
    .output_pointer("fields", "/fields", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Searches for issues using JQL ... For performance reasons, this parameter
    // requires a bounded query."
    let issue_search = common(Operation::get("issue.search", "/rest/api/3/search/jql"))
        .query_input("jql", "jql")
        .query_input("fields", "fields")
        .query_static("maxResults", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("issues", "/issues", ValueScalar::Json, Required::Yes)
        // "Continuation token to fetch the next page. If this result represents
        // the last or the only page this token will be null."
        .output_pointer(
            "next_page_token",
            "/nextPageToken",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("is_last", "/isLast", ValueScalar::Boolean, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "The content of the issue or subtask is defined using `update` and
    // `fields`." Both are declared inputs, because which of them a caller uses
    // is Jira's own choice to offer.
    let issue_create = common(Operation::post("issue.create", "/rest/api/3/issue"))
        .body(JsonTemplate::object([
            ("fields", JsonTemplate::input("fields")),
            ("update", JsonTemplate::input("update")),
        ]))
        .declared_input("fields", ValueScalar::Json, Required::Yes)
        .declared_input("update", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("key", "/key", ValueScalar::String, Required::Yes)
        .output_pointer("self", "/self", ValueScalar::String, Required::Yes)
        .effect(at_most_once("a second issue with a new key")?)
        .build()?;

    // "Edits an issue." The documented default response is `204 No Content`; the
    // `200` form needs `returnIssue=true`, which this declaration does not send,
    // so silence is the documented success.
    let issue_update = common(Operation::put(
        "issue.update",
        "/rest/api/3/issue/{issueIdOrKey}",
    ))
    .path_param("issueIdOrKey", ValueScalar::String)
    .body(JsonTemplate::object([
        ("fields", JsonTemplate::input("fields")),
        ("update", JsonTemplate::input("update")),
    ]))
    .declared_input("fields", ValueScalar::Json, Required::Yes)
    .declared_input("update", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::inventory_only(
        "Atlassian documents `PUT /rest/api/3/issue/{issueIdOrKey}` as \"Edits an issue\" whose \
         \"edits to the issue's fields are defined using `update` and `fields`\" — a partial edit \
         rather than a write to a fixed resource identity — so spec 010 §7's NaturalMethod \
         evidence is not there to cite even though the method is right; no idempotency key is \
         published for it either",
    )?)
    .build()?;

    // "Performs an issue transition." Documented response: `204 No Content`.
    let issue_transition = common(Operation::post(
        "issue.transition",
        "/rest/api/3/issue/{issueIdOrKey}/transitions",
    ))
    .path_param("issueIdOrKey", ValueScalar::String)
    .body(JsonTemplate::object([
        ("transition", JsonTemplate::input("transition")),
        ("fields", JsonTemplate::input("fields")),
    ]))
    .declared_input("transition", ValueScalar::Json, Required::Yes)
    .declared_input("fields", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(at_most_once(
        "a transition applied a second time when the workflow still offers it — and a refusal \
             Atlassian does not publish as a contract when it does not",
    )?)
    .build()?;

    let comment_list = common(Operation::get(
        "comment.list",
        "/rest/api/3/issue/{issueIdOrKey}/comment",
    ))
    .path_param("issueIdOrKey", ValueScalar::String)
    .query_static("maxResults", &PAGE_SIZE.to_string())
    .success_statuses([StatusCode::OK])
    .output_pointer("comments", "/comments", ValueScalar::Json, Required::Yes)
    .output_pointer("start_at", "/startAt", ValueScalar::Int64, Required::No)
    .output_pointer(
        "max_results",
        "/maxResults",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer("total", "/total", ValueScalar::Int64, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "Adds a comment to an issue." The body is an Atlassian Document Format
    // value, which is a JSON document rather than a string, so it is declared as
    // one.
    let comment_add = common(Operation::post(
        "comment.add",
        "/rest/api/3/issue/{issueIdOrKey}/comment",
    ))
    .path_param("issueIdOrKey", ValueScalar::String)
    .body(JsonTemplate::object([(
        "body",
        JsonTemplate::input("body"),
    )]))
    .declared_input("body", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::CREATED])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("self", "/self", ValueScalar::String, Required::Yes)
    .output_pointer("created", "/created", ValueScalar::String, Required::No)
    .effect(at_most_once(
        "a second comment on the same issue, with a new id",
    )?)
    .build()?;

    // Jira publishes no "list an issue's attachments" endpoint: an attachment is
    // a *field* of the issue, and `GET /rest/api/3/attachment/{id}` reads one by
    // its own id. This operation is therefore the issue read narrowed to that one
    // field, which is what `fields` is documented for — "Use it to retrieve a
    // subset of fields" — rather than an endpoint invented here.
    let attachment_list = common(Operation::get(
        "attachment.list",
        "/rest/api/3/issue/{issueIdOrKey}",
    ))
    .path_param("issueIdOrKey", ValueScalar::String)
    .query_static("fields", "attachment")
    .success_statuses([StatusCode::OK])
    .output_pointer("key", "/key", ValueScalar::String, Required::Yes)
    .output_pointer(
        "attachments",
        "/fields/attachment",
        ValueScalar::Json,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    // "`accountId` — The account ID of the user, which uniquely identifies the
    // user across all Atlassian products." The `username` and `key` parameters
    // are documented as "no longer available", so neither is declared.
    let user_get = common(Operation::get("user.get", "/rest/api/3/user"))
        // The input is named `user_account_id` rather than `account_id`: the
        // latter is a deploy-time value on the AWS connectors, and one workspace
        // keeps one meaning for one input name.
        .query_input("accountId", "user_account_id")
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "account_id",
            "/accountId",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "display_name",
            "/displayName",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("active", "/active", ValueScalar::Boolean, Required::Yes)
        // "Privacy controls are applied to the response based on the user's
        // preferences. This could mean, for example, that the user's email
        // address is hidden."
        .output_pointer(
            "email_address",
            "/emailAddress",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        issue_get,
        issue_search,
        issue_create,
        issue_update,
        issue_transition,
        comment_list,
        comment_add,
        attachment_list,
        user_get,
    ])
}
