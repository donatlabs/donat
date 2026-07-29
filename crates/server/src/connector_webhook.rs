//! Signed inbound connector webhooks.
//!
//! This is deliberately a narrow provider ingress boundary, not a generic
//! webhook API.  It selects only a deployment-declared compiled connector,
//! retains the provider's raw bytes through signature verification, and does
//! not parse or expose configuration values in HTTP responses.
//!
//! There is no durable process ingress journal in this phase.  A verified
//! event therefore receives `503 Service Unavailable` rather than a success
//! acknowledgement: returning a 2xx response before durable acceptance would
//! lose a provider event.  In particular, this module owns no queue, retry
//! state, audit record, process signal, activity failure, or `on_error`
//! input.  Task 6 will introduce the durable transaction that can safely turn
//! a verified event into an acknowledgement.

use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};

use crate::{
    connectors::{http::MAX_HTTP_BODY_BYTES, stripe::WebhookRejection},
    state::SharedState,
};

/// The sole provider-facing inbound route. It is intentionally separate from
/// GraphQL, REST, and MCP rather than being a generic caller-configured hook.
pub const CONNECTOR_WEBHOOK_ROUTE: &str = "/v1/connectors/{instance}/webhooks";

/// Build the signed connector-ingress subrouter before the serving binary
/// supplies its immutable application state.
pub fn router() -> Router<SharedState> {
    Router::new().route(CONNECTOR_WEBHOOK_ROUTE, post(receive))
}

/// Receive a signed provider webhook for one compiled connector instance.
///
/// The instance is resolved before the body is read.  This makes undeclared
/// names indistinguishable from other absent routes even if their request body
/// is oversized or malformed.  Only the Stripe module currently declares an
/// inbound verifier; HTTP connector instances have no inbound route.
pub async fn receive(
    State(state): State<SharedState>,
    Path(instance): Path<String>,
    request: Request,
) -> Response {
    let Some(connector) = state.connectors.stripe_webhook_instance(&instance) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let headers = request.headers().clone();
    let raw_body = match to_bytes(request.into_body(), MAX_HTTP_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match connector.verify_completed_webhook(&headers, &raw_body) {
        Ok(_event) => {
            // A verified event cannot be acknowledged until a later durable
            // process-ingress transaction accepts its provider event ID.
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(
            WebhookRejection::MissingSignature
            | WebhookRejection::InvalidSignature
            | WebhookRejection::TimestampOutOfTolerance
            | WebhookRejection::PayloadTooLarge
            | WebhookRejection::MalformedPayload
            | WebhookRejection::UnsupportedEvent,
        ) => StatusCode::BAD_REQUEST.into_response(),
    }
}
