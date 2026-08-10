//! Dropbox's content origin — the one surface that answers with bytes.
//!
//! Ground truth is Dropbox's own published documentation and its own published
//! API description, read on 2026-08-10:
//!
//! * <https://github.com/dropbox/dropbox-api-spec> — `files.stone` declares
//!   `route download (DownloadArg, FileMetadata, DownloadError)` with
//!   `host = "content"` and `style = "download"`, and `DownloadArg` as
//!   "`path` — The path of the file to download."
//! * <https://www.dropbox.com/developers/documentation/http/documentation> —
//!   "Content-download endpoints … Arguments for content-download endpoints are
//!   passed in the `Dropbox-API-Arg` request header or `arg` URL parameter. The
//!   response body contains file content, so the result will appear as JSON in
//!   the `Dropbox-API-Result` response header. These endpoints are on the
//!   `content.dropboxapi.com` domain."
//! * <https://developers.dropbox.com/oauth-guide> and
//!   <https://developers.dropbox.com/error-handling-guide> — the credential form
//!   and the status table, shared with [`crate::providers::dropbox`].
//!
//! # Why this is a second connector and not a second host
//!
//! A connector has one compiled origin, and spec 010 §4 makes it a value
//! nothing in a request, a credential, a provider response, a pagination
//! cursor, or a webhook payload may change. Dropbox serves metadata from
//! `api.dropboxapi.com` and content from `content.dropboxapi.com`, so the two
//! surfaces are two connectors: this one declares the content origin, its own
//! credential contract, and the single operation the batch needs from it. It is
//! the same answer `hubspot` gave for `api.hsforms.com`, taken rather than
//! widened; `knowledgebase/declarative-saas/decisions/074-*` records why the
//! alternative — a declared set of origins per connector — was refused.
//!
//! A deployment that wants both configures two instances with the same OAuth2
//! client. That is the visible cost, and it is the honest one: two origins are
//! two authorities, and a deployment naming both should say so twice.
//!
//! # The argument is a header this module composes
//!
//! The declaration binds `Dropbox-API-Arg` from an input slot the *module*
//! fills: [`download_arg_input`] takes the caller's typed `path` and writes the
//! one-field JSON document Dropbox publishes for it. The slot is
//! `supplied_input`, so no Process can bind it and no caller can put a second
//! field — or a second header line — into a value this connector sends.
//!
//! # Bytes, and the ceiling that carries them
//!
//! Dropbox answers this endpoint with the file itself, and the SDK's response
//! contract is JSON. The module composes the declared output from the response
//! exactly as `google_drive.file.download` does (ADR 056), bounded by the same
//! 1 MiB ceiling the transport enforces, and a body over it is a `validation`
//! failure with **no partial output**: half a file is not a file, and a
//! truncated one is indistinguishable from a complete one downstream.

use std::sync::LazyLock;

use base64::Engine;
use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::providers::dropbox;
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, ConnectorConfiguration, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{Operation, OperationError, Required};
use crate::sdk::pagination::Pagination;
use crate::sdk::transport::MAX_HTTP_BODY_BYTES;

/// The connector name a deployment selects.
pub const NAME: &str = "dropbox_content";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// "These endpoints are on the `content.dropboxapi.com` domain."
const ORIGIN: &str = "https://content.dropboxapi.com";

/// "Arguments for content-download endpoints are passed in the
/// `Dropbox-API-Arg` request header."
pub const ARG_HEADER: &str = "Dropbox-API-Arg";

/// "the result will appear as JSON in the `Dropbox-API-Result` response header."
pub const RESULT_HEADER: &str = "Dropbox-API-Result";

/// The input slot this module composes the argument header from. It is never a
/// caller's: a Process binds `path`, and the module writes this.
const ARG_INPUT: &str = "api_arg";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Dropbox's published content origin is valid"))
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Dropbox content declarations are valid"))
            .build()
            .expect("the Dropbox content declaration is valid")
    });
    &CONNECTOR
}

/// The shared Dropbox error map; the two origins answer with one status table.
pub fn error_map() -> &'static ErrorMap {
    dropbox::error_map()
}

/// A download is one request: there is no collection here to walk.
pub fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

/// A response whose body is larger than this connector will carry.
const DOWNLOAD_TOO_LARGE: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Validation,
    "connector_response_too_large",
    "connector provider response exceeds the declared ceiling",
);

/// Compose the `Dropbox-API-Arg` value from the caller's typed `path`.
///
/// The header is declaration material with an input slot, and this is what
/// fills it. An input that already carries the slot is refused rather than
/// overwritten: it would be a caller choosing the whole argument document, and
/// the point of composing it here is that the caller chooses one field of it.
pub fn download_arg_input(
    _configuration: &ConnectorConfiguration,
    input: &JsonValue,
) -> Result<JsonValue, ConnectorFailure> {
    let Some(fields) = input.as_object() else {
        return Err(ConnectorFailure::invariant(
            "a dropbox_content operation input is a JSON object",
        ));
    };
    if fields.contains_key(ARG_INPUT) {
        return Err(ConnectorFailure::invariant(
            "the Dropbox argument header is composed by this connector and cannot be chosen by \
             input",
        ));
    }
    let Some(JsonValue::String(path)) = fields.get("path") else {
        return Err(ConnectorFailure::invariant(
            "a dropbox_content download declares a string `path`",
        ));
    };
    // Dropbox's `ReadPath` is "The path of the file to download", and its own
    // examples are `/Homework/math/Prime_Numbers.txt`, `id:a4ayc_80_…`, and
    // `rev:a1c10ce0dd78`. Every one of them is visible ASCII-or-UTF-8 text with
    // no control characters; a value carrying one could not be a header value
    // at all, and `serde_json` would escape it into something Dropbox would
    // then read as a different path. Refusing it here names the input.
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(ConnectorFailure::invariant(
            "a dropbox_content download path is non-empty text with no control characters",
        ));
    }
    let mut bound = JsonMap::clone(fields);
    bound.insert(
        ARG_INPUT.to_owned(),
        JsonValue::String(json!({ "path": path }).to_string()),
    );
    Ok(JsonValue::Object(bound))
}

/// Decode one response.
///
/// The status decides first, the ceiling decides second, and only then is the
/// declared output composed — so there is no path by which a truncated or
/// failed download reads as a file.
pub fn decode(
    operation: &Operation,
    status: u16,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<JsonValue, ConnectorFailure> {
    if !operation.is_success(status) {
        return Err(error_map().classify(status, headers, body));
    }
    if body.len() > MAX_HTTP_BODY_BYTES {
        return Err(DOWNLOAD_TOO_LARGE.with_provider_status(status));
    }
    // "the result will appear as JSON in the `Dropbox-API-Result` response
    // header": the file's own metadata, which a caller needs to know what it
    // just received. A header that is not the JSON Dropbox publishes is null
    // rather than a failure — the bytes are the answer, and the metadata is
    // beside them.
    let metadata = headers
        .get(RESULT_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| serde_json::from_str::<JsonValue>(value).ok())
        .unwrap_or(JsonValue::Null);
    let composed = json!({
        "content_base64": base64::engine::general_purpose::STANDARD.encode(body),
        "content_bytes": body.len(),
        "content_type": headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(JsonValue::from)
            .unwrap_or(JsonValue::Null),
        "metadata": metadata,
    });
    operation.extract_output(&composed)
}

/// Every operation this connector publishes: exactly the one the content origin
/// exists for.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // "Download a file from a user's Dropbox." Dropbox publishes this as a
    // content-download endpoint whose argument travels in a header, so the
    // request carries no body at all.
    let file_download = Operation::post("file.download", "/2/files/download")
        .version(VERSION)
        .header_input(ARG_HEADER, ARG_INPUT, ValueScalar::String)
        .declared_input("path", ValueScalar::String, Required::Yes)
        .supplied_input(ARG_INPUT)
        .success_statuses([StatusCode::OK])
        .declared_output("content_base64", ValueScalar::String, Required::Yes)
        .declared_output("content_bytes", ValueScalar::Int64, Required::Yes)
        .declared_output("content_type", ValueScalar::String, Required::No)
        .declared_output("metadata", ValueScalar::Json, Required::No)
        // A `POST` by Dropbox's own transport choice rather than by anything
        // this operation does: spec 023 §3 admits `ReadOnly` for a documented
        // read over `POST` on the provider's own statement, and Dropbox's is
        // "Download a file from a user's Dropbox", with `style = "download"` and
        // a response that is the file's own bytes.
        .effect(Effect::read_only_documented(
            "Dropbox declares `route download (DownloadArg, FileMetadata, DownloadError)` with \
             `style = \"download\"` and documents it as \"Download a file from a user's \
             Dropbox\". Its content-download endpoints are served over `POST` because that is \
             where Dropbox puts every endpoint — \"Arguments for content-download endpoints are \
             passed in the `Dropbox-API-Arg` request header\" — and the call reads the file \
             without changing it.",
        )?)
        .build()?;

    Ok(vec![file_download])
}
