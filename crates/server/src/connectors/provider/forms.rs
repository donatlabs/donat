//! The serving half of the forms and surveys connectors (spec 028, the forms
//! half).
//!
//! Everything module-specific — which operations exist, how a request renders,
//! what a response means — stays in `donat_connectors::providers`; this file
//! reads one instance's deploy-time configuration, resolves the `SecretRef`s it
//! names, and hands the module the values it declared.
//!
//! Two things are this half's own.
//!
//! **A region is a compiled table, not a template.** Jotform publishes three API
//! URLs and two of them spell a prefix in front of `api` rather than a label
//! under a constant suffix, which a templated host cannot produce. The
//! deployment names a region and the declaration is built from it, so a value
//! Jotform does not publish has no declaration at all — strictly narrower than a
//! grammar
//! ([[065-an-origin-is-a-label-a-table-or-a-whole-value-a-deployment-names]]).
//!
//! **The secret half and the non-secret half are configured apart.** The API key
//! or token is a `SecretRef` in `config.secret_key` and reaches only the auth
//! plan; the region is an ordinary `config.settings` value that names a public
//! origin and belongs in the configuration fingerprint, because a pinned
//! operation against the EU origin is not the same deployment as the same
//! operation against the US one (spec 028 §3).

use donat_connectors::providers::{acuity, cal_com, jotform, surveymonkey};
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

/// How one connector in this half reads a response it has already been told is
/// a success. Every module owns its own, because a provider that reports a
/// failure inside a `200` is the only one that knows where it says so.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled forms instance.
///
/// It holds the declaration it was validated against by value, because a
/// region-built declaration is one deployment's rather than a `&'static`
/// ([[048-a-declaration-a-deployment-completes]]).
struct FormsRuntime {
    connector: Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
}

impl FormsRuntime {
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

impl ProviderRuntime for FormsRuntime {
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
    /// No operation in this half declares an idempotency binding: no provider
    /// here publishes a mechanism at all. It is written through the keyed entry
    /// point anyway, because that is what makes the declaration the thing that
    /// decides rather than this file.
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

    /// The page gate is the module's own, because a provider here reports a
    /// failure inside a `200` and the status alone cannot say whether a page of
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
// jotform — a region from a closed table, and an `APIKEY` header
// ===========================================================================

pub(crate) mod jotform_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: jotform::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The region *is* the connector's origin, so it is deploy-time by
        // construction: an operation input that could reach it would be an
        // input choosing an authority.
        settings: &[Key::required(jotform::REGION)],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// The region decides the origin, and it decides it from a compiled table
    /// rather than from a grammar: a deployment names one of the three API URLs
    /// Jotform publishes, and anything else has no declaration at all.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let region = required_setting(&instance.config, jotform::REGION).ok()?;
        jotform::connector(jotform::Region::parse(region).ok()?).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(configured) = required_setting(&instance.config, jotform::REGION)
            && let Err(error) = jotform::Region::parse(configured)
        {
            RULES.refuse_setting(path, jotform::REGION, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let key = resolve_secret_key(config).map_err(&invalid)?;
        // Checked again here rather than trusted from validation: a compiled
        // instance must never be able to send an API key to an origin this
        // connector would not have been allowed to render against.
        let region = required_setting(config, jotform::REGION).map_err(&invalid)?;
        jotform::Region::parse(region).map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = FormsRuntime::compile(
            context.connector.clone(),
            Credential::secret(key),
            // The origin is the declaration's own: a region-built connector
            // carries a fixed origin rather than a template to fill.
            &ConnectorConfiguration::default(),
            jotform::decode,
            jotform::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// surveymonkey — a fixed origin, and a bearer access token
// ===========================================================================

pub(crate) mod surveymonkey_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: surveymonkey::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // SurveyMonkey's origin is a compiled constant and its access token is
        // the whole credential, so this instance configures no setting at all.
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        surveymonkey::connector()
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
        let runtime = FormsRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            &ConnectorConfiguration::default(),
            surveymonkey::decode,
            surveymonkey::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// cal_com — a fixed origin, a bearer API key, and a per-operation version pin
// ===========================================================================

pub(crate) mod cal_com_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: cal_com::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The origin, the paths and the three `cal-api-version` values are all
        // compiled constants of the declaration, so this instance configures
        // nothing but its credential.
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        cal_com::connector()
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
        let key = resolve_secret_key(&context.instance.config).map_err(&invalid)?;
        let runtime = FormsRuntime::compile(
            context.connector.clone(),
            Credential::secret(key),
            &ConnectorConfiguration::default(),
            cal_com::decode,
            cal_com::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// acuity — a numeric User ID as the HTTP Basic username, and an API key beside
// it that is the only secret
// ===========================================================================

pub(crate) mod acuity_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: acuity::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The User ID is the HTTP Basic *username*, which the auth plan carries
        // and no request may choose. It is not a secret: Acuity prints it in
        // its own settings screen beside the key, it identifies an account
        // rather than authenticating one, and it belongs in the configuration
        // fingerprint (spec 028 §3).
        settings: &[Key::required(acuity::USER_ID)],
        secrets: &[],
    };

    /// This instance's declaration.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let user_id = required_setting(&instance.config, acuity::USER_ID).ok()?;
        acuity::connector(user_id).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(user_id) = required_setting(&instance.config, acuity::USER_ID)
            && let Err(error) = acuity::validate_user_id(user_id)
        {
            RULES.refuse_setting(path, acuity::USER_ID, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let key = resolve_secret_key(config).map_err(&invalid)?;
        let user_id = required_setting(config, acuity::USER_ID).map_err(&invalid)?;
        // Checked again here rather than trusted from validation: a compiled
        // instance must never send an API key under a username this connector
        // would not have been allowed to declare.
        acuity::validate_user_id(user_id).map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = FormsRuntime::compile(
            context.connector.clone(),
            // The User ID is a *declared* credential field as well as the plan's
            // compiled username, so startup can answer "is this credential
            // complete" by name rather than at the first `401`.
            Credential::from_fields([
                ("secret", Secret::new(key)),
                (acuity::USER_ID, Secret::new(user_id)),
            ]),
            &ConnectorConfiguration::default(),
            acuity::decode,
            acuity::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The region a deployment names is a compiled table, and every row of it
    /// resolves to the origin Jotform publishes for that region.
    #[test]
    fn the_jotform_declaration_is_built_from_its_configured_region() {
        for (named, origin) in [
            ("us", "https://api.jotform.com/"),
            ("eu", "https://eu-api.jotform.com/"),
            ("hipaa", "https://hipaa-api.jotform.com/"),
        ] {
            let region = jotform::Region::parse(named).expect("a published region");
            let connector = jotform::connector(region).expect("a published region declares");
            assert_eq!(
                connector
                    .resolve_origin(&ConnectorConfiguration::default())
                    .expect("a fixed origin resolves")
                    .as_url()
                    .as_str(),
                origin
            );
        }
        for hostile in ["", "US", "api.jotform.com", "https://attacker.invalid"] {
            assert!(
                jotform::Region::parse(hostile).is_err(),
                "region `{hostile}` must not resolve"
            );
        }
    }
}
