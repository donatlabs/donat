//! What a connector *is* (spec 010 §4): a static declaration of one provider's
//! name, contract version, origin, credential, operations, and triggers.
//!
//! A [`Connector`] is built once, from constants, and then only read. It is
//! held in a `static` and handed out as `&'static Connector`, which is what
//! makes the registry table a table rather than a factory: nothing about a
//! connector is decided at request time.
//!
//! Two things this module is careful about.
//!
//! The first is the origin. Spec 010 §4 spells the templated case
//! `Origin::TemplatedHost`; here it is [`OriginSpec::TemplatedHost`], because
//! [`Origin`] is the *resolved* scheme+host+port a request renders against and
//! a template is not one yet. Keeping them apart makes the resolution a
//! deploy-time step with a type of its own rather than a check every render
//! would have to repeat: by the time an [`Origin`] exists, its host was already
//! filled from [`ConnectorConfiguration`], and there is no API anywhere that
//! rebuilds one from input, a provider response, or a continuation.
//!
//! The second is the effect gate. [`ConnectorBuilder::build`] refuses an
//! operation that carries no effect class at all, so "every operation carries a
//! class" holds for everything a deployment can reach, and
//! [`Connector::admit_operation`] is the single question metadata validation
//! asks before enabling one.
//!
//! ```compile_fail
//! use donat_connectors::sdk::connector::ConnectorConfiguration;
//! // Configuration is deploy-time material: there is no constructor that
//! // takes a provider response, an operation input, or a continuation.
//! let _ = ConnectorConfiguration::from_provider_response(serde_json::json!({}));
//! ```
//!
//! ```compile_fail
//! use donat_connectors::sdk::operation::Origin;
//! // A resolved origin is read-only: nothing can move a request off it.
//! let mut origin = Origin::parse("https://provider.example.test").unwrap();
//! origin.set_host("attacker.invalid");
//! ```
//!
//! The control for those two: every path they name resolves, so each fails for
//! the reason claimed rather than because an import was wrong.
//!
//! ```
//! use donat_connectors::sdk::connector::ConnectorConfiguration;
//! use donat_connectors::sdk::operation::Origin;
//! let _ = ConnectorConfiguration::from_deployment([("tenant", "acme")]);
//! let _ = Origin::parse("https://provider.example.test").expect("a static origin is valid");
//! ```

use std::collections::BTreeMap;
use std::fmt;

use crate::sdk::auth::{AuthPlan, Credential};
use crate::sdk::effect::EffectClass;
use crate::sdk::operation::{
    MAX_HEADER_VALUE_BYTES, Operation, OperationError, Origin, validate_semver_core,
};
use crate::sdk::transport::MAX_HTTP_BODY_BYTES;
use crate::sdk::webhook::{WebhookRejection, WebhookVerifier};

/// The non-secret, deploy-time values a connector instance was configured
/// with.
///
/// This is the *only* source a [`OriginSpec::TemplatedHost`] variable can be
/// filled from. It is built where deployment metadata is read and is immutable
/// afterwards; there is no constructor here that accepts operation input, a
/// provider response body, or a pagination continuation, which is what the
/// templated-host test proves by exhausting those three paths.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectorConfiguration {
    values: BTreeMap<String, String>,
}

impl ConnectorConfiguration {
    pub fn from_deployment<'a>(values: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
}

/// A host with exactly one variable, filled from deploy-time configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatedHost {
    scheme: String,
    prefix: String,
    variable: String,
    suffix: String,
    port: Option<u16>,
}

impl TemplatedHost {
    /// The configuration key that fills this host.
    pub fn variable(&self) -> &str {
        &self.variable
    }
}

/// A connector's declared origin, before and after its host is resolved.
///
/// Spec 010 §4's `Origin::TemplatedHost` is this enum's second variant. All
/// three variants resolve from the same place — deploy-time configuration —
/// and all three resolve exactly once, before any request is rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OriginSpec {
    /// A compile-time constant scheme, host, and port.
    Fixed(Origin),
    /// A provider whose account-specific host varies — a per-tenant subdomain,
    /// a region — whose single variable is filled only from deploy-time
    /// connector configuration.
    TemplatedHost(TemplatedHost),
    /// The whole origin comes from one deploy-time configuration key.
    ///
    /// This exists for the deploy-time declarative connector, where the
    /// deployment names the provider rather than this workspace. It is not an
    /// escape from fixed origins: the value is read once from configuration and
    /// becomes the same immutable [`Origin`] every other connector renders
    /// against.
    DeploymentOrigin { key: &'static str },
}

impl OriginSpec {
    pub fn fixed(origin: &str) -> Result<Self, OperationError> {
        Origin::parse(origin).map(Self::Fixed)
    }

    /// The deploy-time declarative connector's origin, read from one
    /// configuration key.
    pub fn deployment_origin(key: &'static str) -> Result<Self, OperationError> {
        if key.is_empty() {
            return Err(OperationError::new(
                "a deployment origin must name its configuration key",
            ));
        }
        Ok(Self::DeploymentOrigin { key })
    }

    /// A templated host, spelled `{variable}` inside an otherwise constant
    /// host: `("https", "{tenant}.provider.example.test", None)`.
    pub fn templated_host(
        scheme: &str,
        host_template: &str,
        port: Option<u16>,
    ) -> Result<Self, OperationError> {
        if !matches!(scheme, "http" | "https") {
            return Err(OperationError::new("an origin scheme is http or https"));
        }
        let Some((prefix, rest)) = host_template.split_once('{') else {
            return Err(OperationError::new(
                "a templated host declares exactly one {variable}",
            ));
        };
        let Some((variable, suffix)) = rest.split_once('}') else {
            return Err(OperationError::new(
                "a templated host declares exactly one {variable}",
            ));
        };
        if variable.is_empty()
            || !variable.chars().all(|character| {
                character == '_' || character.is_ascii_lowercase() || character.is_ascii_digit()
            })
        {
            return Err(OperationError::new(
                "a templated host variable name must be static and lowercase",
            ));
        }
        if suffix.contains(['{', '}']) || prefix.contains(['{', '}']) {
            return Err(OperationError::new(
                "a templated host declares exactly one {variable}",
            ));
        }
        if !is_host_fragment(prefix) || !is_host_fragment(suffix) || suffix.is_empty() {
            return Err(OperationError::new(
                "a templated host is otherwise a constant host name",
            ));
        }
        Ok(Self::TemplatedHost(TemplatedHost {
            scheme: scheme.to_owned(),
            prefix: prefix.to_owned(),
            variable: variable.to_owned(),
            suffix: suffix.to_owned(),
            port,
        }))
    }

    /// The configuration key this origin needs, if any.
    pub fn host_variable(&self) -> Option<&str> {
        match self {
            Self::Fixed(_) => None,
            Self::TemplatedHost(template) => Some(template.variable()),
            Self::DeploymentOrigin { key } => Some(key),
        }
    }

    /// Resolve to the one origin every request of this connector instance is
    /// rendered against.
    ///
    /// A fixed origin ignores the configuration entirely. A templated one
    /// accepts a single lowercase host label and nothing else: a value with a
    /// dot, a slash, a colon, or an at sign would be a different authority, so
    /// it is refused rather than escaped.
    pub fn resolve(
        &self,
        configuration: &ConnectorConfiguration,
    ) -> Result<Origin, OperationError> {
        match self {
            Self::Fixed(origin) => Ok(origin.clone()),
            Self::TemplatedHost(template) => {
                let value = configuration.get(&template.variable).ok_or_else(|| {
                    OperationError::new(
                        "a templated host variable must be configured at deploy time",
                    )
                })?;
                if !is_host_label(value) {
                    return Err(OperationError::new(
                        "a templated host value must be one lowercase host label",
                    ));
                }
                let port = template
                    .port
                    .map(|port| format!(":{port}"))
                    .unwrap_or_default();
                Origin::parse(&format!(
                    "{}://{}{value}{}{port}",
                    template.scheme, template.prefix, template.suffix
                ))
            }
            Self::DeploymentOrigin { key } => {
                let value = configuration.get(key).ok_or_else(|| {
                    OperationError::new("a deployment origin must be configured at deploy time")
                })?;
                Origin::parse(value)
            }
        }
    }
}

/// The constant part of a templated host: host characters only, and never an
/// authority, a path, or a scheme.
fn is_host_fragment(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || matches!(character, '-' | '.')
    })
}

/// One DNS label: what a per-tenant subdomain or a region name is.
fn is_host_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        && !value.starts_with('-')
        && !value.ends_with('-')
}

/// Whether a declared credential field is a secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldClassification {
    /// Redacted everywhere, applied only by an auth plan.
    Secret,
    /// A non-secret identity that still comes from deploy-time configuration,
    /// such as an account identifier a provider prints on an invoice.
    NonSecret,
}

/// One declared credential field.  It carries the field's name and
/// classification and never its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialField {
    name: String,
    classification: FieldClassification,
}

impl CredentialField {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn classification(&self) -> FieldClassification {
        self.classification
    }

    pub const fn is_secret(&self) -> bool {
        matches!(self.classification, FieldClassification::Secret)
    }
}

/// How a connector's credential reaches the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialApplication {
    /// One of the SDK's closed auth plans.
    Plan(AuthPlan),
    /// The deploy-time declarative connector: the deployment names the header
    /// and the environment variable behind it, so there is no compiled plan
    /// here to name. The header is still resolved once, at startup, and is
    /// still unreachable from operation input.
    DeploymentDeclaredHeaders,
}

/// A connector's credential contract: how the credential is applied, and the
/// typed fields a deployment binds to `SecretRef`s.
///
/// The specification carries no value of any kind — its `Debug` is a list of
/// names and classifications — so a specification can be logged, hashed into a
/// configuration fingerprint, or printed in a startup error safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialSpec {
    application: CredentialApplication,
    fields: Vec<CredentialField>,
}

impl CredentialSpec {
    /// The fields the plan itself applies, each classified secret.
    pub fn for_plan(plan: AuthPlan) -> Self {
        let fields = plan
            .required_fields()
            .iter()
            .map(|name| CredentialField {
                name: (*name).to_owned(),
                classification: FieldClassification::Secret,
            })
            .collect();
        Self {
            application: CredentialApplication::Plan(plan),
            fields,
        }
    }

    /// The deploy-time declarative connector's credential: whatever headers the
    /// deployment declared, resolved from the environment variables it named.
    pub fn deployment_declared_headers() -> Self {
        Self {
            application: CredentialApplication::DeploymentDeclaredHeaders,
            fields: Vec::new(),
        }
    }

    /// Declare a further field this connector reads, such as a non-secret
    /// account identifier a request path needs.
    #[must_use]
    pub fn with_field(mut self, name: &str, classification: FieldClassification) -> Self {
        if !self.fields.iter().any(|field| field.name == name) {
            self.fields.push(CredentialField {
                name: name.to_owned(),
                classification,
            });
        }
        self
    }

    pub const fn application(&self) -> &CredentialApplication {
        &self.application
    }

    /// The compiled auth plan, when this connector has one.
    pub const fn plan(&self) -> Option<&AuthPlan> {
        match &self.application {
            CredentialApplication::Plan(plan) => Some(plan),
            CredentialApplication::DeploymentDeclaredHeaders => None,
        }
    }

    pub fn fields(&self) -> &[CredentialField] {
        &self.fields
    }

    /// Whether a resolved credential carries every declared field.  Startup
    /// asks this before a listener opens; the answer names the missing field
    /// and never a value.
    pub fn admits(&self, credential: &Credential) -> Result<(), MissingCredentialField> {
        for field in &self.fields {
            if !credential.declares(&field.name) {
                return Err(MissingCredentialField {
                    name: field.name.clone(),
                });
            }
        }
        Ok(())
    }
}

/// A declared credential field a deployment did not configure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingCredentialField {
    name: String,
}

impl MissingCredentialField {
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for MissingCredentialField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "connector credential field `{}` is not configured",
            self.name
        )
    }
}

impl std::error::Error for MissingCredentialField {}

/// One inbound trigger: a provider webhook and the verification applied to its
/// exact raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    name: &'static str,
    version: &'static str,
    verification: WebhookVerifier,
    raw_body_max_bytes: usize,
}

impl Trigger {
    pub fn webhook(
        name: &'static str,
        version: &'static str,
        verification: WebhookVerifier,
    ) -> Result<Self, OperationError> {
        if name.is_empty() {
            return Err(OperationError::new("a connector trigger name is required"));
        }
        validate_semver_core(version)?;
        Ok(Self {
            name,
            version,
            verification,
            raw_body_max_bytes: MAX_HTTP_BODY_BYTES,
        })
    }

    /// Lower this trigger's raw-body ceiling.  It can only ever be lowered:
    /// the shared transport ceiling is the maximum a body may be read to.
    pub fn with_raw_body_max_bytes(mut self, bytes: usize) -> Result<Self, OperationError> {
        if bytes == 0 || bytes > MAX_HTTP_BODY_BYTES {
            return Err(OperationError::new(
                "a trigger raw body ceiling is positive and at most the shared body ceiling",
            ));
        }
        self.raw_body_max_bytes = bytes;
        Ok(self)
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn version(&self) -> &'static str {
        self.version
    }

    pub const fn verification(&self) -> &WebhookVerifier {
        &self.verification
    }

    pub const fn raw_body_max_bytes(&self) -> usize {
        self.raw_body_max_bytes
    }

    /// Verify one delivery.  The ceiling is applied first, so an oversized body
    /// is refused before a MAC is computed over it.
    pub fn verify(
        &self,
        headers: &reqwest::header::HeaderMap,
        raw_body: &[u8],
        secret: &crate::sdk::auth::Secret,
        now_unix_seconds: i64,
    ) -> Result<(), WebhookRejection> {
        if raw_body.len() > self.raw_body_max_bytes {
            return Err(WebhookRejection::PayloadTooLarge);
        }
        self.verification
            .verify(headers, raw_body, secret, now_unix_seconds)
    }
}

/// Why an operation a deployment tried to enable was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationRejection {
    /// The connector does not declare an operation by that name.
    Undeclared,
    /// The connector declares it, and its effect class is not executable.
    InventoryOnly,
}

impl OperationRejection {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Undeclared => "connector operation is not compiled into this binary",
            Self::InventoryOnly => {
                "connector operation is inventory-only and cannot be enabled by a deployment"
            }
        }
    }
}

impl fmt::Display for OperationRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

/// One connector: the whole static declaration (spec 010 §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    name: &'static str,
    version: &'static str,
    origin: OriginSpec,
    credential: CredentialSpec,
    operations: Vec<Operation>,
    triggers: Vec<Trigger>,
}

impl Connector {
    pub fn declare(name: &'static str, version: &'static str) -> ConnectorBuilder {
        ConnectorBuilder {
            name,
            version,
            origin: None,
            credential: None,
            operations: Vec::new(),
            triggers: Vec::new(),
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The SemVer core of this connector's *contract*, which changes when the
    /// declaration does rather than when a deployment does.
    pub const fn version(&self) -> &'static str {
        self.version
    }

    pub const fn origin(&self) -> &OriginSpec {
        &self.origin
    }

    pub const fn credential(&self) -> &CredentialSpec {
        &self.credential
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    pub fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }

    pub fn operation(&self, id: &str) -> Option<&Operation> {
        self.operations
            .iter()
            .find(|operation| operation.id() == id)
    }

    /// An operation safe to send once, purely to see whether the provider
    /// accepts this deployment's credential.
    ///
    /// Derived rather than declared, from the two properties that make an
    /// operation safe to spend on a question: it is `ReadOnly`, so sending it
    /// changes nothing at the provider; and it needs no input, so the probe
    /// invents no identifier that could turn a credential answer into a
    /// "no such record" answer. The first such operation in declaration order
    /// is taken, which is stable because the declaration is.
    ///
    /// `None` is an ordinary answer: a connector whose every read needs an id
    /// has nothing that can be sent blind, and the caller reports that it could
    /// not probe rather than inventing an argument.
    pub fn auth_probe(&self) -> Option<&Operation> {
        self.operations.iter().find(|operation| {
            operation.is_executable()
                && operation.effect_class() == Some(EffectClass::ReadOnly)
                && operation
                    .project()
                    .inputs()
                    .iter()
                    .all(|input| !input.required())
        })
    }

    pub fn trigger(&self, name: &str) -> Option<&Trigger> {
        self.triggers.iter().find(|trigger| trigger.name() == name)
    }

    /// The gate metadata validation asks before enabling an operation.
    ///
    /// Every refusal a deployment can earn is one of these two, and neither is
    /// a runtime decision: an unknown name and an inventory-only class are both
    /// answered from the declaration, before a listener opens.
    pub fn admit_operation(&self, name: &str) -> Result<&Operation, OperationRejection> {
        let operation = self.operation(name).ok_or(OperationRejection::Undeclared)?;
        if !operation.is_executable() {
            return Err(OperationRejection::InventoryOnly);
        }
        Ok(operation)
    }

    /// Resolve this connector's one origin for a deployment.
    pub fn resolve_origin(
        &self,
        configuration: &ConnectorConfiguration,
    ) -> Result<Origin, OperationError> {
        self.origin.resolve(configuration)
    }
}

pub struct ConnectorBuilder {
    name: &'static str,
    version: &'static str,
    origin: Option<OriginSpec>,
    credential: Option<CredentialSpec>,
    operations: Vec<Operation>,
    triggers: Vec<Trigger>,
}

impl ConnectorBuilder {
    #[must_use]
    pub fn origin(mut self, origin: OriginSpec) -> Self {
        self.origin = Some(origin);
        self
    }

    #[must_use]
    pub fn credential(mut self, credential: CredentialSpec) -> Self {
        self.credential = Some(credential);
        self
    }

    #[must_use]
    pub fn operation(mut self, operation: Operation) -> Self {
        self.operations.push(operation);
        self
    }

    #[must_use]
    pub fn operations(mut self, operations: impl IntoIterator<Item = Operation>) -> Self {
        self.operations.extend(operations);
        self
    }

    #[must_use]
    pub fn trigger(mut self, trigger: Trigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    pub fn build(self) -> Result<Connector, OperationError> {
        validate_connector_name(self.name)?;
        validate_semver_core(self.version)?;
        let origin = self
            .origin
            .ok_or_else(|| OperationError::new("a connector must declare its origin"))?;
        let credential = self
            .credential
            .ok_or_else(|| OperationError::new("a connector must declare its credential"))?;

        let mut ids = std::collections::BTreeSet::new();
        for operation in &self.operations {
            if !ids.insert(operation.id().to_owned()) {
                return Err(OperationError::new(
                    "a connector operation is declared more than once",
                ));
            }
            // The effect gate, at the one place a connector is assembled: an
            // operation nobody classified is not a class, and a connector may
            // not publish one.
            if operation.effect().is_none() {
                return Err(OperationError::new(
                    "every connector operation must declare an effect class",
                ));
            }
        }
        let mut trigger_names = std::collections::BTreeSet::new();
        for trigger in &self.triggers {
            if !trigger_names.insert(trigger.name()) {
                return Err(OperationError::new(
                    "a connector trigger is declared more than once",
                ));
            }
        }
        Ok(Connector {
            name: self.name,
            version: self.version,
            origin,
            credential,
            operations: self.operations,
            triggers: self.triggers,
        })
    }
}

/// The connector name grammar, matching `donat_connector_abi`'s identity
/// grammar so a declaration and a catalog identity cannot disagree.
fn validate_connector_name(name: &str) -> Result<(), OperationError> {
    let valid =
        !name.is_empty()
            && name.len() <= 96
            && name.chars().all(|character| {
                character.is_ascii_lowercase()
                    || character.is_ascii_digit()
                    || matches!(character, '.' | '_' | '-')
            })
            && name.chars().next().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            })
            && name.chars().last().is_some_and(|character| {
                character.is_ascii_lowercase() || character.is_ascii_digit()
            });
    if !valid {
        return Err(OperationError::new(
            "a connector name is lowercase ASCII, 1 to 96 characters, and starts and ends alphanumerically",
        ));
    }
    // A header value ceiling is unrelated to a name, but a name reaches a
    // fingerprint and a startup message, so keep it bounded by something.
    debug_assert!(name.len() < MAX_HEADER_VALUE_BYTES);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use donat_value_contract::ValueScalar;
    use reqwest::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::sdk::auth::Secret;
    use crate::sdk::effect::{Effect, EffectClass, ExplicitKeyEvidence, IdempotencyBinding};
    use crate::sdk::errors::ConnectorErrorClass;
    use crate::sdk::operation::Required;
    use crate::sdk::pagination::{Pagination, PaginationBudget, undeclared_status_gate};
    use crate::sdk::transport::RawHttpResponse;
    use crate::sdk::webhook::SignatureEncoding;

    fn read() -> Operation {
        Operation::get("item.get", "/v1/items/{id}")
            .version("1.0.0")
            .path_param("id", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .effect(Effect::read_only())
            .build()
            .expect("a static read declaration is valid")
    }

    fn inventory() -> Operation {
        Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::CREATED])
            .effect(
                Effect::inventory_only("the provider publishes no idempotency key for this create")
                    .expect("a recorded reason is required"),
            )
            .build()
            .expect("an inventory-only declaration is valid")
    }

    fn credential() -> CredentialSpec {
        CredentialSpec::for_plan(AuthPlan::bearer())
    }

    fn connector() -> Connector {
        Connector::declare("provider", "1.0.0")
            .origin(OriginSpec::fixed("https://provider.example.test").expect("a static origin"))
            .credential(credential())
            .operation(read())
            .operation(inventory())
            .build()
            .expect("a complete declaration is valid")
    }

    #[test]
    fn a_declaration_carries_its_name_version_origin_credential_operations_and_triggers() {
        let trigger = Trigger::webhook(
            "item.changed",
            "1.0.0",
            WebhookVerifier::hmac_body("X-Signature", SignatureEncoding::Hex)
                .expect("a static header name is valid"),
        )
        .expect("a static trigger declaration is valid");
        let connector = Connector::declare("provider", "1.2.3")
            .origin(OriginSpec::fixed("https://provider.example.test").expect("a static origin"))
            .credential(credential())
            .operation(read())
            .trigger(trigger)
            .build()
            .expect("a complete declaration is valid");

        assert_eq!(connector.name(), "provider");
        assert_eq!(connector.version(), "1.2.3");
        assert_eq!(connector.operations().len(), 1);
        assert_eq!(
            connector.operation("item.get").map(Operation::id),
            Some("item.get")
        );
        assert!(connector.operation("item.missing").is_none());
        assert_eq!(
            connector.trigger("item.changed").map(Trigger::version),
            Some("1.0.0")
        );
        assert_eq!(
            connector.credential().fields().len(),
            1,
            "a bearer plan declares exactly its one secret field"
        );
        assert!(connector.credential().fields()[0].is_secret());
    }

    /// The declaration is where "every operation carries a class" is enforced:
    /// an unclassified operation cannot be published by a connector at all.
    #[test]
    fn a_connector_refuses_an_operation_that_declares_no_effect_class() {
        let unclassified = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::CREATED])
            .build()
            .expect("an unclassified operation still renders");

        assert!(
            Connector::declare("provider", "1.0.0")
                .origin(
                    OriginSpec::fixed("https://provider.example.test").expect("a static origin")
                )
                .credential(credential())
                .operation(unclassified)
                .build()
                .is_err(),
            "a connector may not publish an operation nobody classified"
        );
    }

    /// The gate a deployment meets: an unknown name and an inventory-only class
    /// are both refused from the declaration.
    #[test]
    fn only_a_declared_executable_operation_is_admitted() {
        let connector = connector();
        assert_eq!(
            connector.admit_operation("item.get").map(Operation::id),
            Ok("item.get")
        );
        assert_eq!(
            connector.admit_operation("item.refund"),
            Err(OperationRejection::Undeclared)
        );
        assert_eq!(
            connector.admit_operation("item.create"),
            Err(OperationRejection::InventoryOnly)
        );
        assert_eq!(
            connector
                .operation("item.create")
                .and_then(Operation::effect_class),
            Some(EffectClass::InventoryOnly),
            "an inventory-only operation stays declared, typed, and testable"
        );
    }

    #[test]
    fn a_connector_declaration_is_static_and_complete() {
        for name in ["", "Provider", "provider/one", "-provider", "provider-"] {
            assert!(
                Connector::declare(Box::leak(name.to_owned().into_boxed_str()), "1.0.0")
                    .origin(
                        OriginSpec::fixed("https://provider.example.test")
                            .expect("a static origin")
                    )
                    .credential(credential())
                    .build()
                    .is_err(),
                "connector name {name} must not build"
            );
        }
        for version in ["", "v1", "1.0", "1.0.0-rc.1"] {
            assert!(
                Connector::declare("provider", Box::leak(version.to_owned().into_boxed_str()))
                    .origin(
                        OriginSpec::fixed("https://provider.example.test")
                            .expect("a static origin")
                    )
                    .credential(credential())
                    .build()
                    .is_err(),
                "connector version {version} must not build"
            );
        }
        assert!(
            Connector::declare("provider", "1.0.0")
                .credential(credential())
                .build()
                .is_err(),
            "a connector without an origin does not build"
        );
        assert!(
            Connector::declare("provider", "1.0.0")
                .origin(
                    OriginSpec::fixed("https://provider.example.test").expect("a static origin")
                )
                .build()
                .is_err(),
            "a connector without a credential does not build"
        );
        assert!(
            Connector::declare("provider", "1.0.0")
                .origin(
                    OriginSpec::fixed("https://provider.example.test").expect("a static origin")
                )
                .credential(credential())
                .operation(read())
                .operation(read())
                .build()
                .is_err(),
            "one operation id is declared once"
        );
    }

    #[test]
    fn a_credential_specification_carries_names_and_classifications_and_never_a_value() {
        const SENTINEL: &str = "donat-secret-sentinel-do-not-log";
        let specification = CredentialSpec::for_plan(AuthPlan::bearer())
            .with_field("account_id", FieldClassification::NonSecret);
        assert_eq!(
            specification
                .fields()
                .iter()
                .map(|field| (field.name(), field.is_secret()))
                .collect::<Vec<_>>(),
            [("secret", true), ("account_id", false)]
        );
        assert!(!format!("{specification:?}").contains(SENTINEL));

        let complete = Credential::from_fields([
            ("secret", Secret::new(SENTINEL)),
            ("account_id", Secret::new("acct_1")),
        ]);
        assert_eq!(specification.admits(&complete), Ok(()));

        let incomplete = Credential::secret(SENTINEL);
        let missing = specification
            .admits(&incomplete)
            .expect_err("a declared field a deployment did not configure is refused");
        assert_eq!(missing.name(), "account_id");
        assert!(!missing.to_string().contains(SENTINEL));
    }

    /// `Origin::TemplatedHost`, spec 010 §4: the host comes from deploy-time
    /// configuration and from nowhere else.  Input, a provider response, and a
    /// pagination continuation each get a turn, and none of them moves it.
    #[tokio::test]
    async fn a_templated_host_is_filled_only_from_deploy_time_configuration() {
        let specification =
            OriginSpec::templated_host("https", "{tenant}.provider.example.test", None)
                .expect("a static templated host is valid");
        assert_eq!(specification.host_variable(), Some("tenant"));

        let origin = specification
            .resolve(&ConnectorConfiguration::from_deployment([(
                "tenant", "acme",
            )]))
            .expect("a configured tenant resolves");
        assert_eq!(
            origin.as_url().as_str(),
            "https://acme.provider.example.test/"
        );

        // 1. Operation input.  A path value that spells a whole other origin
        //    stays one percent-encoded path segment.
        let operation = Operation::get("item.get", "/v1/items/{id}")
            .version("1.0.0")
            .path_param("id", ValueScalar::String)
            .query_input("q", "q")
            .success_statuses([StatusCode::OK])
            .effect(Effect::read_only())
            .build()
            .expect("a static declaration is valid");
        let request = operation
            .plan_request(
                &origin,
                &json!({ "id": "https://attacker.invalid/x", "q": "@attacker.invalid" }),
            )
            .expect("a hostile input renders");
        assert_eq!(
            request.url().host_str(),
            Some("acme.provider.example.test"),
            "input cannot reach the host"
        );
        assert_eq!(request.url().scheme(), "https");
        assert_eq!(
            request.url().path(),
            "/v1/items/https%3A%2F%2Fattacker%2Einvalid%2Fx"
        );

        // 2. A provider response.  A body naming another host is data: the
        //    declared output contract is the only thing read out of it, and the
        //    origin the next request renders against is the same object.
        let output = operation
            .extract_output(&json!({ "id": "https://attacker.invalid" }))
            .expect("the declared contract is satisfied");
        assert_eq!(output, json!({ "id": "https://attacker.invalid" }));
        assert_eq!(
            operation
                .plan_request(&origin, &json!({ "id": "1", "q": "x" }))
                .expect("the next request renders")
                .url()
                .host_str(),
            Some("acme.provider.example.test")
        );

        // 3. A pagination continuation.  A `Link` to another origin is refused
        //    rather than followed, on a templated origin exactly as on a fixed
        //    one.
        let pagination =
            Pagination::link_header("/data", "next").expect("a static link plan is valid");
        let budget = PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5));
        let failure = pagination
            .collect(
                request,
                &origin,
                &budget,
                undeclared_status_gate,
                |_| async {
                    Ok(
                        RawHttpResponse::json(StatusCode::OK, json!({ "data": [] })).with_header(
                            "link",
                            "<https://attacker.invalid/v1/items?page=2>; rel=\"next\"",
                        ),
                    )
                },
            )
            .await
            .expect_err("a cross-origin continuation is not followed");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        assert_eq!(failure.code(), "connector_pagination_cross_origin");

        // And the configuration itself admits one host label and nothing else.
        for hostile in [
            "acme.attacker.invalid",
            "acme/../evil",
            "acme:8080",
            "user@acme",
            "",
            "-acme",
            "ACME",
        ] {
            assert!(
                specification
                    .resolve(&ConnectorConfiguration::from_deployment([(
                        "tenant", hostile
                    )]))
                    .is_err(),
                "configuration value {hostile} must not resolve"
            );
        }
        assert!(
            specification
                .resolve(&ConnectorConfiguration::from_deployment([(
                    "other", "acme"
                )]))
                .is_err(),
            "an unconfigured host variable is a startup failure"
        );
    }

    #[test]
    fn a_templated_host_declaration_is_static() {
        assert!(OriginSpec::templated_host("ftp", "{tenant}.example.test", None).is_err());
        assert!(OriginSpec::templated_host("https", "provider.example.test", None).is_err());
        assert!(
            OriginSpec::templated_host("https", "{a}.{b}.example.test", None).is_err(),
            "exactly one variable"
        );
        assert!(OriginSpec::templated_host("https", "{Tenant}.example.test", None).is_err());
        assert!(
            OriginSpec::templated_host("https", "{tenant}", None).is_err(),
            "a bare variable is a whole host chosen by configuration"
        );
        assert!(OriginSpec::templated_host("https", "{tenant}.example.test/path", None).is_err());
        assert!(OriginSpec::templated_host("https", "{tenant}@example.test", None).is_err());

        let ported = OriginSpec::templated_host("https", "{tenant}.example.test", Some(8443))
            .expect("a port is part of the declared origin");
        assert_eq!(
            ported
                .resolve(&ConnectorConfiguration::from_deployment([(
                    "tenant", "acme"
                )]))
                .expect("a configured tenant resolves")
                .as_url()
                .as_str(),
            "https://acme.example.test:8443/"
        );
    }

    /// The deploy-time declarative connector: the deployment names the whole
    /// origin, and it still becomes one immutable resolved origin.
    #[test]
    fn a_deployment_origin_resolves_once_from_its_configuration_key() {
        let specification =
            OriginSpec::deployment_origin("base_url").expect("a static key is valid");
        assert_eq!(specification.host_variable(), Some("base_url"));
        assert_eq!(
            specification
                .resolve(&ConnectorConfiguration::from_deployment([(
                    "base_url",
                    "https://logistics.example.test"
                )]))
                .expect("a configured base URL resolves")
                .as_url()
                .as_str(),
            "https://logistics.example.test/"
        );
        for hostile in [
            "https://user:pw@logistics.example.test",
            "https://logistics.example.test/v1",
            "ftp://logistics.example.test",
            "not a url",
        ] {
            assert!(
                specification
                    .resolve(&ConnectorConfiguration::from_deployment([(
                        "base_url", hostile
                    )]))
                    .is_err(),
                "a deployment origin obeys the same origin rules: {hostile}"
            );
        }
        assert!(
            specification
                .resolve(&ConnectorConfiguration::default())
                .is_err()
        );
        assert!(OriginSpec::deployment_origin("").is_err());
    }

    #[test]
    fn a_fixed_origin_ignores_configuration_entirely() {
        let specification =
            OriginSpec::fixed("https://api.provider.example.test").expect("a static origin");
        assert_eq!(specification.host_variable(), None);
        assert_eq!(
            specification
                .resolve(&ConnectorConfiguration::from_deployment([(
                    "tenant", "attacker"
                )]))
                .expect("a fixed origin resolves to itself")
                .as_url()
                .host_str(),
            Some("api.provider.example.test")
        );
    }

    #[test]
    fn a_trigger_bounds_its_raw_body_before_it_verifies_one() {
        let trigger = Trigger::webhook(
            "item.changed",
            "1.0.0",
            WebhookVerifier::hmac_body("X-Signature", SignatureEncoding::Hex)
                .expect("a static header name is valid"),
        )
        .expect("a static trigger declaration is valid")
        .with_raw_body_max_bytes(64)
        .expect("a lowered ceiling is valid");

        assert_eq!(trigger.raw_body_max_bytes(), 64);
        assert_eq!(
            trigger
                .verify(
                    &reqwest::header::HeaderMap::new(),
                    &[b'x'; 65],
                    &Secret::new("whsec"),
                    0,
                )
                .expect_err("an oversized body is refused before a MAC is computed"),
            WebhookRejection::PayloadTooLarge
        );
        assert_eq!(
            trigger
                .verify(
                    &reqwest::header::HeaderMap::new(),
                    &[b'x'; 64],
                    &Secret::new("whsec"),
                    0,
                )
                .expect_err("the exact ceiling is admitted and then fails on its signature"),
            WebhookRejection::MissingSignature
        );

        assert!(
            Trigger::webhook(
                "item.changed",
                "1.0.0",
                WebhookVerifier::hmac_body("X-Signature", SignatureEncoding::Hex)
                    .expect("a static header name is valid"),
            )
            .expect("a static trigger declaration is valid")
            .with_raw_body_max_bytes(MAX_HTTP_BODY_BYTES + 1)
            .is_err(),
            "a trigger ceiling may be lowered, never raised"
        );
        assert!(
            Trigger::webhook(
                "item.changed",
                "v1",
                WebhookVerifier::hmac_body("X-Signature", SignatureEncoding::Hex)
                    .expect("a static header name is valid"),
            )
            .is_err(),
            "a trigger version is a SemVer core"
        );
    }

    /// An explicit key survives the round trip through a declaration: what a
    /// connector declares is what the gate reads back.
    #[test]
    fn an_executable_mutation_keeps_its_evidence_inside_the_declaration() {
        let day = Duration::from_secs(24 * 60 * 60);
        let create = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::CREATED])
            .effect(Effect::provider_idempotent_explicit_key(
                ExplicitKeyEvidence::documented(
                    IdempotencyBinding::header("Idempotency-Key")
                        .expect("a static header name is valid"),
                    "account",
                    day,
                    Duration::from_secs(300),
                    "the provider documents Idempotency-Key with a 24 hour retention",
                )
                .expect("complete evidence is admitted"),
            ))
            .build()
            .expect("a documented explicit key builds");
        let connector = Connector::declare("provider", "1.0.0")
            .origin(OriginSpec::fixed("https://provider.example.test").expect("a static origin"))
            .credential(credential())
            .operation(create)
            .build()
            .expect("a complete declaration is valid");

        let admitted = connector
            .admit_operation("item.create")
            .expect("a documented explicit key is executable");
        assert_eq!(
            admitted.effect_class(),
            Some(EffectClass::ProviderIdempotentExplicitKey)
        );
        assert_eq!(
            admitted
                .idempotency_binding()
                .and_then(IdempotencyBinding::as_header)
                .map(reqwest::header::HeaderName::as_str),
            Some("idempotency-key")
        );
    }
}
