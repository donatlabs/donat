//! Excel workbooks on OneDrive, through the Microsoft Graph v1.0 API.
//!
//! Ground truth is Microsoft's own v1.0 reference on `learn.microsoft.com`,
//! read on 2026-08-10; the shared facts (origin, error envelope, throttling,
//! `@odata.nextLink`, permissions) are in
//! [`crate::providers::microsoft_graph`].
//!
//! * `GET /me/drive/items/{id}/workbook/worksheets` — "Retrieve a list of
//!   worksheet objects."
//! * `GET …/workbook/worksheets/{id|name}/range(address='<address>')` —
//!   "Retrieve the properties and relationships of range object."
//! * `PATCH …/workbook/worksheets/{id|name}/range(address='<address>')` —
//!   "Update the properties of range object."
//! * `GET /me/drive/items/{id}/workbook/tables` — "Retrieve a list of table
//!   objects."
//! * `GET …/workbook/tables/{id|name}/rows` — "Retrieve a list of tablerow
//!   objects."
//! * `POST …/workbook/tables/{id|name}/rows/add` — "Adds rows to the end of the
//!   table."
//! * `GET /me/drive/items/{id}/children` — the Drive listing, which is the only
//!   documented way to find a workbook (below).
//!
//! # Effect classification: `worksheet.update_range` is inventory-only
//!
//! Spec 015 §2 classified this operation `NM` **subject to verification**, and
//! the verification failed in both of the two ways it could.
//!
//! *By method*: Microsoft publishes it as a `PATCH`, and spec 010 §7 admits
//! `NaturalMethod` for `PUT` and `DELETE` only.
//!
//! *By evidence*: the page's entire description is one sentence — "Update the
//! properties of range object." — and its request-body rule is the partial-merge
//! one: "supply the values for relevant fields that should be updated. Existing
//! properties that aren't included in the request body maintains their previous
//! values or be recalculated based on changes to other property values."
//! Microsoft publishes **no** statement of repeat-safety, idempotency,
//! deduplication, `If-Match`, or at-most-once semantics for this endpoint, and
//! its own workbook best-practice page publishes the opposite of a way to check:
//! "when you receive a failure response, there is no way to confirm the status
//! of other pending requests, which makes it difficult to determine or to
//! recover the state of the workbook."
//!
//! A repeated identical `PATCH` on a fixed `address` with a fixed `values` grid
//! is *derivably* the same write twice, and Microsoft's own `null`-is-a-skip,
//! `""`-is-a-clear, and single-value-broadcast rules are all pure functions of
//! the request. But ADR 042 admits evidence, not derivations, and a derivation
//! is exactly what "the provider documents its repeat-safe semantics" excludes.
//! The class is therefore `InventoryOnly`, recorded with this evidence in
//! `providers/INVENTORY.md`. ADR 063 does not admit it either: a repeat of this
//! `PATCH` is derivably the same write, so what it needs is a class that permits
//! the retry rather than the at-most-once class that forbids it.
//!
//! `table.add_row` is `AtMostOnce` (ADR 063) on its own semantics — "Adds rows
//! to the end of the table", so a repeat appends a second copy — and carries the batch's sharpest published contrast:
//! Microsoft's only retry guidance in the whole Excel surface is on this very
//! endpoint, "This request might occasionally receive a 504 HTTP error. The
//! appropriate response to this error is to repeat the request", said of an
//! *append*, with nothing published about the duplicate row that repeat can
//! produce. That is guidance about a transport failure, not an idempotency
//! contract, and this connector does not read it as one.
//!
//! # This connector is sessionless, by declaration
//!
//! Microsoft publishes three modes: a persistent session, a non-persistent
//! session whose "changes made by the API aren't saved to the source location",
//! and sessionless. The header that selects one is `workbook-session-id`, and it
//! is optional: "The session header is not required for an Excel API to work…
//! If you don't use a session header, changes made during the API call *are*
//! persisted to the file."
//!
//! No operation here declares that header, and none may take it from input.
//! A session id is a handle to a *different* API call's state, with its own
//! expiry — "Typically the persistent session expires after about 5 minutes of
//! inactivity" — and a value from operation input could silently redirect a
//! deployment's writes into a temporary copy that is thrown away. Spec 015 §3.5
//! asks that a header an operation needs to be valid be declared statically;
//! for Excel the honest declaration is that there is no such header and no path
//! to one.
//!
//! # Continuations
//!
//! The workbook collections publish **no** `@odata.nextLink`. What Microsoft
//! documents for them is offsets — "Use the `$top` and `$skip` query parameters
//! to page through large numbers of tables", and for rows "For reliable results,
//! use the `$top` and `$skip` query parameters to page through the results" — so
//! their declared plan is [`Pagination::offset_limit`], which spends its values
//! as query parameters and has no way to express a destination at all. Only
//! `workbook.list`, which is a Drive listing rather than a workbook one, walks
//! `@odata.nextLink`, through the one origin-checked constructor in
//! [`microsoft_graph::next_link`].
//!
//! # Finding a workbook
//!
//! Microsoft publishes no workbook-listing API. What it publishes is the
//! relationship — "workbook … For files that are Excel spreadsheets, access to
//! the workbook API to work with the spreadsheet's contents" — and the address:
//! "You can access a workbook through the Drive API by identifying the location
//! of the file in the URL." `workbook.list` is therefore a Drive listing whose
//! output carries each child's `name` and `file.mimeType`, which is what lets a
//! Process tell a workbook from anything else. Microsoft publishes no `$filter`
//! on a MIME type and no documented `.xlsx` search, so this connector invents
//! neither; it does publish "The Excel REST API supports only Office Open XML
//! file formatted workbooks. The `.xls` extension workbooks aren't supported."

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::providers::microsoft_graph::{self, PermissionRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "microsoft_excel";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Every workbook operation's permission table reads "Least privileged
/// permission: Files.ReadWrite. Higher privileged permissions: Not available."
/// — including the reads.
const FILES_READ_WRITE: &str = "Files.ReadWrite";
const FILES_READ: &str = "Files.Read";
const FILES_READ_ALL: &str = "Files.Read.All";
const FILES_READ_WRITE_ALL: &str = "Files.ReadWrite.All";
const SITES_READ_ALL: &str = "Sites.Read.All";
const SITES_READ_WRITE_ALL: &str = "Sites.ReadWrite.All";

/// The workbook APIs: one permission, for reads as well as writes.
const WORKBOOK_PERMISSIONS: &[&str] = &[FILES_READ_WRITE];

/// `driveitem-list-children`, which `workbook.list` is: least privileged
/// `Files.Read`.
const DRIVE_READ_PERMISSIONS: &[&str] = &[
    FILES_READ,
    FILES_READ_WRITE,
    FILES_READ_ALL,
    FILES_READ_WRITE_ALL,
    SITES_READ_ALL,
    SITES_READ_WRITE_ALL,
];

/// The driveItem properties `workbook.list` declares outputs for.
const CHILD_FIELDS: &str = "id,name,size,file,folder,webUrl,lastModifiedDateTime";

/// The page size of the offset walks. Microsoft publishes no maximum for the
/// workbook collections; a hundred rows of a wide table stay inside the SDK's
/// 1 MiB response ceiling.
const WORKBOOK_PAGE_SIZE: u32 = 100;

/// "If a collection exceeds the default page size (200 items), the
/// **@odata.nextLink** property is returned".
const CHILD_PAGE_SIZE: &str = "200";

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
            .operations(operations().expect("the Microsoft Excel declarations are valid"))
            .build()
            .expect("the Microsoft Excel declaration is valid")
    });
    &CONNECTOR
}

/// The shared Microsoft Graph error map; see `providers/microsoft_graph.rs`.
pub fn error_map() -> &'static ErrorMap {
    microsoft_graph::error_map()
}

/// The continuation plan of each collection.
///
/// The workbook collections page by offset, which cannot carry a destination;
/// only the Drive listing walks `@odata.nextLink`.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static OFFSET: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::offset_limit(
            microsoft_graph::ITEMS_POINTER,
            "$skip",
            "$top",
            WORKBOOK_PAGE_SIZE,
        )
        .expect("the workbook offset plan is valid")
    });
    static CHILDREN: LazyLock<Pagination> = LazyLock::new(|| {
        microsoft_graph::next_link(microsoft_graph::ITEMS_POINTER)
            .expect("the Graph collection plan is valid")
    });
    match operation_id {
        "worksheet.list" | "table.list" | "table.get_rows" => Some(&OFFSET),
        "workbook.list" => Some(&CHILDREN),
        _ => None,
    }
}

/// The delegated permissions one operation is authorized by.
pub fn permissions(operation_id: &str) -> Option<PermissionRequirement> {
    let workbook = PermissionRequirement::documented(FILES_READ_WRITE, WORKBOOK_PERMISSIONS);
    match operation_id {
        "worksheet.list"
        | "worksheet.get_range"
        | "worksheet.update_range"
        | "table.list"
        | "table.get_rows"
        | "table.add_row" => Some(workbook),
        "workbook.list" => Some(PermissionRequirement::documented(
            FILES_READ,
            DRIVE_READ_PERMISSIONS,
        )),
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

/// The output contract of one range resource.
fn range_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        // "Represents the range reference in A1-style. Address value contains
        // the Sheet reference (for example, Sheet1!A1:B4). Read-only."
        .output_pointer("address", "/address", ValueScalar::String, Required::Yes)
        .output_pointer(
            "address_local",
            "/addressLocal",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("values", "/values", ValueScalar::Json, Required::No)
        .output_pointer("formulas", "/formulas", ValueScalar::Json, Required::No)
        .output_pointer("text", "/text", ValueScalar::Json, Required::No)
        .output_pointer(
            "value_types",
            "/valueTypes",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer("row_count", "/rowCount", ValueScalar::Int64, Required::No)
        .output_pointer(
            "column_count",
            "/columnCount",
            ValueScalar::Int64,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Retrieve a list of worksheet objects."
    let worksheet_list = common(Operation::get(
        "worksheet.list",
        "/v1.0/me/drive/items/{item_id}/workbook/worksheets",
    ))
    .path_param("item_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("worksheets", "/value", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Retrieve the properties and relationships of range object." The address
    // is a value inside an OData function call, so it is a path *value* and is
    // percent-encoded as one; a worksheet id is Microsoft's own
    // `{FC034FA8-…}`, "which need to be URL encoded for the API to work".
    let worksheet_get_range = range_outputs(
        common(Operation::get(
            "worksheet.get_range",
            "/v1.0/me/drive/items/{item_id}/workbook/worksheets/{worksheet}/range(address='{address}')",
        ))
        .path_param("item_id", ValueScalar::String)
        .path_param("worksheet", ValueScalar::String)
        // The address is an argument inside an OData function call, so a quote
        // in it is doubled before it is percent-encoded: a sheet whose name has
        // a space is written `'My Sheet'!A1:B2`, and Graph decodes `%27` before
        // it parses the expression.
        .odata_literal_path_param("address", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "Update the properties of range object."
    let worksheet_update_range = range_outputs(
        common(Operation::patch(
            "worksheet.update_range",
            "/v1.0/me/drive/items/{item_id}/workbook/worksheets/{worksheet}/range(address='{address}')",
        ))
        .path_param("item_id", ValueScalar::String)
        .path_param("worksheet", ValueScalar::String)
        // The address is an argument inside an OData function call, so a quote
        // in it is doubled before it is percent-encoded: a sheet whose name has
        // a space is written `'My Sheet'!A1:B2`, and Graph decodes `%27` before
        // it parses the expression.
        .odata_literal_path_param("address", ValueScalar::String)
        .body(JsonTemplate::object([(
            "values",
            JsonTemplate::input("values"),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Spec 015 §2 asked for verification, and it fails twice. Microsoft publishes this as a \
         `PATCH`, and spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only; and the page \
         publishes no repeat-safety at all — its whole description is \"Update the properties of \
         range object.\", its body rule is the partial-merge one, and the workbook best-practice \
         page states that after a failure \"there is no way to confirm the status of other pending \
         requests\". A repeated identical write of a fixed grid to a fixed address is derivably \
         the same write twice, and a derivation is not the documented evidence ADR 042 admits.",
    )?)
    .build()?;

    // "Retrieve a list of table objects."
    let table_list = common(Operation::get(
        "table.list",
        "/v1.0/me/drive/items/{item_id}/workbook/tables",
    ))
    .path_param("item_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("tables", "/value", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Retrieve a list of tablerow objects."
    let table_get_rows = common(Operation::get(
        "table.get_rows",
        "/v1.0/me/drive/items/{item_id}/workbook/tables/{table}/rows",
    ))
    .path_param("item_id", ValueScalar::String)
    .path_param("table", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("rows", "/value", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "Adds rows to the end of the table." Microsoft documents this endpoint as
    // answering `200 OK` with a `workbookTableRow`; the sibling `POST …/rows`
    // answers `201 Created`, and this connector publishes the one it declares.
    let table_add_row = common(Operation::post(
        "table.add_row",
        "/v1.0/me/drive/items/{item_id}/workbook/tables/{table}/rows/add",
    ))
    .path_param("item_id", ValueScalar::String)
    .path_param("table", ValueScalar::String)
    .body(JsonTemplate::object([(
        "values",
        JsonTemplate::input("values"),
    )]))
    .success_statuses([StatusCode::OK])
    .output_pointer("index", "/index", ValueScalar::Int64, Required::Yes)
    .output_pointer("values", "/values", ValueScalar::Json, Required::No)
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "the table-row reference page enumerates its complete request contract and publishes no \
             idempotency key, client request identifier, or deduplication behaviour for any \
             workbook endpoint; its one retry sentence — \"This request might occasionally \
             receive a 504 HTTP error. The appropriate response to this error is to repeat the \
             request.\" — is guidance about a transport failure on an append rather than a \
             contract that absorbs the duplicate",
        "a second copy of the same rows at the end of the table: Microsoft documents the \
             operation as \"Adds rows to the end of the table\"",
    )?))
    .build()?;

    // The Drive listing, which is the only documented way to find a workbook.
    let workbook_list = common(Operation::get(
        "workbook.list",
        "/v1.0/me/drive/items/{item_id}/children",
    ))
    .path_param("item_id", ValueScalar::String)
    .query_static("$select", CHILD_FIELDS)
    .query_static("$top", CHILD_PAGE_SIZE)
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
        worksheet_list,
        worksheet_get_range,
        worksheet_update_range,
        table_list,
        table_get_rows,
        table_add_row,
        workbook_list,
    ])
}
