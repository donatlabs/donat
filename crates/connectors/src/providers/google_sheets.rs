//! The Google Sheets API v4.
//!
//! Ground truth is Google's own discovery document,
//! `https://sheets.googleapis.com/$discovery/rest?version=v4`, read on
//! 2026-08-10 at revision `20260803`, plus *Usage limits* for the quota half.
//! Every quoted sentence below is that document's own `description` text.
//!
//! * `"baseUrl": "https://sheets.googleapis.com/"`.
//! * `spreadsheets.values.get` — "Returns a range of values from a spreadsheet.
//!   The caller must specify the spreadsheet ID and a range."
//! * `spreadsheets.values.batchGet` — "Returns one or more ranges of values
//!   from a spreadsheet."
//! * `spreadsheets.values.update` — "Sets values in a range of a spreadsheet.
//!   The caller must specify the spreadsheet ID, range, and a
//!   valueInputOption."
//! * `spreadsheets.values.append` — "Appends values to a spreadsheet. … Values
//!   will be appended to the next row of the table".
//! * `spreadsheets.values.clear` — "Clears values from a spreadsheet. … Only
//!   values are cleared -- all other properties of the cell (such as
//!   formatting, data validation, etc..) are kept."
//! * `spreadsheets.get` — "Returns the spreadsheet at the given ID."
//! * `spreadsheets.create` — "Creates a spreadsheet, returning the newly
//!   created spreadsheet."
//!
//! # Effect classification
//!
//! `values.update` is the batch's clearest `NaturalMethod`: a `PUT` on
//! `/v4/spreadsheets/{spreadsheetId}/values/{range}`, which is a fixed resource
//! identity, and Google's own verb for it is "Sets values in a range" — the
//! second `PUT` of the same body sets the same cells to the same values.
//!
//! `values.clear` is idempotent *in effect* — clearing a cleared range clears
//! nothing — and is still `InventoryOnly`, because spec 010 §7 admits
//! `NaturalMethod` for `PUT` and `DELETE` only and Google publishes this one as
//! a `POST` (`values/{range}:clear`). ADR 063 does not reach it either: a class
//! that trades the retry away is the wrong one for a write that is safe to
//! repeat. `values.append` and `spreadsheet.create` are `AtMostOnce` on their
//! own semantics: a repeat appends a second copy of the rows and creates a
//! second spreadsheet. All three are recorded in `providers/INVENTORY.md`.
//!
//! Sheets publishes **no** idempotency key, client request identifier, or
//! deduplication behaviour: the strings `idempot` and `dedup` do not occur
//! anywhere in the discovery document, and *Usage limits* describes retrying
//! only as "truncated exponential backoff".
//!
//! # Pagination and continuations
//!
//! **Sheets offers no continuation of any kind.** `pageToken` does not occur in
//! the discovery document, and none of the seven operations here returns a
//! next-page field. [`pagination`] therefore answers `None` for every one of
//! them, which is the strongest form of "a page token cannot leave the origin":
//! there is no plan that could spend a provider-chosen value as a destination.
//!
//! That matters here rather than being a formality, because Sheets *does* hand
//! back a provider-chosen URL: `Spreadsheet.spreadsheetUrl`, on
//! `docs.google.com` rather than on this connector's own origin. It is declared
//! as a `String` output — data a Process may read — and there is nothing in
//! this connector that could turn it into a request.
//!
//! # What a declared input means here
//!
//! Every input slot the SDK renders is required at render: a declared query
//! key with no value is a failure, not an omitted parameter. These operations
//! therefore declare only the parameters Google itself documents as required —
//! `spreadsheetId`, `range`, `ranges`, and the `valueInputOption` that
//! "the caller must specify" — and take Google's own documented defaults for
//! the rest (`majorDimension` "ROWS", `valueRenderOption` "FORMATTED_VALUE",
//! and, for `spreadsheets.get`, "By default, data within grids is not
//! returned"). Publishing an optional tuning parameter as a required input
//! would be a contract this connector does not have.
//!
//! # Scopes
//!
//! The discovery document lists five scopes across these methods. The reads
//! accept `spreadsheets.readonly` and everything broader; the writes accept
//! `spreadsheets` and everything broader. `drive.file` — "See, edit, create,
//! and delete only the specific Google Drive files you use with this app" — is
//! documented for every one of them and is accepted, but it is never the
//! *least* scope this connector asks for, because it is a Drive grant rather
//! than a Sheets one.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::providers::google::{self, ScopeRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "google_sheets";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `"baseUrl": "https://sheets.googleapis.com/"`.
const ORIGIN: &str = "https://sheets.googleapis.com";

/// "See all your Google Sheets spreadsheets".
const SPREADSHEETS_READONLY: &str = "https://www.googleapis.com/auth/spreadsheets.readonly";
/// "See, edit, create, and delete all your Google Sheets spreadsheets".
const SPREADSHEETS: &str = "https://www.googleapis.com/auth/spreadsheets";
/// "See and download all your Google Drive files".
const DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
/// "See, edit, create, and delete all of your Google Drive files".
const DRIVE: &str = "https://www.googleapis.com/auth/drive";
/// "See, edit, create, and delete only the specific Google Drive files you use
/// with this app".
const DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";

/// The scope set of every read: the five the discovery document lists.
const READ_SCOPES: &[&str] = &[
    SPREADSHEETS_READONLY,
    SPREADSHEETS,
    DRIVE_READONLY,
    DRIVE,
    DRIVE_FILE,
];

/// The scope set of every write: the three the discovery document lists.
const WRITE_SCOPES: &[&str] = &[SPREADSHEETS, DRIVE, DRIVE_FILE];

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Google's published origin is valid"))
            // Google Workspace is authorization-code OAuth2 and nothing else:
            // the access token is the credential store's, per attempt, and this
            // connector configures no secret of its own (spec 011 §2).
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Google Sheets declarations are valid"))
            .build()
            .expect("the Google Sheets declaration is valid")
    });
    &CONNECTOR
}

/// The shared Google error map; see `providers/google.rs`.
pub fn error_map() -> &'static ErrorMap {
    google::error_map()
}

/// Sheets publishes no continuation for any operation this connector declares.
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

/// The scopes one operation is authorized by, as the discovery document lists
/// them for that exact method.
pub fn scopes(operation_id: &str) -> Option<ScopeRequirement> {
    let read = ScopeRequirement::documented(SPREADSHEETS_READONLY, READ_SCOPES);
    let write = ScopeRequirement::documented(SPREADSHEETS, WRITE_SCOPES);
    match operation_id {
        "values.get" | "values.batch_get" | "spreadsheet.get" => Some(read),
        "values.update" | "values.append" | "values.clear" | "spreadsheet.create" => Some(write),
        _ => None,
    }
}

/// Decode one response.
///
/// Sheets publishes no per-item failure shape for these operations — no
/// response schema in the discovery document carries an `errors` collection —
/// so the only guard is the shared fail-closed one.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    google::decode(
        operation,
        status,
        headers,
        body,
        google::no_partial_failures,
    )
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Returns a range of values from a spreadsheet."
    let values_get = common(Operation::get(
        "values.get",
        "/v4/spreadsheets/{spreadsheet_id}/values/{range}",
    ))
    .path_param("spreadsheet_id", ValueScalar::String)
    // A1 notation carries `!`, `:`, and a sheet title a user chose, so the
    // range is a path *value* and is percent-encoded as one.
    .path_param("range", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("range", "/range", ValueScalar::String, Required::Yes)
    .output_pointer(
        "major_dimension",
        "/majorDimension",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("values", "/values", ValueScalar::Json, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "Returns one or more ranges of values from a spreadsheet." `ranges` is
    // `"repeated": true`, so a caller repeats the query key.
    let values_batch_get = common(Operation::get(
        "values.batch_get",
        "/v4/spreadsheets/{spreadsheet_id}/values:batchGet",
    ))
    .path_param("spreadsheet_id", ValueScalar::String)
    .query_input("ranges", "ranges")
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "spreadsheet_id",
        "/spreadsheetId",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "value_ranges",
        "/valueRanges",
        ValueScalar::Json,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Sets values in a range of a spreadsheet." A `PUT` on a fixed range.
    let values_update = common(Operation::put(
        "values.update",
        "/v4/spreadsheets/{spreadsheet_id}/values/{range}",
    ))
    .path_param("spreadsheet_id", ValueScalar::String)
    .path_param("range", ValueScalar::String)
    .query_input("valueInputOption", "value_input_option")
    .body(JsonTemplate::object([
        ("range", JsonTemplate::input("range")),
        ("values", JsonTemplate::input("values")),
    ]))
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "spreadsheet_id",
        "/spreadsheetId",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "updated_range",
        "/updatedRange",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "updated_cells",
        "/updatedCells",
        ValueScalar::Int64,
        Required::No,
    )
    .output_pointer(
        "updated_rows",
        "/updatedRows",
        ValueScalar::Int64,
        Required::No,
    )
    .effect(Effect::provider_idempotent_natural_method(
        "Google documents `spreadsheets.values.update` as `PUT \
         /v4/spreadsheets/{spreadsheetId}/values/{range}`, which \"Sets values in a range of a \
         spreadsheet\" — a write of the caller's own values to a range the request names, so a \
         repeat of the same request leaves the same cells holding the same values.",
    )?)
    .build()?;

    // "Appends values to a spreadsheet. … Values will be appended to the next
    // row of the table". A repeat appends a second copy.
    let values_append = common(Operation::post(
        "values.append",
        "/v4/spreadsheets/{spreadsheet_id}/values/{range}:append",
    ))
    .path_param("spreadsheet_id", ValueScalar::String)
    .path_param("range", ValueScalar::String)
    .query_input("valueInputOption", "value_input_option")
    .body(JsonTemplate::object([(
        "values",
        JsonTemplate::input("values"),
    )]))
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "spreadsheet_id",
        "/spreadsheetId",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "table_range",
        "/tableRange",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "updated_range",
        "/updates/updatedRange",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in the `sheets:v4` discovery document, \
             which describes every method, parameter, request body, and response schema of this \
             API",
        "a second copy of the same rows below the first: Google documents the append as writing \
             to \"the next row of the table\", and nothing in the request identifies the rows it \
             wrote",
    )?))
    .build()?;

    // "Clears values from a spreadsheet." Idempotent in effect, published as a
    // `POST`, so the gate does not admit it.
    let values_clear = common(Operation::post(
        "values.clear",
        "/v4/spreadsheets/{spreadsheet_id}/values/{range}:clear",
    ))
    .path_param("spreadsheet_id", ValueScalar::String)
    .path_param("range", ValueScalar::String)
    // "ClearValuesRequest" has no fields; Google's own reference prints the
    // request body as `{}`.
    .body(JsonTemplate::object([]))
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "spreadsheet_id",
        "/spreadsheetId",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "cleared_range",
        "/clearedRange",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::inventory_only(
        "Clearing an already-cleared range clears nothing, so this operation is idempotent in \
         effect — but Google publishes it as `POST .../values/{range}:clear`, and spec 010 §7 \
         admits `NaturalMethod` for `PUT` and `DELETE` only. ADR 063 deliberately does not admit \
         it either: at-most-once is for a write whose repeat leaves a second thing behind, and \
         this one leaves nothing, so it waits for a class that would permit the retry rather \
         than one that forbids it.",
    )?)
    .build()?;

    // "Returns the spreadsheet at the given ID."
    let spreadsheet_get = common(Operation::get(
        "spreadsheet.get",
        "/v4/spreadsheets/{spreadsheet_id}",
    ))
    .path_param("spreadsheet_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "spreadsheet_id",
        "/spreadsheetId",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "title",
        "/properties/title",
        ValueScalar::String,
        Required::No,
    )
    // A provider-chosen URL on `docs.google.com`. It is data, and no plan in
    // this connector can turn it into a destination.
    .output_pointer(
        "spreadsheet_url",
        "/spreadsheetUrl",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer("sheets", "/sheets", ValueScalar::Json, Required::No)
    .effect(Effect::read_only())
    .build()?;

    // "Creates a spreadsheet, returning the newly created spreadsheet."
    let spreadsheet_create = common(Operation::post("spreadsheet.create", "/v4/spreadsheets"))
        .body(JsonTemplate::object([(
            "properties",
            JsonTemplate::object([("title", JsonTemplate::input("title"))]),
        )]))
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "spreadsheet_id",
            "/spreadsheetId",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "spreadsheet_url",
            "/spreadsheetUrl",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::MachineReadableDescription,
            "neither `idempot` nor `dedup` occurs anywhere in the `sheets:v4` discovery document",
            "a second spreadsheet with a new `spreadsheetId` and a new `spreadsheetUrl`",
        )?))
        .build()?;

    Ok(vec![
        values_get,
        values_batch_get,
        values_update,
        values_append,
        values_clear,
        spreadsheet_get,
        spreadsheet_create,
    ])
}
