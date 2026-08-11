//! OpenAI's REST API (chat completions, embeddings, models, files).
//!
//! Ground truth is OpenAI's own published OpenAPI document,
//! <https://github.com/openai/openai-openapi> (`openapi.yaml`), read on
//! 2026-08-10, together with the reference pages it renders:
//!
//! * `servers: - url: https://api.openai.com/v1` and
//!   `ApiKeyAuth: {type: http, scheme: bearer}`.
//! * `POST /chat/completions` with `required: [model, messages]`, response
//!   `CreateChatCompletionResponse` with `required: [choices, created, id,
//!   model, object]`.
//! * `POST /embeddings` with `required: [model, input]`, response
//!   `CreateEmbeddingResponse` with `required: [object, model, data, usage]`.
//! * `GET /models`, response `ListModelsResponse` with `required: [object,
//!   data]`.
//! * `GET /files` with `limit` ("Limit can range between 1 and 10,000") and
//!   `after` ("A cursor for use in pagination. `after` is an object ID that
//!   defines your place in the list"), response `ListFilesResponse` with
//!   `required: [object, data, first_id, last_id, has_more]`.
//! * `GET /files/{file_id}`, response `OpenAIFile` with `required: [id, object,
//!   bytes, created_at, filename, purpose, status]`.
//! * the error body `ErrorResponse` → `Error` with `required: [type, message,
//!   param, code]`, whose `type` is documented as "The type of error (e.g.,
//!   \"invalid_request_error\", \"server_error\")".
//!
//! # Pagination
//!
//! The documented protocol is the `after` cursor above, and the connector
//! declares it. OpenAI's documented end-of-list signal is `has_more: false`,
//! which spec 010 §8's plan set cannot read: the `Cursor` plan stops when the
//! cursor pointer is absent, null, or empty, so a complete walk here costs one
//! extra call that returns an empty page. A `has_more`-aware cursor is an SDK
//! improvement, not a provider module's business; until it lands, the walk is
//! correct, terminates, and is bounded by the shared budget in both cases —
//! both are proven in `tests/openai.rs`.
//!
//! # Effect classification
//!
//! OpenAI's published OpenAPI document contains no `Idempotency-Key`, no
//! client-supplied request identifier, and no documented deduplication of a
//! repeated request on any of these endpoints; the only occurrences of
//! "idempotent" in the document are on the certificate activation endpoints,
//! which this connector does not publish. `chat.complete` and
//! `embedding.create` are therefore `InventoryOnly` (see `INVENTORY.md`) — and
//! they stay there under ADR 063, on the reasoning spec 012 §2 gives and this
//! module recorded before the class existed: a generative call is billed and
//! non-deterministic, so what an ambiguous outcome leaves behind is a charge
//! nobody can look up and an answer nobody can reproduce. Admitting them is a
//! product decision about this provider, not a gap in the class.
//!
//! # Streaming is out of scope
//!
//! `POST /chat/completions` also publishes a `text/event-stream` response for
//! `stream: true`. This declaration never sets `stream`, and the SDK's bounded
//! transport reads one complete body, so the streaming half of the contract is
//! unreachable from here (spec 012 §6).

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "openai";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// OpenAI's one published API origin.
const ORIGIN: &str = "https://api.openai.com";

/// "Limit can range between 1 and 10,000, and the default is 10,000." The
/// declaration fixes a far smaller page, so one activity's aggregate stays
/// inside the shared budget.
const PAGE_SIZE: u32 = 100;

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("OpenAI's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the OpenAI declarations are valid"))
            .build()
            .expect("the OpenAI declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// The status rules come first on purpose: OpenAI answers an unauthenticated
/// request with `401` and an error `type` of `invalid_request_error`, so a
/// type-first map would call a rejected credential a validation failure and
/// send a durable activity back to retry the same key.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/type")
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_status(404, ConnectorErrorClass::Permanent)
            .on_statuses([413, 422], ConnectorErrorClass::Validation)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // "The type of error (e.g., \"invalid_request_error\",
            // \"server_error\")": whatever status carries them.
            .on_code("invalid_request_error", ConnectorErrorClass::Validation)
            .on_code("server_error", ConnectorErrorClass::Http5xx)
            // OpenAI's own examples read the support handle off this header:
            // `curl -s -D >(grep -i x-request-id >&2)`.
            .correlation_header("request_id", "x-request-id")
            .build()
            .expect("the OpenAI error map is a valid declaration")
    });
    &MAP
}

/// The pagination plan of the one endpoint OpenAI documents as paginated here.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static FILES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::cursor("/data", "after", "/last_id", "limit", PAGE_SIZE)
            .expect("the OpenAI file pagination plan is valid")
    });
    match operation_id {
        "file.list" => Some(&FILES),
        _ => None,
    }
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let chat = Operation::post("chat.complete", "/v1/chat/completions")
        .version(VERSION)
        .body(JsonTemplate::object([
            ("model", JsonTemplate::input("model")),
            ("messages", JsonTemplate::input("messages")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("model", "/model", ValueScalar::String, Required::Yes)
        .output_pointer("created", "/created", ValueScalar::Int64, Required::Yes)
        .output_pointer("choices", "/choices", ValueScalar::Json, Required::Yes)
        .output_pointer("usage", "/usage", ValueScalar::Json, Required::No)
        .effect(Effect::inventory_only(
            "OpenAI's published OpenAPI document declares no idempotency key, request identifier, \
             or deduplication for POST /chat/completions; a repeated request is billed again and \
             may answer differently, so it cannot survive being sent twice",
        )?)
        .build()?;

    let embedding = Operation::post("embedding.create", "/v1/embeddings")
        .version(VERSION)
        .body(JsonTemplate::object([
            ("model", JsonTemplate::input("model")),
            ("input", JsonTemplate::input("input")),
        ]))
        .success_statuses([StatusCode::OK])
        .output_pointer("object", "/object", ValueScalar::String, Required::Yes)
        .output_pointer("model", "/model", ValueScalar::String, Required::Yes)
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer("usage", "/usage", ValueScalar::Json, Required::Yes)
        .effect(Effect::inventory_only(
            "OpenAI's published OpenAPI document declares no idempotency key, request identifier, \
             or deduplication for POST /embeddings; a repeated request is billed again",
        )?)
        .build()?;

    let models = Operation::get("model.list", "/v1/models")
        .version(VERSION)
        .success_statuses([StatusCode::OK])
        .output_pointer("object", "/object", ValueScalar::String, Required::Yes)
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let files = Operation::get("file.list", "/v1/files")
        .version(VERSION)
        .success_statuses([StatusCode::OK])
        .output_pointer("object", "/object", ValueScalar::String, Required::Yes)
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        // `first_id`/`last_id` are required by the schema but are the walk's
        // own bookkeeping, so the declaration publishes them optionally: an
        // empty page carries no identifier to publish.
        .output_pointer("last_id", "/last_id", ValueScalar::String, Required::No)
        .output_pointer("has_more", "/has_more", ValueScalar::Boolean, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let file = Operation::get("file.get", "/v1/files/{file_id}")
        .version(VERSION)
        .path_param("file_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("object", "/object", ValueScalar::String, Required::Yes)
        .output_pointer("bytes", "/bytes", ValueScalar::Int64, Required::Yes)
        .output_pointer(
            "created_at",
            "/created_at",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer("filename", "/filename", ValueScalar::String, Required::Yes)
        .output_pointer("purpose", "/purpose", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![chat, embedding, models, files, file])
}
