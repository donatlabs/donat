//! Compiled connector registry.
//!
//! Connector instances are selected from deploy-time metadata.  This module
//! intentionally contains a fixed table of in-binary modules: it never loads
//! code, follows a package URL, starts a subprocess, or discovers anything on
//! the network.  Runtime construction resolves only the environment variables
//! named by that metadata and publishes an immutable registry before serving.

use std::collections::BTreeMap;
use std::fmt;

use donat_metadata::Metadata;
use serde_json::Value as JsonValue;

use crate::state::{ConnectorStartupError, validate_connector_startup};

pub mod http;

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
    Http(Box<http::HttpConnector>),
    /// Stripe is deliberately only a compiled identity in this slice.  Its
    /// narrow protocol implementation arrives in Task 4, not as a fallback
    /// arbitrary HTTP client.
    Stripe,
}

/// Immutable lookup table of deployment-selected connector instances.
pub struct ConnectorRegistry {
    instances: BTreeMap<String, RegistryInstance>,
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

        let mut instances = BTreeMap::new();
        for instance in &metadata.connectors {
            let registered = match instance.module.as_str() {
                "http" => RegistryInstance::Http(Box::new(
                    http::HttpConnector::from_metadata_config(&instance.config).map_err(
                        |error| ConnectorRegistryError::InvalidConfiguration {
                            instance: instance.name.clone(),
                            message: error.to_string(),
                        },
                    )?,
                )),
                "stripe" => RegistryInstance::Stripe,
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
        Ok(Self { instances })
    }

    /// Empty immutable registry for existing server tests that deliberately do
    /// not load connector metadata.
    pub fn empty() -> Self {
        Self {
            instances: BTreeMap::new(),
        }
    }

    pub fn http_instance(&self, name: &str) -> Option<&http::HttpConnector> {
        match self.instances.get(name) {
            Some(RegistryInstance::Http(connector)) => Some(connector),
            Some(RegistryInstance::Stripe) | None => None,
        }
    }
}

#[derive(Debug)]
pub enum ConnectorRegistryError {
    Startup(ConnectorStartupError),
    UnknownModule { instance: String, module: String },
    InvalidConfiguration { instance: String, message: String },
}

impl fmt::Display for ConnectorRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
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
