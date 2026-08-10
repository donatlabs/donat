//! Typeform's Create, Responses, and Webhooks APIs.
//!
//! Ground truth is Typeform's own published developer documentation, read on
//! 2026-08-10:
//!
//! * <https://www.typeform.com/developers/get-started/> — "The base URL for
//!   Create, Responses, and Webhooks is `https://api.typeform.com/`", and "For
//!   the Create and Responses APIs, you can send two requests per second, per
//!   Typeform account."
//! * <https://www.typeform.com/developers/get-started/personal-access-token/> —
//!   "you need to pass your personal access token in the Authorization header
//!   of your requests", with
//!   <https://www.typeform.com/developers/get-started/applications/> giving the
//!   scheme: "token_type … Access token type. Always Bearer."
//! * <https://www.typeform.com/developers/create/reference/retrieve-forms/> —
//!   `GET https://api.typeform.com/forms`, `page` (integer, default 1),
//!   `page_size` (number, default 10, max 200), response `{total_items,
//!   page_count, items, _links}`.
//! * <https://www.typeform.com/developers/responses/reference/retrieve-responses/>,
//!   <https://www.typeform.com/developers/responses/reference/delete-responses/>,
//!   <https://www.typeform.com/developers/webhooks/reference/retrieve-webhooks/>.
//! * <https://www.typeform.com/developers/troubleshooting/> — the error object
//!   with its `code` ("short, readable key") and `description`, the status
//!   table (400 Bad Request, 401 Unauthorized, 402 Payment Required, 403
//!   Forbidden, 404 Not Found, 500 Internal Server Error, 503 Service
//!   Unavailable), and the documented codes this module's map names.
//!
//! # Regions
//!
//! Typeform publishes EU-resident origins (`https://api.eu.typeform.com`) for
//! accounts with EU data residency. An origin is part of a connector's
//! identity, so this declaration is the global origin only.
//!
//! # Pagination
//!
//! `form.list` is the one endpoint here whose documented protocol a plan in
//! spec 010 §8 expresses: `page` is one-based, exactly as the `PageNumber` plan
//! walks it. The responses endpoint paginates with `before`/`after` cursors
//! whose values are the tokens of individual items — "Return responses
//! submitted after this cursor (exclusive)" — and publishes no top-level
//! cursor, so no plan in the closed set can read one; declaring `Cursor`
//! against a pointer that does not exist would produce a walk that silently
//! stops after one page. `response.list` therefore declares no continuation
//! plan and returns one page whose size is fixed by the declaration. An SDK
//! plan whose cursor is read from the last item of the page is the missing
//! piece, and it belongs in `sdk/pagination.rs` with its own test.
//!
//! # Effect classification
//!
//! `response.delete` is `ProviderIdempotent::NaturalMethod` on Typeform's own
//! statement that a repeat is harmless: "Not found response IDs will be
//! ignored." Everything else here is a `GET`. This connector declares no
//! create, so it contributes no inventory-only operation at all.
//!
//! # The inbound half (spec 013)
//!
//! Typeform's webhook verification is published at
//! <https://www.typeform.com/developers/webhooks/secure-your-webhooks/> and its
//! payload at <https://www.typeform.com/developers/webhooks/example-payload/>,
//! both read on 2026-08-10. The scheme is four documented steps: "Using the HMAC
//! SHA-256 algorithm, create a hash (using `secret` as a key) of the entire
//! received payload as binary", "Encode the binary hash in `base64` format",
//! "Add prefix `sha256=` to the binary hash", and "Compare the created value
//! with the signature you received in the `Typeform-Signature` header".
//!
//! Three facts about it are worth recording because they shape what this
//! connector can promise.
//!
//! * **There is no timestamp.** The signature covers "the entire received
//!   payload" and nothing else, so an authentic delivery stays verifiable
//!   forever and replay protection cannot come from a window.
//! * **There is no delivery header.** The only header Typeform names on a
//!   delivery is `Typeform-Signature`; the identifier is `event_id` in the body,
//!   documented as "Unique ID for the webhook. Automatically assigned by
//!   Typeform." Typeform never states whether a retry carries the same one.
//! * **Retries are documented and success is a `2XX`.** "Your webhook should
//!   send a `2XX` HTTP response status code back", "If your webhook doesn't
//!   respond within 30 seconds, Typeform will mark the delivery as failed", and
//!   a `404` or `410` from the endpoint disables the subscription outright.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::providers::inbound::{EventIdentifier, TriggerEvent};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec, Trigger};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{Operation, OperationError, Required};
use crate::sdk::pagination::Pagination;
use crate::sdk::webhook::{SignatureEncoding, WebhookVerifier};

/// The connector name a deployment selects.
pub const NAME: &str = "typeform";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Typeform's one published API origin for Create, Responses, and Webhooks.
const ORIGIN: &str = "https://api.typeform.com";

/// "page_size (number, default: 10, max: 200)" on the forms collection.
const FORM_PAGE_SIZE: u32 = 200;

/// The responses endpoint's page, fixed by the declaration. Typeform's own
/// guidance is to narrow the scope rather than to ask for everything: "If your
/// typeform has more than 1000 responses, use the since / until or before /
/// after query parameters to narrow the scope of your request."
const RESPONSE_PAGE_SIZE: &str = "100";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        let mut builder = Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Typeform's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Typeform declarations are valid"));
        for event in events() {
            builder = builder.trigger(
                Trigger::webhook(event.provider_event(), VERSION, verification())
                    .expect("a Typeform trigger declaration is valid"),
            );
        }
        builder.build().expect("the Typeform declaration is valid")
    });
    &CONNECTOR
}

/// "`Typeform-Signature`" — the one header a delivery carries.
pub const SIGNATURE_HEADER: &str = "Typeform-Signature";

/// Typeform's inbound signature scheme; see the module documentation.
///
/// A value without the declared `sha256=` prefix offers no candidate at all,
/// which matches Typeform's own Python sample: it splits the header on `=`,
/// refuses anything whose first element is not `sha256`, and only then compares.
pub fn verification() -> WebhookVerifier {
    WebhookVerifier::hmac_body_with_prefix(SIGNATURE_HEADER, "sha256=", SignatureEncoding::Base64)
        .expect("the Typeform signature scheme is a valid declaration")
}

/// The inbound events this connector declares (spec 013 §3).
///
/// Typeform publishes no enumeration of event types; the two keys that appear
/// anywhere in its developer documentation are `form_response` — the
/// `event_type` of the published sample payload — and `form_response_partial`,
/// which appears only as a subscription key and for which Typeform publishes no
/// sample payload and never states what the delivered `event_type` is. Only the
/// documented one is declared.
///
/// `calculated`, `variables`, and `hidden` are the three fields Typeform marks
/// conditional ("If your typeform has variables…"), so none of them is declared
/// required; `answers` and `definition` are described unconditionally but with
/// hedged element shapes ("generally includes", "may include"), so the
/// declaration exposes them as opaque JSON rather than typing a shape Typeform
/// did not commit to.
pub fn events() -> &'static [TriggerEvent] {
    static EVENTS: LazyLock<Vec<TriggerEvent>> = LazyLock::new(|| {
        vec![
            TriggerEvent::declare(
                "form_response",
                EventIdentifier::BodyPointer("/event_id"),
                [
                    ("event_id", "/event_id", ValueScalar::String, Required::Yes),
                    (
                        "event_type",
                        "/event_type",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "form_id",
                        "/form_response/form_id",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    // "Unique ID for the typeform submission. This is identical
                    // to response id in the Responses API" — the business key a
                    // later correlation would join on.
                    (
                        "token",
                        "/form_response/token",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "submitted_at",
                        "/form_response/submitted_at",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "landed_at",
                        "/form_response/landed_at",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "answers",
                        "/form_response/answers",
                        ValueScalar::Json,
                        Required::Yes,
                    ),
                    (
                        "hidden",
                        "/form_response/hidden",
                        ValueScalar::Json,
                        Required::No,
                    ),
                ],
            )
            .expect("the Typeform form_response event declaration is valid"),
        ]
    });
    &EVENTS
}

/// The ordered error map.
///
/// The status rules are declared first, so a documented status decides the
/// class and a documented code refines whatever the status table does not name.
/// Typeform's `description` is prose and is never matched on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/code")
            // "400 Bad Request (validation errors)".
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized (incorrect access token)" and "403 Forbidden
            // (authentication error)".
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "402 Payment Required (feature requires PRO/PRO+ account)" is
            // permanent for this deployment: repeating it changes nothing.
            .on_status(402, ConnectorErrorClass::Permanent)
            // "404 Not Found (missing resource)" and the "405 Method not
            // allowed" the Responses API documents.
            .on_statuses([404, 405], ConnectorErrorClass::Permanent)
            // Typeform documents a rate limit of "two requests per second, per
            // Typeform account" without naming its status; `429` is the one
            // HTTP defines for it.
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error" and "503 Service Unavailable".
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // The documented codes, for any status the table above does not
            // name: `AUTHENTICATION_ERROR`/`INVALID_AUTHORIZATION`
            // ("incorrect access token"), `UNAUTHORIZED`, `VALIDATION_ERROR`,
            // `NOT_EXISTING_ID` ("missing resource"), `SERVER_ERROR`, and
            // `SERVICE_UNAVAILABLE`.
            .on_code("AUTHENTICATION_ERROR", ConnectorErrorClass::Authentication)
            .on_code("INVALID_AUTHORIZATION", ConnectorErrorClass::Authentication)
            .on_code("UNAUTHORIZED", ConnectorErrorClass::Authentication)
            .on_code("VALIDATION_ERROR", ConnectorErrorClass::Validation)
            .on_code("NOT_EXISTING_ID", ConnectorErrorClass::Permanent)
            .on_code("SERVER_ERROR", ConnectorErrorClass::Http5xx)
            .on_code("SERVICE_UNAVAILABLE", ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Typeform error map is a valid declaration")
    });
    &MAP
}

/// The pagination plan of the one operation whose documented protocol a plan in
/// the closed set expresses; see the module documentation.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static FORMS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/items", "page", "page_size", FORM_PAGE_SIZE)
            .expect("the Typeform form pagination plan is valid")
    });
    match operation_id {
        "form.list" => Some(&FORMS),
        _ => None,
    }
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let form_list = Operation::get("form.list", "/forms")
        .version(VERSION)
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "total_items",
            "/total_items",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer(
            "page_count",
            "/page_count",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer("items", "/items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let form_get = Operation::get("form.get", "/forms/{form_id}")
        .version(VERSION)
        .path_param("form_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("title", "/title", ValueScalar::String, Required::Yes)
        .output_pointer("fields", "/fields", ValueScalar::Json, Required::No)
        .output_pointer("settings", "/settings", ValueScalar::Json, Required::No)
        .effect(Effect::read_only())
        .build()?;

    let response_list = Operation::get("response.list", "/forms/{form_id}/responses")
        .version(VERSION)
        .path_param("form_id", ValueScalar::String)
        .query_static("page_size", RESPONSE_PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "total_items",
            "/total_items",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer(
            "page_count",
            "/page_count",
            ValueScalar::Int64,
            Required::Yes,
        )
        .output_pointer("items", "/items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    // "included_response_ids: Limit request to the specified response_ids",
    // success "200 OK". Typeform documents no body for the success, so the
    // declaration carries no output pointer and says the success is
    // no-content: an empty body is the documented answer, not a malformed one.
    let response_delete = Operation::delete("response.delete", "/forms/{form_id}/responses")
        .version(VERSION)
        .path_param("form_id", ValueScalar::String)
        .query_input("included_response_ids", "included_response_ids")
        .success_statuses([StatusCode::OK])
        .no_content_statuses([StatusCode::OK])
        .effect(Effect::provider_idempotent_natural_method(
            "Typeform documents the delete against the response identities named in the request \
             and states that a repeat is harmless: \"Not found response IDs will be ignored.\"",
        )?)
        .build()?;

    let webhook_list = Operation::get("webhook.list", "/forms/{form_id}/webhooks")
        .version(VERSION)
        .path_param("form_id", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("items", "/items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        form_list,
        form_get,
        response_list,
        response_delete,
        webhook_list,
    ])
}
