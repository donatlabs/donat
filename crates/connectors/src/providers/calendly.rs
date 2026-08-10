//! Calendly's v2 API and its timestamped webhook signatures.
//!
//! Ground truth is Calendly's own published documentation, read on 2026-08-10.
//! `developer.calendly.com/api-docs` renders one OpenAPI 3.0 document, and the
//! quotes below are that document's own text:
//!
//! * `servers: [{ url: 'https://api.calendly.com' }]`, and
//!   `info.description`: "Calendly's API is REST-based and has predictable
//!   resource-oriented URLs."
//! * Both security schemes carry the same description: "Put the access token in
//!   the `Authorization: Bearer <TOKEN>` header".
//! * Get Event — "Returns information about a specified Event." / "Required
//!   scopes: `scheduled_events:read`"; List Events, Get Event Invitee, List
//!   Event Invitees, and Get current user — "Returns basic information about
//!   your user account." / "Required scopes: `users:read`".
//! * <https://developer.calendly.com/api-docs/ZG9jOjE1MDE3NzI-api-conventions> —
//!   "When calling an endpoint that returns a collection of multiple resources,
//!   you will notice a `pagination` object in your response body."
//! * <https://developer.calendly.com/api-docs/edca8074633f8-api-rate-limits> —
//!   the `429` response and the `X-RateLimit-*` headers.
//! * <https://developer.calendly.com/api-docs/4c305798a61d3-webhook-signatures>
//!   for the inbound half.
//!
//! # Pagination
//!
//! Calendly publishes both halves of its continuation: `pagination.next_page`,
//! "URI to return the next page of an ordered list", and
//! `pagination.next_page_token`, "Token to return the next page of an ordered
//! list". This connector declares the **token**, and the reason is in Calendly's
//! own documentation: the List Event Invitees example emits `next_page` on the
//! wrong host (`https://calendly.com/...` rather than `https://api.calendly.com/...`).
//! A `TokenInBody` value can only ever be spent as a query parameter on this
//! connector's own compiled origin, so a body that names another host cannot
//! become a destination — which is exactly the distinction ADR 047 drew between
//! `TokenInBody` and `NextUriInBody`. Both fields are documented as `null` when
//! the walk is over, which is where the plan stops.
//!
//! # Effect classification
//!
//! Every operation here is a `GET`, so each is read-only by its method and there
//! is no mutating class to argue about. Recorded for completeness because a
//! later batch will add writes: Calendly publishes **no** idempotency key, no
//! client-supplied request identifier, and no deduplication anywhere in its v2
//! API — the terms do not occur in the OpenAPI document or in any of its
//! documentation nodes.
//!
//! # The signing key is write-only
//!
//! `POST /webhook_subscriptions` takes `signing_key` — "Optional secret key
//! shared between your application and Calendly" — and the created subscription
//! resource does **not** carry it back; the `Webhook Subscription` model has no
//! such property. A deployment therefore chooses the secret and keeps it, which
//! is precisely how `config.webhook_secret` is configured here.

use std::sync::LazyLock;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::providers::inbound::{EventIdentifier, TriggerEvent};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec, Trigger};
use crate::sdk::effect::Effect;
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;
use crate::sdk::webhook::{SignatureEncoding, WebhookVerifier};

/// The connector name a deployment selects.
pub const NAME: &str = "calendly";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ url: 'https://api.calendly.com' }]`.
const ORIGIN: &str = "https://api.calendly.com";

/// "`count` — The number of rows to return", `maximum: 100`.
const PAGE_SIZE: &str = "100";

/// Calendly's documented replay tolerance: "In the example below, the tolerance
/// zone is set to 3 minutes, so any webhooks received that are older than 3
/// minutes will be rejected."
///
/// Calendly presents 180 seconds as its own sample application's choice rather
/// than as a protocol constant it enforces, and this declaration adopts it as
/// written. The SDK's window closes in both directions, which is stricter than
/// Calendly's sample — that one rejects only a timestamp that is too old, so a
/// signature dated far in the future would pass it.
const TIMESTAMP_TOLERANCE: Duration = Duration::from_secs(180);

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        let mut builder = Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Calendly's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Calendly declarations are valid"));
        for event in events() {
            builder = builder.trigger(
                Trigger::webhook(event.provider_event(), VERSION, verification())
                    .expect("a Calendly trigger declaration is valid"),
            );
        }
        builder.build().expect("the Calendly declaration is valid")
    });
    &CONNECTOR
}

/// "`Calendly-Webhook-Signature`" — the one header a delivery carries beyond the
/// ordinary HTTP set.
pub const SIGNATURE_HEADER: &str = "Calendly-Webhook-Signature";

/// Calendly's inbound signature scheme.
///
/// "When Calendly sends your app a webhook, it will include the
/// `Calendly-Webhook-Signature` header in the following format:
/// `Calendly-Webhook-Signature: t=1492774577,v1=5257a869...`", the signed payload
/// is built by "concatenating the timestamp (t), the character '.', and the
/// request body's JSON payload", and the digest is computed "by computing an
/// HMAC with the SHA256 hash function" and rendered as lowercase hexadecimal.
///
/// Calendly's own Ruby sample reads `request.body.read` — the raw bytes — while
/// its Node.js sample re-serializes the parsed body with `JSON.stringify`. This
/// connector verifies the raw bytes, which is the Ruby sample's reading and the
/// only one that does not depend on a serializer reproducing Calendly's own
/// output byte for byte.
pub fn verification() -> WebhookVerifier {
    WebhookVerifier::hmac_timestamped(SIGNATURE_HEADER)
        .expect("a static header name is valid")
        .signature_element("v1")
        .timestamp_element("t")
        .separator(".")
        .encoding(SignatureEncoding::Hex)
        .tolerance(TIMESTAMP_TOLERANCE)
        .build()
        .expect("the Calendly signature scheme is a valid declaration")
}

/// The inbound events this connector declares (spec 013 §3).
///
/// **Calendly publishes no per-delivery identifier.** The webhook payload object
/// has exactly four top-level properties — `event`, `created_at`, `created_by`,
/// `payload` — all four required, and none of them is a delivery or event id;
/// no delivery header carries one either. Since Calendly also retries a failed
/// delivery for 24 hours, duplicates are expected and there is nothing
/// provider-issued to key them on. That is recorded as
/// [`EventIdentifier::Unpublished`] rather than papered over with a synthesized
/// value dressed up as the provider's.
///
/// `created_by` is deliberately not exposed as an actor: Calendly documents it
/// as "The user who created the webhook", which is the subscription's owner
/// rather than the person who booked or cancelled.
pub fn events() -> &'static [TriggerEvent] {
    static EVENTS: LazyLock<Vec<TriggerEvent>> = LazyLock::new(|| {
        let invitee = |event: &'static str| {
            TriggerEvent::declare(
                event,
                EventIdentifier::Unpublished,
                [
                    (
                        "created_at",
                        "/created_at",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    // "Canonical reference (unique identifier) for the invitee".
                    (
                        "invitee_uri",
                        "/payload/uri",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    // "A reference to the event" — the flat URI, which is the
                    // same field name the REST invitee resource carries, so a
                    // webhook and a read join on one key.
                    (
                        "scheduled_event_uri",
                        "/payload/event",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "email",
                        "/payload/email",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    ("name", "/payload/name", ValueScalar::String, Required::Yes),
                    (
                        "status",
                        "/payload/status",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "cancel_url",
                        "/payload/cancel_url",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "reschedule_url",
                        "/payload/reschedule_url",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("a Calendly invitee event declaration is valid")
        };
        vec![invitee("invitee.created"), invitee("invitee.canceled")]
    });
    &EVENTS
}

/// The ordered error map.
///
/// Calendly publishes an `Error Response` object whose required fields are
/// `title` and `message` — both human-readable — and an optional `details` array
/// whose items carry a `code`. There is no stable top-level machine-readable
/// code, so the map is keyed on the documented statuses and reads nothing from
/// the body.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 — Request is not valid".
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 — Cannot authenticate caller", "403 — Permission Denied or
            // insufficient scope".
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 — Requested resource not found", "409 — Attempt to create a
            // resource that already exists", "424 — Unable to access external
            // calendar".
            .on_statuses([404, 409, 424], ConnectorErrorClass::Permanent)
            // "429 Too Many Requests", whose documented retry signal is
            // `X-RateLimit-Reset`; Calendly publishes no `Retry-After` at all,
            // so a retry delay only ever arrives if it sends the standard header.
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 — An error has occurred on the server."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Calendly error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection; see the module documentation for
/// why it is the token rather than the URI.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::token_in_body("/collection", "/pagination/next_page_token", "page_token")
            .expect("the Calendly token plan is valid")
    });
    match operation_id {
        "event.list" | "invitee.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let event_get = common(Operation::get("event.get", "/scheduled_events/{uuid}"))
        .path_param("uuid", ValueScalar::String)
        .success_statuses([StatusCode::OK])
        .output_pointer("uri", "/resource/uri", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/resource/name", ValueScalar::String, Required::Yes)
        .output_pointer(
            "status",
            "/resource/status",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "start_time",
            "/resource/start_time",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "end_time",
            "/resource/end_time",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "event_type",
            "/resource/event_type",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    // Calendly marks none of this endpoint's query parameters `required`, but
    // documents that a caller must scope the request — its own 403 messages
    // include "Please also specify organization when requesting events for a
    // user within your organization." Both scoping parameters are therefore
    // declared inputs, and which of them a caller fills is the caller's.
    let event_list = common(Operation::get("event.list", "/scheduled_events"))
        .query_input("user", "user")
        .query_input("organization", "organization")
        .query_input("status", "status")
        .query_static("count", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        .output_pointer(
            "collection",
            "/collection",
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "next_page_token",
            "/pagination/next_page_token",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let invitee_get = common(Operation::get(
        "invitee.get",
        "/scheduled_events/{event_uuid}/invitees/{invitee_uuid}",
    ))
    .path_param("event_uuid", ValueScalar::String)
    .path_param("invitee_uuid", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("uri", "/resource/uri", ValueScalar::String, Required::Yes)
    .output_pointer(
        "email",
        "/resource/email",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer("name", "/resource/name", ValueScalar::String, Required::Yes)
    .output_pointer(
        "status",
        "/resource/status",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "event",
        "/resource/event",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let invitee_list = common(Operation::get(
        "invitee.list",
        "/scheduled_events/{uuid}/invitees",
    ))
    .path_param("uuid", ValueScalar::String)
    .query_input("status", "status")
    .query_static("count", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "collection",
        "/collection",
        ValueScalar::Json,
        Required::Yes,
    )
    .output_pointer(
        "next_page_token",
        "/pagination/next_page_token",
        ValueScalar::String,
        Required::No,
    )
    .effect(Effect::read_only())
    .build()?;

    // "`uuid` — User unique identifier, or the constant \"me\" to reference the
    // caller." The caller is the only user this connector's credential can
    // name, so the path is the documented `/users/me` and takes no input.
    let user_me = common(Operation::get("user.me", "/users/me"))
        .success_statuses([StatusCode::OK])
        .output_pointer("uri", "/resource/uri", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/resource/name", ValueScalar::String, Required::Yes)
        .output_pointer(
            "email",
            "/resource/email",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "scheduling_url",
            "/resource/scheduling_url",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "current_organization",
            "/resource/current_organization",
            ValueScalar::String,
            Required::Yes,
        )
        .effect(Effect::read_only())
        .build()?;

    Ok(vec![
        event_get,
        event_list,
        invitee_get,
        invitee_list,
        user_me,
    ])
}
