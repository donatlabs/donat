//! What a connector declares about one inbound event (spec 013 §3).
//!
//! An SDK [`crate::sdk::Trigger`] carries the part the *route* needs — the
//! raw-body ceiling and the verification applied to the exact bytes — and
//! deliberately nothing else, because verification must not depend on anything
//! a parser produced. What a future signal mapper needs is the other half: the
//! provider's own event name, the typed fields that event exposes, and where
//! the provider publishes the identifier that names one delivery.
//!
//! That half lives here, as inert data. Nothing in this module reads a body, so
//! a declaration cannot be mistaken for a decoder, and a trigger's fields are
//! visible to a test and to a reviewer before any correlation exists to use
//! them.
//!
//! Two shapes of event identifier are declared, because the six providers in
//! this batch publish both: GitHub's `X-GitHub-Delivery` and Shopify's
//! `X-Shopify-Webhook-Id` are *headers*, while Telegram's `update_id` and
//! Typeform's `event_id` are *body* fields. A provider that publishes neither is
//! recorded as publishing neither rather than given an invented one.

use donat_value_contract::ValueScalar;

use crate::sdk::operation::{OperationError, Required};

/// A static, absolute JSON pointer into a verified provider body.
///
/// The same rule the SDK's own output pointers obey, restated here because the
/// SDK's copy is crate-private to `sdk` and a second, weaker rule would be
/// worse than a second identical one.
fn validate_json_pointer(pointer: &str) -> Result<(), OperationError> {
    if !pointer.starts_with('/') || pointer.ends_with('/') || pointer.contains(['{', '}']) {
        return Err(OperationError::new(
            "a JSON pointer must be static and absolute",
        ));
    }
    Ok(())
}

/// Where a provider publishes the identifier of one delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventIdentifier {
    /// A delivery header, such as GitHub's `X-GitHub-Delivery`.
    Header(&'static str),
    /// A JSON pointer into the verified body, such as Typeform's `/event_id`.
    BodyPointer(&'static str),
    /// The provider publishes no per-delivery identifier at all.
    ///
    /// This is recorded rather than worked around: a dedupe layer that keys on
    /// a value the provider never promised to keep unique is worse than one
    /// that knows it has nothing to key on.
    Unpublished,
}

/// One typed field a verified event exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerField {
    name: &'static str,
    pointer: &'static str,
    scalar: ValueScalar,
    required: Required,
}

impl TriggerField {
    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn pointer(&self) -> &'static str {
        self.pointer
    }

    pub const fn scalar(&self) -> &ValueScalar {
        &self.scalar
    }

    pub const fn required(&self) -> Required {
        self.required
    }
}

/// One provider event a connector's trigger set declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerEvent {
    provider_event: &'static str,
    event_identifier: EventIdentifier,
    fields: Vec<TriggerField>,
}

impl TriggerEvent {
    /// Declare one event. The provider event name is the string the provider
    /// itself publishes — `issues`, `orders/create`, `invitee.created` — and is
    /// the name the connector's SDK trigger carries.
    pub fn declare(
        provider_event: &'static str,
        event_identifier: EventIdentifier,
        fields: impl IntoIterator<Item = (&'static str, &'static str, ValueScalar, Required)>,
    ) -> Result<Self, OperationError> {
        if provider_event.is_empty() {
            return Err(OperationError::new(
                "a trigger event must name the provider's own event",
            ));
        }
        if let EventIdentifier::BodyPointer(pointer) = event_identifier {
            validate_json_pointer(pointer)?;
        }
        let mut declared: Vec<TriggerField> = Vec::new();
        for (name, pointer, scalar, required) in fields {
            validate_json_pointer(pointer)?;
            if name.is_empty() || declared.iter().any(|field| field.name == name) {
                return Err(OperationError::new(
                    "a trigger field is named once and its name is not empty",
                ));
            }
            declared.push(TriggerField {
                name,
                pointer,
                scalar,
                required,
            });
        }
        if declared.is_empty() {
            return Err(OperationError::new(
                "a trigger event must expose at least one typed field",
            ));
        }
        Ok(Self {
            provider_event,
            event_identifier,
            fields: declared,
        })
    }

    pub const fn provider_event(&self) -> &'static str {
        self.provider_event
    }

    pub const fn event_identifier(&self) -> &EventIdentifier {
        &self.event_identifier
    }

    pub fn fields(&self) -> &[TriggerField] {
        &self.fields
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trigger_event_declaration_is_static_and_complete() {
        let event = TriggerEvent::declare(
            "issues",
            EventIdentifier::Header("X-GitHub-Delivery"),
            [
                ("action", "/action", ValueScalar::String, Required::Yes),
                (
                    "issue_number",
                    "/issue/number",
                    ValueScalar::Int64,
                    Required::Yes,
                ),
            ],
        )
        .expect("a complete declaration is valid");
        assert_eq!(event.provider_event(), "issues");
        assert_eq!(
            event.event_identifier(),
            &EventIdentifier::Header("X-GitHub-Delivery")
        );
        assert_eq!(
            event
                .fields()
                .iter()
                .map(|field| (field.name(), field.pointer(), field.required()))
                .collect::<Vec<_>>(),
            [
                ("action", "/action", Required::Yes),
                ("issue_number", "/issue/number", Required::Yes)
            ]
        );

        assert!(
            TriggerEvent::declare("", EventIdentifier::Unpublished, []).is_err(),
            "an unnamed event does not declare"
        );
        assert!(
            TriggerEvent::declare("push", EventIdentifier::Unpublished, []).is_err(),
            "an event exposing no field gives a signal mapper nothing"
        );
        assert!(
            TriggerEvent::declare(
                "push",
                EventIdentifier::BodyPointer("after"),
                [("after", "/after", ValueScalar::String, Required::Yes)],
            )
            .is_err(),
            "an event identifier pointer is a JSON pointer"
        );
        assert!(
            TriggerEvent::declare(
                "push",
                EventIdentifier::Unpublished,
                [
                    ("after", "/after", ValueScalar::String, Required::Yes),
                    ("after", "/before", ValueScalar::String, Required::Yes),
                ],
            )
            .is_err(),
            "one field name is declared once"
        );
    }
}
