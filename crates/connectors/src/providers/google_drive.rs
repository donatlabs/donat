//! The Google Drive API v3.
//!
//! Ground truth is Google's own discovery document,
//! `https://www.googleapis.com/discovery/v1/apis/drive/v3/rest`, read on
//! 2026-08-10 at revision `20260805`, plus *Resolve errors* for the failure
//! half. Every quoted sentence below is that document's own `description` text.
//!
//! * `"baseUrl": "https://www.googleapis.com/drive/v3/"`.
//! * `files.get` — "Gets a file's metadata or content by ID. … If you provide
//!   the URL parameter `alt=media`, then the response includes the file
//!   contents in the response body."
//! * `files.list` — "Lists the user's files. … This method returns *all* files
//!   by default, including trashed files."
//! * `files.update` — "Updates a file's metadata, content, or both. … This
//!   method supports patch semantics."
//! * `files.delete` — "Permanently deletes a file owned by the user without
//!   moving it to the trash. … If the target is a folder, all descendants owned
//!   by the user are also deleted."
//! * `files.copy` — "Creates a copy of a file and applies any requested updates
//!   with patch semantics."
//! * `files.create` — "Creates a file."
//! * `permissions.list` — "Lists a file's or shared drive's permissions."
//! * `permissions.create` — "Creates a permission for a file or shared drive.
//!   **Warning:** Concurrent permissions operations on the same file aren't
//!   supported; only the last update is applied."
//!
//! # Effect classification
//!
//! `file.delete` is `NaturalMethod`: a `DELETE` on `files/{fileId}`, a fixed
//! resource identity, which Google documents as "Permanently deletes a file".
//! A repeat of the same request finds nothing left to delete and answers `404`,
//! which the error map classifies `permanent` — the retry does not create a
//! second effect.
//!
//! `file.update_metadata` is `InventoryOnly` on the method: Google publishes it
//! as a `PATCH` and documents it as supporting "patch semantics", and spec
//! 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only, and ADR 063 does
//! not reach it either: what a repeat of a patch body produces is exactly what
//! Drive does not publish. `file.copy`, `permission.create`, and `folder.create`
//! are `AtMostOnce` (ADR 063) on their own semantics — each call produces a new
//! resource with a new id, and Drive publishes no idempotency key anywhere: the
//! strings `idempot` and `dedup` do not occur in the discovery document.
//! `permission.create` is the sharpest of them, because Google's own warning
//! says concurrent permission writes are not supported at all.
//!
//! # A field mask is part of the declaration
//!
//! Drive v3 returns a *partial* resource unless the request names the fields it
//! wants, so an operation whose output contract were declared without one would
//! be asking for fields the provider was never told to send. Each operation
//! here therefore pins a static `fields` mask that names exactly the pointers it
//! declares. The mask is declaration material like a path: nothing in operation
//! input can widen it.
//!
//! # Continuations
//!
//! `files.list` and `permissions.list` publish `nextPageToken`, which Google
//! documents as a value to send back — "The page token for the next page of
//! files. This will be absent if the end of the files list has been reached."
//! It is declared as a token in the body, so the SDK can only ever spend it as
//! a query value on this connector's compiled origin; a token that spelled
//! another host would travel as one percent-encoded query value and change no
//! destination.
//!
//! # `incompleteSearch`
//!
//! `FileList` carries "`incompleteSearch`: Whether the search process was
//! incomplete. If true, then some search results might be missing, since all
//! documents were not searched." A listing that answers `true` is refused
//! rather than returned, for the reason the SDK's own pagination gives for
//! never emitting a partial aggregate: a truncated result is indistinguishable
//! from a complete one downstream. Google's own remedy — "clients narrow their
//! query" — is a change to the request, so the class is `permanent`.

use std::sync::LazyLock;

use base64::Engine;
use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use crate::providers::google::{self, ScopeRequirement};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;
use crate::sdk::transport::MAX_HTTP_BODY_BYTES;

/// The connector name a deployment selects.
pub const NAME: &str = "google_drive";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `"baseUrl": "https://www.googleapis.com/drive/v3/"`.
const ORIGIN: &str = "https://www.googleapis.com";

/// "See and download all your Google Drive files".
const DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";
/// "See, edit, create, and delete all of your Google Drive files".
const DRIVE: &str = "https://www.googleapis.com/auth/drive";
/// "See, edit, create, and delete only the specific Google Drive files you use
/// with this app".
const DRIVE_FILE: &str = "https://www.googleapis.com/auth/drive.file";
/// "See information about your Google Drive files".
const DRIVE_METADATA_READONLY: &str = "https://www.googleapis.com/auth/drive.metadata.readonly";
const DRIVE_METADATA: &str = "https://www.googleapis.com/auth/drive.metadata";
const DRIVE_APPDATA: &str = "https://www.googleapis.com/auth/drive.appdata";
const DRIVE_PHOTOS_READONLY: &str = "https://www.googleapis.com/auth/drive.photos.readonly";
const DRIVE_MEET_READONLY: &str = "https://www.googleapis.com/auth/drive.meet.readonly";
const DRIVE_SCRIPTS: &str = "https://www.googleapis.com/auth/drive.scripts";

/// `files.get` and `files.list`: the eight scopes the discovery document lists.
const FILE_READ_SCOPES: &[&str] = &[
    DRIVE_METADATA_READONLY,
    DRIVE_READONLY,
    DRIVE_METADATA,
    DRIVE_FILE,
    DRIVE,
    DRIVE_APPDATA,
    DRIVE_PHOTOS_READONLY,
    DRIVE_MEET_READONLY,
];

/// `files.create` and `files.delete`.
const FILE_WRITE_SCOPES: &[&str] = &[DRIVE_FILE, DRIVE, DRIVE_APPDATA];

/// `files.update`, which additionally lists the metadata and script scopes.
const FILE_UPDATE_SCOPES: &[&str] = &[
    DRIVE_FILE,
    DRIVE,
    DRIVE_APPDATA,
    DRIVE_METADATA,
    DRIVE_SCRIPTS,
];

/// `files.copy`, which additionally lists the photos scope.
const FILE_COPY_SCOPES: &[&str] = &[DRIVE_FILE, DRIVE, DRIVE_APPDATA, DRIVE_PHOTOS_READONLY];

/// `permissions.list`: the seven scopes the discovery document lists.
const PERMISSION_READ_SCOPES: &[&str] = &[
    DRIVE_METADATA_READONLY,
    DRIVE_READONLY,
    DRIVE_METADATA,
    DRIVE_FILE,
    DRIVE,
    DRIVE_PHOTOS_READONLY,
    DRIVE_MEET_READONLY,
];

/// `permissions.create`.
const PERMISSION_WRITE_SCOPES: &[&str] = &[DRIVE_FILE, DRIVE];

/// The field mask of a single file, naming exactly the pointers this connector
/// declares for one.
const FILE_FIELDS: &str =
    "id,name,mimeType,modifiedTime,size,trashed,parents,webViewLink,md5Checksum";

/// The field mask of a listing: the continuation, the completeness flag, and
/// the per-file fields above.
const FILE_LIST_FIELDS: &str = "nextPageToken,incompleteSearch,\
     files(id,name,mimeType,modifiedTime,size,trashed,parents,webViewLink,md5Checksum)";

const PERMISSION_FIELDS: &str = "id,type,role,emailAddress,domain,displayName";
const PERMISSION_LIST_FIELDS: &str =
    "nextPageToken,permissions(id,type,role,emailAddress,domain,displayName)";

/// "The maximum value is 1000; values above 1000 will be coerced to 1000", with
/// a documented default of 100. The default is what this connector pins: a
/// thousand fully-masked files would not fit the SDK's 1 MiB response ceiling,
/// and the pagination budget walks the pages either way.
const FILE_PAGE_SIZE: &str = "100";

/// "The maximum value is 100; values above 100 will be coerced to 100."
const PERMISSION_PAGE_SIZE: &str = "100";

/// "Value: the fixed string `application/vnd.google-apps.folder`" — Google's
/// own MIME type for a folder, which is the whole of what makes `files.create`
/// a folder create.
pub const FOLDER_MIME_TYPE: &str = "application/vnd.google-apps.folder";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Google's published origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Google Drive declarations are valid"))
            .build()
            .expect("the Google Drive declaration is valid")
    });
    &CONNECTOR
}

/// The shared Google error map; see `providers/google.rs`.
pub fn error_map() -> &'static ErrorMap {
    google::error_map()
}

/// The continuation plan of each listing.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static FILES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/files", "/nextPageToken", "pageToken")
            .expect("the Drive file listing plan is valid")
    });
    static PERMISSIONS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/permissions", "/nextPageToken", "pageToken")
            .expect("the Drive permission listing plan is valid")
    });
    match operation_id {
        "file.list" => Some(&FILES),
        "permission.list" => Some(&PERMISSIONS),
        _ => None,
    }
}

/// The scopes one operation is authorized by, as the discovery document lists
/// them for that exact method.
pub fn scopes(operation_id: &str) -> Option<ScopeRequirement> {
    match operation_id {
        "file.get" | "file.list" | "file.download" => Some(ScopeRequirement::documented(
            DRIVE_METADATA_READONLY,
            FILE_READ_SCOPES,
        )),
        "file.update_metadata" => {
            Some(ScopeRequirement::documented(DRIVE_FILE, FILE_UPDATE_SCOPES))
        }
        "file.delete" | "folder.create" => {
            Some(ScopeRequirement::documented(DRIVE_FILE, FILE_WRITE_SCOPES))
        }
        "file.copy" => Some(ScopeRequirement::documented(DRIVE_FILE, FILE_COPY_SCOPES)),
        "permission.list" => Some(ScopeRequirement::documented(
            DRIVE_METADATA_READONLY,
            PERMISSION_READ_SCOPES,
        )),
        "permission.create" => Some(ScopeRequirement::documented(
            DRIVE_FILE,
            PERMISSION_WRITE_SCOPES,
        )),
        _ => None,
    }
}

/// A `file.download` whose body is larger than the SDK will carry.
const DOWNLOAD_TOO_LARGE: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Validation,
    "connector_response_too_large",
    "connector provider response exceeds the declared ceiling",
);

/// Decode one response.
///
/// `file.download` is the one operation whose success body is not JSON: Google
/// documents `alt=media` as returning "the file contents in the response body".
/// The bytes are composed into the declared output contract here, which is what
/// [`crate::sdk::operation::OperationBuilder::declared_output`] exists for.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if operation.id() == "file.download" {
        return decode_download(operation, status, headers, body);
    }
    google::decode(operation, status, headers, body, refuse_partial_result)
}

/// Drive's one documented partial answer inside a success.
fn refuse_partial_result(operation_id: &str, value: &JsonValue) -> Result<(), ConnectorFailure> {
    if operation_id == "file.list" && value.get("incompleteSearch") == Some(&JsonValue::Bool(true))
    {
        return Err(google::INCOMPLETE_RESULT);
    }
    Ok(())
}

fn decode_download(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(google::error_map().classify(status, headers, body));
    }
    if body.len() > MAX_HTTP_BODY_BYTES {
        return Err(DOWNLOAD_TOO_LARGE.with_provider_status(status));
    }
    let composed = json!({
        "content_base64": base64::engine::general_purpose::STANDARD.encode(body),
        "content_bytes": body.len(),
        "content_type": headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(JsonValue::from)
            .unwrap_or(JsonValue::Null),
    });
    operation.extract_output(&composed)
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The output contract of one file resource, shared by every operation that
/// returns a `File` under the mask above.
fn file_outputs(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/name", ValueScalar::String, Required::No)
        .output_pointer("mime_type", "/mimeType", ValueScalar::String, Required::No)
        .output_pointer(
            "modified_time",
            "/modifiedTime",
            ValueScalar::String,
            Required::No,
        )
        // "size" is documented as a string, because a Drive file can be larger
        // than a JSON number is safe for.
        .output_pointer("size", "/size", ValueScalar::String, Required::No)
        .output_pointer("trashed", "/trashed", ValueScalar::Boolean, Required::No)
        .output_pointer("parents", "/parents", ValueScalar::Json, Required::No)
        .output_pointer(
            "web_view_link",
            "/webViewLink",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "md5_checksum",
            "/md5Checksum",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let file_get = file_outputs(
        common(Operation::get("file.get", "/drive/v3/files/{file_id}"))
            .path_param("file_id", ValueScalar::String)
            .query_static("fields", FILE_FIELDS)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    // "This method accepts the `q` parameter, which is a search query combining
    // one or more search terms." It is declared rather than defaulted, because
    // the documented default is "*all* files … including trashed files", and a
    // deployment should say which files it means.
    let file_list = common(Operation::get("file.list", "/drive/v3/files"))
        .query_input("q", "q")
        .query_static("pageSize", FILE_PAGE_SIZE)
        .query_static("fields", FILE_LIST_FIELDS)
        .success_statuses([StatusCode::OK])
        .output_pointer("files", "/files", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "next_page_token",
            "/nextPageToken",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    // "If you provide the URL parameter `alt=media`, then the response includes
    // the file contents in the response body." The three outputs are composed
    // by this module from the response rather than read through a pointer.
    let file_download = common(Operation::get("file.download", "/drive/v3/files/{file_id}"))
        .path_param("file_id", ValueScalar::String)
        .query_static("alt", "media")
        .success_statuses([StatusCode::OK])
        .declared_output("content_base64", ValueScalar::String, Required::Yes)
        .declared_output("content_bytes", ValueScalar::Int64, Required::Yes)
        .declared_output("content_type", ValueScalar::String, Required::No)
        .effect(Effect::read_only())
        .build()?;

    // "Updates a file's metadata … This method supports patch semantics."
    let file_update_metadata = file_outputs(
        common(Operation::patch(
            "file.update_metadata",
            "/drive/v3/files/{file_id}",
        ))
        .path_param("file_id", ValueScalar::String)
        .query_static("fields", FILE_FIELDS)
        .body(JsonTemplate::object([(
            "name",
            JsonTemplate::input("name"),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(
        "Google publishes this as a `PATCH` and documents it as supporting \"patch semantics\", \
         and spec 010 §7 admits `NaturalMethod` for `PUT` and `DELETE` only. A patch body of \
         absolute values is repeat-safe and one whose intent is relative is not, and Drive \
         publishes nothing that tells the two apart.",
    )?)
    .build()?;

    // "Permanently deletes a file owned by the user without moving it to the
    // trash." Google documents the response as empty.
    let file_delete = common(Operation::delete(
        "file.delete",
        "/drive/v3/files/{file_id}",
    ))
    .path_param("file_id", ValueScalar::String)
    // Google documents the successful response body as empty and publishes no
    // status code in the discovery document, so both of the statuses it uses
    // for an empty success are declared rather than one of them guessed.
    .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .no_content_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
    .effect(Effect::provider_idempotent_natural_method(
        "Google documents `files.delete` as `DELETE /drive/v3/files/{fileId}` — a fixed resource \
         identity — which \"Permanently deletes a file owned by the user without moving it to the \
         trash\". A repeat names the same file, finds it gone, and answers `404`, which this \
         connector classifies `permanent`; it never deletes a second file.",
    )?)
    .build()?;

    // "Creates a copy of a file and applies any requested updates with patch
    // semantics."
    let file_copy = file_outputs(
        common(Operation::post(
            "file.copy",
            "/drive/v3/files/{file_id}/copy",
        ))
        .path_param("file_id", ValueScalar::String)
        .query_static("fields", FILE_FIELDS)
        .body(JsonTemplate::object([(
            "name",
            JsonTemplate::input("name"),
        )]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in the `drive:v3` discovery document",
        "a second copy of the file, with a new file id",
    )?))
    .build()?;

    // "Creates a file." A folder is a file whose MIME type is Google's own
    // folder type, which is why it is a literal here rather than an input: an
    // operation named `folder.create` that could create anything else would be
    // describing a request nobody reviewed.
    let folder_create = file_outputs(
        common(Operation::post("folder.create", "/drive/v3/files"))
            .query_static("fields", FILE_FIELDS)
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("mimeType", JsonTemplate::literal(json!(FOLDER_MIME_TYPE))),
                // Drive documents the alias `root` for the user's root folder,
                // so a deployment that means the root says so rather than
                // relying on an omitted field.
                ("parents", JsonTemplate::input("parents")),
            ]))
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in the `drive:v3` discovery document",
        "a second folder with the same name in the same parent, and a new file id: Drive does \
             not merge folders by name",
    )?))
    .build()?;

    let permission_list = common(Operation::get(
        "permission.list",
        "/drive/v3/files/{file_id}/permissions",
    ))
    .path_param("file_id", ValueScalar::String)
    .query_static("pageSize", PERMISSION_PAGE_SIZE)
    .query_static("fields", PERMISSION_LIST_FIELDS)
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "permissions",
        "/permissions",
        ValueScalar::Json,
        Required::Yes,
    )
    .output_pointer(
        "next_page_token",
        "/nextPageToken",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "Creates a permission for a file or shared drive." This connector
    // publishes the form that names a person: `type` is Google's `user` or
    // `group`, and `emailAddress` is the address it documents alongside them.
    // The `anyone` and `domain` forms take different fields and are not
    // published by this batch.
    let permission_create = common(Operation::post(
        "permission.create",
        "/drive/v3/files/{file_id}/permissions",
    ))
    .path_param("file_id", ValueScalar::String)
    .query_static("fields", PERMISSION_FIELDS)
    .body(JsonTemplate::object([
        ("type", JsonTemplate::input("type")),
        ("role", JsonTemplate::input("role")),
        ("emailAddress", JsonTemplate::input("email_address")),
    ]))
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
    .output_pointer("type", "/type", ValueScalar::String, Required::No)
    .output_pointer("role", "/role", ValueScalar::String, Required::No)
    .output_pointer(
        "email_address",
        "/emailAddress",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "neither `idempot` nor `dedup` occurs anywhere in the `drive:v3` discovery document, and \
             Google's own reference warns that \"Concurrent permissions operations on the same \
             file aren't supported; only the last update is applied\"",
        "a second permission resource with a new id — and, if it overlapped another writer, an \
             outcome Google declines to define, which is the reason a repeat is never made",
    )?))
    .build()?;

    Ok(vec![
        file_get,
        file_list,
        file_download,
        file_update_metadata,
        file_delete,
        file_copy,
        folder_create,
        permission_list,
        permission_create,
    ])
}
