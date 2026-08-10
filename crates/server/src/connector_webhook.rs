//! Signed inbound connector webhooks.
//!
//! This is deliberately a narrow provider ingress boundary, not a generic
//! webhook API. It selects only a deployment-declared compiled connector,
//! retains exact raw bytes through signature verification, then passes only a
//! bounded normalized event to the source-local durable Process transaction.
//! A success response is impossible until the delivery audit, provider-event
//! dedupe identity, and optional Process event have committed together.

use std::collections::BTreeMap;

use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use donat_connector_catalog::TriggerSpec;
use donat_ir::TypedValue;
use sha2::{Digest, Sha256};

use crate::{
    connectors::{VerifiedDelivery, WebhookRejection},
    processes::{
        InvalidSignatureStatus, persist_invalid_from_engine, persist_verified_from_engine,
    },
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
/// is oversized or malformed. Only the Stripe module currently declares an
/// inbound verifier; HTTP connector instances have no inbound route.
pub async fn receive(
    State(state): State<SharedState>,
    Path(instance): Path<String>,
    request: Request,
) -> Response {
    let Some(connector) = state.connectors.webhook_instance(&instance) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let source_name = connector.source_name().to_owned();
    // A trigger this deployment can correlate publishes a catalog snapshot; one
    // that only verifies publishes none, and answers `503` below rather than
    // borrowing the correlated path's machinery.
    let trigger_id = match connector.trigger() {
        None => None,
        Some(TriggerSpec::Webhook { trigger, .. }) => Some(*trigger),
        Some(TriggerSpec::Poll { .. }) => {
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let raw_body_max_bytes = connector.raw_body_max_bytes();

    let headers = request.headers().clone();
    let raw_body = match to_bytes(request.into_body(), raw_body_max_bytes).await {
        Ok(body) => body,
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    // The borrowed registry instance ends here: every durable step below works
    // from the immutable Engine snapshot and the owned verified event.
    let verification = connector.verify(&headers, &raw_body);

    match verification {
        // A verified delivery of a connector whose Process-owned inbound
        // transaction has not landed yet (spec 013 §0). It is authentic, and
        // there is nowhere to put it: nothing is parsed beyond what the
        // signature needed, nothing is stored, and the answer is the one the
        // route matrix names.
        Ok(VerifiedDelivery::Unacknowledged) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Ok(VerifiedDelivery::Correlated(event)) => {
            let event = *event;
            let Some(trigger) = trigger_id.and_then(|trigger_id| {
                state
                    .connectors
                    .trigger_spec_handle(&source_name, &instance, trigger_id)
            }) else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let engine = state.engine_snapshot().await;
            match persist_verified_from_engine(
                engine.as_ref(),
                state.connectors.as_ref(),
                &source_name,
                &instance,
                trigger.as_ref(),
                event,
            )
            .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => {
                    tracing::error!(
                        source = %source_name,
                        connector = %instance,
                        error = %error,
                        "verified connector webhook could not be committed"
                    );
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                }
            }
        }
        Err(WebhookRejection::PayloadTooLarge) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        // A rejection of a connector with no inbound transaction is the same
        // `400` a correlated one earns, and it writes no audit row: this batch
        // delivers verification and rejection, and the audit belongs to the
        // transaction that does not exist yet.
        Err(_) if trigger_id.is_none() => StatusCode::BAD_REQUEST.into_response(),
        Err(rejection) => {
            let status = match rejection {
                WebhookRejection::MissingSignature => InvalidSignatureStatus::Missing,
                WebhookRejection::InvalidSignature => InvalidSignatureStatus::Invalid,
                WebhookRejection::TimestampOutOfTolerance => InvalidSignatureStatus::Expired,
                WebhookRejection::MalformedPayload => InvalidSignatureStatus::Malformed,
                WebhookRejection::UnsupportedEvent => InvalidSignatureStatus::Unsupported,
                WebhookRejection::PayloadTooLarge => {
                    unreachable!("payload-too-large rejection returned above")
                }
            };
            let payload_digest: [u8; 32] = Sha256::digest(&raw_body).into();
            let redacted_metadata = BTreeMap::from([(
                "reason".to_owned(),
                TypedValue::String(rejection.code().to_owned()),
            )]);
            let engine = state.engine_snapshot().await;
            if let Err(error) = persist_invalid_from_engine(
                engine.as_ref(),
                state.connectors.as_ref(),
                &source_name,
                &instance,
                status,
                &payload_digest,
                &redacted_metadata,
            )
            .await
            {
                tracing::warn!(
                    source = %source_name,
                    connector = %instance,
                    error = %error,
                    "invalid connector webhook audit could not be committed"
                );
            }
            StatusCode::BAD_REQUEST.into_response()
        }
    }
}
