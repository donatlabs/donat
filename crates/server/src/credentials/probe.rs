//! One read-only request, sent to find out whether the provider accepts the
//! credential this deployment just stored.
//!
//! Everything else in this module tree proves that a token was obtained,
//! sealed, and can be unsealed. None of it proves the provider will take it:
//! the authorization code flow can complete perfectly against a token endpoint
//! and still leave a credential the API refuses, because the scopes were wrong,
//! the app is not approved for the account, or the connector's origin is not
//! where that account lives. The only thing that settles it is asking the
//! provider, and at `donat connector authorize` the operator is standing there
//! with the credential in hand — which is the one moment this is free to do.
//!
//! ## Three outcomes, and why not two
//!
//! A probe answers `Accepted`, `Rejected`, or `Unverified`, and the third is
//! the load-bearing one. A `404` from a wrong path, a timeout, a provider
//! having an outage — none of these say anything about the credential, and
//! rendering any of them as either a tick or a cross is a lie. The rule is that
//! **a probe that could not reach a verdict must never read as success**, and
//! must never read as a credential failure either.
//!
//! It follows that the verdict is advisory. The credential is already written
//! before the probe runs, and no outcome here deletes it or fails the command:
//! an operator whose provider is briefly down should not be left without a
//! stored credential, and an operator whose credential really is refused is
//! better served by a message than by a rollback that hides the evidence.
//!
//! ## What the classes mean here
//!
//! The verdict reads the connector's own error classification rather than a
//! raw status code, because the error map is where each provider's conventions
//! already live — one provider's `403` on a valid key with a narrow scope is
//! another's hard refusal, and that difference is encoded per module rather
//! than guessed here.

use donat_connectors::sdk::{Connector, ConnectorErrorClass, ConnectorFailure, Operation};

/// What one probe established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The provider answered the request. The credential reached it and was
    /// not refused.
    Accepted,
    /// The provider refused the credential itself.
    Rejected,
    /// The probe reached no verdict. This is not a failure of the credential
    /// and not a success — it is the absence of evidence.
    Unverified,
}

impl ProbeOutcome {
    /// The word an operator reads, chosen so `Unverified` cannot be mistaken
    /// for either of the other two at a glance.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "REFUSED",
            Self::Unverified => "not verified",
        }
    }
}

/// One probe's verdict, and the sentence explaining it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeVerdict {
    pub outcome: ProbeOutcome,
    /// Names the operation that was sent, so an operator can repeat it.
    pub operation: Option<String>,
    pub detail: String,
}

impl ProbeVerdict {
    /// The connector declares nothing that can be sent blind.
    ///
    /// Reported rather than worked around: inventing an identifier so that
    /// *some* request can go out would turn a credential question into a "no
    /// such record" answer, which is the false-negative this whole module is
    /// built to avoid.
    pub fn no_probe_available() -> Self {
        Self {
            outcome: ProbeOutcome::Unverified,
            operation: None,
            detail: "this connector declares no read that needs no input, so there is \
                     nothing that can be sent without inventing an argument"
                .to_owned(),
        }
    }

    /// The provider answered the probe.
    pub fn accepted(operation: &str) -> Self {
        Self {
            outcome: ProbeOutcome::Accepted,
            operation: Some(operation.to_owned()),
            detail: "the provider answered this read, so it took the credential".to_owned(),
        }
    }

    /// Classify a failed probe.
    pub fn from_failure(operation: &str, failure: &ConnectorFailure) -> Self {
        let (outcome, detail) = match failure.class() {
            // The one class that is about the credential.
            ConnectorErrorClass::Authentication => (
                ProbeOutcome::Rejected,
                "the provider refused the credential itself. Check the scopes the \
                 authorization granted, and that the account is the one this instance \
                 is meant to act as."
                    .to_owned(),
            ),
            // A refusal that is about permissions, not identity: the provider
            // knows who this is and will not let them do it. That is worth
            // saying precisely, and it is not a credential failure.
            ConnectorErrorClass::Permanent => (
                ProbeOutcome::Unverified,
                "the provider refused this particular read. The credential may still be \
                 good — this probe may name a path the account cannot see, or one this \
                 workspace declared wrongly."
                    .to_owned(),
            ),
            ConnectorErrorClass::Transport | ConnectorErrorClass::Timeout => (
                ProbeOutcome::Unverified,
                "the provider could not be reached, which says nothing about the \
                 credential. Try again when it answers."
                    .to_owned(),
            ),
            ConnectorErrorClass::Http429 | ConnectorErrorClass::Http5xx => (
                ProbeOutcome::Unverified,
                "the provider is rate-limiting or failing, which says nothing about the \
                 credential."
                    .to_owned(),
            ),
            other => (
                ProbeOutcome::Unverified,
                format!(
                    "the probe failed as `{other:?}`, which says nothing about the \
                     credential; this is more likely a defect in the probe than in \
                     the credential"
                ),
            ),
        };
        Self {
            outcome,
            operation: Some(operation.to_owned()),
            detail,
        }
    }
}

/// The operation this connector can spend on a probe, if it has one.
pub fn probe_operation(connector: &Connector) -> Option<&Operation> {
    connector.auth_probe()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(class: ConnectorErrorClass) -> ConnectorFailure {
        ConnectorFailure::new(class, "probe_test", "a probe answer")
    }

    /// Only an authentication class is a verdict about the credential.
    ///
    /// Every other class describes something between here and the answer — a
    /// wrong path, an outage, a rate limit — and reporting any of them as a
    /// refused credential sends an operator to rotate a key that was fine.
    #[test]
    fn only_authentication_refuses_the_credential() {
        assert_eq!(
            ProbeVerdict::from_failure("x", &failure(ConnectorErrorClass::Authentication)).outcome,
            ProbeOutcome::Rejected
        );
        for class in [
            ConnectorErrorClass::Transport,
            ConnectorErrorClass::Timeout,
            ConnectorErrorClass::Http429,
            ConnectorErrorClass::Http5xx,
            ConnectorErrorClass::Permanent,
            ConnectorErrorClass::Validation,
            ConnectorErrorClass::Invariant,
        ] {
            assert_eq!(
                ProbeVerdict::from_failure("x", &failure(class)).outcome,
                ProbeOutcome::Unverified,
                "{class:?} is not evidence about the credential"
            );
        }
    }

    /// No failure class ever renders as success.
    ///
    /// This is the rule the module exists for: a false green tick on a probe
    /// that proved nothing is worse than no probe, because it retires the
    /// question.
    #[test]
    fn no_failure_is_ever_reported_as_accepted() {
        for class in [
            ConnectorErrorClass::Authentication,
            ConnectorErrorClass::Transport,
            ConnectorErrorClass::Timeout,
            ConnectorErrorClass::Http429,
            ConnectorErrorClass::Http5xx,
            ConnectorErrorClass::Permanent,
            ConnectorErrorClass::Validation,
            ConnectorErrorClass::Invariant,
        ] {
            assert_ne!(
                ProbeVerdict::from_failure("x", &failure(class)).outcome,
                ProbeOutcome::Accepted
            );
        }
    }

    /// A connector with nothing to send says so, and says it as "unverified"
    /// rather than as either verdict.
    #[test]
    fn a_connector_with_no_blind_read_is_unverified() {
        let verdict = ProbeVerdict::no_probe_available();
        assert_eq!(verdict.outcome, ProbeOutcome::Unverified);
        assert!(verdict.operation.is_none());
    }

    /// The three labels are distinct, and the refusal is the one that shouts.
    #[test]
    fn the_labels_cannot_be_confused_for_one_another() {
        assert_eq!(ProbeOutcome::Accepted.label(), "accepted");
        assert_eq!(ProbeOutcome::Rejected.label(), "REFUSED");
        assert_eq!(ProbeOutcome::Unverified.label(), "not verified");
    }
}
