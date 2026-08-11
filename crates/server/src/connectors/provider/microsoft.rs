//! The serving half of the Microsoft 365 connectors (spec 015).
//!
//! These four share everything the Google Workspace four do — a stored
//! authorization-code OAuth2 credential rather than deploy-time configuration,
//! a per-operation permission requirement checked against the deployment's own
//! declaration, and a module-owned decode step — and differ in exactly two
//! places, both Microsoft's own.
//!
//! **A permission has two spellings and no case.** Microsoft documents
//! `scope=User.Read` and `https://graph.microsoft.com/User.Read` as the same
//! grant, and its own pages write both `Mail.Read` and `mail.read`, so the
//! comparison in [`donat_connectors::providers::microsoft_graph`] normalizes
//! both. A deployment that writes either form of either case is right, and this
//! module refuses only a permission no enabled operation is authorized by, or an
//! operation no declared permission authorizes.
//!
//! **`offline_access` is a protocol scope, not an API permission.** It is what
//! makes the token response carry a refresh token at all — "your app must
//! explicitly request the `offline_access` scope, to receive refresh tokens" —
//! and it is deliberately *not* required in `config.oauth2.scopes`, because
//! Microsoft also publishes "If any delegated permission is granted,
//! offline_access is implicitly granted", and `donat connector authorize`
//! refuses to write a row whose granted set does not cover the declared one. See
//! `knowledgebase/declarative-saas/decisions/057-*`.

use donat_connectors::providers::microsoft_graph::{PermissionRequirement, permission_report};
use donat_connectors::providers::{
    microsoft_excel, microsoft_onedrive, microsoft_outlook, microsoft_teams,
};
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

/// How one Microsoft 365 connector reads a response it has already been told is
/// a success.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled Microsoft 365 instance.
///
/// It holds no credential: `Credential::from_fields([])` is the accurate
/// statement that this connector configures nothing, and the declared
/// [`AuthPlan::oauth2_authorization_code`] refuses to render a request without
/// the token one attempt is given.
struct MicrosoftRuntime {
    connector: &'static Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
}

impl MicrosoftRuntime {
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

impl ProviderRuntime for MicrosoftRuntime {
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

/// Check one deployment's declared permissions against the operations it
/// enabled.
///
/// This is the startup half of `<name>_permission_shortfall_fails_closed`: it
/// runs in metadata validation, before a listener opens, so a deployment whose
/// credential could not authorize an operation it enabled never serves. The
/// other half — that the *stored* grant covers the declaration — is
/// `CredentialRuntime::validate_stored_credentials`.
fn validate_permissions(
    module: &'static str,
    permissions: fn(&str) -> Option<PermissionRequirement>,
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
    let report = permission_report(permissions, &enabled, &oauth2.scopes);
    for (operation, least) in report.missing {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.oauth2.scopes"),
            format!(
                "connector operation `{operation}` on module `{module}` is not authorized by any \
                 declared scope; Microsoft documents this method under least privileged permission \
                 `{least}`, so add it and re-run `donat connector authorize`"
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

/// One Microsoft 365 connector's deploy-time section.
///
/// Every one of them reads the same configuration — `config.oauth2` and nothing
/// else — so the section is generated rather than written four times, and the
/// only per-module material is the declaration, the permission table, and the
/// decoder.
macro_rules! microsoft_module {
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
                validate_permissions(
                    $provider::NAME,
                    $provider::permissions,
                    instance,
                    path,
                    errors,
                );
            }

            pub(crate) fn build_registered_instance(
                context: &mut ModuleContext<'_>,
            ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
                let runtime = MicrosoftRuntime::compile(
                    connector(),
                    $provider::decode,
                    $provider::pagination,
                )
                .map_err(invalid_configuration(context.instance))?;
                build_provider_instance(context, Box::new(runtime))
            }
        }
    };
}

microsoft_module!(microsoft_outlook_module, microsoft_outlook);
microsoft_module!(microsoft_teams_module, microsoft_teams);
microsoft_module!(microsoft_excel_module, microsoft_excel);
microsoft_module!(microsoft_onedrive_module, microsoft_onedrive);
