//! The serving half of the storage and messaging connectors (spec 025).
//!
//! Seven modules in one file, because they share one runtime and differ only in
//! their deploy-time configuration. Everything module-specific — which
//! operations exist, how a request renders, what a response means — stays in
//! `donat_connectors::providers`; this file reads one instance's configuration,
//! resolves its `SecretRef`s, and hands the module the values it declared.
//!
//! Four things are this batch's own.
//!
//! **Two of the seven are one provider.** Dropbox serves metadata from
//! `api.dropboxapi.com` and content from `content.dropboxapi.com`, and a
//! connector has one compiled origin, so a deployment that needs both
//! configures two instances against the same OAuth2 client
//! ([[074-a-second-origin-is-a-second-connector-and-a-download-is-composed-under-its-bound]]).
//!
//! **One operation answers with bytes rather than JSON.**
//! `dropbox_content.file.download` composes its declared output from the
//! response inside its own module, so the runtime calls the module's `decode`
//! rather than the declaration's — the same seam the Google and Microsoft
//! connectors use.
//!
//! **Two deployments name their own host.** Mattermost is self-hosted and names
//! a whole origin; Mailchimp names the data-centre label in its own key. Both
//! are checked here, at deploy time, so a host a deployment mistyped is a
//! startup refusal rather than a `404` on the first activity attempt.
//!
//! **Two providers publish the scopes their operations need.** Box and Zoom both
//! do, so an instance that enabled an operation its declared scopes do not
//! authorize is refused before a listener opens, exactly as Batch C's Google
//! connectors are.

use donat_connectors::providers::{
    box_platform, discord, dropbox, dropbox_content, mailchimp, mattermost, zoom,
};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorFailure, Credential, Operation,
    OperationRejection, Origin, Pagination, RequestPlan,
};
use donat_metadata::ConnectorInstance;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use super::{
    CredentialShape, Key, PaginationLookup, ProviderRules, ProviderRuntime, WebhookShape,
    bind_nothing, build_registered_instance as build_provider_instance, invalid_configuration,
    required_setting, resolve_secret_key,
};
use crate::connectors::{ConnectorRegistryError, ModuleContext, RegisteredConnector};
use crate::state::ConnectorConfigError;

/// How one connector in this batch reads a response.
///
/// Six of the seven answer JSON, and for those the *declaration* decides: the
/// declared success statuses, then the declared output pointers. The seventh
/// answers a file, and only its own module knows how to compose that into the
/// contract it published — and how to bound it.
#[derive(Clone, Copy)]
enum Decoding {
    Declared,
    Module(fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>),
}

/// How one connector binds the values the *module* supplies into an operation's
/// input.
type Binder = fn(&ConnectorConfiguration, &JsonValue) -> Result<JsonValue, ConnectorFailure>;

/// One compiled storage or messaging instance.
struct StorageRuntime {
    connector: &'static Connector,
    origin: Origin,
    credential: Credential,
    configuration: ConnectorConfiguration,
    bind: Binder,
    decode: Decoding,
    error_map: &'static donat_connectors::sdk::ErrorMap,
    pagination: PaginationLookup,
    /// Whether this module's credential is the source-local OAuth2 store's.
    stored_oauth2: bool,
}

impl StorageRuntime {
    fn compile(
        connector: &'static Connector,
        credential: Credential,
        configuration: ConnectorConfiguration,
        error_map: &'static donat_connectors::sdk::ErrorMap,
        bind: Binder,
        decode: Decoding,
        pagination: PaginationLookup,
    ) -> Result<Self, String> {
        let origin = connector
            .resolve_origin(&configuration)
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
            configuration,
            bind,
            decode,
            error_map,
            pagination,
            stored_oauth2,
        })
    }

    fn operation(&self, id: &str) -> Result<&'static Operation, ConnectorFailure> {
        self.connector.operation(id).ok_or_else(|| {
            ConnectorFailure::invariant("connector operation is not compiled into this binary")
        })
    }
}

impl ProviderRuntime for StorageRuntime {
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

    /// Render one request, with the values the module supplies bound first and
    /// the activity's stable key written into whatever binding the operation's
    /// class declared.
    ///
    /// No operation in this batch is `ProviderIdempotent::ExplicitKey`, so
    /// `plan_keyed_request` is exactly `plan_request` here — it is called
    /// anyway, because a later operation that reached that class must not
    /// depend on somebody remembering to change this line
    /// ([[070-a-declared-idempotency-key-is-written-by-the-executor-and-a-window-is-a-startup-check]]).
    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let bound = (self.bind)(&self.configuration, input)?;
        self.operation(id)?
            .plan_keyed_request(&self.origin, &bound, idempotency_key)
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        let operation = self.operation(id)?;
        match self.decode {
            Decoding::Module(decode) => decode(operation, status, headers, body),
            Decoding::Declared => {
                if !operation.is_success(status) {
                    return Err(self.error_map.classify(status, headers, body));
                }
                operation.decode_response(status, body)
            }
        }
    }

    fn pagination(&self, id: &str) -> Option<Pagination> {
        (self.pagination)(id).cloned()
    }

    /// The page gate is the declaration's: every provider in this batch reports
    /// a failure with a status, and the aggregate — not each page — is what the
    /// declared output describes.
    fn admit_page(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(), ConnectorFailure> {
        if self.operation(id)?.is_success(status) {
            return Ok(());
        }
        Err(self.error_map.classify(status, headers, body))
    }
}

/// Build one instance whose response contract is entirely its declaration.
fn build_declared(
    context: &mut ModuleContext<'_>,
    credential: Credential,
    configuration: ConnectorConfiguration,
    connector: &'static Connector,
    error_map: &'static donat_connectors::sdk::ErrorMap,
    bind: Binder,
    pagination: PaginationLookup,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    let invalid = invalid_configuration(context.instance);
    let runtime = StorageRuntime::compile(
        connector,
        credential,
        configuration,
        error_map,
        bind,
        Decoding::Declared,
        pagination,
    )
    .map_err(&invalid)?;
    build_provider_instance(context, Box::new(runtime))
}

// ===========================================================================
// dropbox — the metadata origin
// ===========================================================================

pub(crate) mod dropbox_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: dropbox::NAME,
        // No `secret_key`, no `config.secrets`: the credential is the
        // source-local store's, written by `donat connector authorize`.
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        dropbox::connector()
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
        build_declared(
            context,
            Credential::from_fields([]),
            ConnectorConfiguration::default(),
            dropbox::connector(),
            dropbox::error_map(),
            bind_nothing,
            dropbox::pagination,
        )
    }
}

// ===========================================================================
// dropbox_content — the content origin, and the one download in this batch
// ===========================================================================

pub(crate) mod dropbox_content_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: dropbox_content::NAME,
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        dropbox_content::connector()
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
        let runtime = StorageRuntime::compile(
            dropbox_content::connector(),
            Credential::from_fields([]),
            ConnectorConfiguration::default(),
            dropbox_content::error_map(),
            // The argument header is composed from the caller's typed `path`,
            // in the module, before the request renders.
            dropbox_content::download_arg_input,
            // The response is a file rather than JSON, so the module composes
            // the declared output and bounds it.
            Decoding::Module(dropbox_content::decode),
            dropbox_content::pagination,
        )
        .map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// box — a stored OAuth2 credential and a published scope set
// ===========================================================================

pub(crate) mod box_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: box_platform::NAME,
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        box_platform::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        let Some(oauth2) = &instance.config.oauth2 else {
            return;
        };
        for operation in &instance.operations {
            let required = box_platform::scopes(&operation.name);
            if required
                .iter()
                .any(|scope| oauth2.scopes.iter().any(|held| held == scope))
            {
                continue;
            }
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.oauth2.scopes"),
                format!(
                    "connector operation `{}` on module `{}` is not authorized by any declared \
                     scope; Box publishes `{}` as \"Read all files and folders stored in Box\" and \
                     `{}` as \"Read and write all files and folders stored in Box\"",
                    operation.name,
                    box_platform::NAME,
                    box_platform::READ_SCOPE,
                    box_platform::WRITE_SCOPE
                ),
            ));
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        build_declared(
            context,
            Credential::from_fields([]),
            ConnectorConfiguration::default(),
            box_platform::connector(),
            box_platform::error_map(),
            bind_nothing,
            box_platform::pagination,
        )
    }
}

// ===========================================================================
// discord — a configured bot token under the provider's own scheme
// ===========================================================================

pub(crate) mod discord_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: discord::NAME,
        credential: CredentialShape::SecretKey,
        // The gateway is a long-lived socket this engine does not open, and no
        // trigger is declared for it (spec 025 §5).
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        discord::connector()
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
        build_declared(
            context,
            Credential::secret(token),
            ConnectorConfiguration::default(),
            discord::connector(),
            discord::error_map(),
            bind_nothing,
            discord::pagination,
        )
    }
}

// ===========================================================================
// mattermost — the deployment's own server origin
// ===========================================================================

pub(crate) mod mattermost_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: mattermost::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The server is the connector's *origin*, so it is deploy-time by
        // construction: an operation input that could reach it would be an input
        // choosing an authority.
        settings: &[Key::required(mattermost::SERVER_ORIGIN)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        mattermost::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // The origin's own rules: `https`, because the credential is a bearer
        // token on every request, and no path, because an origin is a scheme, a
        // host, and a port.
        if let Ok(server) = required_setting(&instance.config, mattermost::SERVER_ORIGIN)
            && let Err(error) = mattermost::validate_server_origin(server)
        {
            RULES.refuse_setting(path, mattermost::SERVER_ORIGIN, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let server = required_setting(config, mattermost::SERVER_ORIGIN)
            .map_err(&invalid)?
            .to_owned();
        // Validated again here rather than trusted from validation, because a
        // compiled instance must never be able to reach a server this connector
        // would not have been allowed to authenticate to.
        mattermost::validate_server_origin(&server)
            .map_err(|error| invalid(error.message().to_owned()))?;
        build_declared(
            context,
            Credential::secret(token),
            ConnectorConfiguration::from_deployment([(mattermost::SERVER_ORIGIN, server.as_str())]),
            mattermost::connector(),
            mattermost::error_map(),
            bind_nothing,
            mattermost::pagination,
        )
    }
}

// ===========================================================================
// mailchimp — the data centre as a host label
// ===========================================================================

pub(crate) mod mailchimp_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: mailchimp::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[Key::required(mailchimp::SERVER)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        mailchimp::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // The templated host's own grammar, checked at deploy time rather than
        // at the first activity attempt.
        if let Ok(server) = required_setting(&instance.config, mailchimp::SERVER)
            && let Err(error) = mailchimp::connector().resolve_origin(
                &ConnectorConfiguration::from_deployment([(mailchimp::SERVER, server)]),
            )
        {
            RULES.refuse_setting(path, mailchimp::SERVER, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let api_key = resolve_secret_key(config).map_err(&invalid)?;
        let server = required_setting(config, mailchimp::SERVER)
            .map_err(&invalid)?
            .to_owned();
        build_declared(
            context,
            Credential::secret(api_key),
            ConnectorConfiguration::from_deployment([(mailchimp::SERVER, server.as_str())]),
            mailchimp::connector(),
            mailchimp::error_map(),
            bind_nothing,
            mailchimp::pagination,
        )
    }
}

// ===========================================================================
// zoom — a stored OAuth2 credential and a published scope set
// ===========================================================================

pub(crate) mod zoom_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: zoom::NAME,
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        zoom::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        let Some(oauth2) = &instance.config.oauth2 else {
            return;
        };
        for operation in &instance.operations {
            let required = zoom::scopes(&operation.name);
            if required
                .iter()
                .any(|scope| oauth2.scopes.iter().any(|held| held == scope))
            {
                continue;
            }
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.oauth2.scopes"),
                format!(
                    "connector operation `{}` on module `{}` is not authorized by any declared \
                     scope; Zoom publishes `{}` and `{}` for its meeting surface",
                    operation.name,
                    zoom::NAME,
                    zoom::READ_SCOPE,
                    zoom::WRITE_SCOPE
                ),
            ));
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        build_declared(
            context,
            Credential::from_fields([]),
            ConnectorConfiguration::default(),
            zoom::connector(),
            zoom::error_map(),
            bind_nothing,
            zoom::pagination,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two Dropbox connectors are two origins and two deployment names, and
    /// neither publishes the other's operations (spec 025 §2).
    #[test]
    fn the_dropbox_content_surface_is_its_own_connector() {
        assert_ne!(dropbox::NAME, dropbox_content::NAME);
        let metadata = dropbox::connector()
            .resolve_origin(&ConnectorConfiguration::default())
            .expect("a fixed origin resolves with no configuration");
        let content = dropbox_content::connector()
            .resolve_origin(&ConnectorConfiguration::default())
            .expect("a fixed origin resolves with no configuration");
        assert_eq!(metadata.as_url().host_str(), Some("api.dropboxapi.com"));
        assert_eq!(content.as_url().host_str(), Some("content.dropboxapi.com"));
        assert!(dropbox::connector().operation("file.download").is_none());
        assert!(
            dropbox_content::connector()
                .operation("file.get_metadata")
                .is_none()
        );
    }

    /// The runtime a deployment dispatches on is the one that composes the
    /// download's argument header and its byte output — not merely the module
    /// that declares them
    /// ([[034-a-declaration-the-runtime-ignores-is-a-defect]]).
    #[test]
    fn the_compiled_content_runtime_composes_the_argument_and_the_bytes() {
        let runtime = StorageRuntime::compile(
            dropbox_content::connector(),
            Credential::from_fields([]),
            ConnectorConfiguration::default(),
            dropbox_content::error_map(),
            dropbox_content::download_arg_input,
            Decoding::Module(dropbox_content::decode),
            dropbox_content::pagination,
        )
        .expect("a configured content instance compiles");

        // The argument header is the module's, composed from the caller's typed
        // path, and it reaches the rendered request.
        let request = runtime
            .plan(
                "file.download",
                &serde_json::json!({ "path": "/Homework/math/Prime_Numbers.txt" }),
                "activity-1",
            )
            .expect("the declared request renders");
        assert_eq!(
            request
                .headers()
                .get("dropbox-api-arg")
                .and_then(|value| value.to_str().ok()),
            Some(r#"{"path":"/Homework/math/Prime_Numbers.txt"}"#)
        );
        assert!(request.body().is_empty(), "a download sends no body");
        // An input carrying the composed slot is refused by the runtime, not
        // only by the module.
        assert!(
            runtime
                .plan(
                    "file.download",
                    &serde_json::json!({ "path": "/a", "api_arg": "{\"path\":\"/b\"}" }),
                    "activity-1",
                )
                .is_err()
        );

        // The bytes are composed into the declared output, and one byte past
        // the ceiling is a refusal with nothing partial in it.
        let decoded = runtime
            .decode("file.download", 200, &HeaderMap::new(), b"2 3 5 7")
            .expect("a download decodes");
        assert_eq!(decoded.get("content_bytes"), Some(&serde_json::json!(7)));
        let failure = runtime
            .decode(
                "file.download",
                200,
                &HeaderMap::new(),
                &vec![b'x'; donat_connectors::sdk::MAX_HTTP_BODY_BYTES + 1],
            )
            .expect_err("a body past the ceiling is not a download");
        assert_eq!(failure.code(), "connector_response_too_large");
    }

    /// Only the four stored-credential modules apply an issued header; the three
    /// configured ones never accept one.
    #[test]
    fn only_the_stored_credential_modules_apply_an_applied_header() {
        fn scheme(connector: &Connector) -> Option<&'static str> {
            connector
                .credential()
                .plan()
                .and_then(AuthPlan::oauth2_authorization_scheme)
        }
        for stored in [
            dropbox::connector(),
            dropbox_content::connector(),
            box_platform::connector(),
            zoom::connector(),
        ] {
            assert_eq!(
                scheme(stored),
                Some(donat_connectors::sdk::BEARER_SCHEME),
                "`{}` applies a stored bearer token",
                stored.name()
            );
        }
        for configured in [
            discord::connector(),
            mattermost::connector(),
            mailchimp::connector(),
        ] {
            assert!(
                scheme(configured).is_none(),
                "`{}` authenticates with a configured key",
                configured.name()
            );
        }
    }
}
