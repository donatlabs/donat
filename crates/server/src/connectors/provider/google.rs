//! The serving half of the Google Workspace connectors (spec 014).
//!
//! These four are the first hand-written connectors whose credential is not
//! deploy-time configuration. A deployment configures no secret for them at
//! all: it declares `config.oauth2`, an operator runs `donat connector
//! authorize` once, and every attempt is handed a live `Authorization` header
//! by `crate::credentials` ([[041-a-credential-the-engine-writes-is-still-not-an-admin-api]],
//! [[043-the-credential-seam-refuses-before-it-sends]]).
//!
//! Two things are therefore different from every other section in
//! `provider/modules.rs`, and both are startup obligations rather than runtime
//! behaviour.
//!
//! **The scope set follows the enabled operations.** Google lists, per method,
//! every scope that admits it. A deployment declares what it will ask Google
//! for in `config.oauth2.scopes`, and this module checks that declaration
//! against the operations the deployment enabled: an operation whose documented
//! scope set is not met is refused with its metadata path, and so is a scope no
//! enabled operation is authorized by. A deployment that enables only reads is
//! therefore never able to hold a write scope by accident, and never asked to
//! grant one (spec 014 §1, §3.1).
//!
//! **The decode step is the module's own.** Google reports some failures inside
//! a `200`, so these connectors do not use the declaration-driven
//! [`super::DeclaredProvider`]: they render from the declaration exactly as it
//! does and then run their own guard before the declared output pointers are
//! read. See `crates/connectors/src/providers/google.rs`.

use donat_connectors::providers::google::{ScopeRequirement, scope_report};
use donat_connectors::providers::{google_calendar, google_drive, google_gmail, google_sheets};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorFailure, Credential, Operation,
    OperationRejection, Origin, Pagination, RequestPlan,
};
use donat_metadata::ConnectorInstance;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use super::{
    CredentialShape, PaginationLookup, ProviderRules, ProviderRuntime, WebhookShape,
    build_registered_instance as build_provider_instance, invalid_configuration,
};
use crate::connectors::{ConnectorRegistryError, ModuleContext, RegisteredConnector};
use crate::state::ConnectorConfigError;

/// How one Google connector reads a response it has already been told is a
/// success. Every module owns its own, because the per-item failure shapes
/// differ and only the module knows which of its operations publish one.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled Google Workspace instance.
///
/// It holds no credential: `Credential::from_fields([])` is not a placeholder
/// but the accurate statement that this connector configures nothing, and the
/// declared [`AuthPlan::oauth2_authorization_code`] refuses to render a request
/// without the token one attempt is given.
struct GoogleRuntime {
    connector: &'static Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
}

impl GoogleRuntime {
    fn compile(
        connector: &'static Connector,
        decode: Decoder,
        pagination: PaginationLookup,
    ) -> Result<Self, String> {
        let origin = connector
            .resolve_origin(&ConnectorConfiguration::default())
            .map_err(|error| error.message().to_owned())?;
        let credential = Credential::from_fields([]);
        connector
            .credential()
            .admits(&credential)
            .map_err(|missing| missing.to_string())?;
        Ok(Self {
            connector,
            origin,
            credential,
            decode,
            pagination,
        })
    }

    fn operation(&self, id: &str) -> Result<&'static Operation, ConnectorFailure> {
        self.connector.operation(id).ok_or_else(|| {
            ConnectorFailure::invariant("connector operation is not compiled into this binary")
        })
    }
}

impl ProviderRuntime for GoogleRuntime {
    fn origin(&self) -> &Origin {
        &self.origin
    }

    fn auth_plan(&self) -> Option<&AuthPlan> {
        self.connector.credential().plan()
    }

    fn credential(&self) -> &Credential {
        &self.credential
    }

    fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        self.connector.admit_operation(id)
    }

    fn applies_stored_oauth2(&self) -> bool {
        true
    }

    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        _idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.operation(id)?.plan_request(&self.origin, input)
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        (self.decode)(self.operation(id)?, status, headers, body)
    }

    fn pagination(&self, id: &str) -> Option<Pagination> {
        (self.pagination)(id).cloned()
    }

    /// The page gate is the module's own, for the same reason its `decode` is:
    /// this provider reports some failures inside a `200`, so the status alone
    /// cannot say whether a page of a walk is a page at all.
    fn admit_page(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(), ConnectorFailure> {
        self.decode(id, status, headers, body).map(|_| ())
    }
}

/// Check one deployment's declared scopes against the operations it enabled.
///
/// This is the startup half of `<name>_scope_shortfall_fails_closed`: it runs
/// in metadata validation, before a listener opens, so a deployment whose
/// credential could not authorize an operation it enabled never serves. The
/// other half — that the *stored* grant covers the declaration — is
/// `CredentialRuntime::validate_stored_credentials`.
fn validate_scopes(
    module: &'static str,
    scopes: fn(&str) -> Option<ScopeRequirement>,
    instance: &ConnectorInstance,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
) {
    let Some(oauth2) = &instance.config.oauth2 else {
        // `ProviderRules` already refused the instance for having no `oauth2`
        // block at all; reporting it twice helps nobody.
        return;
    };
    let enabled = instance
        .operations
        .iter()
        .map(|operation| operation.name.clone())
        .collect::<Vec<_>>();
    let report = scope_report(scopes, &enabled, &oauth2.scopes);
    for (operation, least) in report.missing {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.oauth2.scopes"),
            format!(
                "connector operation `{operation}` on module `{module}` is not authorized by any \
                 declared scope; Google documents this method under `{least}` among others, so \
                 add it and re-run `donat connector authorize`"
            ),
        ));
    }
    for scope in report.surplus {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.oauth2.scopes"),
            format!(
                "the `{module}` connector declares scope `{scope}`, which no enabled operation of \
                 this instance is authorized by; remove it or enable the operation that needs it"
            ),
        ));
    }
}

/// One Google Workspace connector's deploy-time section.
///
/// Every one of them reads the same configuration — `config.oauth2` and nothing
/// else — so the section is generated rather than written four times, and the
/// only per-module material is the declaration, the scope table, and the
/// decoder.
macro_rules! google_module {
    ($section:ident, $provider:ident) => {
        pub(crate) mod $section {
            use super::*;

            const RULES: ProviderRules = ProviderRules {
                module: $provider::NAME,
                // No `secret_key`, no `config.secrets`: the credential is the
                // source-local store's, written by `donat connector authorize`.
                credential: CredentialShape::Oauth2,
                webhook: WebhookShape::None,
                settings: &[],
                secrets: &[],
            };

            pub(crate) fn connector() -> &'static Connector {
                $provider::connector()
            }

            pub(crate) fn validate_instance_metadata(
                instance: &ConnectorInstance,
                path: &str,
                errors: &mut Vec<ConnectorConfigError>,
            ) {
                RULES.validate(instance, path, errors);
                validate_scopes($provider::NAME, $provider::scopes, instance, path, errors);
            }

            pub(crate) fn build_registered_instance(
                context: &mut ModuleContext<'_>,
            ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
                let runtime =
                    GoogleRuntime::compile(connector(), $provider::decode, $provider::pagination)
                        .map_err(invalid_configuration(context.instance))?;
                build_provider_instance(context, Box::new(runtime))
            }
        }
    };
}

google_module!(google_sheets_module, google_sheets);
google_module!(google_drive_module, google_drive);
google_module!(google_gmail_module, google_gmail);
google_module!(google_calendar_module, google_calendar);
