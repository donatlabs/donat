//! Compiled connector registry.
//!
//! Connector instances are selected from deploy-time metadata.  This module
//! intentionally contains a fixed table of in-binary modules: it never loads
//! code, follows a package URL, starts a subprocess, or discovers anything on
//! the network.  Runtime construction resolves only the environment variables
//! named by that metadata and publishes an immutable registry before serving.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use donat_connector_abi::{OperationId, TriggerId, VerifiedInboundEvent};
use donat_connector_catalog::{OperationSpec, TriggerSpec};
use donat_metadata::{ConnectorBaseUrl, ConnectorConfig, ConnectorOperation, Metadata};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::state::{ConnectorStartupError, validate_connector_startup};

mod catalog;
pub mod http;
pub mod stripe;

/// A provider-neutral inbound verification failure. It is intentionally
/// separate from activity failures and contains no provider diagnostics,
/// signature bytes, raw body, or secret-derived value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookRejection {
    MissingSignature,
    InvalidSignature,
    TimestampOutOfTolerance,
    PayloadTooLarge,
    MalformedPayload,
    UnsupportedEvent,
}

impl WebhookRejection {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingSignature => "webhook_signature_missing",
            Self::InvalidSignature => "webhook_signature_invalid",
            Self::TimestampOutOfTolerance => "webhook_signature_expired",
            Self::PayloadTooLarge => "webhook_payload_too_large",
            Self::MalformedPayload => "webhook_payload_malformed",
            Self::UnsupportedEvent => "webhook_event_unsupported",
        }
    }
}

/// SHA-256 of a recursively key-sorted JSON value. Connector input is a JSON
/// object contract, so equivalent object order must never produce a different
/// durable activity identity.
pub(crate) fn canonical_json_sha256(input: &JsonValue) -> String {
    fn canonical(value: &JsonValue) -> JsonValue {
        match value {
            JsonValue::Object(object) => BTreeMap::from_iter(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value))),
            )
            .into_iter()
            .collect::<JsonMap<String, JsonValue>>()
            .into(),
            JsonValue::Array(values) => JsonValue::Array(values.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }

    let canonical = serde_json::to_vec(&canonical(input))
        .expect("canonical connector input JSON always serializes");
    format!("{:x}", Sha256::digest(canonical))
}

/// Every activity execution failure belongs to this closed set.  Deployment
/// metadata and startup errors deliberately use separate error types: a
/// connector that cannot start never reaches activity retry routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectorErrorClass {
    Transport,
    Timeout,
    Http429,
    Http5xx,
    Authentication,
    Validation,
    Permanent,
    Invariant,
}

/// A provider-safe, typed activity execution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorFailure {
    pub class: ConnectorErrorClass,
    pub code: &'static str,
    pub safe_message: String,
    pub retry_after: Option<std::time::Duration>,
}

impl ConnectorFailure {
    pub(crate) fn new(
        class: ConnectorErrorClass,
        code: &'static str,
        safe_message: impl Into<String>,
    ) -> Self {
        Self {
            class,
            code,
            safe_message: safe_message.into(),
            retry_after: None,
        }
    }

    pub(crate) fn with_retry_after(mut self, retry_after: Option<std::time::Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }
}

/// The JSON value produced by a connector's declared response extraction.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorSuccess {
    pub output: JsonValue,
    /// SHA-256 of canonical JSON input, suitable for a future durable activity
    /// journal. The raw input is not retained by the connector boundary.
    pub request_fingerprint: String,
}

/// Activity data a connector may observe.  It intentionally omits database
/// handles, a role, mutable process state, and retry policy.
#[derive(Debug, Clone, Copy)]
pub struct ExecutionContext {
    pub deadline: tokio::time::Instant,
}

impl ExecutionContext {
    pub fn with_deadline(deadline: tokio::time::Instant) -> Self {
        Self { deadline }
    }
}

/// Identity of a connector module compiled into this exact binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorDefinition {
    pub module_name: &'static str,
    pub semantic_version: &'static str,
    pub runtime_abi: u32,
}

/// Common minimum for compiled modules.  Execution remains module-specific so
/// callers cannot turn this into a raw HTTP request interface.
pub trait ConnectorModule: Send + Sync {
    fn definition(&self) -> ConnectorDefinition;
}

const HTTP_DEFINITION: ConnectorDefinition = ConnectorDefinition {
    module_name: "http",
    semantic_version: "0.1.0",
    runtime_abi: 1,
};
const STRIPE_DEFINITION: ConnectorDefinition = ConnectorDefinition {
    module_name: "stripe",
    semantic_version: "0.1.0",
    runtime_abi: 1,
};

enum RegistryInstance {
    Http {
        connector: Box<http::HttpConnector>,
        operations: BTreeMap<String, CompiledHttpOperation>,
    },
    Stripe {
        connector: Box<stripe::StripeConnector>,
        operations: BTreeMap<String, CompiledStripeOperation>,
        webhook: CompiledWebhookTrigger,
    },
}

struct CompiledWebhookTrigger {
    source_name: String,
    spec: Arc<TriggerSpec>,
    configuration_fingerprint: String,
}

/// One immutable deployment-selected webhook verifier. The verifier remains
/// module-owned, while its normalized behavior is described by the exact
/// catalog-owned trigger snapshot consumed by Process compilation.
pub struct WebhookInstance<'a> {
    source_name: &'a str,
    trigger: &'a TriggerSpec,
    connector: &'a stripe::StripeConnector,
}

impl WebhookInstance<'_> {
    pub fn source_name(&self) -> &str {
        self.source_name
    }

    pub fn trigger(&self) -> &TriggerSpec {
        self.trigger
    }

    pub fn raw_body_max_bytes(&self) -> usize {
        match self.trigger {
            TriggerSpec::Webhook {
                raw_body_max_bytes, ..
            } => raw_body_max_bytes.get() as usize,
            TriggerSpec::Poll { .. } => {
                unreachable!("an HTTP webhook route cannot retain a poll trigger")
            }
        }
    }

    pub fn verify(
        &self,
        headers: &axum::http::HeaderMap,
        raw_body: &[u8],
    ) -> Result<VerifiedInboundEvent, WebhookRejection> {
        self.connector.verify_completed_webhook(headers, raw_body)
    }
}

/// The immutable compiled contract for one deployed HTTP operation. The
/// fingerprint is intentionally non-secret and therefore safe for a future
/// process-definition revision to retain.
struct CompiledHttpOperation {
    operation: http::ValidatedHttpOperation,
    configuration_fingerprint: String,
}

/// A selected Stripe Checkout operation. The operation name is still checked
/// at dispatch even after startup validation so a future job cannot reach an
/// unenabled provider capability.
struct CompiledStripeOperation {
    configuration_fingerprint: String,
    serialization_key_input: Option<String>,
}

#[derive(Serialize)]
struct HttpConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    operation_name: &'a str,
    operation_version: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    network_policy: &'a Option<String>,
    credential_header_declarations: &'a [donat_metadata::ConnectorHeader],
    operation_profile: &'a donat_metadata::HttpConnectorOperation,
    capacity: &'a donat_metadata::ConnectorCapacity,
    base_url_source: HttpBaseUrlSourceIdentity<'a>,
    base_url_sha256: &'a str,
}

/// The configured origin of a base URL, deliberately excluding its resolved
/// value. An environment-variable name is deploy-time identity, while the
/// resolved endpoint itself is represented only by `base_url_sha256`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HttpBaseUrlSourceIdentity<'a> {
    Literal,
    Environment { variable: &'a str },
}

fn http_configuration_fingerprint(
    definition: ConnectorDefinition,
    config: &ConnectorConfig,
    operation: &ConnectorOperation,
    base_url_digest: &str,
) -> String {
    let profile = operation
        .http()
        .expect("HTTP operation profile was validated before fingerprinting");
    let capacity = operation
        .capacity()
        .expect("HTTP operation capacity was validated before fingerprinting");
    let base_url_source = match config
        .base_url
        .as_ref()
        .expect("HTTP base URL was validated before fingerprinting")
    {
        ConnectorBaseUrl::Literal(_) => HttpBaseUrlSourceIdentity::Literal,
        ConnectorBaseUrl::FromEnv(reference) => HttpBaseUrlSourceIdentity::Environment {
            variable: &reference.value_from_env,
        },
    };
    let canonical = HttpConfigurationFingerprint {
        module_name: definition.module_name,
        module_semantic_version: definition.semantic_version,
        runtime_abi: definition.runtime_abi,
        operation_name: &operation.name,
        operation_version: &profile.version,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        network_policy: &config.network_policy,
        credential_header_declarations: &config.headers,
        operation_profile: profile,
        capacity,
        base_url_source,
        base_url_sha256: base_url_digest,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated connector fingerprint fields always serialize to JSON");
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Serialize)]
struct StripeConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    operation_name: &'a str,
    operation_version: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    api_version: &'a str,
    secret_key_environment: &'a str,
    webhook_secret_environment: &'a str,
    capacity: &'a donat_metadata::ConnectorCapacity,
}

#[derive(Serialize)]
struct StripeWebhookConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    trigger_name: &'a str,
    trigger_version: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    api_version: &'a str,
    webhook_secret_environment: &'a str,
}

fn stripe_configuration_fingerprint(
    config: &ConnectorConfig,
    operation: &ConnectorOperation,
) -> String {
    let secret_key_environment = &config
        .secret_key
        .as_ref()
        .expect("Stripe secret key was validated before fingerprinting")
        .value_from_env;
    let webhook_secret_environment = &config
        .webhook_secret
        .as_ref()
        .expect("Stripe webhook secret was validated before fingerprinting")
        .value_from_env;
    let api_version = config
        .api_version
        .as_deref()
        .expect("Stripe API version was validated before fingerprinting");
    let capacity = operation
        .capacity()
        .expect("Stripe operation capacity was validated before fingerprinting");
    let canonical = StripeConfigurationFingerprint {
        module_name: STRIPE_DEFINITION.module_name,
        module_semantic_version: STRIPE_DEFINITION.semantic_version,
        runtime_abi: STRIPE_DEFINITION.runtime_abi,
        operation_name: &operation.name,
        operation_version: stripe::STRIPE_OPERATION_VERSION,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        api_version,
        secret_key_environment,
        webhook_secret_environment,
        capacity,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated Stripe fingerprint fields always serialize to JSON");
    format!("{:x}", Sha256::digest(bytes))
}

fn stripe_webhook_configuration_fingerprint(config: &ConnectorConfig) -> String {
    let webhook_secret_environment = &config
        .webhook_secret
        .as_ref()
        .expect("Stripe webhook secret was validated before fingerprinting")
        .value_from_env;
    let api_version = config
        .api_version
        .as_deref()
        .expect("Stripe API version was validated before fingerprinting");
    let canonical = StripeWebhookConfigurationFingerprint {
        module_name: STRIPE_DEFINITION.module_name,
        module_semantic_version: STRIPE_DEFINITION.semantic_version,
        runtime_abi: STRIPE_DEFINITION.runtime_abi,
        trigger_name: stripe::COMPLETED_WEBHOOK_TRIGGER,
        trigger_version: stripe::STRIPE_TRIGGER_VERSION,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        api_version,
        webhook_secret_environment,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated Stripe webhook fingerprint fields serialize");
    format!("{:x}", Sha256::digest(bytes))
}

type OperationSpecHandles =
    BTreeMap<String, BTreeMap<String, BTreeMap<OperationId, Arc<OperationSpec>>>>;

/// Immutable lookup table of deployment-selected connector instances.
pub struct ConnectorRegistry {
    instances: BTreeMap<String, RegistryInstance>,
    operation_specs: OperationSpecHandles,
}

impl ConnectorRegistry {
    /// The complete module table compiled into the binary.
    pub fn built_in_module_names() -> [&'static str; 2] {
        [HTTP_DEFINITION.module_name, STRIPE_DEFINITION.module_name]
    }

    /// Resolve and validate deployment configuration before a listener opens.
    /// Error messages contain metadata identities and variable *names* only;
    /// resolved values never enter an activity failure or a log message here.
    pub fn build(metadata: &Metadata) -> Result<Self, ConnectorRegistryError> {
        validate_connector_startup(metadata).map_err(ConnectorRegistryError::Startup)?;

        let source_name = if metadata.connectors.is_empty() {
            None
        } else {
            let postgres_sources = metadata
                .sources
                .iter()
                .filter(|source| source.kind == donat_metadata::SourceKind::Postgres)
                .collect::<Vec<_>>();
            if postgres_sources.len() != 1 {
                return Err(ConnectorRegistryError::ImplicitSourceBinding {
                    postgres_sources: postgres_sources.len(),
                });
            }
            Some(postgres_sources[0].name.clone())
        };
        let mut instances = BTreeMap::new();
        let mut operation_specs = BTreeMap::new();
        if let Some(source_name) = &source_name {
            operation_specs.insert(source_name.clone(), BTreeMap::new());
        }
        for instance in &metadata.connectors {
            let registered = match instance.module.as_str() {
                "http" => {
                    let connector = http::HttpConnector::from_metadata_config(&instance.config)
                        .map_err(|error| ConnectorRegistryError::InvalidConfiguration {
                            instance: instance.name.clone(),
                            message: error.to_string(),
                        })?;
                    let base_url_digest = connector.base_url_digest();
                    let mut operations = BTreeMap::new();
                    let executable_specs = operation_specs
                        .get_mut(
                            source_name
                                .as_deref()
                                .expect("connector instances have one Postgres source"),
                        )
                        .expect("the connector source operation table was initialized")
                        .entry(instance.name.clone())
                        .or_insert_with(BTreeMap::new);
                    for operation in &instance.operations {
                        let validated = http::ValidatedHttpOperation::from_metadata(operation)
                            .map_err(|error| ConnectorRegistryError::InvalidConfiguration {
                                instance: instance.name.clone(),
                                message: error.to_string(),
                            })?;
                        if let Some(spec) = catalog::compile_http_operation_spec(
                            metadata,
                            HTTP_DEFINITION,
                            instance,
                            operation,
                        )
                        .map_err(|message| {
                            ConnectorRegistryError::InvalidConfiguration {
                                instance: instance.name.clone(),
                                message,
                            }
                        })? && executable_specs
                            .insert(spec.operation, Arc::new(spec))
                            .is_some()
                        {
                            return Err(ConnectorRegistryError::InvalidConfiguration {
                                instance: instance.name.clone(),
                                message: format!(
                                    "executable catalog operation `{}` is declared more than once",
                                    operation.name
                                ),
                            });
                        }
                        let compiled = CompiledHttpOperation {
                            configuration_fingerprint: http_configuration_fingerprint(
                                HTTP_DEFINITION,
                                &instance.config,
                                operation,
                                &base_url_digest,
                            ),
                            operation: validated,
                        };
                        if operations
                            .insert(operation.name.clone(), compiled)
                            .is_some()
                        {
                            return Err(ConnectorRegistryError::InvalidConfiguration {
                                instance: instance.name.clone(),
                                message: format!(
                                    "connector operation `{}` is declared more than once",
                                    operation.name
                                ),
                            });
                        }
                    }
                    RegistryInstance::Http {
                        connector: Box::new(connector),
                        operations,
                    }
                }
                "stripe" => {
                    stripe::validate_stripe_instance_metadata(
                        &instance.config,
                        &instance.operations,
                    )
                    .map_err(|error| {
                        ConnectorRegistryError::InvalidConfiguration {
                            instance: instance.name.clone(),
                            message: error.to_string(),
                        }
                    })?;
                    let connector = stripe::StripeConnector::from_metadata_config(&instance.config)
                        .map_err(|error| ConnectorRegistryError::InvalidConfiguration {
                            instance: instance.name.clone(),
                            message: error.to_string(),
                        })?;
                    let webhook_spec = catalog::compile_stripe_checkout_completed_trigger_spec(
                        metadata,
                        STRIPE_DEFINITION,
                    )
                    .map_err(|message| {
                        ConnectorRegistryError::InvalidConfiguration {
                            instance: instance.name.clone(),
                            message,
                        }
                    })?;
                    let webhook = CompiledWebhookTrigger {
                        source_name: source_name
                            .as_ref()
                            .expect("connector instances have one Postgres source")
                            .clone(),
                        spec: Arc::new(webhook_spec),
                        configuration_fingerprint: stripe_webhook_configuration_fingerprint(
                            &instance.config,
                        ),
                    };
                    let mut operations = BTreeMap::new();
                    for operation in &instance.operations {
                        let compiled = CompiledStripeOperation {
                            configuration_fingerprint: stripe_configuration_fingerprint(
                                &instance.config,
                                operation,
                            ),
                            serialization_key_input: operation
                                .capacity()
                                .and_then(|capacity| capacity.serialize_by.as_ref())
                                .map(|binding| binding.input.clone()),
                        };
                        if operations
                            .insert(operation.name.clone(), compiled)
                            .is_some()
                        {
                            return Err(ConnectorRegistryError::InvalidConfiguration {
                                instance: instance.name.clone(),
                                message: format!(
                                    "connector operation `{}` is declared more than once",
                                    operation.name
                                ),
                            });
                        }
                    }
                    RegistryInstance::Stripe {
                        connector: Box::new(connector),
                        operations,
                        webhook,
                    }
                }
                // Keep this defensive branch even though Task 1's static
                // validator rejects it first: no dynamic fallback exists.
                module => {
                    return Err(ConnectorRegistryError::UnknownModule {
                        instance: instance.name.clone(),
                        module: module.to_owned(),
                    });
                }
            };
            instances.insert(instance.name.clone(), registered);
        }
        Ok(Self {
            instances,
            operation_specs,
        })
    }

    /// Empty immutable registry for existing server tests that deliberately do
    /// not load connector metadata.
    pub fn empty() -> Self {
        Self {
            instances: BTreeMap::new(),
            operation_specs: BTreeMap::new(),
        }
    }

    /// Resolve one accepted catalog-owned trigger within its exact deployment
    /// source. Inventory-only or module-local verifier details are absent.
    pub fn trigger_spec_handle(
        &self,
        source_name: &str,
        instance_name: &str,
        trigger: TriggerId,
    ) -> Option<Arc<TriggerSpec>> {
        match self.instances.get(instance_name)? {
            RegistryInstance::Stripe { webhook, .. }
                if webhook.source_name == source_name
                    && matches!(
                        webhook.spec.as_ref(),
                        TriggerSpec::Webhook {
                            trigger: candidate,
                            ..
                        } if *candidate == trigger
                    ) =>
            {
                Some(webhook.spec.clone())
            }
            RegistryInstance::Stripe { .. } | RegistryInstance::Http { .. } => None,
        }
    }

    /// Resolve one executable, catalog-owned operation descriptor within its
    /// deployment source. Runtime transport state, secret values, and resolved
    /// endpoint URLs are deliberately absent from the returned snapshot.
    pub fn operation_spec(
        &self,
        source_name: &str,
        instance_name: &str,
        operation: OperationId,
    ) -> Option<&OperationSpec> {
        self.operation_spec_entry(source_name, instance_name, operation)
            .map(Arc::as_ref)
    }

    /// Clone the shared immutable handle for a compiled dependency that must
    /// outlive a temporary registry borrow. This clones only the `Arc`, never
    /// the catalog-owned behavioral snapshot.
    pub fn operation_spec_handle(
        &self,
        source_name: &str,
        instance_name: &str,
        operation: OperationId,
    ) -> Option<Arc<OperationSpec>> {
        self.operation_spec_entry(source_name, instance_name, operation)
            .cloned()
    }

    fn operation_spec_entry(
        &self,
        source_name: &str,
        instance_name: &str,
        operation: OperationId,
    ) -> Option<&Arc<OperationSpec>> {
        self.operation_specs
            .get(source_name)?
            .get(instance_name)?
            .get(&operation)
    }

    pub fn http_instance(&self, name: &str) -> Option<&http::HttpConnector> {
        match self.instances.get(name) {
            Some(RegistryInstance::Http { connector, .. }) => Some(connector),
            Some(RegistryInstance::Stripe { .. }) | None => None,
        }
    }

    /// Resolve the one compiled inbound verifier currently supported by the
    /// deployment. This returns `None` for both an unknown instance and a
    /// declared module without a webhook verifier, so the HTTP boundary never
    /// exposes connector configuration or capability details to an ingress
    /// caller.
    pub fn webhook_instance(&self, name: &str) -> Option<WebhookInstance<'_>> {
        match self.instances.get(name) {
            Some(RegistryInstance::Stripe {
                connector, webhook, ..
            }) => Some(WebhookInstance {
                source_name: &webhook.source_name,
                trigger: webhook.spec.as_ref(),
                connector,
            }),
            Some(RegistryInstance::Http { .. }) | None => None,
        }
    }

    pub fn trigger_configuration_fingerprint(
        &self,
        instance: &str,
        trigger: TriggerId,
    ) -> Option<&str> {
        match self.instances.get(instance) {
            Some(RegistryInstance::Stripe { webhook, .. })
                if matches!(
                    webhook.spec.as_ref(),
                    TriggerSpec::Webhook {
                        trigger: candidate,
                        ..
                    } if *candidate == trigger
                ) =>
            {
                Some(&webhook.configuration_fingerprint)
            }
            Some(RegistryInstance::Stripe { .. }) | Some(RegistryInstance::Http { .. }) | None => {
                None
            }
        }
    }

    /// Return the immutable, non-secret deployment fingerprint for one
    /// compiled operation. It contains no resolved credential/header value or
    /// raw base URL, so a future process revision can retain it safely.
    pub fn configuration_fingerprint(&self, instance: &str, operation: &str) -> Option<&str> {
        match self.instances.get(instance) {
            Some(RegistryInstance::Http { operations, .. }) => operations
                .get(operation)
                .map(|operation| operation.configuration_fingerprint.as_str()),
            Some(RegistryInstance::Stripe { operations, .. }) => operations
                .get(operation)
                .map(|operation| operation.configuration_fingerprint.as_str()),
            None => None,
        }
    }

    /// Return the optional scalar input that serializes this exact operation.
    /// An unknown operation and an operation without serialization both return
    /// `None`; callers already resolve the operation spec before consulting
    /// this refinement.
    pub fn serialization_key_input(&self, instance: &str, operation: &str) -> Option<&str> {
        match self.instances.get(instance) {
            Some(RegistryInstance::Http { operations, .. }) => operations
                .get(operation)
                .and_then(|operation| operation.operation.serialization_key_input()),
            Some(RegistryInstance::Stripe { operations, .. }) => operations
                .get(operation)
                .and_then(|operation| operation.serialization_key_input.as_deref()),
            None => None,
        }
    }

    /// Execute only a named operation compiled from deployed metadata. This
    /// deliberately accepts neither a raw URL/method/header nor a caller-owned
    /// HTTP client. The future process worker supplies the stable idempotency
    /// key after acquiring its durable capacity reservation.
    pub async fn execute(
        &self,
        instance: &str,
        operation: &str,
        input: JsonValue,
        idempotency_key: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        let Some(instance) = self.instances.get(instance) else {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "connector instance is not declared",
            ));
        };
        match instance {
            RegistryInstance::Http {
                connector,
                operations,
            } => {
                let Some(operation) = operations.get(operation) else {
                    return Err(ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_invariant",
                        "connector operation is not declared",
                    ));
                };
                connector
                    .execute_validated(&operation.operation, input, idempotency_key, deadline)
                    .await
            }
            RegistryInstance::Stripe {
                connector,
                operations,
                ..
            } => {
                if !operations.contains_key(operation) {
                    return Err(ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_invariant",
                        "connector operation is not declared",
                    ));
                }
                if operation != stripe::CREATE_CHECKOUT_SESSION_OPERATION {
                    return Err(ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_invariant",
                        "connector operation is not compiled into this binary",
                    ));
                }
                stripe::execute_checkout_from_json(connector, input, idempotency_key, deadline)
                    .await
            }
        }
    }
}

#[derive(Debug)]
pub enum ConnectorRegistryError {
    Startup(ConnectorStartupError),
    ImplicitSourceBinding { postgres_sources: usize },
    UnknownModule { instance: String, module: String },
    InvalidConfiguration { instance: String, message: String },
}

impl fmt::Display for ConnectorRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ImplicitSourceBinding { postgres_sources } => write!(
                formatter,
                "connector instances without an explicit source require exactly one Postgres source; found {postgres_sources}"
            ),
            Self::UnknownModule { instance, module } => write!(
                formatter,
                "connector instance `{instance}` selects unavailable compiled module `{module}`"
            ),
            Self::InvalidConfiguration { instance, message } => {
                write!(
                    formatter,
                    "connector instance `{instance}` has invalid configuration: {message}"
                )
            }
        }
    }
}

impl std::error::Error for ConnectorRegistryError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ConnectorDefinition, HTTP_DEFINITION, http_configuration_fingerprint};

    #[test]
    fn http_configuration_fingerprint_changes_when_runtime_abi_changes() {
        let operation: donat_metadata::ConnectorOperation = serde_json::from_value(json!({
            "name": "create_shipment",
            "version": "v1",
            "method": "POST",
            "path": "/v1/shipments/{input.order_id}",
            "success_statuses": [200],
            "idempotency": { "header": "Idempotency-Key" },
            "capacity": {
                "max_in_flight": 1,
                "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
            }
        }))
        .expect("fingerprint operation metadata deserializes");
        let config: donat_metadata::ConnectorConfig = serde_json::from_value(json!({
            "endpoint_identity": "logistics_test",
            "credential_identity": "logistics_test_credential",
            "base_url": "https://provider.example.test"
        }))
        .expect("fingerprint config metadata deserializes");

        let baseline =
            http_configuration_fingerprint(HTTP_DEFINITION, &config, &operation, "base-url-digest");
        let changed_abi = http_configuration_fingerprint(
            ConnectorDefinition {
                runtime_abi: HTTP_DEFINITION.runtime_abi + 1,
                ..HTTP_DEFINITION
            },
            &config,
            &operation,
            "base-url-digest",
        );

        assert_ne!(baseline, changed_abi);
    }
}
