//! The serving half of the CRM and helpdesk connectors (spec 023).
//!
//! Six modules in one file, because they share one runtime and differ only in
//! their deploy-time configuration. Everything module-specific — which
//! operations exist, how a request renders, what a response means — stays in
//! `donat_connectors::providers`; this file reads one instance's configuration,
//! resolves its `SecretRef`s, and hands the module the values it declared.
//!
//! Four things are different from `provider/modules.rs` and each is the batch's
//! own.
//!
//! **Four of the six have a per-tenant host.** Zendesk, Freshdesk, and
//! Salesforce fill one templated host label from `config.settings`; WooCommerce
//! names a whole origin, because a store has no vendor suffix to be a label
//! under. Every one of them is checked here, at deploy time, so a host a
//! deployment mistyped is a startup refusal rather than a `404` on the first
//! activity attempt.
//!
//! **One operation in the batch binds an idempotency key.** Zendesk publishes
//! `Idempotency-Key` for ticket creation, so [`CrmRuntime::plan`] renders every
//! request through `Operation::plan_keyed_request`: the operation whose class
//! declared a header binding gets the durable activity's stable key written into
//! it, and every other operation renders exactly as it always did.
//!
//! **Two providers report a failure inside a `2xx`.** Pipedrive carries its own
//! `success` boolean and Zoho carries a per-record `status`, so every module in
//! this batch owns its `decode` and the runtime calls it — the same seam the
//! Google and Microsoft connectors use, for the same reason
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).
//!
//! **One provider publishes an `Authorization` scheme of its own.** Zoho CRM
//! sends `Zoho-oauthtoken` rather than `Bearer`, so its instance answers
//! `oauth2_authorization_scheme` with what its declaration published and the
//! credential lifecycle formats the applied header with it.

use donat_connectors::providers::{
    freshdesk, pipedrive, salesforce, woocommerce, zendesk, zoho_crm,
};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorFailure, Credential, Operation,
    OperationRejection, Origin, Pagination, RequestPlan, Secret,
};
use donat_metadata::ConnectorInstance;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use super::{
    CredentialShape, Key, PaginationLookup, ProviderRules, ProviderRuntime, WebhookShape,
    build_registered_instance as build_provider_instance, invalid_configuration, required_setting,
    resolve_secret_key,
};
use crate::connectors::{ConnectorRegistryError, ModuleContext, RegisteredConnector};
use crate::state::ConnectorConfigError;

/// How one connector in this batch reads a response it has already been told is
/// a success. Every module owns its own, because two of the six answer a failure
/// inside a `2xx` and only the module knows where it says so.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled CRM or helpdesk instance.
///
/// It holds the declaration it was validated against by value, because four of
/// the six are completed by one deployment — an account address, a consumer key,
/// a data centre — exactly as Twilio's and Jira's are
/// ([[048-a-declaration-a-deployment-completes]]).
struct CrmRuntime {
    connector: Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
    /// Whether this module's credential is the source-local OAuth2 store's.
    stored_oauth2: bool,
}

impl CrmRuntime {
    fn compile(
        connector: Connector,
        credential: Credential,
        configuration: &ConnectorConfiguration,
        decode: Decoder,
        pagination: PaginationLookup,
    ) -> Result<Self, String> {
        let origin = connector
            .resolve_origin(configuration)
            .map_err(|error| error.message().to_owned())?;
        // Startup answers "is this credential complete" once, by name, before a
        // listener opens rather than at the first activity attempt.
        connector
            .credential()
            .admits(&credential)
            .map_err(|missing| missing.to_string())?;
        let stored_oauth2 = connector
            .credential()
            .plan()
            .and_then(AuthPlan::oauth2_authorization_scheme)
            .is_some();
        Ok(Self {
            connector,
            origin,
            credential,
            decode,
            pagination,
            stored_oauth2,
        })
    }

    fn operation(&self, id: &str) -> Result<&Operation, ConnectorFailure> {
        self.connector.operation(id).ok_or_else(|| {
            ConnectorFailure::invariant("connector operation is not compiled into this binary")
        })
    }
}

impl ProviderRuntime for CrmRuntime {
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
        self.stored_oauth2
    }

    /// Render one request, with the activity's stable key bound where the
    /// operation's own class declared a header for it.
    ///
    /// For every operation whose class binds nothing this is exactly
    /// `plan_request`; for Zendesk's ticket create it is what makes the
    /// `ProviderIdempotent::ExplicitKey` class true on the wire rather than only
    /// in the declaration ([[034-a-declaration-the-runtime-ignores-is-a-defect]]).
    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.operation(id)?
            .plan_keyed_request(&self.origin, input, idempotency_key)
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

    /// The page gate is the module's own, because two of these providers report
    /// a failure inside a `200` and the status alone cannot say whether a page
    /// of a walk is a page at all.
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

// ===========================================================================
// pipedrive — one API key, one fixed origin, and a body gate
// ===========================================================================

pub(crate) mod pipedrive_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: pipedrive::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        pipedrive::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(&context.instance.config).map_err(&invalid)?;
        let runtime = CrmRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            &ConnectorConfiguration::default(),
            pipedrive::decode,
            pipedrive::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// freshdesk — a templated host, and the API key as the Basic username
// ===========================================================================

pub(crate) mod freshdesk_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: freshdesk::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The helpdesk domain is the connector's *host*, so it is deploy-time by
        // construction: an operation input that could reach it would be an
        // input choosing an authority.
        settings: &[Key::required(freshdesk::DOMAIN)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        freshdesk::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // The templated host's own grammar, checked at deploy time rather than
        // at the first activity attempt.
        if let Ok(domain) = required_setting(&instance.config, freshdesk::DOMAIN)
            && let Err(error) = freshdesk::connector().resolve_origin(
                &ConnectorConfiguration::from_deployment([(freshdesk::DOMAIN, domain)]),
            )
        {
            RULES.refuse_setting(path, freshdesk::DOMAIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let api_key = resolve_secret_key(config).map_err(&invalid)?;
        let domain = required_setting(config, freshdesk::DOMAIN)
            .map_err(&invalid)?
            .to_owned();
        let runtime = CrmRuntime::compile(
            context.connector.clone(),
            Credential::secret(api_key),
            &ConnectorConfiguration::from_deployment([(freshdesk::DOMAIN, domain.as_str())]),
            freshdesk::decode,
            freshdesk::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// zendesk — a templated host, a per-deployment declaration, and the one
// idempotency key in this batch
// ===========================================================================

pub(crate) mod zendesk_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: zendesk::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The subdomain is the connector's *host* and the email is half its
        // HTTP Basic username, so both are deploy-time by construction.
        settings: &[
            Key::required(zendesk::SUBDOMAIN),
            Key::required(zendesk::EMAIL),
        ],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// `zendesk::connector` takes the account address because `AuthPlan::basic`
    /// takes its username where the plan is built, and Zendesk's username is
    /// that address with its own `/token` suffix.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let email = required_setting(&instance.config, zendesk::EMAIL).ok()?;
        zendesk::connector(email).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(email) = required_setting(&instance.config, zendesk::EMAIL)
            && let Err(error) = zendesk::connector(email)
        {
            RULES.refuse_setting(path, zendesk::EMAIL, error.message(), errors);
        }
        if let (Ok(subdomain), Ok(connector)) = (
            required_setting(&instance.config, zendesk::SUBDOMAIN),
            zendesk::declaration_shape(),
        ) && let Err(error) = connector.resolve_origin(&ConnectorConfiguration::from_deployment(
            [(zendesk::SUBDOMAIN, subdomain)],
        )) {
            RULES.refuse_setting(path, zendesk::SUBDOMAIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let api_token = resolve_secret_key(config).map_err(&invalid)?;
        let subdomain = required_setting(config, zendesk::SUBDOMAIN)
            .map_err(&invalid)?
            .to_owned();
        let email = required_setting(config, zendesk::EMAIL)
            .map_err(&invalid)?
            .to_owned();
        let runtime = CrmRuntime::compile(
            context.connector.clone(),
            // The address is Zendesk's declared non-secret credential field as
            // well as half the Basic username its declaration was built with.
            Credential::from_fields([
                ("secret", Secret::new(api_token)),
                (zendesk::EMAIL, Secret::new(&email)),
            ]),
            &ConnectorConfiguration::from_deployment([(zendesk::SUBDOMAIN, subdomain.as_str())]),
            zendesk::decode,
            zendesk::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// woocommerce — the store's whole origin, and the consumer key as the Basic
// username
// ===========================================================================

pub(crate) mod woocommerce_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: woocommerce::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[
            Key::required(woocommerce::STORE_ORIGIN),
            Key::required(woocommerce::CONSUMER_KEY),
        ],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// The consumer key is the HTTP Basic username WooCommerce publishes, and
    /// `AuthPlan::basic` takes its username where the plan is built.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let consumer_key = required_setting(&instance.config, woocommerce::CONSUMER_KEY).ok()?;
        woocommerce::connector(consumer_key).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(consumer_key) = required_setting(&instance.config, woocommerce::CONSUMER_KEY)
            && let Err(error) = woocommerce::connector(consumer_key)
        {
            RULES.refuse_setting(path, woocommerce::CONSUMER_KEY, error.message(), errors);
        }
        // The store origin's own rules: `https`, because WooCommerce publishes
        // Basic authentication for HTTPS only, and no path, because an origin is
        // a scheme, a host, and a port.
        if let Ok(store) = required_setting(&instance.config, woocommerce::STORE_ORIGIN)
            && let Err(error) = woocommerce::validate_store_origin(store)
        {
            RULES.refuse_setting(path, woocommerce::STORE_ORIGIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let consumer_secret = resolve_secret_key(config).map_err(&invalid)?;
        let store = required_setting(config, woocommerce::STORE_ORIGIN)
            .map_err(&invalid)?
            .to_owned();
        let consumer_key = required_setting(config, woocommerce::CONSUMER_KEY)
            .map_err(&invalid)?
            .to_owned();
        // The origin is validated again here rather than trusted from
        // validation, because a compiled instance must never be able to reach a
        // store this connector would not have been allowed to authenticate to.
        woocommerce::validate_store_origin(&store)
            .map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = CrmRuntime::compile(
            context.connector.clone(),
            Credential::from_fields([
                ("secret", Secret::new(consumer_secret)),
                (woocommerce::CONSUMER_KEY, Secret::new(&consumer_key)),
            ]),
            &ConnectorConfiguration::from_deployment([(woocommerce::STORE_ORIGIN, store.as_str())]),
            woocommerce::decode,
            woocommerce::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// salesforce — a templated host and a stored OAuth2 credential
// ===========================================================================

pub(crate) mod salesforce_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: salesforce::NAME,
        // No `secret_key`, no `config.secrets`: the credential is the
        // source-local store's, written by `donat connector authorize`.
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[Key::required(salesforce::MY_DOMAIN)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        salesforce::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(my_domain) = required_setting(&instance.config, salesforce::MY_DOMAIN)
            && let Err(error) =
                salesforce::connector().resolve_origin(&ConnectorConfiguration::from_deployment([
                    (salesforce::MY_DOMAIN, my_domain),
                ]))
        {
            RULES.refuse_setting(path, salesforce::MY_DOMAIN, error.message(), errors);
        }
        // The scope set Salesforce publishes for this surface: "`api` — Allows
        // access to the current, logged-in user's account using APIs, such as
        // REST API", and one of the two spellings of the refresh grant spec
        // 011's stored credential needs.
        let Some(oauth2) = &instance.config.oauth2 else {
            return;
        };
        let declared = |scope: &str| oauth2.scopes.iter().any(|held| held == scope);
        if !declared(salesforce::API_SCOPE) {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.oauth2.scopes"),
                format!(
                    "the `{}` connector needs the `{}` scope, which Salesforce documents as \
                     \"Allows access to the current, logged-in user's account using APIs, such as \
                     REST API and Bulk API 2.0\"; add it and re-run `donat connector authorize`",
                    salesforce::NAME,
                    salesforce::API_SCOPE
                ),
            ));
        }
        if !salesforce::REFRESH_SCOPES.iter().copied().any(declared) {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.oauth2.scopes"),
                format!(
                    "the `{}` connector needs one of the scopes Salesforce publishes for a refresh \
                     token — `{}` or `{}` — because its credential is refreshed on use",
                    salesforce::NAME,
                    salesforce::REFRESH_SCOPES[0],
                    salesforce::REFRESH_SCOPES[1]
                ),
            ));
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let invalid = invalid_configuration(context.instance);
        let my_domain = required_setting(&context.instance.config, salesforce::MY_DOMAIN)
            .map_err(&invalid)?
            .to_owned();
        let runtime = CrmRuntime::compile(
            context.connector.clone(),
            // This connector configures no secret at all; the declared plan
            // refuses to render without the token one attempt is given.
            Credential::from_fields([]),
            &ConnectorConfiguration::from_deployment([(salesforce::MY_DOMAIN, my_domain.as_str())]),
            salesforce::decode,
            salesforce::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// zoho_crm — a data centre from a closed table, and a provider-published
// authorization scheme
// ===========================================================================

pub(crate) mod zoho_crm_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: zoho_crm::NAME,
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[Key::required(zoho_crm::REGION)],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// The region decides the origin, and it decides it from a compiled table
    /// rather than from a grammar: a deployment names one of the eight data
    /// centres Zoho publishes, and anything else has no declaration at all.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let region = required_setting(&instance.config, zoho_crm::REGION).ok()?;
        zoho_crm::connector(zoho_crm::Region::parse(region).ok()?).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        let Ok(configured) = required_setting(&instance.config, zoho_crm::REGION) else {
            return;
        };
        let region = match zoho_crm::Region::parse(configured) {
            Ok(region) => region,
            Err(error) => {
                RULES.refuse_setting(path, zoho_crm::REGION, error.message(), errors);
                return;
            }
        };
        // Zoho serves one org from one data centre, so a token minted at another
        // centre's accounts host does not authenticate here. The two halves of
        // one deployment's configuration are compared before a listener opens
        // rather than at the first refresh.
        if let Some(oauth2) = &instance.config.oauth2
            && !region.admits_token_endpoint(&oauth2.token_endpoint)
        {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.oauth2.token_endpoint"),
                format!(
                    "the `{}` connector is configured for the `{}` data centre, whose accounts \
                     origin Zoho publishes as `{}`; a token exchanged anywhere else does not \
                     authenticate against `{}`",
                    zoho_crm::NAME,
                    region.name(),
                    region.accounts_origin(),
                    region.api_origin()
                ),
            ));
        }
        // Zoho's scope grammar is "service_name.scope_name.operation_type", and
        // every scope this connector's operations need is a `ZohoCRM.` one.
        if let Some(oauth2) = &instance.config.oauth2
            && !oauth2
                .scopes
                .iter()
                .any(|scope| scope.starts_with(zoho_crm::SCOPE_PREFIX))
        {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.oauth2.scopes"),
                format!(
                    "the `{}` connector reads and writes CRM records, so its declared scopes must \
                     include at least one `{}` scope — Zoho publishes the format as \
                     `service_name.scope_name.operation_type`, for example \
                     `ZohoCRM.modules.deals.READ`",
                    zoho_crm::NAME,
                    zoho_crm::SCOPE_PREFIX
                ),
            ));
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let invalid = invalid_configuration(context.instance);
        let runtime = CrmRuntime::compile(
            context.connector.clone(),
            Credential::from_fields([]),
            // The origin is the declaration's own: a region-built connector
            // carries a fixed origin rather than a template to fill.
            &ConnectorConfiguration::default(),
            zoho_crm::decode,
            zoho_crm::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheme_of(connector: &Connector) -> Option<&'static str> {
        connector
            .credential()
            .plan()
            .and_then(AuthPlan::oauth2_authorization_scheme)
    }

    /// Every module in this batch answers the scheme its own declaration
    /// publishes, and exactly one of them answers something other than
    /// RFC 6750's.
    #[test]
    fn the_authorization_scheme_is_the_connectors_own() {
        assert_eq!(
            scheme_of(salesforce::connector()),
            Some(donat_connectors::sdk::BEARER_SCHEME)
        );
        let zoho = zoho_crm::declaration_shape().expect("the declaration shape is valid");
        assert_eq!(scheme_of(&zoho), Some(zoho_crm::AUTHORIZATION_SCHEME));
        assert_ne!(
            zoho_crm::AUTHORIZATION_SCHEME,
            donat_connectors::sdk::BEARER_SCHEME
        );
    }

    /// The two stored-OAuth2 modules apply an issued token; the four configured
    /// ones never accept one.
    #[test]
    fn only_the_stored_credential_modules_apply_an_applied_header() {
        assert!(scheme_of(salesforce::connector()).is_some());
        assert!(
            scheme_of(&zoho_crm::declaration_shape().expect("the declaration shape is valid"))
                .is_some()
        );
        assert!(scheme_of(pipedrive::connector()).is_none());
        assert!(scheme_of(freshdesk::connector()).is_none());
        assert!(
            scheme_of(&zendesk::declaration_shape().expect("the declaration shape is valid"))
                .is_none()
        );
        assert!(
            scheme_of(&woocommerce::declaration_shape().expect("the declaration shape is valid"))
                .is_none()
        );
    }
}
