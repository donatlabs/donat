//! Dropbox's HTTP API v2 — the metadata half.
//!
//! Ground truth is Dropbox's own published documentation and its own published
//! API description, read on 2026-08-10:
//!
//! * <https://github.com/dropbox/dropbox-api-spec> — Dropbox's published Stone
//!   specification of the API, from which every route, argument, result field,
//!   and error union below is taken (`files.stone`, `sharing.stone`).
//! * <https://www.dropbox.com/developers/documentation/http/documentation> —
//!   the HTTP reference and its endpoint styles: "RPC endpoints … accept
//!   arguments as JSON in the request body, and return responses as JSON in the
//!   response body. These endpoints are on the `api.dropboxapi.com` domain",
//!   beside "Content-download endpoints … These endpoints are on the
//!   `content.dropboxapi.com` domain."
//! * <https://developers.dropbox.com/oauth-guide> — "It should be passed with
//!   the `Authorization` HTTP header value of `Bearer <oauth2-access-token>`".
//! * <https://developers.dropbox.com/error-handling-guide> — the status table
//!   this module's error map is built from.
//!
//! # The content endpoints are a second connector
//!
//! `files/download` carries `host = "content"` in Dropbox's own specification,
//! and a connector's origin is a compile-time constant nothing may move (spec
//! 010 §4). So the download is not here: it is
//! [`crate::providers::dropbox_content`], its own module with its own origin,
//! its own credential contract, and its own deployment instance — the same
//! answer `hubspot` gave for `api.hsforms.com`. See
//! `knowledgebase/declarative-saas/decisions/074-*`.
//!
//! # Every endpoint is a `POST`, and that decides the whole write surface
//!
//! Dropbox's RPC style puts every call — read and write alike — on `POST` with
//! a JSON body. Spec 010 §7 admits `ProviderIdempotent::NaturalMethod` for
//! `PUT` and `DELETE` only, because HTTP defines repeat-safety for those two,
//! so **no Dropbox write can reach that class whatever Dropbox does**. That is
//! not a gap in the evidence; it is a property of the provider's transport
//! choice, and it is why three declared writes here are `InventoryOnly`.
//!
//! ADR 063's at-most-once class does not reach them either, and the reason is
//! the opposite of a missing consequence: each of these writes is *idempotent
//! in effect*. Dropbox's published error unions say so — a second `delete_v2`
//! answers `path_lookup/not_found`, a second `create_folder_v2` with
//! `autorename` false answers `path/conflict`, and a second
//! `create_shared_link_with_settings` answers `shared_link_already_exists`. An
//! operation whose repeat changes nothing wants a class that *keeps* the retry,
//! and trading the retry away to protect against a duplicate that cannot happen
//! is a worse contract than leaving it unreachable. They join the population
//! ADR 063 named as still waiting; `providers/INVENTORY.md` records each one.
//!
//! # A continuation is a different route, so this connector declares no walk
//!
//! `list_folder` answers `{entries, cursor, has_more}` and Dropbox documents the
//! continuation as "Pass the cursor into `list_folder/continue`" — a **different
//! endpoint**, taking the cursor in its request body. Every plan in the SDK's
//! closed set re-sends the same path with a changed query, so none of them can
//! spend this cursor ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]] is the
//! same finding one step further on: here the cursor is not only in the body,
//! its destination is another route). `folder.list_continue` and
//! `file.search_continue` are therefore declared as operations a Process drives
//! itself, and `pagination` answers `None` for every operation in this module.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use serde_json::json;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "dropbox";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "These endpoints are on the `api.dropboxapi.com` domain."
const ORIGIN: &str = "https://api.dropboxapi.com";

/// "`limit` — The maximum number of results to return per request. Note: This
/// is an approximate number and there can be slightly more entries returned in
/// some cases", with a published maximum of 2000. 200 is this declaration's
/// choice inside that ceiling: a full page of Dropbox metadata is large, and the
/// SDK carries one bounded response.
const PAGE_LIMIT: u32 = 200;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Dropbox's published RPC origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Dropbox declarations are valid"))
            .build()
            .expect("the Dropbox declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, from Dropbox's own error-handling guide.
///
/// It is keyed on the status alone. Dropbox publishes a machine-readable
/// `error_summary` and then tells a client not to match it exactly — "When
/// programmatically handling 4xx errors, do not use exact string matching on the
/// `error_summary` field. Prefix matching on `error_summary` is acceptable, but
/// the summary may contain additional detail appended to the end of the string"
/// — and the SDK's code map is an exact match on a published code. A map keyed
/// on a value the provider says is not exact would be a map with holes in it.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "A common cause of this is malformed JSON request bodies, or JSON
            // that does not conform to input fields and validation … Responses
            // with 400 error code indicate an issue with the request itself,
            // and thus will not be resolved by retrying them."
            .on_status(400, ConnectorErrorClass::Validation)
            // "These errors are due to the bearer token of the associated
            // request being invalid, expired, or lacking sufficient permission
            // … With the exception of suspension, retrying a 401 will not
            // succeed."
            .on_status(401, ConnectorErrorClass::Authentication)
            // "A 403 indicates that the user or team does not have access to
            // the corresponding call." Dropbox recommends backoff rather than a
            // rapid retry, and nothing in the request changes the answer.
            .on_status(403, ConnectorErrorClass::Permanent)
            // "409 - Conflict (Endpoint Specific Error). Endpoint specific
            // errors can have a variety of different causes … One of the more
            // common causes for many of these categories of errors is
            // `path_not_found`." It is `permanent` rather than `validation`
            // because the request was well formed and the *account* answered.
            .on_status(409, ConnectorErrorClass::Permanent)
            // "429 - Rate Limit (Too Many Requests) … Rate limit responses from
            // the Dropbox API may include a `Retry-After` header."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 - Internal Server Error. Internal server errors are
            // undefined errors on the Dropbox side."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // "All Dropbox API calls (whether successful or 4xx) return an
            // `X-Dropbox-Request-Id` HTTP header in their responses. Should you
            // need help from Dropbox developer support, referring to this value
            // can help expedite troubleshooting."
            .correlation_header("request_id", "x-dropbox-request-id")
            .build()
            .expect("the Dropbox error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection: none, for the reason in the module
/// header — Dropbox's cursor is spent on a different route.
pub fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The statement every *read* in this module carries.
///
/// Dropbox's RPC style serves reads over `POST` too, so the effect gate cannot
/// take the method's word for it: spec 023 §3 admits `ReadOnly` for a documented
/// read over `POST` on the provider's own statement, and this is Dropbox's.
const RPC_READ: &str = "Dropbox serves every endpoint over `POST` — \"RPC endpoints … accept \
                        arguments as JSON in the request body, and return responses as JSON in \
                        the response body\" — so a read of its is a `POST` by the provider's own \
                        transport choice rather than by anything this operation does. Each read \
                        here is declared by Dropbox as a retrieval: \"Returns the metadata for a \
                        file or folder\", \"Starts returning the contents of a folder\", \
                        \"Searches for files and folders\", and the two continuations of those \
                        two, which \"paginate through\" the same results.";

/// The reason every write in this module carries.
///
/// It is one string because it is one finding: the method Dropbox serves them
/// over, and the repeat its own error union publishes.
const RPC_POST: &str = "Dropbox serves every endpoint over `POST` — its published RPC style — so \
                        spec 010 §7's `NaturalMethod`, which admits `PUT` and `DELETE`, cannot \
                        reach this operation whatever its semantics are. ADR 063's at-most-once \
                        class does not reach it either, and for the opposite reason: Dropbox's \
                        own published error union makes the repeat a refusal rather than a second \
                        effect, so this write is idempotent in effect and wants a class that \
                        keeps the retry rather than one that trades it away.";

/// The fields of one `Metadata` entry, as Dropbox's specification declares them.
fn metadata_outputs(builder: OperationBuilder, prefix: &str) -> OperationBuilder {
    builder
        // "union_closed: file FileMetadata, folder FolderMetadata, deleted
        // DeletedMetadata" — the tag is how a caller tells the three apart.
        .output_pointer(
            "kind",
            &format!("{prefix}/.tag"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "id",
            &format!("{prefix}/id"),
            ValueScalar::String,
            Required::No,
        )
        // "The last component of the path (including extension). This never
        // contains a slash."
        .output_pointer(
            "name",
            &format!("{prefix}/name"),
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "path_lower",
            &format!("{prefix}/path_lower"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "path_display",
            &format!("{prefix}/path_display"),
            ValueScalar::String,
            Required::No,
        )
        // "The last time the file was modified on Dropbox."
        .output_pointer(
            "server_modified",
            &format!("{prefix}/server_modified"),
            ValueScalar::String,
            Required::No,
        )
        // "A unique identifier for the current revision of a file."
        .output_pointer(
            "rev",
            &format!("{prefix}/rev"),
            ValueScalar::String,
            Required::No,
        )
        // "The file size in bytes."
        .output_pointer(
            "size",
            &format!("{prefix}/size"),
            ValueScalar::Int64,
            Required::No,
        )
}

/// Every operation this connector publishes.
///
/// The set is the surface a business process drives against a file store: read
/// one item's metadata, walk a folder, search, and the three writes a
/// deployment asks for. Nothing here uploads: Dropbox's upload endpoints are on
/// the content origin and its large-file form is a resumable session, which
/// spec 025 §5 puts out of scope.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Returns the metadata for a file or folder. Note: Metadata for the root
    // folder is unsupported."
    let file_get_metadata = metadata_outputs(
        common(Operation::post(
            "file.get_metadata",
            "/2/files/get_metadata",
        ))
        .body(JsonTemplate::object([(
            "path",
            JsonTemplate::input("path"),
        )]))
        .declared_input("path", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
        "",
    )
    .effect(Effect::read_only_documented(RPC_READ)?)
    .build()?;

    // "Starts returning the contents of a folder. If the result's
    // `ListFolderResult.has_more` field is true, call `list_folder/continue`
    // with the returned ListFolderResult.cursor to retrieve more entries."
    //
    // `recursive` is pinned false rather than taken from input, because Dropbox
    // publishes the cost of the other value — "setting `ListFolderArg.recursive`
    // to `true` may lead to performance issues or errors, especially when
    // traversing folder structures with a large number of items" — and a
    // declaration a caller can turn into that is a declaration nobody reviewed.
    let folder_list = common(Operation::post("folder.list", "/2/files/list_folder"))
        .body(JsonTemplate::object([
            ("path", JsonTemplate::input("path")),
            ("recursive", JsonTemplate::literal(json!(false))),
            ("limit", JsonTemplate::literal(json!(PAGE_LIMIT))),
        ]))
        .declared_input("path", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("entries", "/entries", ValueScalar::Json, Required::Yes)
        .output_pointer("cursor", "/cursor", ValueScalar::String, Required::Yes)
        .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::Yes)
        .effect(Effect::read_only_documented(RPC_READ)?)
        .build()?;

    // "Once a cursor has been retrieved from `list_folder`, use this to
    // paginate through all files and retrieve updates to the folder."
    let folder_list_continue = common(Operation::post(
        "folder.list_continue",
        "/2/files/list_folder/continue",
    ))
    .body(JsonTemplate::object([(
        "cursor",
        JsonTemplate::input("cursor"),
    )]))
    .declared_input("cursor", ValueScalar::String, Required::Yes)
    .success_statuses([StatusCode::OK])
    .output_pointer("entries", "/entries", ValueScalar::Json, Required::Yes)
    .output_pointer("cursor", "/cursor", ValueScalar::String, Required::Yes)
    .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::Yes)
    .effect(Effect::read_only_documented(RPC_READ)?)
    .build()?;

    // "Searches for files and folders." The argument is "`query` — The string to
    // search for. May match across multiple fields based on the request
    // arguments", with a published maximum length of 1000.
    let file_search = common(Operation::post("file.search", "/2/files/search_v2"))
        .body(JsonTemplate::object([(
            "query",
            JsonTemplate::input("query"),
        )]))
        .declared_input("query", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK])
        .output_pointer("matches", "/matches", ValueScalar::Json, Required::Yes)
        .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::Yes)
        // "Pass the cursor into `search/continue:2` to fetch the next page of
        // results." It is optional in Dropbox's own declaration.
        .output_pointer("cursor", "/cursor", ValueScalar::String, Required::No)
        .effect(Effect::read_only_documented(RPC_READ)?)
        .build()?;

    // "Once a cursor has been retrieved from `search:2`, use this to paginate
    // through all matches."
    let file_search_continue = common(Operation::post(
        "file.search_continue",
        "/2/files/search/continue_v2",
    ))
    .body(JsonTemplate::object([(
        "cursor",
        JsonTemplate::input("cursor"),
    )]))
    .declared_input("cursor", ValueScalar::String, Required::Yes)
    .success_statuses([StatusCode::OK])
    .output_pointer("matches", "/matches", ValueScalar::Json, Required::Yes)
    .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::Yes)
    .output_pointer("cursor", "/cursor", ValueScalar::String, Required::No)
    .effect(Effect::read_only_documented(RPC_READ)?)
    .build()?;

    // "Create a folder at a given path." `autorename` is pinned false — "If
    // there's a conflict, have the Dropbox server try to autorename the folder
    // to avoid the conflict" — because the two values have different repeat
    // behaviour, and a declaration whose repeat consequence depends on caller
    // input has no single consequence to record.
    let folder_create = metadata_outputs(
        common(Operation::post(
            "folder.create",
            "/2/files/create_folder_v2",
        ))
        .body(JsonTemplate::object([
            ("path", JsonTemplate::input("path")),
            ("autorename", JsonTemplate::literal(json!(false))),
        ]))
        .declared_input("path", ValueScalar::String, Required::Yes)
        .success_statuses([StatusCode::OK]),
        "/metadata",
    )
    .effect(Effect::inventory_only(RPC_POST)?)
    .build()?;

    // "Delete the file or folder at a given path. If the path is a folder, all
    // its contents will be deleted too. A successful response indicates that
    // the file or folder was deleted."
    let file_delete = metadata_outputs(
        common(Operation::post("file.delete", "/2/files/delete_v2"))
            .body(JsonTemplate::object([(
                "path",
                JsonTemplate::input("path"),
            )]))
            .declared_input("path", ValueScalar::String, Required::Yes)
            .success_statuses([StatusCode::OK]),
        "/metadata",
    )
    .effect(Effect::inventory_only(RPC_POST)?)
    .build()?;

    // "Create a shared link with custom settings. If no settings are given then
    // the default visibility is `RequestedVisibility.public`." The settings
    // object is deliberately not declared: its visibility values change who can
    // read the file, and a connector that let input choose one would publish an
    // access decision as an operation argument.
    let share_link_create = common(Operation::post(
        "share_link.create",
        "/2/sharing/create_shared_link_with_settings",
    ))
    .body(JsonTemplate::object([(
        "path",
        JsonTemplate::input("path"),
    )]))
    .declared_input("path", ValueScalar::String, Required::Yes)
    .success_statuses([StatusCode::OK])
    // "URL of the shared link."
    .output_pointer("url", "/url", ValueScalar::String, Required::Yes)
    .output_pointer("id", "/id", ValueScalar::String, Required::No)
    .output_pointer("name", "/name", ValueScalar::String, Required::Yes)
    .output_pointer(
        "path_lower",
        "/path_lower",
        ValueScalar::String,
        Required::No,
    )
    // "Expiration time, if set. By default the link won't expire."
    .output_pointer("expires", "/expires", ValueScalar::String, Required::No)
    .effect(Effect::inventory_only(RPC_POST)?)
    .build()?;

    Ok(vec![
        file_get_metadata,
        folder_list,
        folder_list_continue,
        file_search,
        file_search_continue,
        folder_create,
        file_delete,
        share_link_create,
    ])
}
