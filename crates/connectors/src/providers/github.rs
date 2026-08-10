//! GitHub's REST API and its signed webhook deliveries.
//!
//! Ground truth is GitHub's own published documentation, read on 2026-08-10:
//!
//! * <https://docs.github.com/en/rest/using-the-rest-api/getting-started-with-the-rest-api>
//!   — "The full path is a URL that includes the base URL for the GitHub REST
//!   API (`https://api.github.com`) and the path of the endpoint", and "Most
//!   GitHub REST API endpoints specify that you should pass an `Accept` header
//!   with a value of `application/vnd.github+json`."
//! * <https://docs.github.com/en/rest/authentication/authenticating-to-the-rest-api>
//!   — "In most cases, you can use `Authorization: Bearer` or `Authorization:
//!   token` to pass a token."
//! * <https://docs.github.com/en/rest/about-the-rest-api/api-versions> — "You
//!   should use the `X-GitHub-Api-Version` header to specify an API version",
//!   and "Requests without the `X-GitHub-Api-Version` header will default to
//!   use the `2022-11-28` version."
//! * <https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api>
//!   — "Requests without a valid `User-Agent` header will be rejected", the
//!   `422 Unprocessable Entity` "Validation Failed" contract, and "If you make
//!   a request to access a private resource and your request isn't properly
//!   authenticated, you will receive a `404 Not Found` response."
//! * <https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api>
//!   — "If you exceed your primary rate limit, you will receive a `403` or
//!   `429` response", and the `x-ratelimit-*` / `retry-after` headers.
//! * <https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api>
//!   — "When a response is paginated, the response headers will include a
//!   `link` header", and "The query parameters in the `link` URLs may differ
//!   between endpoints".
//! * The endpoint references for
//!   [issues](https://docs.github.com/en/rest/issues/issues),
//!   [issue comments](https://docs.github.com/en/rest/issues/comments),
//!   [pulls](https://docs.github.com/en/rest/pulls/pulls),
//!   [repos](https://docs.github.com/en/rest/repos/repos),
//!   [releases](https://docs.github.com/en/rest/releases/releases),
//!   [contents](https://docs.github.com/en/rest/repos/contents), and
//!   [workflows](https://docs.github.com/en/rest/actions/workflows).
//! * <https://docs.github.com/en/webhooks/using-webhooks/validating-webhook-deliveries>
//!   and <https://docs.github.com/en/webhooks/webhook-events-and-payloads> for
//!   the inbound half.
//!
//! # API version
//!
//! The declaration pins `X-GitHub-Api-Version: 2026-03-10`, because omitting
//! the header silently pins a deployment to `2022-11-28` and GitHub's own
//! breaking-changes page records that the workflow dispatch response moved from
//! `204 No Content` to "`200` - Response including the workflow run ID and
//! URLs" in exactly that version. A declared success status has to name one of
//! the two, and pinning the version is what makes the choice honest.
//!
//! # Pagination
//!
//! GitHub publishes `link` with `rel="next"` and states that the query
//! parameters inside those URLs differ between endpoints — the repository issues
//! collection already answers with an opaque `after` cursor rather than a page
//! number. `Pagination::link_header` is therefore the only plan that describes
//! it: the continuation is followed as a destination, checked against the
//! compiled origin, and never rebuilt from a page counter here.
//!
//! # Effect classification
//!
//! GitHub's REST reference publishes **no** idempotency key, client-supplied
//! request identifier, or request deduplication for any endpoint in this set.
//! The complete published request contract for `POST /repos/{owner}/{repo}/issues`
//! is the `accept`, `Authorization`, `X-GitHub-Api-Version` and `User-Agent`
//! headers, the `owner`/`repo` path parameters, and a body whose only required
//! field is `title` — there is no key in it. `issue.create`,
//! `issue.comment_create` and `workflow.dispatch` are therefore `AtMostOnce`
//! (ADR 063): each leaves a second issue, comment, or workflow run behind.
//! `issue.update` stays `InventoryOnly` — it is a `PATCH`, which spec 010 §7
//! does not admit for `NaturalMethod`, and GitHub publishes nothing that tells
//! an absolute update body from a relative one, so what a repeat produces is
//! unrecorded.
//!
//! `file.put` is the one executable mutation. GitHub documents `PUT
//! /repos/{owner}/{repo}/contents/{path}` as "Creates a new file or replaces an
//! existing file in a repository", and requires the blob identity of what is
//! being replaced — "Required if you are updating a file. The blob SHA of the
//! file being replaced" — with a documented `409 Conflict` when that identity no
//! longer holds. Two identical sends therefore leave one file at one path with
//! one content, which is the `NaturalMethod` question. What GitHub does *not*
//! publish is what the second send *answers*: a repeat after a successful first
//! write carries a `sha` the file no longer has, so it is refused rather than
//! applied. The class is about what the repository holds, not about the status a
//! retry sees, and this connector's error map classifies that `409` as
//! `Permanent` rather than pretending it succeeded.
//!
//! # A file path is one percent-encoded segment
//!
//! `file.get` and `file.put` bind GitHub's `{path}` through the SDK's path
//! renderer, which percent-encodes every non-alphanumeric byte, so a nested
//! path arrives as `docs%2Freadme%2Emd`. That is the same shape `aws_s3` sends
//! an object key in, and it is deliberate: a path value that could carry a raw
//! `/` is a path value that could leave its segment.

use std::sync::LazyLock;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::providers::inbound::{EventIdentifier, TriggerEvent};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec, Trigger};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationError, Required};
use crate::sdk::pagination::Pagination;
use crate::sdk::webhook::{SignatureEncoding, WebhookVerifier};

/// The connector name a deployment selects.
pub const NAME: &str = "github";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "the base URL for the GitHub REST API (`https://api.github.com`)".
const ORIGIN: &str = "https://api.github.com";

/// The pinned REST contract version; see the module documentation.
const API_VERSION: &str = "2026-03-10";

/// "Requests without a valid `User-Agent` header will be rejected. You should
/// use your username or the name of your application for the `User-Agent`
/// value."
const USER_AGENT: &str = "donat-connector-github";

/// "For most endpoints, the maximum value of `per_page` is `100`."
const PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        let mut builder = Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("GitHub's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the GitHub declarations are valid"));
        for event in events() {
            builder = builder.trigger(
                Trigger::webhook(event.provider_event(), VERSION, verification())
                    .expect("a GitHub trigger declaration is valid"),
            );
        }
        builder.build().expect("the GitHub declaration is valid")
    });
    &CONNECTOR
}

/// GitHub's inbound signature scheme.
///
/// "The hash signature will appear in each delivery as the value of the
/// `X-Hub-Signature-256` header", "GitHub uses an HMAC hex digest to compute the
/// hash", "The hash signature is generated using your webhook's secret token and
/// the payload contents", and "The hash signature always starts with `sha256=`".
/// The delivery-headers table is more exact still: `X-Hub-Signature-256` "is the
/// HMAC hex digest of the request body, and is generated using the SHA-256 hash
/// function and the `secret` as the HMAC `key`."
///
/// The legacy `X-Hub-Signature` header is deliberately not declared: GitHub says
/// it "uses the HMAC-SHA1 algorithm and is only included for legacy purposes."
pub fn verification() -> WebhookVerifier {
    WebhookVerifier::hmac_body_with_prefix("X-Hub-Signature-256", "sha256=", SignatureEncoding::Hex)
        .expect("the GitHub signature scheme is a valid declaration")
}

/// The inbound events this connector declares (spec 013 §3).
///
/// The event identifier is GitHub's own: `X-GitHub-Delivery` is "A globally
/// unique identifier (GUID) to identify the event", and it is the only
/// client-facing unique delivery identifier GitHub publishes. `push` is the one
/// event here with **no** `action` field — its payload table has no such
/// property — so its declaration does not claim one.
pub fn events() -> &'static [TriggerEvent] {
    static EVENTS: LazyLock<Vec<TriggerEvent>> = LazyLock::new(|| {
        vec![
            TriggerEvent::declare(
                "issues",
                EventIdentifier::Header(DELIVERY_HEADER),
                [
                    ("action", "/action", ValueScalar::String, Required::Yes),
                    ("issue_id", "/issue/id", ValueScalar::Int64, Required::Yes),
                    (
                        "issue_number",
                        "/issue/number",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    (
                        "repository",
                        "/repository/full_name",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("the GitHub issues event declaration is valid"),
            TriggerEvent::declare(
                "pull_request",
                EventIdentifier::Header(DELIVERY_HEADER),
                [
                    ("action", "/action", ValueScalar::String, Required::Yes),
                    // "`number` (integer, Required) — The pull request number."
                    // is a *top-level* property of this payload, not only a
                    // property of the pull request object.
                    ("number", "/number", ValueScalar::Int64, Required::Yes),
                    (
                        "pull_request_id",
                        "/pull_request/id",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    (
                        "repository",
                        "/repository/full_name",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("the GitHub pull_request event declaration is valid"),
            TriggerEvent::declare(
                "push",
                EventIdentifier::Header(DELIVERY_HEADER),
                [
                    // "The full git ref that was pushed."
                    ("ref", "/ref", ValueScalar::String, Required::Yes),
                    // "The SHA of the most recent commit on ref after the push."
                    ("after", "/after", ValueScalar::String, Required::Yes),
                    ("before", "/before", ValueScalar::String, Required::Yes),
                    (
                        "repository",
                        "/repository/full_name",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("the GitHub push event declaration is valid"),
            TriggerEvent::declare(
                "release",
                EventIdentifier::Header(DELIVERY_HEADER),
                [
                    ("action", "/action", ValueScalar::String, Required::Yes),
                    // The webhook page documents `release` only as "**Required.**
                    // The release object."; `id` and `tag_name` are both required
                    // in the REST schema of the same object.
                    (
                        "release_id",
                        "/release/id",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    (
                        "tag_name",
                        "/release/tag_name",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "repository",
                        "/repository/full_name",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("the GitHub release event declaration is valid"),
        ]
    });
    &EVENTS
}

/// "`X-GitHub-Delivery` — A globally unique identifier (GUID) to identify the
/// event."
pub const DELIVERY_HEADER: &str = "X-GitHub-Delivery";

/// "`X-GitHub-Event` — The name of the event that triggered the delivery."
pub const EVENT_HEADER: &str = "X-GitHub-Event";

/// The ordered error map.
///
/// GitHub publishes no stable machine-readable code on its error body — the
/// documented fields are a prose `message` and, for a `422`, an `errors` array
/// whose `code` is nested per field — so the map is keyed on statuses only and
/// the response body is never matched on.
///
/// The one judgement call is `403`. GitHub uses it both for "The server is
/// refusing to respond" and, per the rate-limit page, for a primary or
/// secondary rate limit: "you will receive a `403` or `429` response". The
/// closed map cannot read `x-ratelimit-remaining` to tell them apart, so `403`
/// takes `Authentication`, which is not retried. That is the safe direction: a
/// rate-limited deployment stops rather than continuing to spend the budget it
/// just exhausted, and GitHub's own warning is that "Continuing to make
/// requests while you are rate limited may result in the banning of your
/// integration."
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "you may receive a `422 Unprocessable Entity` response and an
            // 'Invalid request' error message" for a malformed request, and
            // `400` for a body GitHub cannot read at all.
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "Authenticating with invalid credentials will initially return a
            // `401 Unauthorized` response", and the `403` above.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // `404` ("could mean the resource is private"), `409 Conflict` on a
            // contents write, `410 Gone` for a retired API version or a deleted
            // issue, `406`, and the `451` the pinned version introduced.
            .on_statuses([404, 406, 409, 410, 451], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // "`x-github-request-id`" is the handle GitHub support asks for.
            .correlation_header("github_request_id", "x-github-request-id")
            .build()
            .expect("the GitHub error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection.
///
/// Every one of them is `link`-header driven, because that is the only
/// continuation GitHub publishes as stable: "The query parameters in the `link`
/// URLs may differ between endpoints". The items pointer is the document root,
/// since each of these collections answers with a bare JSON array.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("", "next").expect("the GitHub link plan is valid")
    });
    match operation_id {
        "issue.list" | "pull_request.list" | "release.list" => Some(&COLLECTION),
        _ => None,
    }
}

/// The headers every GitHub request carries.
fn common(
    builder: crate::sdk::operation::OperationBuilder,
) -> crate::sdk::operation::OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Accept", "application/vnd.github+json")
        .static_header("X-GitHub-Api-Version", API_VERSION)
        .static_header("User-Agent", USER_AGENT)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let issue_get = common(Operation::get(
        "issue.get",
        "/repos/{owner}/{repo}/issues/{issue_number}",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("issue_number", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("number", "/number", ValueScalar::Int64, Required::Yes)
    .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
    .output_pointer("state", "/state", ValueScalar::String, Required::Yes)
    .output_pointer("body", "/body", ValueScalar::String, Required::No)
    .output_pointer("html_url", "/html_url", ValueScalar::String, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let issue_list = common(Operation::get("issue.list", "/repos/{owner}/{repo}/issues"))
        .path_param("owner", ValueScalar::String)
        .path_param("repo", ValueScalar::String)
        // "`state` … Default: `open`" — the declaration asks for the value the
        // caller wants rather than inheriting a provider default silently.
        .query_input("state", "state")
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        // The collection is a bare JSON array, so the whole document is the output.
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let issue_create = common(Operation::post(
        "issue.create",
        "/repos/{owner}/{repo}/issues",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .body(JsonTemplate::object([
        ("title", JsonTemplate::input("title")),
        ("body", JsonTemplate::input("body")),
    ]))
    .success_statuses([StatusCode::CREATED])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("number", "/number", ValueScalar::Int64, Required::Yes)
    .output_pointer("html_url", "/html_url", ValueScalar::String, Required::Yes)
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "GitHub's complete published request contract for POST /repos/{owner}/{repo}/issues is \
             four headers, two path parameters, and a body whose only required field is `title`, \
             and it carries no idempotency key, client-supplied request identifier, or \
             deduplication; `X-GitHub-Delivery` is inbound dedupe and the contents endpoint's \
             blob `sha` belongs to a different operation",
        "a second issue with a new identifier and a new number",
    )?))
    .build()?;

    let issue_update = common(Operation::patch(
        "issue.update",
        "/repos/{owner}/{repo}/issues/{issue_number}",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("issue_number", ValueScalar::Int64)
    .body(JsonTemplate::object([
        ("title", JsonTemplate::input("title")),
        ("state", JsonTemplate::input("state")),
    ]))
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("number", "/number", ValueScalar::Int64, Required::Yes)
    .output_pointer("state", "/state", ValueScalar::String, Required::Yes)
    .effect(Effect::inventory_only(
        "GitHub publishes no idempotency key for the issue update, and the endpoint is a PATCH, \
         which spec 010 §7 does not admit for NaturalMethod: a partial update whose body is \
         relative rather than absolute is not repeat-safe and the provider publishes nothing that \
         distinguishes the two",
    )?)
    .build()?;

    let comment_create = common(Operation::post(
        "issue.comment_create",
        "/repos/{owner}/{repo}/issues/{issue_number}/comments",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("issue_number", ValueScalar::Int64)
    .body(JsonTemplate::object([(
        "body",
        JsonTemplate::input("body"),
    )]))
    .success_statuses([StatusCode::CREATED])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("html_url", "/html_url", ValueScalar::String, Required::Yes)
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "GitHub's issue-comment create documents one required body field, `body`, and no \
             idempotency key of any kind anywhere in the REST guide pages",
        "a second comment on the same issue, with a new identifier",
    )?))
    .build()?;

    let pull_request_get = common(Operation::get(
        "pull_request.get",
        "/repos/{owner}/{repo}/pulls/{pull_number}",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("pull_number", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("number", "/number", ValueScalar::Int64, Required::Yes)
    .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
    .output_pointer("state", "/state", ValueScalar::String, Required::Yes)
    .output_pointer("draft", "/draft", ValueScalar::Boolean, Required::No)
    .output_pointer("html_url", "/html_url", ValueScalar::String, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let pull_request_list = common(Operation::get(
        "pull_request.list",
        "/repos/{owner}/{repo}/pulls",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .query_input("state", "state")
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let repository_get = common(Operation::get("repository.get", "/repos/{owner}/{repo}"))
        .path_param("owner", ValueScalar::String)
        .path_param("repo", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "full_name",
            "/full_name",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "default_branch",
            "/default_branch",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer("private", "/private", ValueScalar::Boolean, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let release_get = common(Operation::get(
        "release.get",
        "/repos/{owner}/{repo}/releases/{release_id}",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    // "`release_id` (integer) The unique identifier of the release."
    .path_param("release_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("tag_name", "/tag_name", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("draft", "/draft", ValueScalar::Boolean, Required::Yes)
    .output_pointer(
        "prerelease",
        "/prerelease",
        ValueScalar::Boolean,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let release_list = common(Operation::get(
        "release.list",
        "/repos/{owner}/{repo}/releases",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let file_get = common(Operation::get(
        "file.get",
        "/repos/{owner}/{repo}/contents/{path}",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("path", ValueScalar::String)
    .query_input("ref", "ref")
    .success_statuses([StatusCode::OK])
    .output_pointer("type", "/type", ValueScalar::String, Required::Yes)
    .output_pointer("path", "/path", ValueScalar::String, Required::Yes)
    .output_pointer("sha", "/sha", ValueScalar::String, Required::Yes)
    .output_pointer("size", "/size", ValueScalar::Int64, Required::No)
    .output_pointer("content", "/content", ValueScalar::String, Required::No)
    .output_pointer("encoding", "/encoding", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    let file_put = common(Operation::put(
        "file.put",
        "/repos/{owner}/{repo}/contents/{path}",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("path", ValueScalar::String)
    .body(JsonTemplate::object([
        ("message", JsonTemplate::input("message")),
        ("content", JsonTemplate::input("content")),
        ("sha", JsonTemplate::input("sha")),
    ]))
    // "200 OK" for a replacement, "201 Created" for a new file.
    .success_statuses([StatusCode::OK, StatusCode::CREATED])
    .output_pointer("path", "/content/path", ValueScalar::String, Required::Yes)
    .output_pointer("sha", "/content/sha", ValueScalar::String, Required::Yes)
    .output_pointer(
        "commit_sha",
        "/commit/sha",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::provider_idempotent_natural_method(
        "GitHub documents PUT /repos/{owner}/{repo}/contents/{path} as \"Creates a new file or \
         replaces an existing file in a repository\", and requires the identity of what is being \
         replaced — \"Required if you are updating a file. The blob SHA of the file being \
         replaced\" — with a documented 409 Conflict when that identity no longer holds, so two \
         identical sends leave one file at one path",
    )?)
    .build()?;

    let workflow_dispatch = common(Operation::post(
        "workflow.dispatch",
        "/repos/{owner}/{repo}/actions/workflows/{workflow_id}/dispatches",
    ))
    .path_param("owner", ValueScalar::String)
    .path_param("repo", ValueScalar::String)
    .path_param("workflow_id", ValueScalar::String)
    .body(JsonTemplate::object([("ref", JsonTemplate::input("ref"))]))
    // Pinned to 2026-03-10: "The endpoint now always returns `200` with the
    // workflow run details in the response body."
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "workflow_run_id",
        "/workflow_run_id",
        ValueScalar::Int64,
        Required::Yes,
    )
    .output_pointer("run_url", "/run_url", ValueScalar::String, Required::Yes)
    .output_pointer("html_url", "/html_url", ValueScalar::String, Required::Yes)
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "GitHub publishes no idempotency key or client-supplied request identifier for a \
             workflow dispatch, and the pinned version's own response — a fresh \
             `workflow_run_id` per call — is the provider's own proof of the negative",
        "a second workflow run, with whatever that workflow does",
    )?))
    .build()?;

    Ok(vec![
        issue_get,
        issue_list,
        issue_create,
        issue_update,
        comment_create,
        pull_request_get,
        pull_request_list,
        repository_get,
        release_get,
        release_list,
        file_get,
        file_put,
        workflow_dispatch,
    ])
}

/// The raw-body ceiling one delivery may reach.
///
/// GitHub caps a delivery at 25 MB, which is above the SDK's shared 1 MiB
/// ceiling, so the shared ceiling is what applies and the declaration says so
/// rather than pretending to a limit it cannot honour.
pub const RAW_BODY_MAX_BYTES: usize = crate::sdk::transport::MAX_HTTP_BODY_BYTES;

/// The deadline one attempt of a GitHub operation declares.
#[allow(dead_code)]
const ATTEMPT_DEADLINE: Duration = Duration::from_secs(5);
