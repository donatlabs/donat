//! The seam between a stored OAuth2 credential and one provider request.
//!
//! Two things live here and nothing else: the total mapping from a credential
//! failure onto the connector SDK's closed activity-failure set, and the small
//! routing function that hands one attempt a live `Authorization` header.
//!
//! The mapping is the interesting half. `ConnectorErrorClass` is closed on
//! purpose — a Process declares `retry_on` against those eight names, so a
//! ninth would be a class no deployed Process can route. The credential module
//! has its own five-class set so that it does not depend on `crates/connectors`,
//! and mapping between them is therefore an obligation rather than a
//! convenience: every credential class must land on a class that already
//! exists, and the `match` below is exhaustive so that adding a credential class
//! without deciding where it lands does not compile.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::Value as JsonValue;

use crate::credentials::oauth::{CredentialErrorClass, CredentialFailure};
use crate::credentials::refresh::Attempt;
use crate::credentials::runtime::CredentialRuntime;

use super::{
    AuthorizedAttempt, ConnectorErrorClass, ConnectorFailure, ConnectorSuccess, RegisteredConnector,
};

/// A connector instance declares `config.oauth2` and this binary has no
/// credential runtime to satisfy it.
///
/// This is the [[034-a-declaration-the-runtime-ignores-is-a-defect]] guard, and
/// it fails the attempt rather than sending the request: a declared credential
/// that silently does not travel is indistinguishable from a working one until
/// the provider decides otherwise, which is the failure mode that ADR exists to
/// stop.
pub(crate) const NO_CREDENTIAL_RUNTIME: ConnectorFailure = ConnectorFailure::new(
    ConnectorErrorClass::Invariant,
    "connector_credential_runtime_absent",
    "connector instance declares an OAuth2 credential that this deployment did not resolve",
);

/// The activity-failure class one credential failure belongs to.
///
/// Total, and it invents nothing: every arm names a class the SDK already
/// publishes, so a Process that declares `retry_on: [authentication]` routes a
/// credential failure exactly as it routes a provider one.
pub(crate) fn connector_error_class(class: CredentialErrorClass) -> ConnectorErrorClass {
    match class {
        // Spec 011 §7: a missing row, an unusable row, and a refused grant are
        // all `authentication`, whatever a retry policy then decides.
        CredentialErrorClass::Authentication => ConnectorErrorClass::Authentication,
        CredentialErrorClass::Http429 => ConnectorErrorClass::Http429,
        CredentialErrorClass::Http5xx => ConnectorErrorClass::Http5xx,
        CredentialErrorClass::Transport => ConnectorErrorClass::Transport,
        // The token endpoint answered something that is not a token response.
        // There is no `contract` class in the connector set and adding one is
        // not on the table, so this lands on the class that says "asking again
        // will produce the same answer" — which is what a provider that cannot
        // speak RFC 6749 at its own token endpoint will do.
        CredentialErrorClass::Contract => ConnectorErrorClass::Permanent,
    }
}

/// One credential failure as the activity journal records it.
///
/// `code` and `message` cross unchanged because both are `&'static str` written
/// in this workspace — a provider string does not typecheck into either — so
/// nothing a provider said, and no sealed byte, can travel with it.
pub(crate) fn connector_failure(failure: CredentialFailure) -> ConnectorFailure {
    ConnectorFailure::new(
        connector_error_class(failure.class),
        failure.code,
        failure.message,
    )
    .with_retry_after(failure.retry_after)
}

/// Execute one operation under a live `Authorization` header.
///
/// The refresh-and-replay policy is not reimplemented here: it belongs to
/// [`crate::credentials::refresh::with_access_token`], which performs exactly
/// one forced refresh and exactly one replay on a `401` and wipes the header
/// afterwards. This function's whole job is to say what "the provider answered
/// 401" means for a declarative HTTP operation, and to make sure the answer the
/// *operation itself* declared survives.
///
/// That last point is the subtle one. Only the first pass reports
/// `Unauthorized`; the replay's own failure — which is whatever the operation's
/// `error_map` says a `401` is — is returned as the activity's failure. A
/// credential seam that overwrote it would be the same class of defect this
/// wiring exists to remove.
pub(crate) async fn execute_with_credential(
    credentials: &CredentialRuntime,
    registered: Arc<dyn RegisteredConnector>,
    instance: &str,
    operation: &str,
    input: JsonValue,
    idempotency_key: &str,
    deadline: tokio::time::Instant,
) -> Result<ConnectorSuccess, ConnectorFailure> {
    let budget = deadline.saturating_duration_since(tokio::time::Instant::now());
    if budget.is_zero() {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Timeout,
            "connector_timeout",
            "connector operation exceeded its deadline",
        ));
    }
    // The credential exchange runs under the same budget as the operation it is
    // for (spec 011 §6), and never a longer one.
    let budget = budget.min(Duration::from_secs(60));

    // Everything the attempt needs is owned per call, because the callback's
    // future may borrow the applied header and nothing else — which is also
    // what keeps the header's life confined to the attempt.
    let passes = Arc::new(AtomicUsize::new(0));
    // The scheme the connector's own declaration publishes for its tokens. It
    // is read before the exchange, because the header the lifecycle formats and
    // the header the connector admits are one decision.
    let scheme = registered.oauth2_authorization_scheme();
    let operation = operation.to_owned();
    let idempotency_key = idempotency_key.to_owned();
    credentials
        .with_authorization(instance, budget, scheme, |header| {
            let registered = Arc::clone(&registered);
            let passes = Arc::clone(&passes);
            let input = input.clone();
            let operation = operation.clone();
            let idempotency_key = idempotency_key.clone();
            let authorization = header.expose();
            Box::pin(async move {
                let pass = passes.fetch_add(1, Ordering::SeqCst);
                let attempted = registered
                    .execute_authorized(
                        &operation,
                        input,
                        &idempotency_key,
                        deadline,
                        authorization,
                    )
                    .await;
                Ok(match attempted {
                    Ok(AuthorizedAttempt::Done(success)) => Attempt::Done(Ok(success)),
                    // First pass only: refresh once, replay once.
                    Ok(AuthorizedAttempt::Unauthorized(_)) if pass == 0 => Attempt::Unauthorized,
                    Ok(AuthorizedAttempt::Unauthorized(failure)) => Attempt::Done(Err(failure)),
                    Err(failure) => Attempt::Done(Err(failure)),
                })
            })
        })
        .await
        .map_err(connector_failure)?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every credential class lands on a class the SDK already publishes, and
    /// the classes that have an exact counterpart keep it.
    #[test]
    fn the_credential_error_mapping_is_total_and_invents_no_class() {
        let every = [
            CredentialErrorClass::Authentication,
            CredentialErrorClass::Http429,
            CredentialErrorClass::Http5xx,
            CredentialErrorClass::Transport,
            CredentialErrorClass::Contract,
        ];
        for class in every {
            let mapped = connector_error_class(class);
            assert!(
                [
                    ConnectorErrorClass::Transport,
                    ConnectorErrorClass::Timeout,
                    ConnectorErrorClass::Http429,
                    ConnectorErrorClass::Http5xx,
                    ConnectorErrorClass::Authentication,
                    ConnectorErrorClass::Validation,
                    ConnectorErrorClass::Permanent,
                    ConnectorErrorClass::Invariant,
                ]
                .contains(&mapped),
                "{class:?} mapped outside the SDK's closed set"
            );
        }
        // The four that have an exact counterpart keep their own name, so a
        // Process routing `retry_on: [http_429]` still retries a throttled
        // token endpoint.
        assert_eq!(
            connector_error_class(CredentialErrorClass::Authentication),
            ConnectorErrorClass::Authentication
        );
        assert_eq!(
            connector_error_class(CredentialErrorClass::Http429),
            ConnectorErrorClass::Http429
        );
        assert_eq!(
            connector_error_class(CredentialErrorClass::Http5xx),
            ConnectorErrorClass::Http5xx
        );
        assert_eq!(
            connector_error_class(CredentialErrorClass::Transport),
            ConnectorErrorClass::Transport
        );
        // ...and the one that does not lands on the class that says "the same
        // question gets the same answer".
        assert_eq!(
            connector_error_class(CredentialErrorClass::Contract),
            ConnectorErrorClass::Permanent
        );
    }

    /// A credential failure crosses the seam with its own code, message, and
    /// retry hint, and carries nothing else.
    #[test]
    fn a_credential_failure_crosses_the_seam_whole() {
        let throttled = CredentialFailure::new(
            CredentialErrorClass::Http429,
            "token_endpoint_throttled",
            "the connector's token endpoint asked us to slow down",
        )
        .with_retry_after(Some(Duration::from_secs(7)));
        let mapped = connector_failure(throttled);
        assert_eq!(mapped.class(), ConnectorErrorClass::Http429);
        assert_eq!(mapped.code(), "token_endpoint_throttled");
        assert_eq!(mapped.retry_after(), Some(Duration::from_secs(7)));

        let missing = connector_failure(crate::credentials::oauth::NO_CREDENTIAL);
        assert_eq!(missing.class(), ConnectorErrorClass::Authentication);
        assert_eq!(missing.code(), "credential_missing");
    }
}
