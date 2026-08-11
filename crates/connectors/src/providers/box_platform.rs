//! The Box Platform API, v2.0.
//!
//! Ground truth is Box's own published OpenAPI description and the reference
//! pages generated from it, read on 2026-08-10:
//!
//! * <https://github.com/box/box-openapi> — Box's published
//!   `openapi.json`, whose one `servers` entry is
//!   `{ "url": "https://api.box.com/2.0", "description": "Box Platform API
//!   server." }`, and from which every path, parameter, required body field,
//!   and success status below is taken.
//! * <https://developer.box.com/guides/api-calls/permissions-and-errors/common-errors/>
//!   — the status table this module's error map is built from, and the error
//!   body: `{"type": "error", "status": 400, "code": "bad_digest", "message":
//!   …, "request_id": "abcdef123456"}`.
//! * <https://developer.box.com/guides/api-calls/permissions-and-errors/rate-limits/>
//!   — "When an application hits a rate limit, the API will return an API
//!   response with a HTTP status code of `429 Too Many Requests`", with a
//!   `retry-after` header.
//!
//! The credential is authorization-code OAuth2 (spec 011): Box's published
//! security scheme is `OAuth2Security`, "The access token received from the
//! authorization server in the OAuth 2.0 flow", with the authorization endpoint
//! `https://account.box.com/api/oauth2/authorize` and the token endpoint
//! `https://api.box.com/oauth2/token`.
//!
//! # `file.download` is not declared, and the reason is a third origin
//!
//! Box publishes `GET /files/{file_id}/content` and publishes what it answers:
//! "`302` — If the file is available for download the response will include a
//! `Location` header for the file on `dl.boxcloud.com`. The `dl.boxcloud.com`
//! URL is not persistent and clients will need to follow the redirect to
//! actually download the file", and "`200` — Returns the requested file **if
//! the client has the follow redirects setting enabled** to automatically
//! follow HTTP `3xx` responses as redirects. If not, the request will return
//! `302` instead."
//!
//! The SDK's transport follows no redirect, by design, and a connector has one
//! compiled origin that nothing in a provider response may change (spec 010 §4).
//! Box's bytes therefore live on an origin this deployment never declared and
//! whose URL Box says is not persistent, so there is no origin a second
//! connector could compile either — which is exactly what separates this from
//! Dropbox's content host. The operation is declared nowhere rather than
//! declared and quietly broken, and a test asserts that every request this
//! connector can render stays on `api.box.com`. See
//! `knowledgebase/declarative-saas/decisions/074-*`.
//!
//! # A field mask is part of the declaration
//!
//! Box publishes `fields` as a parameter that *replaces* the standard response:
//! "Be aware that specifying this parameter will have the effect that none of
//! the standard fields are returned in the response unless explicitly
//! specified." Each operation here therefore pins a static mask naming exactly
//! the pointers it declares, the same rule `google_drive` follows: the mask is
//! declaration material like a path, and nothing in input can widen it.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur once in
//! Box's published OpenAPI description — 1.77 MB, 186 paths — and neither does
//! `dedup`: no request header, no body property, no response field carries a
//! client-supplied request identifier.
//!
//! `file.delete` is the one executable write. Box publishes it as `DELETE
//! /files/{file_id}` against a fixed identity and publishes what a second send
//! answers: "`404` — Returned if the file is not found **or has already been
//! deleted**, or the user does not have access to the file." That is the
//! provider's own repeat statement, which is what
//! `ProviderIdempotent::NaturalMethod` is admitted on.
//!
//! `folder.delete` is the same method and shape and is **not** admitted, and the
//! difference is the evidence: Box's `404` there says only "Returns an error if
//! the folder could not be found, or the authenticated user does not have
//! access to the folder", and its `503` says "Returns an error when the
//! operation takes longer than 600 seconds. The operation will continue after
//! this response has been returned" — a delete still running when the client is
//! told it failed. A provider that documents "or has already been deleted" where
//! it means it, and does not document it here, has not said this is repeat-safe
//! (the `salesforce.record.delete` finding, one batch on).
//!
//! `folder.create` and `file.share_link_create` are `InventoryOnly` for the
//! reason `INVENTORY.md` calls idempotent-in-effect: Box publishes `409
//! item_name_in_use` for a repeated create in the same parent, and the shared
//! link is a `PUT` whose repeat Box publishes nothing at all about. Both want a
//! class that keeps the retry rather than one that trades it away.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "box";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ "url": "https://api.box.com/2.0" }]`, with the path half
/// declared per operation.
const ORIGIN: &str = "https://api.box.com";

/// "Read all files and folders stored in Box."
pub const READ_SCOPE: &str = "root_readonly";
/// "Read and write all files and folders stored in Box."
pub const WRITE_SCOPE: &str = "root_readwrite";

/// "`limit` — The maximum number of items to return per page." Box's published
/// default for a folder listing is 100 and its marker regime admits up to 1000;
/// 200 is this declaration's choice inside that ceiling.
const PAGE_SIZE: u32 = 200;

/// The mask of one file, naming exactly the pointers this connector declares.
const FILE_FIELDS: &str = "id,type,etag,name,size,created_at,modified_at,parent,shared_link";

/// The mask of one folder.
const FOLDER_FIELDS: &str = "id,type,etag,name,created_at,modified_at,parent,item_status";

/// The mask of a collection entry: Box's own mini representation, plus the two
/// fields a caller sorts and displays by.
const ITEM_FIELDS: &str = "id,type,etag,name,size,modified_at";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Box's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Box declarations are valid"))
            .build()
            .expect("the Box declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from Box's published common-errors table.
///
/// It is keyed on the status alone. Box does publish a closed `code` enum on its
/// error schema, and this map deliberately does not read it: every value in that
/// enum is the status spelled again (`unauthorized`, `not_found`,
/// `too_many_requests`), and the two codes that would say something new —
/// `operation_blocked_temporary`, which Box documents as "The operation can be
/// retried at a later point" — have no home in the eight closed classes. The
/// retryable four all mean "the transport or the server failed", and a `409`
/// conflict is neither; claiming one would tell a Process something that is not
/// true about what happened.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request", "411 Length Required", "413 Request Entity Too
            // Large", "415 Unsupported Media Type" — each names something about
            // the request this deployment sent.
            .on_statuses([400, 411, 413, 415], ConnectorErrorClass::Validation)
            // "401 Unauthorized — Authorization token is not authorized" and
            // "invalid_token — The access token provided is invalid."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "403 Forbidden — Access denied – insufficient permission", "404
            // Not Found", "405 Method Not Allowed", "409 Conflict — A resource
            // with this value already exists", "410 Gone", "412 Precondition
            // Failed — The resource has been modified." None is fixed by
            // sending the same request again.
            .on_statuses(
                [403, 404, 405, 409, 410, 412],
                ConnectorErrorClass::Permanent,
            )
            // "429 Too Many requests — Request rate limit exceeded, try again
            // later … retry their request after the amount of time specified by
            // the `retry-after` header."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Service Error", "502 Bad Gateway", "503 Unavailable
            // — If a Retry-After header is provided in the response, the client
            // should retry the request according to the header value."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // Box publishes `request_id` in the error body and `BoxAPI` echoes
            // it as a header on every response; it is the handle its own
            // support asks for.
            .correlation_header("request_id", "box-request-id")
            .build()
            .expect("the Box error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection.
///
/// The folder listing walks Box's own marker regime — "By setting this value to
/// true, the API will return a `marker` field that can be passed as a parameter
/// to this endpoint to get the next page of the response" — and stops when
/// `next_marker` is absent. The search walks the offset regime it publishes, and
/// stops on a short page; Box's own ceiling is published beside it ("Queries
/// with offset parameter value exceeding 10000 will be rejected with a 400"),
/// which the SDK's page budget never reaches.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static ITEMS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor("/entries", "marker", "/next_marker", "limit", PAGE_SIZE)
            .expect("the Box marker plan is valid")
    });
    static SEARCH: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit("/entries", "offset", "limit", PAGE_SIZE)
            .expect("the Box search offset plan is valid")
    });
    match operation_id {
        "folder.items" => Some(&ITEMS),
        "search" => Some(&SEARCH),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The reason a create carries.
const CREATE_CONFLICTS: &str = "Box publishes no idempotency key anywhere — neither `idempot` nor `dedup` occurs once in its \
     1.77 MB published OpenAPI description — and it publishes what a repeat does instead: `409 \
     item_name_in_use`, \"A resource with this value already exists\". A create whose repeat is \
     refused rather than duplicated is idempotent in effect over a method spec 010 §7 admits for \
     neither mutating class, so it wants a class that keeps the retry rather than ADR 063's, \
     which trades it away.";

/// The reason the shared link carries.
const SHARED_LINK_SILENT: &str = "Box publishes this as `PUT /files/{file_id}` against a fixed identity, which is the shape \
     `NaturalMethod` is for, and publishes no statement at all about a second send: its success \
     text is \"Returns the base representation of a file with a new shared link attached\", which \
     describes the first one. ADR 042 admits the class on the provider's own repeat statement, and \
     there is none — the `salesforce.record.delete` finding, on another provider.";

/// The fields of one file resource, under [`FILE_FIELDS`].
fn file_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("type", "/type", ValueScalar::String, Required::No)
        // "The HTTP `etag` of this file. This can be used within some API
        // endpoints in the `If-Match` and `If-None-Match` headers."
        .output_pointer("etag", "/etag", ValueScalar::String, Required::No)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("size", "/size", ValueScalar::Int64, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "modified_at",
            "/modified_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("parent", "/parent", ValueScalar::Json, Required::No)
        .output_pointer(
            "shared_link",
            "/shared_link",
            ValueScalar::Json,
            Required::No,
        )
}

/// The fields of one folder resource, under [`FOLDER_FIELDS`].
fn folder_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("type", "/type", ValueScalar::String, Required::No)
        .output_pointer("etag", "/etag", ValueScalar::String, Required::No)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "modified_at",
            "/modified_at",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("parent", "/parent", ValueScalar::Json, Required::No)
        .output_pointer(
            "item_status",
            "/item_status",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
///
/// The set is the surface a business process drives against a content store:
/// the two item reads, the folder walk, the enterprise search, the folder
/// create, the two deletes, and the shared link. Box's metadata templates,
/// watermarks, collaborations, retention policies, and upload sessions are its
/// own object model and are not ported here.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Retrieves the details about a file."
    let file_get = file_outputs(
        common(Operation::get("file.get", "/2.0/files/{file_id}"))
            .path_param("file_id", ValueScalar::String)
            .query_static("fields", FILE_FIELDS)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Retrieves details for a folder, including the first 100 entries in the
    // folder." This declaration reads the folder itself; the entries are
    // `folder.items`, which is what Box's own reference recommends — "To fetch
    // more items within the folder, use the Get items in a folder endpoint."
    let folder_get = folder_outputs(
        common(Operation::get("folder.get", "/2.0/folders/{folder_id}"))
            .path_param("folder_id", ValueScalar::String)
            .query_static("fields", FOLDER_FIELDS)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Retrieves a page of items in a folder. These items can be files,
    // folders, and web links." `usemarker` is pinned true because Box publishes
    // the two regimes as mutually exclusive — "Only one pagination method can be
    // used at a time" — and publishes the other one as unreliable: "Offset-based
    // pagination is not guaranteed to work reliably for high offset values and
    // may fail for large datasets."
    let folder_items = common(Operation::get(
        "folder.items",
        "/2.0/folders/{folder_id}/items",
    ))
    .path_param("folder_id", ValueScalar::String)
    .query_static("usemarker", "true")
    .query_static("fields", ITEM_FIELDS)
    .success_statuses([StatusCode::OK])
    .output_pointer("entries", "/entries", ValueScalar::Json, Required::Yes)
    // "The marker for the start of the next page of results."
    .output_pointer(
        "next_marker",
        "/next_marker",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Searches for files, folders, web links, and shared files across the
    // users content or across the entire enterprise." `scope` is left to the
    // caller only in the sense that it is *not* declared: Box's published
    // default is `user_content`, "which limits the search to content owned or
    // shared with the authenticated user", and a connector that let input ask
    // for `enterprise_content` would publish a widening of the credential's
    // reach as an operation argument.
    let search = common(Operation::get("search", "/2.0/search"))
        .query_input("query", "query")
        .query_static("fields", ITEM_FIELDS)
        .success_statuses([StatusCode::OK])
        .output_pointer("entries", "/entries", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_count",
            "/total_count",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "Creates a new empty folder within the specified parent folder." The two
    // required fields are `name` and `parent`; Box documents the root folder's
    // id as the string `0`.
    let folder_create = folder_outputs(
        common(Operation::post("folder.create", "/2.0/folders"))
            .query_static("fields", FOLDER_FIELDS)
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                (
                    "parent",
                    JsonTemplate::object([("id", JsonTemplate::input("parent_id"))]),
                ),
            ]))
            .declared_input("name", ValueScalar::String, Required::Yes)
            .declared_input("parent_id", ValueScalar::String, Required::Yes)
            // "201 — Returns a folder object."
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::inventory_only(CREATE_CONFLICTS)?)
    .build()?;

    // "Deletes a file, either permanently or by moving it to the trash. The
    // enterprise settings determine whether the item will be permanently
    // deleted from Box or moved to the trash."
    let file_delete = common(Operation::delete("file.delete", "/2.0/files/{file_id}"))
        .path_param("file_id", ValueScalar::String)
        // "204 — Returns an empty response when the file has been successfully
        // deleted."
        .success_statuses([StatusCode::NO_CONTENT])
        .no_content_statuses([StatusCode::NO_CONTENT])
        .effect(Effect::provider_idempotent_natural_method(
            "Box publishes this as `DELETE /files/{file_id}` — a fixed resource identity — and \
             publishes what a second send answers: \"404 — Returned if the file is not found or \
             has already been deleted, or the user does not have access to the file.\" A repeat \
             names the same file, finds it already deleted, and answers `404`, which this \
             connector classifies `permanent`; it never deletes a second file.",
        )?)
        .build()?;

    // "Deletes a folder, either permanently or by moving it to the trash."
    // `recursive` is pinned false: Box publishes `400 folder_not_empty` for a
    // non-empty folder, and an operation that could delete a subtree because a
    // caller passed a flag is a blast radius nobody declared.
    let folder_delete = common(Operation::delete(
        "folder.delete",
        "/2.0/folders/{folder_id}",
    ))
    .path_param("folder_id", ValueScalar::String)
    .query_static("recursive", "false")
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::inventory_only(
        "Box publishes `404 — Returns an error if the folder could not be found, or the \
         authenticated user does not have access to the folder` and says nothing about a second \
         send, where for a file it says \"or has already been deleted\". It also publishes `503 — \
         Returns an error when the operation takes longer than 600 seconds. The operation will \
         continue after this response has been returned`, so a repeat may name a folder Box is \
         still deleting. ADR 042 admits `NaturalMethod` on the provider's own repeat statement, \
         and there is none here.",
    )?)
    .build()?;

    // "Adds a shared link to a file." Box publishes the access levels as
    // `open`, `company`, and `collaborators`, and publishes that omitting the
    // field takes "the access level specified by the enterprise admin" — which
    // is the value this declaration sends, because a connector should not widen
    // an enterprise's own default from an operation argument.
    let file_share_link_create = file_outputs(
        common(Operation::put(
            "file.share_link_create",
            "/2.0/files/{file_id}",
        ))
        .path_param("file_id", ValueScalar::String)
        .query_static("fields", FILE_FIELDS)
        .body(JsonTemplate::object([(
            "shared_link",
            JsonTemplate::object([]),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(SHARED_LINK_SILENT)?)
    .build()?;

    Ok(vec![
        file_get,
        folder_get,
        folder_items,
        search,
        folder_create,
        file_delete,
        folder_delete,
        file_share_link_create,
    ])
}

/// The scopes Box publishes for this connector's surface, for the deploy-time
/// check that a deployment authorized what it enabled.
///
/// Box's scope set is coarse: it publishes `root_readonly` as "Read all files
/// and folders stored in Box" and `root_readwrite` as "Read and write all files
/// and folders stored in Box", and there is no per-endpoint scope to name.
#[must_use]
pub fn scopes(operation_id: &str) -> &'static [&'static str] {
    match operation_id {
        "file.get" | "folder.get" | "folder.items" | "search" => &[READ_SCOPE, WRITE_SCOPE],
        _ => &[WRITE_SCOPE],
    }
}

/// The published root folder identifier, for the fixtures and for a deployment
/// reading this module: Box documents the root folder of an account as the id
/// `0`.
pub const ROOT_FOLDER_ID: &str = "0";
