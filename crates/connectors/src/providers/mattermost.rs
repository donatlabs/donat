//! Mattermost's Web Services API, v4.
//!
//! Ground truth is Mattermost's own published OpenAPI description, read on
//! 2026-08-10:
//!
//! * <https://developers.mattermost.com/mattermost-openapi-v4.yaml> — the
//!   `openapi: 3.0.0` document behind every path, parameter, required body
//!   field, and success status below. Its one `servers` entry is
//!   `{your-mattermost-url}`, a variable with no vendor default beyond
//!   `http://localhost:8065`.
//! * Its own *Schema & Conventions*: "All API access is through HTTP(S)
//!   requests at `your-mattermost-url/api/v4`", "All request and response bodies
//!   are `application/json`", and, for pagination, "Maximum items per page: 200
//!   (requests exceeding this will be silently truncated)".
//! * *Authentication*: "Include the `Token` as part of the `Authorization`
//!   header on your future API requests with the `Bearer` method", with the
//!   example `curl -i -H 'Authorization: Bearer ckh3t4knu3fzujt76o57f5jo4w'`.
//! * *Error Handling*: "All errors will return an appropriate HTTP response code
//!   along with the following JSON body: `{"id": "the.error.id", "message":
//!   "Something went wrong", "request_id": "", "status_code": 0, "is_oauth":
//!   false}`".
//! * *Rate Limiting*: "If you exceed your rate limit for a window you will
//!   receive the following error in the body of the response: `HTTP/1.1 429 Too
//!   Many Requests`", with `X-Ratelimit-Limit`, `X-Ratelimit-Remaining`, and
//!   `X-Ratelimit-Reset`.
//!
//! # The deployment names the provider
//!
//! Mattermost is self-hosted: there is no vendor host and no suffix a label
//! could be filled into, exactly as there is none for a WooCommerce store. This
//! connector therefore declares `OriginSpec::DeploymentOrigin`, the third shape
//! of [[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]],
//! and carries the two refusals that variant owes:
//!
//! * **`https` only.** Mattermost's own published credential is a bearer token
//!   on the `Authorization` header, and its `servers` default is a plain-HTTP
//!   loopback for local development. Sending a session or personal access token
//!   to an `http://` server would put it on the wire in clear, so
//!   [`validate_server_origin`] refuses the scheme and names the configuration
//!   key.
//! * **No path.** `Origin::parse` refuses one, and the refusal is kept:
//!   Mattermost publishes its API at `your-mattermost-url/api/v4` and publishes
//!   nothing about serving it under a subdirectory, so a deployment whose
//!   installation lives under a path is refused rather than served a URL this
//!   module composed by guessing.
//!
//! # A channel's posts are a map, so that one collection is not walked
//!
//! `GET /channels/{channel_id}/posts` answers a `PostList`: `order`, an array of
//! ids, beside `posts`, "additionalProperties: Post" — a **map keyed by id**. A
//! walked aggregate writes the collected items back where the plan declared
//! them, so a plan collecting `/order` would return every page's ids beside the
//! last page's posts, which is a silently wrong answer rather than a bounded
//! one. That operation therefore declares `page` and `per_page` as caller inputs
//! and no plan; the two collections that answer with a bare array walk
//! Mattermost's own `page`/`per_page` regime.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Mattermost's published OpenAPI description — 1.18 MB — and neither does
//! `dedup`: no request header, no body property, and no response field of
//! `POST /posts` carries a client-supplied request identifier. `post.create` is
//! therefore `AtMostOnce` (ADR 063): a repeat leaves a second post in the
//! channel, with a new id, visible to everyone who can read it.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{
    JsonTemplate, Operation, OperationBuilder, OperationError, Origin, Required,
};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "mattermost";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the server's whole origin.
pub const SERVER_ORIGIN: &str = "server_origin";

/// "All API access is through HTTP(S) requests at `your-mattermost-url/api/v4`."
const PREFIX: &str = "/api/v4";

/// "Maximum items per page: 200 (requests exceeding this will be silently
/// truncated)."
const PAGE_SIZE: u32 = 200;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::deployment_origin(SERVER_ORIGIN)
                    .expect("the Mattermost origin key is valid"),
            )
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Mattermost declarations are valid"))
            .build()
            .expect("the Mattermost declaration is valid")
    });
    &CONNECTOR
}

/// Whether a configured server origin is one this connector may send its
/// declared credential to.
///
/// See the module documentation: `https` is required because the credential is a
/// bearer token on every request, and a path is refused because an origin is a
/// scheme, a host, and a port.
pub fn validate_server_origin(value: &str) -> Result<(), OperationError> {
    let origin = Origin::parse(value)?;
    if origin.as_url().scheme() != "https" {
        return Err(OperationError::new(
            "a Mattermost server origin must be https: this connector's credential is a bearer \
             token on every request, and Mattermost's own plain-HTTP example is a local \
             development server",
        ));
    }
    Ok(())
}

/// The ordered error map, from Mattermost's published error handling.
///
/// It is keyed on the status alone. Mattermost publishes an `id` — "the.error.id"
/// — in every error body, and publishes no enumeration of its values anywhere in
/// its own description, so there is no stable code set to key on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // Every endpoint in the published description declares `400` as
            // "Invalid or missing parameters in URL or request body".
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 — No access token provided."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "403 — Do not have appropriate permissions", "404 — Resource not
            // found", "501 — Feature is disabled". None is fixed by sending the
            // same request again, and the `403` is deliberately not
            // `authentication`: Mattermost documents it as a permission on the
            // channel rather than as a credential fault.
            .on_statuses([403, 404, 405, 409, 501], ConnectorErrorClass::Permanent)
            // "HTTP/1.1 429 Too Many Requests … X-RateLimit-Reset: 1". The
            // reset is a header the SDK does not read as a retry hint;
            // Mattermost publishes no `Retry-After`, so the connector invents
            // none.
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // The error body's own `request_id`, echoed as a header on every
            // response Mattermost sends.
            .correlation_header("request_id", "x-request-id")
            .build()
            .expect("the Mattermost error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection.
///
/// Both walked collections answer with a bare JSON array, which is why the item
/// pointer is the whole document; the walk ends on a short page, which is what
/// Mattermost's own "Maximum items per page: 200" makes a definite answer.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static PAGES: LazyLock<Pagination> = LazyLock::new(|| {
        // Mattermost numbers its pages from zero: every paged endpoint in its
        // description publishes "`page` — The page to select" with a default of
        // `0`, and a walk that started at one would silently skip the first
        // page.
        Pagination::page_number_from("", "page", "per_page", PAGE_SIZE, 0)
            .expect("the Mattermost page plan is valid")
    });
    match operation_id {
        "channel.list" | "channel.member_list" => Some(&PAGES),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The fields of one post, as Mattermost's `Post` schema declares them.
fn post_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer(
            "channel_id",
            "/channel_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("user_id", "/user_id", ValueScalar::String, Required::No)
        .output_pointer("message", "/message", ValueScalar::String, Required::No)
        .output_pointer("root_id", "/root_id", ValueScalar::String, Required::No)
        // Mattermost publishes its timestamps as milliseconds since the epoch.
        .output_pointer("create_at", "/create_at", ValueScalar::Int64, Required::No)
        .output_pointer("update_at", "/update_at", ValueScalar::Int64, Required::No)
}

/// Every operation this connector publishes.
///
/// The set is spec 025 §3's messaging surface: send, read one, read a page,
/// read a channel, list a team's channels, list a channel's members, plus the
/// user read a member list is only useful beside. Mattermost's teams,
/// preferences, plugins, compliance exports, and system console are its own
/// surface and are not ported here.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Create a new post in a channel. To create the post as a comment on
    // another post, provide `root_id`." The required fields are `channel_id`
    // and `message`.
    let post_create = post_outputs(
        common(Operation::post("post.create", &format!("{PREFIX}/posts")))
            .body(JsonTemplate::object([
                ("channel_id", JsonTemplate::input("channel_id")),
                ("message", JsonTemplate::input("message")),
                ("root_id", JsonTemplate::input("root_id")),
            ]))
            .declared_input("channel_id", ValueScalar::String, Required::Yes)
            .declared_input("message", ValueScalar::String, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in Mattermost's published 1.18 MB \
         OpenAPI description: `POST /api/v4/posts` declares `channel_id`, `message`, `root_id`, \
         `file_ids`, `props`, and `metadata`, and no request header, body property, or response \
         field carries a client-supplied request identifier",
        "a second post in the same channel, with a new id, delivered to every member who can read \
         it and broadcast again over the WebSocket",
    )?))
    .build()?;

    // "Get a single post."
    let post_get = post_outputs(
        common(Operation::get(
            "post.get",
            &format!("{PREFIX}/posts/{{post_id}}"),
        ))
        .path_param("post_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Get channel from the provided channel id string."
    let channel_get = common(Operation::get(
        "channel.get",
        &format!("{PREFIX}/channels/{{channel_id}}"),
    ))
    .path_param("channel_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("team_id", "/team_id", ValueScalar::String, Required::No)
    // "The unique handle for the channel, will be present in the channel URL."
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer(
        "display_name",
        "/display_name",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("type", "/type", ValueScalar::String, Required::No)
    .output_pointer("purpose", "/purpose", ValueScalar::String, Required::No)
    .output_pointer("header", "/header", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "Get a page of posts in a channel." The answer is a `PostList`, whose
    // `posts` is a map keyed by id — see the module header for why this one
    // declares no continuation plan.
    let channel_posts = common(Operation::get(
        "channel.posts",
        &format!("{PREFIX}/channels/{{channel_id}}/posts"),
    ))
    .path_param("channel_id", ValueScalar::String)
    .query_input("page", "page")
    .query_static("per_page", &PAGE_SIZE.to_string())
    .success_statuses([StatusCode::OK])
    // "order" is the array of post ids in order; "posts" is the map they index.
    .output_pointer("order", "/order", ValueScalar::Json, Required::Yes)
    .output_pointer("posts", "/posts", ValueScalar::Json, Required::Yes)
    // "Whether there are more items after this page."
    .output_pointer("has_next", "/has_next", ValueScalar::Boolean, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "Get a page of public channels on a team based on query string parameters
    // - page and per_page."
    let channel_list = common(Operation::get(
        "channel.list",
        &format!("{PREFIX}/teams/{{team_id}}/channels"),
    ))
    .path_param("team_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    // The collection is a bare JSON array, so the whole document is the output.
    .declared_output("channels", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Get a page of members for a channel."
    let channel_member_list = common(Operation::get(
        "channel.member_list",
        &format!("{PREFIX}/channels/{{channel_id}}/members"),
    ))
    .path_param("channel_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    // The collection is a bare JSON array, so the whole document is the output.
    .declared_output("members", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Get a user a object. Sensitive information will be sanitized out."
    let user_get = common(Operation::get(
        "user.get",
        &format!("{PREFIX}/users/{{user_id}}"),
    ))
    .path_param("user_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("username", "/username", ValueScalar::String, Required::No)
    .output_pointer("email", "/email", ValueScalar::String, Required::No)
    .output_pointer("nickname", "/nickname", ValueScalar::String, Required::No)
    .output_pointer(
        "first_name",
        "/first_name",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("last_name", "/last_name", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        post_create,
        post_get,
        channel_get,
        channel_posts,
        channel_list,
        channel_member_list,
        user_get,
    ])
}
