//! The behavioural snapshot one declared operation projects.
//!
//! A catalog `OperationSpec` — the thing Process compilation binds against —
//! needs more of an operation than its id, version, method, and effect class:
//! it needs the request shape, the statuses that count as success, and the two
//! value contracts a Process binds its input to and reads its output from.
//!
//! Those facts already exist exactly once, in the declaration. This module is
//! the *derivation* of them, and it lives beside the declaration for the reason
//! `knowledgebase/declarative-saas/decisions/049-*` records: the alternative is
//! a second description of one provider written by whoever last read the
//! module, and two descriptions of one provider is the defect this whole design
//! exists to avoid.
//!
//! # What it exposes, and what it deliberately does not
//!
//! A projection is inert, owned data with no constructor of its own outside the
//! SDK: there is no way to turn one back into an [`Operation`], a
//! [`crate::sdk::RequestPlan`], or a URL. It carries no credential, no resolved
//! origin, no provider text, and no value — only the static declaration
//! material a catalog snapshot is made of. Reading it tells a caller what the
//! operation *is*; it does not let a caller aim, compose, or send anything.

use std::time::Duration;

use donat_value_contract::ValueScalar;

use crate::sdk::effect::{EffectClass, ExplicitKeyEvidence};

/// Where one declared request value comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueSource {
    /// A constant written into the declaration.
    Static(String),
    /// One declared, named input slot.
    Input(String),
}

/// One declared query key and where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryProjection {
    pub(in crate::sdk) key: String,
    pub(in crate::sdk) value: ValueSource,
}

impl QueryProjection {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub const fn value(&self) -> &ValueSource {
        &self.value
    }
}

/// One declared request header and where its value comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderProjection {
    pub(in crate::sdk) name: String,
    pub(in crate::sdk) value: ValueSource,
}

impl HeaderProjection {
    /// The canonical lowercase header name, which is the spelling a catalog
    /// snapshot holds.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn value(&self) -> &ValueSource {
        &self.value
    }
}

/// What the operation's request body is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestBodyProjection {
    /// No body at all.
    None,
    /// A static JSON template whose only dynamic leaves are the named input
    /// slots listed here, in declaration order.
    Json { inputs: Vec<String> },
    /// Bytes a named processor in this workspace assembles, in the media type
    /// the provider documents. The *shape* of those bytes is the processor's
    /// and is deliberately not described here.
    Processor { content_type: String },
}

/// One field of the operation's declared input contract: what a Process binds.
///
/// A slot the connector fills itself — deploy-time configuration, the durable
/// activity's own key, a value composed from other declared inputs — is not
/// here, because a Process must not bind one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputProjection {
    pub(in crate::sdk) name: String,
    pub(in crate::sdk) scalar: ValueScalar,
    pub(in crate::sdk) required: bool,
}

impl InputProjection {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn scalar(&self) -> &ValueScalar {
        &self.scalar
    }

    pub const fn required(&self) -> bool {
        self.required
    }
}

/// One field of the operation's declared output contract, which is the
/// activity's output schema (`knowledgebase/declarative-saas/decisions/029-*`).
///
/// `pointer` is present when the field is read from a JSON pointer into the
/// provider body, and absent when the module composes the field itself — from
/// response headers, or from a document the JSON decoder cannot read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputProjection {
    pub(in crate::sdk) name: String,
    pub(in crate::sdk) pointer: Option<String>,
    pub(in crate::sdk) scalar: ValueScalar,
    pub(in crate::sdk) required: bool,
}

impl OutputProjection {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn pointer(&self) -> Option<&str> {
        self.pointer.as_deref()
    }

    pub const fn scalar(&self) -> &ValueScalar {
        &self.scalar
    }

    pub const fn required(&self) -> bool {
        self.required
    }
}

/// The whole behavioural snapshot of one declared operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationProjection {
    pub(in crate::sdk) id: String,
    pub(in crate::sdk) version: String,
    pub(in crate::sdk) method: &'static str,
    pub(in crate::sdk) path_template: String,
    pub(in crate::sdk) query: Vec<QueryProjection>,
    pub(in crate::sdk) headers: Vec<HeaderProjection>,
    pub(in crate::sdk) body: RequestBodyProjection,
    pub(in crate::sdk) success_statuses: Vec<u16>,
    pub(in crate::sdk) inputs: Vec<InputProjection>,
    pub(in crate::sdk) outputs: Vec<OutputProjection>,
    /// `None` is not a class: it is an operation nobody classified, which is
    /// never executable and never published. It is kept distinct from
    /// `InventoryOnly`, which is a class with a recorded reason behind it.
    pub(in crate::sdk) effect_class: Option<EffectClass>,
    pub(in crate::sdk) explicit_key: Option<ExplicitKeyEvidence>,
    pub(in crate::sdk) deadline: Duration,
}

impl OperationProjection {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// The declared method, in its wire spelling.
    pub const fn method(&self) -> &'static str {
        self.method
    }

    pub fn path_template(&self) -> &str {
        &self.path_template
    }

    pub fn query(&self) -> &[QueryProjection] {
        &self.query
    }

    pub fn headers(&self) -> &[HeaderProjection] {
        &self.headers
    }

    pub const fn body(&self) -> &RequestBodyProjection {
        &self.body
    }

    pub fn success_statuses(&self) -> &[u16] {
        &self.success_statuses
    }

    /// The contract a Process binds this operation's input to.
    pub fn inputs(&self) -> &[InputProjection] {
        &self.inputs
    }

    /// The contract a Process reads this operation's output from.
    pub fn outputs(&self) -> &[OutputProjection] {
        &self.outputs
    }

    pub const fn effect_class(&self) -> Option<EffectClass> {
        self.effect_class
    }

    /// Whether a Process may reference the operation this snapshot describes.
    pub const fn is_executable(&self) -> bool {
        match self.effect_class {
            Some(class) => class.is_executable(),
            None => false,
        }
    }

    /// The evidence an `ExplicitKey` operation was admitted on: the binding a
    /// durable activity writes its stable key into, the uniqueness scope, the
    /// documented retention, and the clock margin held below it.
    pub const fn explicit_key(&self) -> Option<&ExplicitKeyEvidence> {
        self.explicit_key.as_ref()
    }

    /// The deadline one attempt of this operation declares.
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }
}

#[cfg(test)]
mod tests {
    use donat_value_contract::ValueScalar;
    use reqwest::StatusCode;

    use super::*;
    use crate::sdk::effect::Effect;
    use crate::sdk::operation::{JsonTemplate, Operation, Required};

    /// `sdk_projects_its_declaration`: everything a catalog snapshot is built
    /// from is derived from the declaration, and nothing else is.
    #[test]
    fn sdk_projects_its_declaration() {
        let operation = Operation::post("item.search", "/v1/tenants/{tenant}/items/search")
            .version("1.2.3")
            .path_param("tenant", ValueScalar::String)
            .query_static("api-version", "2026-01-01")
            .query_input("locale", "locale")
            .static_header("X-Provider", "donat")
            .body(JsonTemplate::object([
                ("selector", JsonTemplate::input("selector")),
                ("limit", JsonTemplate::input("limit")),
            ]))
            .success_statuses([StatusCode::OK])
            .output_pointer("items", "/items", ValueScalar::Json, Required::Yes)
            .output_pointer("cursor", "/cursor", ValueScalar::String, Required::No)
            .declared_input("limit", ValueScalar::Int64, Required::No)
            .supplied_input("tenant")
            .effect(
                Effect::read_only_documented("the provider documents this search as read-only")
                    .expect("a cited statement is an assertion"),
            )
            .build()
            .expect("the declaration is valid");

        let projection = operation.project();

        assert_eq!(projection.id(), "item.search");
        assert_eq!(projection.version(), "1.2.3");
        assert_eq!(projection.method(), "POST");
        assert_eq!(
            projection.path_template(),
            "/v1/tenants/{tenant}/items/search"
        );
        assert_eq!(projection.success_statuses(), [200]);
        assert_eq!(projection.effect_class(), Some(EffectClass::ReadOnly));
        assert!(projection.explicit_key().is_none());

        assert_eq!(
            projection.query(),
            [
                QueryProjection {
                    key: "api-version".to_owned(),
                    value: ValueSource::Static("2026-01-01".to_owned()),
                },
                QueryProjection {
                    key: "locale".to_owned(),
                    value: ValueSource::Input("locale".to_owned()),
                },
            ]
        );
        assert_eq!(
            projection.headers(),
            [HeaderProjection {
                name: "x-provider".to_owned(),
                value: ValueSource::Static("donat".to_owned()),
            }]
        );
        assert_eq!(
            projection.body(),
            &RequestBodyProjection::Json {
                inputs: vec!["selector".to_owned(), "limit".to_owned()],
            }
        );

        // The input contract: every declared slot, typed where the declaration
        // types it, minus the slot this connector fills itself.
        let inputs = projection
            .inputs()
            .iter()
            .map(|input| (input.name(), input.scalar().clone(), input.required()))
            .collect::<Vec<_>>();
        assert_eq!(
            inputs,
            [
                ("limit", ValueScalar::Int64, false),
                ("locale", ValueScalar::Json, true),
                ("selector", ValueScalar::Json, true),
            ],
            "a supplied slot is never part of the contract a Process binds"
        );

        let outputs = projection
            .outputs()
            .iter()
            .map(|output| (output.name(), output.pointer(), output.required()))
            .collect::<Vec<_>>();
        assert_eq!(
            outputs,
            [
                ("cursor", Some("/cursor"), false),
                ("items", Some("/items"), true),
            ]
        );
    }

    /// An operation whose module composes its own output — from response
    /// headers, or from a document the JSON decoder cannot read — declares that
    /// output as a contract, and the projection carries it without a pointer.
    #[test]
    fn a_composed_output_is_declared_and_projected_without_a_pointer() {
        let operation = Operation::head("object.head", "/{key}")
            .version("1.0.0")
            .path_param("key", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .declared_output("etag", ValueScalar::String, Required::Yes)
            .declared_output("content_type", ValueScalar::String, Required::No)
            .effect(Effect::read_only())
            .build()
            .expect("the declaration is valid");

        let projection = operation.project();
        assert_eq!(
            projection
                .outputs()
                .iter()
                .map(|output| (output.name(), output.pointer(), output.required()))
                .collect::<Vec<_>>(),
            [("content_type", None, false), ("etag", None, true),]
        );
        assert_eq!(projection.body(), &RequestBodyProjection::None);
    }

    /// The explicit-key evidence travels with the projection, because a catalog
    /// snapshot's provider-idempotent effect *is* that evidence.
    #[test]
    fn an_explicit_key_projects_the_evidence_it_was_admitted_on() {
        use std::time::Duration;

        use crate::sdk::effect::{ExplicitKeyEvidence, IdempotencyBinding};

        let operation = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .body(JsonTemplate::object([(
                "Key",
                JsonTemplate::input("dedup"),
            )]))
            .success_statuses([StatusCode::OK])
            .supplied_input("dedup")
            .effect(Effect::provider_idempotent_explicit_key(
                ExplicitKeyEvidence::documented(
                    IdempotencyBinding::body_pointer("/Key").expect("a static pointer is valid"),
                    "account",
                    Duration::from_secs(300),
                    Duration::from_secs(30),
                    "the provider documents a five minute deduplication interval",
                )
                .expect("complete evidence is admitted"),
            ))
            .build()
            .expect("the declaration is valid");

        let projection = operation.project();
        assert_eq!(
            projection.effect_class(),
            Some(EffectClass::ProviderIdempotentExplicitKey)
        );
        let evidence = projection
            .explicit_key()
            .expect("an explicit-key operation projects its evidence");
        assert_eq!(evidence.retention().scope(), "account");
        assert_eq!(evidence.retention().minimum(), Duration::from_secs(300));
        assert_eq!(
            evidence.retention().clock_safety_margin(),
            Duration::from_secs(30)
        );
        assert!(
            projection.inputs().is_empty(),
            "the key slot is the connector's, never a caller's"
        );
    }
}
