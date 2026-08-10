//! Notion's API, at the version where a database became a list of data sources.
//!
//! Ground truth is Notion's own published documentation, read on 2026-08-10:
//!
//! * <https://developers.notion.com/reference/intro> — the base URL
//!   `https://api.notion.com`, the required headers `Authorization: Bearer`,
//!   `Notion-Version` (currently `2026-03-11`) and `Content-Type:
//!   application/json`, and the shared pagination contract: `start_cursor`, "A
//!   `next_cursor` value returned in a previous response", `page_size`
//!   ("Default: `100`", "Maximum: `100`"), `has_more` — "Whether the response
//!   includes the end of the list" — and `next_cursor`.
//! * <https://developers.notion.com/reference/status-codes> — the error object's
//!   `code` and `message`, and the full status/code table this module's error map
//!   is built from.
//! * <https://developers.notion.com/reference/request-limits> — "an average of
//!   three requests per second", the `"rate_limited"` error code with "an HTTP
//!   429 response", and "respect the `Retry-After` response header. The header
//!   value is an integer number of seconds."
//! * The endpoint references for
//!   [retrieve a page](https://developers.notion.com/reference/retrieve-a-page),
//!   [create a page](https://developers.notion.com/reference/post-page),
//!   [update page properties](https://developers.notion.com/reference/patch-page),
//!   [retrieve a database](https://developers.notion.com/reference/retrieve-a-database),
//!   [query a data source](https://developers.notion.com/reference/query-a-data-source),
//!   [retrieve block children](https://developers.notion.com/reference/get-block-children),
//!   [append block children](https://developers.notion.com/reference/patch-block-children),
//!   [list all users](https://developers.notion.com/reference/get-users), and
//!   [search](https://developers.notion.com/reference/post-search).
//!
//! # `database.query` is not what spec 016 named
//!
//! Spec 016 §2 lists `database.query`, and that endpoint no longer exists at a
//! current version. Notion's own reference marks `POST
//! /v1/databases/{database_id}/query` "Deprecated as of version 2025-09-03", and
//! the upgrade guide states: "When you update the API version, the path of this
//! API changes, and now accepts a data source ID." The replacement is `POST
//! /v1/data_sources/{data_source_id}/query`, and it takes a different
//! identifier — so declaring it under the old operation id would be publishing a
//! name that does not describe the input it demands.
//!
//! This connector therefore publishes **`data_source.query`**, and keeps
//! `database.get` beside it because that endpoint is how a deployment learns the
//! identifier: the same upgrade guide says "The Retrieve Database API is now
//! repurposed to return a list of `data_sources` (each with an `id` and
//! `name`)." A deployment reads the database, picks a data source, and queries
//! it. Notion marks the retrieve endpoint deprecated as well; it is declared
//! anyway, with that recorded, because it is the only published way to resolve
//! the identifier the query needs.
//!
//! # A cursor in a body is not a pagination plan
//!
//! Notion paginates two different ways with one vocabulary. The `GET`
//! collections take `start_cursor` and `page_size` as *query* parameters, which
//! is exactly [`Pagination::cursor`]. `POST /v1/search` and `POST
//! /v1/data_sources/{id}/query` take the same two names in the *request body*,
//! which no SDK plan can express — every plan spends its continuation as a query
//! value or follows it as a URL. Those two operations therefore declare
//! `start_cursor` as an input the caller echoes back verbatim and publish
//! `next_cursor`/`has_more` as outputs, with the page size fixed by this
//! declaration. See `knowledgebase/declarative-saas/decisions/055-*`.
//!
//! # A page id is one percent-encoded segment
//!
//! Every identifier here binds through the SDK's path renderer, which
//! percent-encodes every non-alphanumeric byte, so a Notion UUID arrives as
//! `59833787%2D2cf9%2D…`. That is the same encoding `github.file.get` and
//! `aws_s3.object.get` send an identifier in, and it is equivalent under RFC
//! 3986 §2.3: "URIs that differ in the replacement of an unreserved character
//! with its corresponding percent-encoded US-ASCII octet are equivalent."
//!
//! # Effect classification
//!
//! Notion publishes **no** idempotency key, client-supplied request identifier,
//! or request deduplication. It publishes the opposite, in its own retry advice:
//! a request that is "idempotent, such as GET or DELETE" may be retried on a
//! `500`, `502`, `503` or `504`, while a non-idempotent request "should not be
//! retried" on those "without its own idempotency protection". `page.create` and
//! `block.children_append` are therefore `AtMostOnce` (ADR 063) — the append is
//! additive by name, "Existing blocks cannot be moved using this endpoint", so a
//! repeat appends a second copy of the same children — and `page.update` stays
//! `InventoryOnly`: it is a `PATCH`, which spec 010 §7 admits for neither
//! mutating class, and a repeat sets the same properties to the same values.

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
pub const NAME: &str = "notion";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The published base URL.
const ORIGIN: &str = "https://api.notion.com";

/// The pinned contract version.
///
/// Notion requires the header on every request and answers a request without one
/// with `missing_version`, so there is no "latest" to drift onto: a deployment
/// either pins a version or does not work. `2026-03-11` is the version published
/// today, and it is the one whose database/data-source split this declaration
/// describes.
pub const API_VERSION: &str = "2026-03-11";

/// "`page_size` — Default: `100`", "Maximum: `100`".
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Notion's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Notion declarations are valid"))
            .build()
            .expect("the Notion declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Notion publishes a stable machine-readable `code` on every error body — "the
/// error object contains a `code` and a `message`" — and publishes the whole
/// status/code table, so the map is keyed on the code first and on the status as
/// the answer for a response that never reached the API.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/code")
            // "429 — `rate_limited` — This request exceeds the number of
            // requests allowed."
            .on_code("rate_limited", ConnectorErrorClass::Http429)
            // "401 — `unauthorized`", "403 — `restricted_resource`", and the
            // `invalid_grant` the token endpoints publish.
            .on_code("unauthorized", ConnectorErrorClass::Authentication)
            .on_code("restricted_resource", ConnectorErrorClass::Authentication)
            .on_code("invalid_grant", ConnectorErrorClass::Authentication)
            // The `400` family, every one of which needs a different request.
            .on_code("invalid_json", ConnectorErrorClass::Validation)
            .on_code("invalid_request_url", ConnectorErrorClass::Validation)
            .on_code("invalid_request", ConnectorErrorClass::Validation)
            .on_code("validation_error", ConnectorErrorClass::Validation)
            .on_code("missing_version", ConnectorErrorClass::Validation)
            .on_code("invalid_beta", ConnectorErrorClass::Validation)
            // "404 — `object_not_found`", "409 — `conflict_error` — The
            // transaction could not be completed, potentially due to a data
            // collision."
            .on_code("object_not_found", ConnectorErrorClass::Permanent)
            .on_code("conflict_error", ConnectorErrorClass::Permanent)
            // The server family, including the `529 service_overload` Notion
            // tells a client to honour `Retry-After` for.
            .on_code("internal_server_error", ConnectorErrorClass::Http5xx)
            .on_code("bad_gateway", ConnectorErrorClass::Http5xx)
            .on_code("service_unavailable", ConnectorErrorClass::Http5xx)
            .on_code(
                "database_connection_unavailable",
                ConnectorErrorClass::Http5xx,
            )
            .on_code("gateway_timeout", ConnectorErrorClass::Http5xx)
            .on_code("service_overload", ConnectorErrorClass::Http5xx)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            .on_statuses([404, 409], ConnectorErrorClass::Permanent)
            .on_statuses([500, 502, 503, 504, 529], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Notion error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each `GET` collection.
///
/// Only the `GET`s have one: Notion's two `POST` reads carry the same cursor in
/// their request body, which is data no plan in the closed set can spend. See
/// the module documentation.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static RESULTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor(
            "/results",
            "start_cursor",
            "/next_cursor",
            "page_size",
            PAGE_SIZE,
        )
        .expect("the Notion cursor plan is valid")
    });
    match operation_id {
        "block.children_list" | "user.list" => Some(&RESULTS),
        _ => None,
    }
}

/// Every Notion request carries the pinned contract version.
fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header("Notion-Version", API_VERSION)
}

/// The two continuation fields every paginated read publishes.
fn cursor_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer(
            "next_cursor",
            "/next_cursor",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::Yes)
}

/// The reason every write in this module carries.
const NO_KEY: &str = "Notion publishes no idempotency key, client-supplied request identifier, or \
                      request deduplication; its own retry guidance says the opposite, telling a \
                      client that a non-idempotent request should not be retried on a 5xx \
                      \"without its own idempotency protection\"";

/// One write whose repeat would leave a second thing behind (ADR 063).
///
/// The search is the module's and the consequence is the operation's: both are
/// what a Process author accepts when they declare `at_most_once`.
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let page_get = common(Operation::get("page.get", "/v1/pages/{page_id}"))
        .path_param("page_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("url", "/url", ValueScalar::String, Required::Yes)
        .output_pointer(
            "properties",
            "/properties",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer("parent", "/parent", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "created_time",
            "/created_time",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "last_edited_time",
            "/last_edited_time",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    // "The `parent` object ... `page_id`, `database_id`, `data_source_id`, or
    // `workspace`" — which of them a caller sends is the caller's, so the whole
    // object is one declared input rather than a shape this declaration guesses.
    let page_create = common(Operation::post("page.create", "/v1/pages"))
        .body(JsonTemplate::object([
            ("parent", JsonTemplate::input("parent")),
            ("properties", JsonTemplate::input("properties")),
        ]))
        .declared_input("parent", ValueScalar::Json, Required::Yes)
        .declared_input("properties", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("url", "/url", ValueScalar::String, Required::Yes)
        .output_pointer(
            "created_time",
            "/created_time",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(at_most_once(
            "a second page under the same parent, with a new page id",
        )?)
        .build()?;

    let page_update = common(Operation::patch("page.update", "/v1/pages/{page_id}"))
        .path_param("page_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("properties", JsonTemplate::input("properties")),
            ("archived", JsonTemplate::input("archived")),
        ]))
        .declared_input("properties", ValueScalar::Json, Required::Yes)
        .declared_input("archived", ValueScalar::Boolean, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("url", "/url", ValueScalar::String, Required::Yes)
        .output_pointer(
            "last_edited_time",
            "/last_edited_time",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::inventory_only(NO_KEY)?)
        .build()?;

    let data_source_query = cursor_output(
        common(Operation::post(
            "data_source.query",
            "/v1/data_sources/{data_source_id}/query",
        ))
        .path_param("data_source_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("filter", JsonTemplate::input("filter")),
            ("sorts", JsonTemplate::input("sorts")),
            ("start_cursor", JsonTemplate::input("start_cursor")),
            (
                "page_size",
                JsonTemplate::literal(serde_json::json!(PAGE_SIZE)),
            ),
        ]))
        .declared_input("filter", ValueScalar::Json, Required::Yes)
        .declared_input("sorts", ValueScalar::Json, Required::Yes)
        // The cursor is nullable — the first page of a walk has none — so the
        // slot admits an explicit `null` and is published as JSON.
        .declared_input("start_cursor", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only_documented(
        "Notion's data source query is a read that reaches the API as a POST because its filter \
         and sort objects do not fit a query string; the endpoint creates and changes nothing and \
         its documented response is \"a list of Pages and/or Databases contained in the database\"",
    )?)
    .build()?;

    let database_get = common(Operation::get(
        "database.get",
        "/v1/databases/{database_id}",
    ))
    .path_param("database_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    // "The Retrieve Database API is now repurposed to return a list of
    // `data_sources` (each with an `id` and `name`)" — which is the identifier
    // `data_source.query` takes.
    .output_pointer(
        "data_sources",
        "/data_sources",
        ValueScalar::Json,
        Required::Yes,
    )
    .output_pointer("title", "/title", ValueScalar::Json, Required::No)
    .output_pointer("url", "/url", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    let block_children_list = cursor_output(
        common(Operation::get(
            "block.children_list",
            "/v1/blocks/{block_id}/children",
        ))
        .path_param("block_id", ValueScalar::String)
        .query_static("page_size", &PAGE_SIZE.to_string())
        .success_statuses([StatusCode::OK])
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Arrays of block children longer than 100 will result in an error", and
    // "Existing blocks cannot be moved using this endpoint" — the append is
    // additive, which is why a repeat is a second copy rather than a no-op.
    let block_children_append = common(Operation::patch(
        "block.children_append",
        "/v1/blocks/{block_id}/children",
    ))
    .path_param("block_id", ValueScalar::String)
    .body(JsonTemplate::object([(
        "children",
        JsonTemplate::input("children"),
    )]))
    .declared_input("children", ValueScalar::Json, Required::Yes)
    .success_statuses([StatusCode::OK])
    .output_pointer("results", "/results", ValueScalar::Json, Required::Yes)
    .effect(at_most_once(
        "a second copy of the same children appended below the first: Notion documents the \
             append as additive and states that \"Existing blocks cannot be moved using this \
             endpoint\", so there is no version of it that is a no-op",
    )?)
    .build()?;

    let user_get = common(Operation::get("user.get", "/v1/users/{user_id}"))
        .path_param("user_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("type", "/type", ValueScalar::String, Required::Yes)
        // "`name` (string | null)", "`avatar_url` (string | null)".
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer(
            "avatar_url",
            "/avatar_url",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let user_list = cursor_output(
        common(Operation::get("user.list", "/v1/users"))
            .query_static("page_size", &PAGE_SIZE.to_string())
            .success_statuses([StatusCode::OK])
            .output_pointer("results", "/results", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only())
    .build()?;

    let search = cursor_output(
        common(Operation::post("search", "/v1/search"))
            .body(JsonTemplate::object([
                ("query", JsonTemplate::input("query")),
                ("filter", JsonTemplate::input("filter")),
                ("sort", JsonTemplate::input("sort")),
                ("start_cursor", JsonTemplate::input("start_cursor")),
                (
                    "page_size",
                    JsonTemplate::literal(serde_json::json!(PAGE_SIZE)),
                ),
            ]))
            .declared_input("query", ValueScalar::String, Required::Yes)
            .declared_input("filter", ValueScalar::Json, Required::Yes)
            .declared_input("sort", ValueScalar::Json, Required::Yes)
            .declared_input("start_cursor", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK])
            .output_pointer("results", "/results", ValueScalar::Json, Required::Yes),
    )
    .effect(Effect::read_only_documented(
        "Notion's search \"Searches all parent or child pages and data_sources that have been \
         shared with a connection\" and reaches the API as a POST because its filter and sort \
         objects do not fit a query string; it creates and changes nothing",
    )?)
    .build()?;

    Ok(vec![
        page_get,
        page_create,
        page_update,
        data_source_query,
        database_get,
        block_children_list,
        block_children_append,
        user_get,
        user_list,
        search,
    ])
}
