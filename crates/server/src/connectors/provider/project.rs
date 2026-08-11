//! The serving half of the project-tracking and collaboration connectors
//! (spec 024).
//!
//! Six modules in one file, because they share one runtime and differ only in
//! their deploy-time configuration. Everything module-specific — which
//! operations exist, how a request renders, what a response means — stays in
//! `donat_connectors::providers`; this file reads one instance's configuration,
//! resolves its `SecretRef`s, and hands the module the values it declared.
//!
//! Three things are different from `provider/crm.rs` and each is this batch's
//! own.
//!
//! **One credential is two secrets.** Trello's key names the application and its
//! token names the authorization, and neither authenticates alone, so its
//! instance resolves two `config.secrets` entries into the two fields
//! `AuthPlan::api_key_query_pair` reads
//! ([[066-a-credential-can-be-two-query-parameters-and-an-account-is-a-compiled-path-prefix]]).
//!
//! **One declaration carries a path prefix rather than a host.** Every Basecamp
//! URL is `https://3.basecampapi.com/{account_id}/…`, so its declaration is
//! built per deployment with the account id compiled into every path and the
//! `User-Agent` Basecamp demands compiled into every request. Both are checked
//! here, at deploy time, so a mistyped value is a startup refusal rather than a
//! `400` on the first activity attempt.
//!
//! **Two providers report a failure inside a `2xx`.** monday answers an
//! application-level error with a `200` and a GraphQL `errors` array, so every
//! module in this batch owns its `decode` and the runtime calls it — the same
//! seam Batch G uses, for the same reason
//! ([[056-a-scope-is-a-property-of-an-operation-and-a-success-envelope-can-carry-a-failure]]).

use donat_connectors::providers::{asana, basecamp, clickup, monday, todoist, trello};
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
    resolve_secret, resolve_secret_key,
};
use crate::connectors::{ConnectorRegistryError, ModuleContext, RegisteredConnector};
use crate::state::ConnectorConfigError;

/// How one connector in this batch reads a response it has already been told is
/// a success. Every module owns its own, because one of the six answers a
/// failure inside a `200` and only the module knows where it says so.
type Decoder = fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>;

/// One compiled project-tracking instance.
///
/// It holds the declaration it was validated against by value, because one of
/// the six is completed by a deployment — Basecamp's account id and user
/// agent — exactly as Twilio's and Jira's are
/// ([[048-a-declaration-a-deployment-completes]]).
struct ProjectRuntime {
    connector: Connector,
    origin: Origin,
    credential: Credential,
    decode: Decoder,
    pagination: PaginationLookup,
    /// Whether this module's credential is the source-local OAuth2 store's.
    stored_oauth2: bool,
}

impl ProjectRuntime {
    fn compile(
        connector: Connector,
        credential: Credential,
        decode: Decoder,
        pagination: PaginationLookup,
    ) -> Result<Self, String> {
        // Every connector in this batch has a fixed origin: five publish one
        // host, and Basecamp puts its per-account part in the path rather than
        // in the authority.
        let origin = connector
            .resolve_origin(&ConnectorConfiguration::default())
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

impl ProviderRuntime for ProjectRuntime {
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
    /// No operation in this batch declares an idempotency binding — the one
    /// provider that publishes a key publishes an escape clause with it
    /// ([[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]]) —
    /// so this is exactly `plan_request` today. It is written through the keyed
    /// entry point anyway, because that is what makes the declaration the thing
    /// that decides, rather than this file.
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
// asana — one personal access token, one fixed origin
// ===========================================================================

pub(crate) mod asana_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: asana::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        asana::connector()
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
        let runtime = ProjectRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            asana::decode,
            asana::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// trello — the batch's two-secret credential, both halves on the query string
// ===========================================================================

pub(crate) mod trello_module {
    use super::*;

    /// The two `config.secrets` entries this module reads.
    ///
    /// They are `secrets` rather than `settings` because both are credentials:
    /// Trello's key identifies the application to the provider and its token
    /// identifies the authorization, and a deployment that published either in
    /// plain metadata would have published a credential.
    const API_KEY: &str = "api_key";
    const API_TOKEN: &str = "api_token";

    const RULES: ProviderRules = ProviderRules {
        module: trello::NAME,
        credential: CredentialShape::NamedSecrets,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[Key::required(API_KEY), Key::required(API_TOKEN)],
    };

    pub(crate) fn connector() -> &'static Connector {
        trello::connector()
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
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let api_key = resolve_secret(config, API_KEY).map_err(&invalid)?;
        let api_token = resolve_secret(config, API_TOKEN).map_err(&invalid)?;
        let runtime = ProjectRuntime::compile(
            context.connector.clone(),
            // The plan reads `api_key` and `secret`; the second name is the
            // SDK's for "the credential this plan's owner resolved", and the
            // deployment spells it `api_token` because Trello does.
            Credential::from_fields([
                ("api_key", Secret::new(api_key)),
                ("secret", Secret::new(api_token)),
            ]),
            trello::decode,
            trello::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// clickup — one personal token, sent as the whole `Authorization` value
// ===========================================================================

pub(crate) mod clickup_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: clickup::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        clickup::connector()
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
        let runtime = ProjectRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            clickup::decode,
            clickup::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// monday — GraphQL, and a body gate over a `200`
// ===========================================================================

pub(crate) mod monday_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: monday::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        monday::connector()
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
        let runtime = ProjectRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            monday::decode,
            monday::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// todoist — one bearer token, one fixed origin
// ===========================================================================

pub(crate) mod todoist_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: todoist::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        todoist::connector()
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
        let runtime = ProjectRuntime::compile(
            context.connector.clone(),
            Credential::secret(token),
            todoist::decode,
            todoist::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// basecamp — an account id compiled into every path, and a mandatory identity
// ===========================================================================

pub(crate) mod basecamp_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: basecamp::NAME,
        // No `secret_key`, no `config.secrets`: the credential is the
        // source-local store's, written by `donat connector authorize`.
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        // The account id is the first segment of every path this connector
        // renders and the user agent is on every request, so both are
        // deploy-time by construction: an operation input that could reach
        // either would be an input choosing an account or an identity.
        settings: &[
            Key::required(basecamp::ACCOUNT_ID),
            Key::required(basecamp::USER_AGENT),
        ],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// `basecamp::connector` takes both values because both are compiled into
    /// the operations themselves: the account into every path, the user agent
    /// into every request's headers.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let account_id = required_setting(&instance.config, basecamp::ACCOUNT_ID).ok()?;
        let user_agent = required_setting(&instance.config, basecamp::USER_AGENT).ok()?;
        basecamp::connector(account_id, user_agent).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Ok(account_id) = required_setting(&instance.config, basecamp::ACCOUNT_ID)
            && let Err(error) = basecamp::validate_account_id(account_id)
        {
            RULES.refuse_setting(path, basecamp::ACCOUNT_ID, error.message(), errors);
        }
        if let Ok(user_agent) = required_setting(&instance.config, basecamp::USER_AGENT)
            && let Err(error) = basecamp::validate_user_agent(user_agent)
        {
            RULES.refuse_setting(path, basecamp::USER_AGENT, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        // Both values are validated again here rather than trusted from
        // validation, because a compiled instance must never be able to reach an
        // account this connector would not have been allowed to render a path
        // for.
        let account_id = required_setting(config, basecamp::ACCOUNT_ID).map_err(&invalid)?;
        let user_agent = required_setting(config, basecamp::USER_AGENT).map_err(&invalid)?;
        basecamp::connector(account_id, user_agent)
            .map_err(|error| invalid(error.message().to_owned()))?;
        let runtime = ProjectRuntime::compile(
            context.connector.clone(),
            // This connector configures no secret at all; the declared plan
            // refuses to render without the token one attempt is given.
            Credential::from_fields([]),
            basecamp::decode,
            basecamp::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly one module in this batch reads a stored OAuth2 credential, and
    /// the other five never accept an applied `Authorization` header (ADR 043).
    #[test]
    fn only_basecamp_applies_a_stored_credential() {
        fn scheme_of(connector: &Connector) -> Option<&'static str> {
            connector
                .credential()
                .plan()
                .and_then(AuthPlan::oauth2_authorization_scheme)
        }

        let basecamp = basecamp::declaration_shape().expect("the declaration shape is valid");
        assert_eq!(
            scheme_of(&basecamp),
            Some(donat_connectors::sdk::BEARER_SCHEME),
            "Basecamp publishes RFC 6750's scheme and nothing of its own"
        );
        for connector in [
            asana::connector(),
            trello::connector(),
            clickup::connector(),
            monday::connector(),
            todoist::connector(),
        ] {
            assert!(scheme_of(connector).is_none());
        }
    }

    /// Trello's declared credential is two fields and neither is declaration
    /// material, so the compiled contract names both without carrying either.
    #[test]
    fn the_trello_credential_names_two_secrets_and_carries_neither() {
        let plan = trello::connector()
            .credential()
            .plan()
            .expect("Trello declares a credential plan");
        assert_eq!(plan.required_fields(), ["api_key", "secret"]);
        let printed = format!("{:?}", trello::connector().credential());
        assert!(printed.contains("api_key"), "{printed}");
        assert!(printed.contains("secret"), "{printed}");
    }

    /// Basecamp's account prefix and identity are both refused at deploy time,
    /// which is what keeps them out of a compiled instance.
    #[test]
    fn the_basecamp_declaration_refuses_a_value_its_provider_would() {
        assert!(basecamp::connector("999999999", "Donat (ops@example.test)").is_ok());
        assert!(basecamp::connector("9999/9999", "Donat (ops@example.test)").is_err());
        assert!(basecamp::connector("999999999", "Donat").is_err());
    }
}
