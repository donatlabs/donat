//! OneDrive drive items, through the Microsoft Graph v1.0 API.
//!
//! Ground truth is Microsoft's own v1.0 reference on `learn.microsoft.com`,
//! read on 2026-08-10; the shared facts (origin, error envelope, throttling,
//! `@odata.nextLink`, permissions) are in
//! [`crate::providers::microsoft_graph`].
//!
//! * `GET /me/drive/items/{item-id}` — "Retrieve the metadata for a driveItem
//!   in a drive by file system path or ID."
//! * `GET /me/drive/items/{item-id}/children` — "Return a collection of
//!   DriveItems in the **children** relationship of a DriveItem."
//! * `POST /me/drive/items/{item-id}/copy` — "Create a copy of a driveItem
//!   asynchronously, including child items."
//! * `PATCH /me/drive/items/{item-id}` — "To move a driveItem to a new parent
//!   item, your app requests to update the **parentReference** of the
//!   DriveItem", and "Update the metadata for a driveItem by ID or path."
//! * `DELETE /me/drive/items/{item-id}` — "Delete a DriveItem by using its ID or
//!   path. Deleting items using this method moves the items to the recycle bin
//!   instead of permanently deleting the item."
//! * `POST /me/drive/items/{parent-item-id}/children` — "Create a new folder or
//!   DriveItem in a Drive with a specified parent item or path."
//! * `GET /me/drive/root/search(q='{search-text}')` — "Search the hierarchy of
//!   items for items matching a query."
//!
//! # A download URL is data, and `@odata.nextLink` is a destination
//!
//! These two are the same shape — a provider-chosen absolute URL — and this
//! connector treats them oppositely, which is the whole point.
//!
//! `GET /me/drive/items/{item-id}/content` "Returns a `302 Found` response
//! redirecting to a preauthenticated download URL for the file", and Microsoft's
//! own example `Location` is on `b0mpua-by3301.files.1drv.com` — not on
//! `graph.microsoft.com`, and Microsoft publishes no allowlist of the hosts it
//! can be. The SDK never follows a redirect and admits only `2xx` as a success,
//! so this connector cannot declare that operation at all. It declares the
//! alternative Microsoft publishes on the same page, verbatim: `GET
//! /drive/items/{item-ID}?select=id,@microsoft.graph.downloadUrl`, whose
//! `@microsoft.graph.downloadUrl` is "A URL that can be used to download this
//! file's content. Authentication isn't required with this URL." The URL is a
//! declared **output**: a Process may read it, and nothing in this connector can
//! spend it as a destination. Microsoft's own pages disagree about how long it
//! lives — "might expire within minutes" against "invalidated after for a short
//! period of time (1 hour)" — so this module says only that it is short-lived.
//!
//! `@odata.nextLink`, by contrast, *is* a destination the walk follows, which is
//! why it goes through [`microsoft_graph::next_link`] and is refused when it
//! resolves off the compiled origin.
//!
//! # Effect classification
//!
//! `file.delete` is the one executable mutation, `NaturalMethod`: a `DELETE`
//! against `/me/drive/items/{item-id}`, a fixed resource identity, which
//! Microsoft documents as moving *that* item to the recycle bin and answering
//! "`204 No Content` … to indicate that resource was deleted and there was
//! nothing to return". A repeat names the same item and cannot delete a second
//! one; it is answered with the same `204` or with `itemNotFound`, which this
//! connector classifies `permanent`. Microsoft publishes no sentence about the
//! repeat itself.
//!
//! Everything else that mutates is classified on a published reason, and all of
//! them are recorded in `providers/INVENTORY.md`:
//!
//! * `file.move` and `file.rename` are `PATCH`es with partial-merge semantics,
//!   which spec 010 §7 does not admit, and Microsoft publishes no
//!   `@microsoft.graph.conflictBehavior` for either — so what a repeat that
//!   collides with an existing name does is unpublished, which is why ADR 063
//!   leaves both `InventoryOnly` too.
//! * `folder.create` is `AtMostOnce` (ADR 063): it creates a new item with a new
//!   id, Microsoft publishes no default conflict behaviour for `POST /children`
//!   at all, and this declaration pins `fail`, so a repeat is answered
//!   `nameAlreadyExists` — a different outcome from the first call.
//! * `file.copy` is the sharpest of the four, because its `202 Accepted` is not
//!   a result: "The response indicates whether the copy operation was accepted
//!   or rejected", and Microsoft's own Example 3 shows a copy that is accepted
//!   and then fails during processing with `nameAlreadyExists`. The monitor URL
//!   it hands back is on `contoso.sharepoint.com`, another origin this connector
//!   would refuse to follow, so a second call could not even be told from the
//!   first.
//!
//! # Two headers this connector deliberately does not send
//!
//! `if-match` is documented as optional on the `PATCH`es and the `DELETE` —
//! "If this request header is included and the eTag (or cTag) provided doesn't
//! match the current eTag on the folder, a `412 Precondition Failed` response is
//! returned" — and it is a *precondition value* rather than a declaration, so it
//! would have to come from operation input. This batch publishes no optimistic
//! concurrency and therefore declares no such header.
//!
//! `prefer: bypass-shared-lock` is documented on the delete: "A value of
//! `bypass-shared-lock` bypasses any shared locks on the driveItem (for example,
//! from a coauthoring session)." Overriding somebody else's lock is a decision,
//! not a default, and one a Process must not be able to make per call by putting
//! a value in an input slot. The declaration sends neither, and
//! `microsoft_onedrive_consistency_headers_are_declared` holds that shut.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use crate::providers::microsoft_graph::{self, PermissionRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "microsoft_onedrive";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

const FILES_READ: &str = "Files.Read";
const FILES_READ_WRITE: &str = "Files.ReadWrite";
const FILES_READ_ALL: &str = "Files.Read.All";
const FILES_READ_WRITE_ALL: &str = "Files.ReadWrite.All";
const SITES_READ_ALL: &str = "Sites.Read.All";
const SITES_READ_WRITE_ALL: &str = "Sites.ReadWrite.All";

/// The reads: "Least privileged permission: Files.Read. Higher privileged
/// permissions: Files.ReadWrite, Files.Read.All, Files.ReadWrite.All,
/// Sites.Read.All, Sites.ReadWrite.All."
const READ_PERMISSIONS: &[&str] = &[
    FILES_READ,
    FILES_READ_WRITE,
    FILES_READ_ALL,
    FILES_READ_WRITE_ALL,
    SITES_READ_ALL,
    SITES_READ_WRITE_ALL,
];

/// The writes: "Least privileged permission: Files.ReadWrite. Higher privileged
/// permissions: Files.ReadWrite.All, Sites.ReadWrite.All."
const WRITE_PERMISSIONS: &[&str] = &[FILES_READ_WRITE, FILES_READ_WRITE_ALL, SITES_READ_WRITE_ALL];

/// The driveItem properties this connector declares outputs for.
const ITEM_FIELDS: &str = "id,name,size,webUrl,eTag,cTag,lastModifiedDateTime,file,folder,\
                           parentReference";

/// The download form, exactly as Microsoft's own example writes it.
const DOWNLOAD_FIELDS: &str = "id,name,size,file,@microsoft.graph.downloadUrl";

/// "A URL that can be used to download this file's content. Authentication
/// isn't required with this URL. Read-only."
const DOWNLOAD_URL_POINTER: &str = "/@microsoft.graph.downloadUrl";

/// "If a collection exceeds the default page size (200 items), the
/// **@odata.nextLink** property is returned".
const PAGE_SIZE: &str = "200";

/// "fail | The entire operation fails when a conflict occurs."
///
/// Microsoft publishes no default for `POST /children`, so this connector
/// declares the value whose outcome is an answer rather than a silent second
/// folder or a replaced one. Microsoft's own resource page says the parameter
/// "should be included in the URL instead of the body of the request" while its
/// create-folder page puts it in the body; this declaration follows the
/// create-folder page, which is the one that documents this operation.
pub const CONFLICT_FAIL: &str = "fail";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::fixed(microsoft_graph::ORIGIN)
                    .expect("Microsoft's published Graph origin is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Microsoft OneDrive declarations are valid"))
            .build()
            .expect("the Microsoft OneDrive declaration is valid")
    });
    &CONNECTOR
}

/// The shared Microsoft Graph error map; see `providers/microsoft_graph.rs`.
pub fn error_map() -> &'static ErrorMap {
    microsoft_graph::error_map()
}

/// The continuation plan of each collection.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        microsoft_graph::next_link(microsoft_graph::ITEMS_POINTER)
            .expect("the Graph collection plan is valid")
    });
    match operation_id {
        "file.list_children" | "item.search" => Some(&COLLECTION),
        _ => None,
    }
}

/// The delegated permissions one operation is authorized by.
pub fn permissions(operation_id: &str) -> Option<PermissionRequirement> {
    let read = PermissionRequirement::documented(FILES_READ, READ_PERMISSIONS);
    let write = PermissionRequirement::documented(FILES_READ_WRITE, WRITE_PERMISSIONS);
    match operation_id {
        "file.get" | "file.list_children" | "file.download" | "item.search" => Some(read),
        "file.copy" | "file.move" | "file.rename" | "file.delete" | "folder.create" => Some(write),
        _ => None,
    }
}

/// Decode one response.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    microsoft_graph::decode(
        operation,
        status,
        headers,
        body,
        microsoft_graph::no_partial_failures,
    )
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The output contract of one driveItem, under [`ITEM_FIELDS`].
fn item_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("size", "/size", ValueScalar::Int64, Required::No)
        // A provider-chosen URL on the tenant's SharePoint host. Data, never a
        // destination.
        .output_pointer("web_url", "/webUrl", ValueScalar::String, Required::No)
        .output_pointer("etag", "/eTag", ValueScalar::String, Required::No)
        .output_pointer("ctag", "/cTag", ValueScalar::String, Required::No)
        .output_pointer(
            "last_modified_at",
            "/lastModifiedDateTime",
            ValueScalar::String,
            Required::No,
        )
        // "file | File metadata, if the item is a file" / "folder | Folder
        // metadata, if the item is a folder": which one is present is how a
        // Process tells them apart.
        .output_pointer(
            "mime_type",
            "/file/mimeType",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "child_count",
            "/folder/childCount",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "parent_id",
            "/parentReference/id",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let file_get = item_outputs(
        common(Operation::get("file.get", "/v1.0/me/drive/items/{item_id}"))
            .path_param("item_id", ValueScalar::String)
            .query_static("$select", ITEM_FIELDS)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let file_list_children = common(Operation::get(
        "file.list_children",
        "/v1.0/me/drive/items/{item_id}/children",
    ))
    .path_param("item_id", ValueScalar::String)
    .query_static("$select", ITEM_FIELDS)
    .query_static("$top", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .output_pointer("items", "/value", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "next_link",
        microsoft_graph::NEXT_LINK_POINTER,
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // The `?select=id,@microsoft.graph.downloadUrl` form Microsoft publishes for
    // clients that cannot follow the `302` of `/content`.
    let file_download = common(Operation::get(
        "file.download",
        "/v1.0/me/drive/items/{item_id}",
    ))
    .path_param("item_id", ValueScalar::String)
    .query_static("$select", DOWNLOAD_FIELDS)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("size", "/size", ValueScalar::Int64, Required::No)
    .output_pointer(
        "mime_type",
        "/file/mimeType",
        ValueScalar::String,
        Required::No,
    )
    // The pre-authenticated URL, on a host Microsoft does not publish and this
    // connector would refuse to visit. It is output, never a destination.
    .output_pointer(
        "download_url",
        DOWNLOAD_URL_POINTER,
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Create a copy of a driveItem asynchronously… `202 Accepted`" with a
    // monitor URL in `Location`, on another origin.
    let file_copy = common(Operation::post(
        "file.copy",
        "/v1.0/me/drive/items/{item_id}/copy",
    ))
    .path_param("item_id", ValueScalar::String)
    .body(JsonTemplate::object([
        ("name", JsonTemplate::input("name")),
        (
            "parentReference",
            JsonTemplate::object([("id", JsonTemplate::input("parent_id"))]),
        ),
    ]))
    .success_statuses([StatusCode::ACCEPTED])
    .no_content_statuses([StatusCode::ACCEPTED])
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the copy's reference page enumerates its complete request contract and publishes no \
             idempotency key; its `202 Accepted` is acceptance rather than result — \"The \
             response indicates whether the copy operation was accepted or rejected\" — and the \
             progress handle is a monitor URL on the tenant's SharePoint host, another origin \
             this connector does not follow",
        "a second copy of the file, or the `nameAlreadyExists` failure Microsoft's own example \
             shows a copy failing with during processing — and no way for this engine to tell \
             either from the first attempt",
    )?))
    .build()?;

    // "To move a driveItem to a new parent item, your app requests to update the
    // parentReference of the DriveItem."
    let file_move = item_outputs(
        common(Operation::patch(
            "file.move",
            "/v1.0/me/drive/items/{item_id}",
        ))
        .path_param("item_id", ValueScalar::String)
        .query_static("$select", ITEM_FIELDS)
        .body(JsonTemplate::object([(
            "parentReference",
            JsonTemplate::object([("id", JsonTemplate::input("parent_id"))]),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Microsoft publishes the move as a `PATCH` whose body \"supply the new value for the \
         parentReference property. Existing properties that aren't included in the request body \
         maintain their previous values or be recalculated\", and spec 010 §7 admits \
         `NaturalMethod` for `PUT` and `DELETE` only. No `@microsoft.graph.conflictBehavior` is \
         documented for this endpoint, so what a repeat that now collides with a name in the \
         destination does is unpublished.",
    )?)
    .build()?;

    // "Update the metadata for a driveItem by ID or path."
    let file_rename = item_outputs(
        common(Operation::patch(
            "file.rename",
            "/v1.0/me/drive/items/{item_id}",
        ))
        .path_param("item_id", ValueScalar::String)
        .query_static("$select", ITEM_FIELDS)
        .body(JsonTemplate::object([(
            "name",
            JsonTemplate::input("name"),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "The same `PATCH` with partial-merge semantics as `file.move`, which spec 010 §7 does not \
         admit, and with the same unpublished collision behaviour: Microsoft documents no \
         `@microsoft.graph.conflictBehavior` for a rename.",
    )?)
    .build()?;

    // "Delete a DriveItem by using its ID or path… `204 No Content`."
    let file_delete = common(Operation::delete(
        "file.delete",
        "/v1.0/me/drive/items/{item_id}",
    ))
    .path_param("item_id", ValueScalar::String)
    .success_statuses([StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Microsoft documents `DELETE /me/drive/items/{item-id}` as \"Delete a DriveItem by using \
         its ID or path. Deleting items using this method moves the items to the recycle bin \
         instead of permanently deleting the item\" — a fixed resource identity — answering \"`204 \
         No Content` … to indicate that resource was deleted and there was nothing to return\". A \
         repeat names the same item and cannot delete a second one; it is answered with the same \
         `204` or with `itemNotFound`, which this connector classifies `permanent`. Microsoft \
         publishes no sentence about the repeat itself, and the evidence admitted here is the \
         fixed identity of the documented request.",
    )?)
    .build()?;

    // "Create a new folder or DriveItem in a Drive with a specified parent item
    // or path."
    let folder_create = item_outputs(
        common(Operation::post(
            "folder.create",
            "/v1.0/me/drive/items/{item_id}/children",
        ))
        .path_param("item_id", ValueScalar::String)
        .query_static("$select", ITEM_FIELDS)
        .body(JsonTemplate::object([
            ("name", JsonTemplate::input("name")),
            // "folder | Folder metadata, if the item is a folder" — an empty
            // facet is what makes this request a folder create, so it is a
            // literal rather than an input.
            ("folder", JsonTemplate::literal(json!({}))),
            (
                "@microsoft.graph.conflictBehavior",
                JsonTemplate::literal(json!(CONFLICT_FAIL)),
            ),
        ]))
        .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the children reference page enumerates its complete request contract and publishes no \
             idempotency key for the Drive API, nor any default \
             `@microsoft.graph.conflictBehavior` for `POST /children`",
        "a `nameAlreadyExists` failure rather than a second folder, because this declaration \
             pins `fail` — still a different outcome from the first call rather than the same \
             one, so a repeat cannot tell \"I created it\" from \"somebody else did\"",
    )?))
    .build()?;

    // "Search the hierarchy of items for items matching a query."
    let item_search = common(Operation::get(
        "item.search",
        "/v1.0/me/drive/root/search(q='{query}')",
    ))
    // The search text is an argument inside an OData function call, so a quote
    // in it is doubled before it is percent-encoded: `O'Brien` is a legitimate
    // search, and Graph decodes `%27` before it parses the expression.
    .odata_literal_path_param("query", ValueScalar::String)
    .query_static("$select", ITEM_FIELDS)
    .query_static("$top", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .output_pointer("items", "/value", ValueScalar::Json, Required::Yes)
    .output_pointer(
        "next_link",
        microsoft_graph::NEXT_LINK_POINTER,
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        file_get,
        file_list_children,
        file_download,
        file_copy,
        file_move,
        file_rename,
        file_delete,
        folder_create,
        item_search,
    ])
}
