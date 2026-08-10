//! The serving half of the scheduling and people connectors (spec 028).
//!
//! Everything module-specific — which operations exist, how a request renders,
//! what a response means — stays in `donat_connectors::providers`; this file
//! reads one instance's deploy-time configuration, resolves its `SecretRef`s,
//! and hands the module the values it declared.
//!
//! **What is different here is the split between a secret and a configured
//! identity.** Harvest sends two values on every request: a Personal Access
//! Token, which is a secret and reaches the wire only through
//! `AuthPlan::bearer`, and an account identifier, which is not one. The account
//! id is `config.settings` material, so it enters the instance's configuration
//! fingerprint — changing which account a pinned operation reaches changes what
//! that operation *is* — while the token contributes only the *name* of the
//! environment variable behind it. That asymmetry is the whole point of spec
//! 028 §3, and it is the shape Twilio's Account SID established
//! ([[048-a-declaration-a-deployment-completes]]).
//!
//! Because both values are compiled into the operations themselves — the
//! account into a `Harvest-Account-Id` header and the identity Harvest demands
//! into `User-Agent` — the declaration is one a deployment completes, exactly as
//! Basecamp's path prefix is
//! ([[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]]).

use donat_connectors::providers::{bamboohr, clockify, eventbrite, harvest};
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
/// a success. It is the module's own, because only the module knows whether its
/// provider can report a failure inside a `2xx`.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled scheduling-or-people instance.
///
/// It holds the declaration by value, because these declarations are ones a
/// deployment completes rather than `&'static` constants.
struct PeopleRuntime {
    connector: Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
}

impl PeopleRuntime {
    fn compile(
        connector: Connector,
        credential: Credential,
        configuration: &ConnectorConfiguration,
        decode: Decoder,
        pagination: PaginationLookup,
    ) -> Result<Self, String> {
        // One origin per instance, resolved once. Harvest's is a compile-time
        // constant — its per-account part is a header, not a host — and
        // BambooHR's is one lowercase company label filled from this
        // deployment's configuration and from nothing else.
        let origin = connector
            .resolve_origin(configuration)
            .map_err(|error| error.message().to_owned())?;
        // Startup answers "is this credential complete" once, by name, before a
        // listener opens. It is the check that catches an instance which
        // resolved a token but was never configured with an account.
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

    fn operation(&self, id: &str) -> Result<&Operation, ConnectorFailure> {
        self.connector.operation(id).ok_or_else(|| {
            ConnectorFailure::invariant("connector operation is not compiled into this binary")
        })
    }
}

impl ProviderRuntime for PeopleRuntime {
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

    /// Render one request, with the activity's stable key bound where the
    /// operation's own class declared one.
    ///
    /// No operation in this batch declares an idempotency binding, so this is
    /// exactly `plan_request` today. It is written through the keyed entry point
    /// anyway, because that is what makes the declaration the thing that
    /// decides ([[070-a-declared-idempotency-key-is-written-by-the-executor-and-a-window-is-a-startup-check]]).
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
}

// ===========================================================================
// harvest — a bearer token that is a secret, beside an account id that is not
// ===========================================================================

pub(crate) mod harvest_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: harvest::NAME,
        // One `config.secret_key`: the Personal Access Token, and nothing else.
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // Neither of these is a secret. The account id is what Harvest prints
        // beside the token it issues and it selects which account a request
        // reaches; the user agent identifies this deployment to the provider and
        // Harvest answers `400` without it. Both are compiled into every
        // operation's headers, so an input that could reach either would be an
        // input choosing a tenant or an identity.
        settings: &[
            Key::required(harvest::ACCOUNT_ID),
            Key::required(harvest::USER_AGENT),
        ],
        secrets: &[],
    };

    /// This instance's declaration.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let account_id = required_setting(&instance.config, harvest::ACCOUNT_ID).ok()?;
        let user_agent = required_setting(&instance.config, harvest::USER_AGENT).ok()?;
        harvest::connector(account_id, user_agent).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(account_id) = required_setting(&instance.config, harvest::ACCOUNT_ID)
            && let Err(error) = harvest::validate_account_id(account_id)
        {
            RULES.refuse_setting(path, harvest::ACCOUNT_ID, error.message(), errors);
        }
        if let Ok(user_agent) = required_setting(&instance.config, harvest::USER_AGENT)
            && let Err(error) = harvest::validate_user_agent(user_agent)
        {
            RULES.refuse_setting(path, harvest::USER_AGENT, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        // Both settings are validated again here rather than trusted from
        // validation, because a compiled instance must never be able to name an
        // account this connector would not have been allowed to declare.
        let account_id = required_setting(config, harvest::ACCOUNT_ID).map_err(&invalid)?;
        let user_agent = required_setting(config, harvest::USER_AGENT).map_err(&invalid)?;
        harvest::connector(account_id, user_agent)
            .map_err(|error| invalid(error.message().to_owned()))?;
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let runtime = PeopleRuntime::compile(
            context.connector.clone(),
            // The declared credential contract names both fields: the secret the
            // plan spends, and the non-secret account the declaration published
            // so that startup can refuse an instance that configured neither.
            Credential::from_fields([
                ("secret", Secret::new(token)),
                (harvest::ACCOUNT_ID, Secret::new(account_id)),
            ]),
            // Harvest's origin is fixed, so it needs no configuration.
            &ConnectorConfiguration::default(),
            harvest::decode,
            harvest::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// bamboohr — the API key in the HTTP Basic username, under a company host label
// ===========================================================================

pub(crate) mod bamboohr_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: bamboohr::NAME,
        // One `config.secret_key`: the API key. BambooHR's password is the
        // constant `x` its own example sends, so it is compiled into the plan
        // and is not a value any deployment binds.
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The company subdomain is this connector's *host*, so it is deploy-time
        // by construction: an operation input that could reach it would be an
        // input choosing an authority.
        settings: &[Key::required(bamboohr::COMPANY_DOMAIN)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        bamboohr::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // The templated host's own grammar, checked at deploy time rather than
        // at the first activity attempt.
        if let Ok(company) = required_setting(&instance.config, bamboohr::COMPANY_DOMAIN)
            && let Err(error) = bamboohr::connector().resolve_origin(
                &ConnectorConfiguration::from_deployment([(bamboohr::COMPANY_DOMAIN, company)]),
            )
        {
            RULES.refuse_setting(path, bamboohr::COMPANY_DOMAIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let api_key = resolve_secret_key(config).map_err(&invalid)?;
        let company = required_setting(config, bamboohr::COMPANY_DOMAIN)
            .map_err(&invalid)?
            .to_owned();
        let runtime = PeopleRuntime::compile(
            context.connector.clone(),
            // The one field the plan reads. BambooHR spends it as the HTTP
            // Basic *username*, which is why the declaration carries no
            // username of its own.
            Credential::secret(api_key),
            &ConnectorConfiguration::from_deployment([(
                bamboohr::COMPANY_DOMAIN,
                company.as_str(),
            )]),
            bamboohr::decode,
            bamboohr::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// clockify — a configured key, and a workspace compiled into every scoped path
// ===========================================================================

pub(crate) mod clockify_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: clockify::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The workspace is the first scoped segment of every path this
        // connector renders, so it is deploy-time by construction: an operation
        // input that could reach it would be an input choosing a tenant. It is
        // not a secret — Clockify prints it in its own web URLs — which is why
        // it is a `setting` rather than a `config.secrets` entry.
        settings: &[Key::required(clockify::WORKSPACE_ID)],
        secrets: &[],
    };

    /// This instance's declaration, with the workspace compiled into its paths.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let workspace = required_setting(&instance.config, clockify::WORKSPACE_ID).ok()?;
        clockify::connector(workspace).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(workspace) = required_setting(&instance.config, clockify::WORKSPACE_ID)
            && let Err(error) = clockify::validate_workspace_id(workspace)
        {
            RULES.refuse_setting(path, clockify::WORKSPACE_ID, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        // Validated again here rather than trusted from validation, because a
        // compiled instance must never be able to render a path into a
        // workspace this connector would not have been allowed to declare.
        let workspace = required_setting(config, clockify::WORKSPACE_ID).map_err(&invalid)?;
        clockify::connector(workspace).map_err(|error| invalid(error.message().to_owned()))?;
        let api_key = resolve_secret_key(config).map_err(&invalid)?;
        let runtime = PeopleRuntime::compile(
            context.connector.clone(),
            Credential::from_fields([
                ("secret", Secret::new(api_key)),
                (clockify::WORKSPACE_ID, Secret::new(workspace)),
            ]),
            // Clockify's origin is fixed; its per-tenant part is in the path.
            &ConnectorConfiguration::default(),
            clockify::decode,
            clockify::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// eventbrite — a private token, and an organization compiled into two paths
// ===========================================================================

pub(crate) mod eventbrite_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: eventbrite::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The organization is a path segment of the event collection and the
        // event create, so it is deploy-time by construction: an input that
        // could reach it would be an input choosing whose events to create. It
        // is not a secret — Eventbrite prints it in its own organizer URLs.
        settings: &[Key::required(eventbrite::ORGANIZATION_ID)],
        secrets: &[],
    };

    /// This instance's declaration, with the organization compiled into its
    /// two scoped paths.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let organization = required_setting(&instance.config, eventbrite::ORGANIZATION_ID).ok()?;
        eventbrite::connector(organization).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(organization) = required_setting(&instance.config, eventbrite::ORGANIZATION_ID)
            && let Err(error) = eventbrite::validate_organization_id(organization)
        {
            RULES.refuse_setting(path, eventbrite::ORGANIZATION_ID, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let organization =
            required_setting(config, eventbrite::ORGANIZATION_ID).map_err(&invalid)?;
        eventbrite::connector(organization).map_err(|error| invalid(error.message().to_owned()))?;
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let runtime = PeopleRuntime::compile(
            context.connector.clone(),
            Credential::from_fields([
                ("secret", Secret::new(token)),
                (eventbrite::ORGANIZATION_ID, Secret::new(organization)),
            ]),
            // Eventbrite's origin is fixed; its per-tenant part is in the path.
            &ConnectorConfiguration::default(),
            eventbrite::decode,
            eventbrite::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Harvest's account id and identity are both refused at deploy time, which
    /// is what keeps a value its provider would not accept out of a compiled
    /// instance.
    #[test]
    fn the_eventbrite_declaration_refuses_an_organization_its_provider_would() {
        assert!(eventbrite::connector("123456789012").is_ok());
        assert!(eventbrite::connector("123456789012/../0").is_err());
        assert!(eventbrite::connector("12345678901a").is_err());
        assert!(eventbrite::connector("").is_err());
    }

    #[test]
    fn the_clockify_declaration_refuses_a_workspace_its_provider_would() {
        assert!(clockify::connector("64a687e29ae1f428e7ebe303").is_ok());
        assert!(clockify::connector("64a687e29ae1f428e7ebe303/../deadbeef").is_err());
        assert!(clockify::connector("64A687E29AE1F428E7EBE303").is_err());
        assert!(clockify::connector("").is_err());
    }

    #[test]
    fn the_harvest_declaration_refuses_a_value_its_provider_would() {
        assert!(harvest::connector("1234567", "Donat (ops@example.test)").is_ok());
        assert!(harvest::connector("12345/67", "Donat (ops@example.test)").is_err());
        assert!(harvest::connector("", "Donat (ops@example.test)").is_err());
        assert!(harvest::connector("1234567", "Donat").is_err());
    }

    /// BambooHR's credential is the one wire form its provider publishes: the
    /// key is the HTTP Basic *username*, so the declaration carries no username
    /// and the plan reads the same `secret` field every other plan does.
    #[test]
    fn the_bamboohr_credential_is_the_basic_username_and_never_the_declaration() {
        let plan = bamboohr::connector()
            .credential()
            .plan()
            .expect("bamboohr declares a credential plan");
        assert_eq!(plan.required_fields(), ["secret"]);
        // Exactly one connector in this half of the batch declares the
        // secret-username plan, and neither applies a stored OAuth2 token.
        for connector in [
            bamboohr::connector(),
            &harvest::declaration_shape().expect("the declaration shape is valid"),
        ] {
            assert!(
                connector
                    .credential()
                    .plan()
                    .and_then(AuthPlan::oauth2_authorization_scheme)
                    .is_none()
            );
        }
    }

    /// The declared credential contract says which half is a secret, and the
    /// compiled declaration carries neither value.
    #[test]
    fn the_harvest_credential_names_one_secret_and_one_configured_identity() {
        let connector = harvest::declaration_shape().expect("the declaration shape is valid");
        let fields = connector.credential().fields();
        assert_eq!(fields.len(), 2, "{fields:?}");
        assert!(
            fields
                .iter()
                .any(|field| field.name() == "secret" && field.is_secret())
        );
        assert!(
            fields
                .iter()
                .any(|field| field.name() == harvest::ACCOUNT_ID && !field.is_secret())
        );
        // This connector authenticates with a configured key, so it never
        // accepts an applied stored `Authorization` header (ADR 043).
        assert!(
            connector
                .credential()
                .plan()
                .and_then(AuthPlan::oauth2_authorization_scheme)
                .is_none()
        );
    }
}
