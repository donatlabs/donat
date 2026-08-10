//! The serving half of the development and monitoring connectors (spec 027).
//!
//! Six modules in one file, because they share one runtime and differ only in
//! their deploy-time configuration. Everything module-specific — which
//! operations exist, how a request renders, what a response means — stays in
//! `donat_connectors::providers`; this file reads one instance's configuration,
//! resolves the `SecretRef`s it names, and hands the module the values it
//! declared.
//!
//! Four things are different from `provider/crm.rs` and `provider/project.rs`,
//! and each is this batch's own.
//!
//! **Two origins are the deployment's own instance.** GitLab and Grafana are
//! usually run by the deployment rather than by a vendor, so each names a whole
//! origin rather than a label under a constant suffix
//! ([[082-an-instance-a-deployment-operates-is-a-whole-origin-it-names]]). Both
//! are validated here, at deploy time — `https` only, no path — so a mistyped or
//! unsafe instance is a startup refusal rather than a bearer token on a cleartext
//! connection.
//!
//! **One credential is an `Authorization` authentication parameter.** PagerDuty
//! publishes "The API Key with format `Token token=<API_KEY>`", which is RFC
//! 9110's `auth-param` production; the SDK gained one plan for it and exactly one
//! connector declares it
//! ([[081-a-credential-is-an-authentication-parameter-and-a-body-credential-is-a-version-that-was-superseded]]).
//!
//! **Two declarations are completed by a deployment.** PagerDuty's `From`
//! address is the account user every write is attributed to, and Bitbucket's
//! Atlassian account address is the HTTP Basic username; both are compiled where
//! the declaration is built, so neither is reachable from operation input
//! ([[048-a-declaration-a-deployment-completes]]).
//!
//! **One provider reports a failure inside a `2xx`.** Cloudflare's envelope
//! carries `success`, constrained to `true` on its documented success schema, so
//! every module in this batch owns its `decode` and the runtime calls it — the
//! same seam Batch G uses, for the same reason
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).

use donat_connectors::providers::{bitbucket, cloudflare, gitlab, grafana, pagerduty, uptimerobot};
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
/// a success. Every module owns its own, because one of the six answers a
/// failure inside a `200` and only the module knows where it says so.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled development or monitoring instance.
///
/// It holds the declaration it was validated against by value, because two of
/// the six are completed by one deployment — an account address, a `From`
/// identity — exactly as Twilio's and Jira's are
/// ([[048-a-declaration-a-deployment-completes]]).
struct DevOpsRuntime {
    connector: Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
    /// Whether this module's credential is the source-local OAuth2 store's.
    stored_oauth2: bool,
}

impl DevOpsRuntime {
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

impl ProviderRuntime for DevOpsRuntime {
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
    /// operation's own class declared one.
    ///
    /// No operation in this batch declares an idempotency binding: the one
    /// provider that publishes a deduplication key on a declared endpoint —
    /// PagerDuty's `incident_key` — publishes a rejection rather than an
    /// absorption and no retention at all
    /// ([[080-a-deduplication-that-lapses-when-the-incident-is-resolved-is-not-a-retention]]).
    /// It is written through the keyed entry point anyway, because that is what
    /// makes the declaration the thing that decides, rather than this file.
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

    /// The page gate is the module's own, because one of these providers reports
    /// a failure inside a `200` and the status alone cannot say whether a page of
    /// a walk is a page at all.
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
// gitlab — the deployment's own instance, and a bearer personal access token
// ===========================================================================

pub(crate) mod gitlab_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: gitlab::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The instance is the connector's *origin*, so it is deploy-time by
        // construction: an operation input that could reach it would be an
        // input choosing an authority.
        settings: &[Key::required(gitlab::INSTANCE_ORIGIN)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        gitlab::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(origin) = required_setting(&instance.config, gitlab::INSTANCE_ORIGIN)
            && let Err(error) = gitlab::validate_instance_origin(origin)
        {
            RULES.refuse_setting(path, gitlab::INSTANCE_ORIGIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let origin = required_setting(config, gitlab::INSTANCE_ORIGIN)
            .map_err(&invalid)?
            .to_owned();
        // Checked again here rather than trusted from validation: a compiled
        // instance must never be able to send a bearer token to an origin this
        // connector would not have been allowed to render against.
        gitlab::validate_instance_origin(&origin)
            .map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = DevOpsRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            &ConnectorConfiguration::from_deployment([(gitlab::INSTANCE_ORIGIN, origin.as_str())]),
            gitlab::decode,
            gitlab::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// grafana — the deployment's own instance, and a service account token
// ===========================================================================

pub(crate) mod grafana_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: grafana::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[Key::required(grafana::INSTANCE_ORIGIN)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        grafana::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(origin) = required_setting(&instance.config, grafana::INSTANCE_ORIGIN)
            && let Err(error) = grafana::validate_instance_origin(origin)
        {
            RULES.refuse_setting(path, grafana::INSTANCE_ORIGIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let origin = required_setting(config, grafana::INSTANCE_ORIGIN)
            .map_err(&invalid)?
            .to_owned();
        grafana::validate_instance_origin(&origin)
            .map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = DevOpsRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            &ConnectorConfiguration::from_deployment([(grafana::INSTANCE_ORIGIN, origin.as_str())]),
            grafana::decode,
            grafana::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// bitbucket — a fixed origin, and the Atlassian account address as the Basic
// username
// ===========================================================================

pub(crate) mod bitbucket_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: bitbucket::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[Key::required(bitbucket::ACCOUNT_EMAIL)],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// `bitbucket::connector` takes the address because it is the HTTP Basic
    /// username, which the auth plan carries and no request may choose.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let account_email = required_setting(&instance.config, bitbucket::ACCOUNT_EMAIL).ok()?;
        bitbucket::connector(account_email).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(account_email) = required_setting(&instance.config, bitbucket::ACCOUNT_EMAIL)
            && let Err(error) = bitbucket::validate_account_email(account_email)
        {
            RULES.refuse_setting(path, bitbucket::ACCOUNT_EMAIL, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let account_email = required_setting(config, bitbucket::ACCOUNT_EMAIL).map_err(&invalid)?;
        bitbucket::connector(account_email).map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = DevOpsRuntime::compile(
            context.connector.clone(),
            // The address is a *declared* credential field as well as the plan's
            // compiled username, so startup can answer "is this credential
            // complete" by name rather than at the first `401`.
            Credential::from_fields([
                ("secret", Secret::new(token)),
                (bitbucket::ACCOUNT_EMAIL, Secret::new(account_email)),
            ]),
            &ConnectorConfiguration::default(),
            bitbucket::decode,
            bitbucket::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// pagerduty — an authentication parameter, and a compiled `From` identity
// ===========================================================================

pub(crate) mod pagerduty_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: pagerduty::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // Every write is attributed to this account user, so it is deploy-time
        // by construction: an operation input that could reach it would be a
        // Process choosing whom to act as.
        settings: &[Key::required(pagerduty::FROM_EMAIL)],
        secrets: &[],
    };

    /// This instance's declaration.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let from_email = required_setting(&instance.config, pagerduty::FROM_EMAIL).ok()?;
        pagerduty::connector(from_email).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(from_email) = required_setting(&instance.config, pagerduty::FROM_EMAIL)
            && let Err(error) = pagerduty::validate_from_email(from_email)
        {
            RULES.refuse_setting(path, pagerduty::FROM_EMAIL, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let key = resolve_secret_key(config).map_err(&invalid)?;
        let from_email = required_setting(config, pagerduty::FROM_EMAIL).map_err(&invalid)?;
        pagerduty::connector(from_email).map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = DevOpsRuntime::compile(
            context.connector.clone(),
            Credential::from_fields([
                ("secret", Secret::new(key)),
                (pagerduty::FROM_EMAIL, Secret::new(from_email)),
            ]),
            &ConnectorConfiguration::default(),
            pagerduty::decode,
            pagerduty::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// uptimerobot — one bearer API token against the v3 surface
// ===========================================================================

pub(crate) mod uptimerobot_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: uptimerobot::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        uptimerobot::connector()
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
        let runtime = DevOpsRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            &ConnectorConfiguration::default(),
            uptimerobot::decode,
            uptimerobot::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// cloudflare — one bearer API token, and a body gate over a `200`
// ===========================================================================

pub(crate) mod cloudflare_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: cloudflare::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        cloudflare::connector()
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
        let runtime = DevOpsRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            &ConnectorConfiguration::default(),
            cloudflare::decode,
            cloudflare::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use donat_connectors::sdk::EffectClass;

    /// No module in this batch reads a stored OAuth2 credential: every one of
    /// the six is a deploy-time key, so none of them ever accepts an applied
    /// `Authorization` header from the credential lifecycle (ADR 043).
    #[test]
    fn no_devops_module_applies_a_stored_credential() {
        fn scheme_of(connector: &Connector) -> Option<&'static str> {
            connector
                .credential()
                .plan()
                .and_then(AuthPlan::oauth2_authorization_scheme)
        }

        for connector in [
            gitlab::connector(),
            grafana::connector(),
            uptimerobot::connector(),
            cloudflare::connector(),
        ] {
            assert!(scheme_of(connector).is_none());
        }
        for connector in [
            bitbucket::declaration_shape().expect("the declaration shape is valid"),
            pagerduty::declaration_shape().expect("the declaration shape is valid"),
        ] {
            assert!(scheme_of(&connector).is_none());
        }
    }

    /// PagerDuty is the only connector in the workspace whose credential is an
    /// `Authorization` authentication parameter, and its declared contract names
    /// one secret field without carrying it.
    #[test]
    fn only_pagerduty_declares_an_authorization_parameter() {
        let pagerduty = pagerduty::declaration_shape().expect("the declaration shape is valid");
        let plan = pagerduty
            .credential()
            .plan()
            .expect("PagerDuty declares a credential plan");
        assert_eq!(
            *plan,
            AuthPlan::api_key_authorization_parameter("Token", "token")
                .expect("the published form is valid")
        );
        assert_eq!(plan.required_fields(), ["secret"]);

        let others = crate::connectors::connectors()
            .into_iter()
            .filter(|connector| {
                connector.credential().plan()
                    == Some(
                        &AuthPlan::api_key_authorization_parameter("Token", "token")
                            .expect("valid"),
                    )
            })
            .count();
        assert_eq!(
            others, 0,
            "no `&'static` declaration carries this plan; PagerDuty's is per-deployment"
        );
    }

    /// The two declarations a deployment completes refuse a value their provider
    /// would, at the place a deployment configures them.
    #[test]
    fn the_completed_declarations_refuse_a_value_their_provider_would() {
        assert!(bitbucket::connector("ci@example.test").is_ok());
        assert!(bitbucket::connector("ci@example").is_err());
        assert!(bitbucket::connector("ci:token@example.test").is_err());

        assert!(pagerduty::connector("oncall@example.test").is_ok());
        assert!(pagerduty::connector("oncall").is_err());
        assert!(pagerduty::connector("on call@example.test").is_err());
    }

    /// Spec 027 §3: a pipeline trigger, an incident create and a monitor pause
    /// all look like small requests and each has a real consequence, so none of
    /// them is classified `ReadOnly` — asserted here across the batch as well as
    /// in each connector's own test.
    #[test]
    fn devops_triggering_is_not_a_read() {
        let acting = [
            (gitlab::connector(), "pipeline.trigger"),
            (uptimerobot::connector(), "monitor.pause"),
            (cloudflare::connector(), "dns_record.update"),
            (grafana::connector(), "alert_rule.update"),
        ];
        for (connector, id) in acting {
            let class = connector
                .operation(id)
                .unwrap_or_else(|| panic!("{id} is declared"))
                .effect_class()
                .expect("every operation carries a class");
            assert_ne!(class, EffectClass::ReadOnly, "{id}");
        }
        let pagerduty = pagerduty::declaration_shape().expect("the declaration shape is valid");
        for id in ["incident.create", "incident_note.create", "incident.update"] {
            assert_ne!(
                pagerduty
                    .operation(id)
                    .expect("declared")
                    .effect_class()
                    .expect("a class"),
                EffectClass::ReadOnly,
                "{id}"
            );
        }
    }
}
