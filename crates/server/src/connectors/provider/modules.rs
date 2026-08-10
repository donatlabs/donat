//! One section per hand-written connector: its deploy-time configuration
//! surface, and how one instance of it compiles.
//!
//! Everything here is deliberately small. A section names the configuration
//! keys its provider reads, checks the ones whose grammar the provider
//! publishes, and hands the resolved values to the module in
//! `donat_connectors::providers`. No request shape, effect class, error map, or
//! provider fact lives in this file — all of that is the connector module's,
//! and duplicating any of it here would be a second description of one
//! provider that could disagree with the first.

use donat_connectors::providers::{
    airtable, aws, aws_s3, aws_ses, aws_sqs, calendly, github, hubspot, intercom, jira, linear,
    mercado_pago, notion, openai, paddle, paypal, postmark, sendgrid, sentry, shopify, slack,
    telegram, twilio, typeform, xero,
};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorFailure, Credential, Operation,
    OperationRejection, Origin, Pagination, PaginationBudget, RequestPlan, Secret,
};
use donat_metadata::{ConnectorConfig, ConnectorInstance};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use super::{
    CredentialShape, Key, PaginationLookup, ProviderRules, ProviderRuntime, WebhookShape,
    bind_nothing, build_declared_instance, build_declared_instance_with_webhook,
    build_registered_instance as build_provider_instance, invalid_configuration, no_pagination,
    optional_usize_setting, required_setting, resolve_optional_secret, resolve_secret,
    resolve_secret_key, resolve_webhook_secret,
};
use crate::connectors::{ConnectorRegistryError, ModuleContext, RegisteredConnector};
use crate::state::ConnectorConfigError;

/// The AWS credential fields every AWS connector configures identically.
const AWS_SECRETS: &[Key] = &[
    Key::required("access_key_id"),
    Key::required("secret_access_key"),
    // AWS documents the session token as belonging to temporary credentials
    // only, so a deployment signing with long-term keys has none and must not
    // be asked for one.
    Key::optional("session_token"),
];

/// The resolved AWS credential of one instance.
fn aws_credential(config: &ConnectorConfig, region: &str) -> Result<Credential, String> {
    Ok(aws::credential(
        &resolve_secret(config, "access_key_id")?,
        &resolve_secret(config, "secret_access_key")?,
        region,
        resolve_optional_secret(config, "session_token")?.as_deref(),
    ))
}

// ===========================================================================
// airtable
// ===========================================================================

pub(crate) mod airtable_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: airtable::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // Airtable's personal access tokens are granted per base, so which base
        // this deployment talks to is configuration rather than input.
        settings: &[Key::required(airtable::BASE_ID)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        airtable::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // The base identifier's grammar is the provider module's own: the check
        // a request would make is made here instead, at deploy time, so a
        // mistyped base fails before a listener opens rather than at the first
        // activity attempt.
        if let Ok(base) = required_setting(&instance.config, airtable::BASE_ID)
            && let Err(failure) = airtable::base_scoped_input(
                &ConnectorConfiguration::from_deployment([(airtable::BASE_ID, base)]),
                &json!({}),
            )
        {
            RULES.refuse_setting(path, airtable::BASE_ID, failure.safe_message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let base = required_setting(config, airtable::BASE_ID)
            .map_err(&invalid)?
            .to_owned();
        build_declared_instance(
            context,
            // The base is part of Airtable's declared credential contract as a
            // non-secret field, and it is also the deploy-time value the path
            // binder fills, so it is handed over as both.
            Credential::from_fields([
                ("secret", Secret::new(token)),
                (airtable::BASE_ID, Secret::new(&base)),
            ]),
            [(airtable::BASE_ID, base.as_str())],
            airtable::error_map(),
            airtable::base_scoped_input,
            airtable::pagination,
        )
    }
}

// ===========================================================================
// sendgrid
// ===========================================================================

pub(crate) mod sendgrid_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: sendgrid::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        sendgrid::connector()
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
        let token = resolve_secret_key(&context.instance.config)
            .map_err(invalid_configuration(context.instance))?;
        build_declared_instance(
            context,
            Credential::secret(token),
            [],
            sendgrid::error_map(),
            bind_nothing,
            sendgrid::pagination,
        )
    }
}

// ===========================================================================
// postmark
// ===========================================================================

pub(crate) mod postmark_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: postmark::NAME,
        // Postmark's server token is applied as a fixed header by the module's
        // own `ApiKeyHeader` plan; the deployment configures only its value.
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        postmark::connector()
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
        let token = resolve_secret_key(&context.instance.config)
            .map_err(invalid_configuration(context.instance))?;
        build_declared_instance(
            context,
            Credential::secret(token),
            [],
            postmark::error_map(),
            bind_nothing,
            postmark::pagination,
        )
    }
}

// ===========================================================================
// twilio — the one declaration built per deployment
// ===========================================================================

pub(crate) mod twilio_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: twilio::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The Account SID is both the HTTP Basic username and a path segment of
        // every resource, which is why this connector's *declaration* — not
        // just its instance — is built from configuration.
        settings: &[Key::required(twilio::ACCOUNT_SID)],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// `twilio::connector` takes the Account SID because `AuthPlan::basic`
    /// takes its username where the plan is built. A `&'static` declaration
    /// would therefore have to carry a placeholder username, which is a
    /// credential contract that does not describe what reaches the wire — so
    /// the module table holds this function instead of a constant.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let account_sid = required_setting(&instance.config, twilio::ACCOUNT_SID).ok()?;
        twilio::connector(account_sid).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // Twilio's own grammar for an Account SID. Without a declaration there
        // are no operations to admit either, which is why the refusal has to
        // land here rather than in the operation loop.
        if let Ok(account_sid) = required_setting(&instance.config, twilio::ACCOUNT_SID)
            && let Err(error) = twilio::connector(account_sid)
        {
            RULES.refuse_setting(path, twilio::ACCOUNT_SID, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let auth_token = resolve_secret_key(config).map_err(&invalid)?;
        let account_sid = required_setting(config, twilio::ACCOUNT_SID)
            .map_err(&invalid)?
            .to_owned();
        build_declared_instance(
            context,
            // The Account SID is Twilio's declared non-secret credential field
            // as well as the Basic username its declaration was built with.
            Credential::from_fields([
                ("secret", Secret::new(auth_token)),
                (twilio::ACCOUNT_SID, Secret::new(&account_sid)),
            ]),
            [(twilio::ACCOUNT_SID, account_sid.as_str())],
            twilio::error_map(),
            twilio::account_scoped_input,
            twilio::pagination,
        )
    }
}

// ===========================================================================
// openai
// ===========================================================================

pub(crate) mod openai_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: openai::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        openai::connector()
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
        let token = resolve_secret_key(&context.instance.config)
            .map_err(invalid_configuration(context.instance))?;
        build_declared_instance(
            context,
            Credential::secret(token),
            [],
            openai::error_map(),
            bind_nothing,
            openai::pagination,
        )
    }
}

// ===========================================================================
// typeform
// ===========================================================================

pub(crate) mod typeform_module {
    use super::*;

    // Batch A gave this connector its reads; spec 013 adds its `form_response`
    // trigger, so a deployment now configures the webhook secret Typeform signs
    // with as well as the personal access token it reads with.
    const RULES: ProviderRules = ProviderRules {
        module: typeform::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::RequiredSecret,
        settings: &[],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        typeform::connector()
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
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let webhook_secret = resolve_webhook_secret(config).map_err(&invalid)?;
        build_declared_instance_with_webhook(
            context,
            Credential::secret(token),
            [],
            typeform::error_map(),
            bind_nothing,
            typeform::pagination,
            Some(webhook_secret),
        )
    }
}

// ===========================================================================
// aws_s3
// ===========================================================================

/// The compiled S3 instance, behind the shared executor's one trait.
struct S3Runtime {
    instance: aws_s3::S3Instance,
    credential: Credential,
    plan: AuthPlan,
}

impl ProviderRuntime for S3Runtime {
    fn origin(&self) -> &Origin {
        self.instance.origin()
    }

    /// S3 answers `ListObjectsV2` in XML, and every plan in the SDK's closed
    /// set reads a JSON pointer. `object.list` is therefore one bounded page
    /// whose continuation token the module publishes as a declared output for a
    /// Process to carry, and this instance's own budget says so: one call, one
    /// page.
    fn pagination(&self, _id: &str) -> Option<Pagination> {
        None
    }

    fn pagination_budget(&self, _id: &str) -> PaginationBudget {
        self.instance
            .list_budget(donat_connectors::sdk::operation::DEFAULT_OPERATION_DEADLINE)
    }

    fn auth_plan(&self) -> Option<&AuthPlan> {
        Some(&self.plan)
    }

    fn credential(&self) -> &Credential {
        &self.credential
    }

    fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        self.instance.admit_operation(id)
    }

    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        _idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.instance
            .plan(operation(self.instance.operation(id))?, input)
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        self.instance.decode(
            operation(self.instance.operation(id))?,
            status,
            headers,
            body,
        )
    }
}

pub(crate) mod aws_s3_module {
    use super::*;

    const BUCKET: &str = "bucket";
    const VERSIONING: &str = "bucket_versioning";
    const MAX_OBJECT_BYTES: &str = "max_object_bytes";

    const RULES: ProviderRules = ProviderRules {
        module: aws_s3::CONNECTOR_NAME,
        credential: CredentialShape::NamedSecrets,
        webhook: WebhookShape::None,
        settings: &[
            Key::required(aws_s3::REGION_CONFIGURATION_KEY),
            Key::required(BUCKET),
            // The bucket's versioning decides one effect class, so it is
            // required rather than assumed
            // (`knowledgebase/declarative-saas/decisions/046-*`).
            Key::required(VERSIONING),
            Key::optional(MAX_OBJECT_BYTES),
        ],
        secrets: AWS_SECRETS,
    };

    pub(crate) fn connector() -> &'static Connector {
        aws_s3::connector()
    }

    /// One deployment's validated S3 configuration.
    fn configuration(
        config: &ConnectorConfig,
    ) -> Result<aws_s3::S3Configuration, (&'static str, String)> {
        let region = required_setting(config, aws_s3::REGION_CONFIGURATION_KEY)
            .map_err(|error| (aws_s3::REGION_CONFIGURATION_KEY, error))?;
        let bucket = required_setting(config, BUCKET).map_err(|error| (BUCKET, error))?;
        let versioning = aws_s3::BucketVersioning::parse(
            required_setting(config, VERSIONING).map_err(|error| (VERSIONING, error))?,
        )
        .map_err(|error| (VERSIONING, error.message().to_owned()))?;
        let mut compiled = aws_s3::S3Configuration::new(region, bucket, versioning)
            .map_err(|error| (error.setting(), error.message().to_owned()))?;
        if let Some(bytes) = optional_usize_setting(config, MAX_OBJECT_BYTES)
            .map_err(|error| (MAX_OBJECT_BYTES, error))?
        {
            compiled = compiled
                .with_max_object_bytes(bytes)
                .map_err(|error| (MAX_OBJECT_BYTES, error.message().to_owned()))?;
        }
        Ok(compiled)
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        match configuration(&instance.config) {
            Err((setting, message)) => RULES.refuse_setting(path, setting, message, errors),
            // The instance's own gate, which is stricter than the declaration's:
            // on a versioning-enabled bucket a keyless delete leaves a second
            // delete marker, so `object.delete` is not executable there.
            Ok(configuration) => match aws_s3::S3Instance::compile(&configuration) {
                Err(error) => RULES.refuse_setting(path, error.setting(), error.message(), errors),
                Ok(compiled) => {
                    admit_configured_operations(connector(), instance, path, errors, |id| {
                        compiled.admit_operation(id).map(|_| ())
                    });
                }
            },
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let configuration = configuration(config)
            .map_err(|(setting, message)| invalid(format!("{setting}: {message}")))?;
        let credential = aws_credential(config, configuration.region()).map_err(&invalid)?;
        let instance = aws_s3::S3Instance::compile(&configuration)
            .map_err(|error| invalid(error.to_string()))?;
        let runtime = S3Runtime {
            instance,
            credential,
            plan: AuthPlan::aws_sigv4(aws_s3::SERVICE)
                .map_err(|error| invalid(error.message().to_owned()))?,
        };
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// aws_sqs
// ===========================================================================

struct SqsRuntime {
    instance: aws_sqs::SqsInstance,
    credential: Credential,
    plan: AuthPlan,
}

impl ProviderRuntime for SqsRuntime {
    fn origin(&self) -> &Origin {
        self.instance.origin()
    }

    /// A queue is not a collection: `ReceiveMessage` returns what is visible
    /// now, bounded by the queue's own maximum, and there is no continuation to
    /// follow.
    fn pagination(&self, _id: &str) -> Option<Pagination> {
        None
    }

    fn auth_plan(&self) -> Option<&AuthPlan> {
        Some(&self.plan)
    }

    fn credential(&self) -> &Credential {
        &self.credential
    }

    fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        self.instance.admit_operation(id)
    }

    /// The FIFO send binds the durable activity's own stable key to
    /// `MessageDeduplicationId`; the module derives the identifier, and no
    /// operation reads one from input.
    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.instance.plan(
            operation(self.instance.operation(id))?,
            input,
            Some(idempotency_key),
        )
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        self.instance.decode(
            operation(self.instance.operation(id))?,
            status,
            headers,
            body,
        )
    }
}

pub(crate) mod aws_sqs_module {
    use super::*;

    const ACCOUNT_ID: &str = "account_id";
    const QUEUE_NAME: &str = "queue_name";
    const QUEUE_TYPE: &str = "queue_type";
    const SEND_HORIZON_MS: &str = "send_horizon_ms";
    const MAX_MESSAGE_BYTES: &str = "max_message_bytes";

    const RULES: ProviderRules = ProviderRules {
        module: aws_sqs::CONNECTOR_NAME,
        credential: CredentialShape::NamedSecrets,
        webhook: WebhookShape::None,
        settings: &[
            Key::required(aws_sqs::REGION_CONFIGURATION_KEY),
            Key::required(ACCOUNT_ID),
            Key::required(QUEUE_NAME),
            // The queue type decides whether `message.send` is executable at
            // all, and it is checked against the queue's own name.
            Key::required(QUEUE_TYPE),
            Key::optional(SEND_HORIZON_MS),
            Key::optional(MAX_MESSAGE_BYTES),
        ],
        secrets: AWS_SECRETS,
    };

    pub(crate) fn connector() -> &'static Connector {
        aws_sqs::connector()
    }

    fn configuration(
        config: &ConnectorConfig,
    ) -> Result<aws_sqs::SqsConfiguration, (&'static str, String)> {
        let region = required_setting(config, aws_sqs::REGION_CONFIGURATION_KEY)
            .map_err(|error| (aws_sqs::REGION_CONFIGURATION_KEY, error))?;
        let account_id =
            required_setting(config, ACCOUNT_ID).map_err(|error| (ACCOUNT_ID, error))?;
        let queue_name =
            required_setting(config, QUEUE_NAME).map_err(|error| (QUEUE_NAME, error))?;
        let queue_type = aws_sqs::QueueType::parse(
            required_setting(config, QUEUE_TYPE).map_err(|error| (QUEUE_TYPE, error))?,
        )
        .map_err(|error| (QUEUE_TYPE, error.message().to_owned()))?;
        let mut compiled =
            aws_sqs::SqsConfiguration::new(region, account_id, queue_name, queue_type)
                .map_err(|error| (error.setting(), error.message().to_owned()))?;
        if let Some(milliseconds) = optional_usize_setting(config, SEND_HORIZON_MS)
            .map_err(|error| (SEND_HORIZON_MS, error))?
        {
            compiled = compiled
                .with_send_horizon(std::time::Duration::from_millis(milliseconds as u64))
                .map_err(|error| (SEND_HORIZON_MS, error.message().to_owned()))?;
        }
        if let Some(bytes) = optional_usize_setting(config, MAX_MESSAGE_BYTES)
            .map_err(|error| (MAX_MESSAGE_BYTES, error))?
        {
            compiled = compiled
                .with_max_message_bytes(bytes)
                .map_err(|error| (MAX_MESSAGE_BYTES, error.message().to_owned()))?;
        }
        Ok(compiled)
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        match configuration(&instance.config) {
            Err((setting, message)) => RULES.refuse_setting(path, setting, message, errors),
            Ok(configuration) => match aws_sqs::SqsInstance::compile(&configuration) {
                Err(error) => RULES.refuse_setting(path, error.setting(), error.message(), errors),
                // A standard queue publishes no deduplication, so its
                // `message.send` compiles inventory-only and cannot be enabled;
                // its reads still can.
                Ok(compiled) => {
                    admit_configured_operations(connector(), instance, path, errors, |id| {
                        compiled.admit_operation(id).map(|_| ())
                    });
                }
            },
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let configuration = configuration(config)
            .map_err(|(setting, message)| invalid(format!("{setting}: {message}")))?;
        let credential = aws_credential(config, configuration.region()).map_err(&invalid)?;
        let instance = aws_sqs::SqsInstance::compile(&configuration)
            .map_err(|error| invalid(error.to_string()))?;
        let runtime = SqsRuntime {
            instance,
            credential,
            plan: AuthPlan::aws_sigv4(aws_sqs::SERVICE)
                .map_err(|error| invalid(error.message().to_owned()))?,
        };
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// aws_ses
// ===========================================================================

struct SesRuntime {
    instance: aws_ses::SesInstance,
    credential: Credential,
    plan: AuthPlan,
}

impl ProviderRuntime for SesRuntime {
    fn origin(&self) -> &Origin {
        self.instance.origin()
    }

    /// SES publishes a `NextToken` its listings echo back, and this instance
    /// declares both the plan and the ceilings one attempt may spend on it.
    fn pagination(&self, id: &str) -> Option<Pagination> {
        self.instance.pagination(id)
    }

    fn pagination_budget(&self, id: &str) -> PaginationBudget {
        self.instance.list_budget(
            id,
            donat_connectors::sdk::operation::DEFAULT_OPERATION_DEADLINE,
        )
    }

    fn auth_plan(&self) -> Option<&AuthPlan> {
        Some(&self.plan)
    }

    fn credential(&self) -> &Credential {
        &self.credential
    }

    fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        self.instance.admit_operation(id)
    }

    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        _idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.instance
            .plan(operation(self.instance.operation(id))?, input)
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        self.instance.decode(
            operation(self.instance.operation(id))?,
            status,
            headers,
            body,
        )
    }
}

pub(crate) mod aws_ses_module {
    use super::*;

    const FROM_EMAIL_ADDRESS: &str = "from_email_address";
    const MAX_MESSAGE_BYTES: &str = "max_message_bytes";

    const RULES: ProviderRules = ProviderRules {
        module: aws_ses::CONNECTOR_NAME,
        credential: CredentialShape::NamedSecrets,
        webhook: WebhookShape::None,
        settings: &[
            Key::required(aws_ses::REGION_CONFIGURATION_KEY),
            // The sending identity is deploy-time material: an operation input
            // may name recipients, never the sender.
            Key::required(FROM_EMAIL_ADDRESS),
            Key::optional(MAX_MESSAGE_BYTES),
        ],
        secrets: AWS_SECRETS,
    };

    pub(crate) fn connector() -> &'static Connector {
        aws_ses::connector()
    }

    fn configuration(
        config: &ConnectorConfig,
    ) -> Result<aws_ses::SesConfiguration, (&'static str, String)> {
        let region = required_setting(config, aws_ses::REGION_CONFIGURATION_KEY)
            .map_err(|error| (aws_ses::REGION_CONFIGURATION_KEY, error))?;
        let from = required_setting(config, FROM_EMAIL_ADDRESS)
            .map_err(|error| (FROM_EMAIL_ADDRESS, error))?;
        let mut compiled = aws_ses::SesConfiguration::new(region, from)
            .map_err(|error| (error.setting(), error.message().to_owned()))?;
        if let Some(bytes) = optional_usize_setting(config, MAX_MESSAGE_BYTES)
            .map_err(|error| (MAX_MESSAGE_BYTES, error))?
        {
            compiled = compiled
                .with_max_message_bytes(bytes)
                .map_err(|error| (MAX_MESSAGE_BYTES, error.message().to_owned()))?;
        }
        Ok(compiled)
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        match configuration(&instance.config) {
            Err((setting, message)) => RULES.refuse_setting(path, setting, message, errors),
            Ok(configuration) => {
                if let Err(error) = aws_ses::SesInstance::compile(&configuration) {
                    RULES.refuse_setting(path, error.setting(), error.message(), errors);
                }
            }
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let configuration = configuration(config)
            .map_err(|(setting, message)| invalid(format!("{setting}: {message}")))?;
        let credential = aws_credential(config, configuration.region()).map_err(&invalid)?;
        let instance = aws_ses::SesInstance::compile(&configuration)
            .map_err(|error| invalid(error.to_string()))?;
        let runtime = SesRuntime {
            instance,
            credential,
            plan: AuthPlan::aws_sigv4(aws_ses::SERVICE)
                .map_err(|error| invalid(error.message().to_owned()))?,
        };
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// Shared helpers
// ===========================================================================

/// The operation a compiled instance carries, or the invariant failure of
/// asking a module for one it does not declare. Registry admission already
/// answered this at startup; the check is here so a future caller cannot reach
/// a provider capability this binary was not built with.
fn operation(operation: Option<&Operation>) -> Result<&Operation, ConnectorFailure> {
    operation.ok_or_else(|| {
        ConnectorFailure::invariant("connector operation is not compiled into this binary")
    })
}

/// Apply one compiled instance's own admission rule to the operations a
/// deployment enabled.
///
/// This is what makes a configuration-dependent effect class a *startup*
/// refusal: the central check in `validate_enabled_operations` asks the static
/// declaration, and a module whose class depends on its target asks its own
/// compiled instance here.
fn admit_configured_operations(
    connector: &Connector,
    instance: &ConnectorInstance,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
    admit: impl Fn(&str) -> Result<(), OperationRejection>,
) {
    for (index, operation) in instance.operations.iter().enumerate() {
        // What the *declaration* refuses is already reported once, centrally.
        // What is left here is the refusal only this deployment's target earns.
        if connector.admit_operation(&operation.name).is_err() {
            continue;
        }
        if let Err(rejection) = admit(&operation.name) {
            errors.push(ConnectorConfigError::new(
                format!("{path}.operations[{index}].name"),
                format!(
                    "connector operation `{}` on module `{}`: {}",
                    operation.name,
                    instance.module,
                    rejection.message()
                ),
            ));
        }
    }
}

// ===========================================================================
// Batch B: the webhook-bearing connectors (spec 013)
//
// Each of these publishes an inbound route as well as an operation set, so each
// reads one further deploy-time value: `config.webhook_secret`, the secret its
// provider's verification scheme is applied under. A verified delivery of any
// of them answers `503` and persists nothing until the Process-owned inbound
// transaction lands ([[053-a-verified-delivery-with-nowhere-to-go]]).
// ===========================================================================

/// One Batch B module: one API key, one webhook secret, no further settings.
macro_rules! signed_inbound_module {
    ($module:ident, $provider:ident, $pagination:expr) => {
        pub(crate) mod $module {
            use super::*;

            const RULES: ProviderRules = ProviderRules {
                module: $provider::NAME,
                credential: CredentialShape::SecretKey,
                webhook: WebhookShape::RequiredSecret,
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
            }

            pub(crate) fn build_registered_instance(
                context: &mut ModuleContext<'_>,
            ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
                let config = &context.instance.config;
                let invalid = invalid_configuration(context.instance);
                let token = resolve_secret_key(config).map_err(&invalid)?;
                let webhook_secret = resolve_webhook_secret(config).map_err(&invalid)?;
                build_declared_instance_with_webhook(
                    context,
                    Credential::secret(token),
                    [],
                    $provider::error_map(),
                    bind_nothing,
                    $pagination,
                    Some(webhook_secret),
                )
            }
        }
    };
}

signed_inbound_module!(github_module, github, github::pagination);
// Telegram's `getUpdates` is a long-poll offset protocol rather than a
// collection walk, and Sentry's `Link` header offers a next page even when
// there is none — it marks exhaustion with a `results="false"` parameter the
// SDK's link plan does not read. Both therefore declare no plan and send
// exactly one request per attempt
// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
signed_inbound_module!(telegram_module, telegram, no_pagination);
signed_inbound_module!(calendly_module, calendly, calendly::pagination);
signed_inbound_module!(sentry_module, sentry, no_pagination);

// ===========================================================================
// shopify — the one templated host
// ===========================================================================

pub(crate) mod shopify_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: shopify::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::RequiredSecret,
        // The store's own `myshopify.com` label. It is the connector's *host*,
        // so it is deploy-time configuration by construction: an operation
        // input that could reach it would be an operation input choosing an
        // authority.
        settings: &[Key::required(shopify::SHOP)],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        shopify::connector()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // The templated host's own grammar, checked at deploy time rather than
        // at the first activity attempt: the SDK admits one lowercase host
        // label, which is a strict subset of Shopify's published
        // `[a-zA-Z0-9][a-zA-Z0-9\-]*`.
        if let Ok(shop) = required_setting(&instance.config, shopify::SHOP)
            && let Err(error) = connector().resolve_origin(
                &ConnectorConfiguration::from_deployment([(shopify::SHOP, shop)]),
            )
        {
            RULES.refuse_setting(path, shopify::SHOP, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let token = resolve_secret_key(config).map_err(&invalid)?;
        let webhook_secret = resolve_webhook_secret(config).map_err(&invalid)?;
        let shop = required_setting(config, shopify::SHOP)
            .map_err(&invalid)?
            .to_owned();
        build_declared_instance_with_webhook(
            context,
            Credential::secret(token),
            [(shopify::SHOP, shop.as_str())],
            shopify::error_map(),
            bind_nothing,
            shopify::pagination,
            Some(webhook_secret),
        )
    }
}

// ===========================================================================
// Batch E: the product SaaS connectors (spec 016)
//
// Two of these providers report failure inside a `200 OK` — Slack with
// `{"ok": false, "error": …}` and Linear with a GraphQL `errors` array — so the
// declaration alone cannot decide whether a response is a success. Each such
// module owns a `decode` of its own, and `BodyGatedRuntime` is the seam that
// calls it: everything else about the instance is still its declaration.
// ===========================================================================

/// One instance of a connector whose provider answers `200` with a body-level
/// failure.
///
/// It is [`DeclaredProvider`] with exactly one thing moved: the question "is
/// this response a success" is the module's rather than the status code's. The
/// module's `decode` is the *only* path to an output, so there is no spelling in
/// which a provider error is reported as an activity success.
struct BodyGatedRuntime {
    connector: Connector,
    origin: Origin,
    credential: Credential,
    decode: fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>,
    pagination: PaginationLookup,
}

impl BodyGatedRuntime {
    fn compile(
        connector: Connector,
        credential: Credential,
        configuration: &ConnectorConfiguration,
        decode: fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>,
        pagination: PaginationLookup,
    ) -> Result<Self, String> {
        let origin = connector
            .resolve_origin(configuration)
            .map_err(|error| error.message().to_owned())?;
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
}

impl ProviderRuntime for BodyGatedRuntime {
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

    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        _idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        operation(self.connector.operation(id))?.plan_request(&self.origin, input)
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        (self.decode)(
            operation(self.connector.operation(id))?,
            status,
            headers,
            body,
        )
    }

    fn pagination(&self, id: &str) -> Option<Pagination> {
        (self.pagination)(id).cloned()
    }

    /// The page gate is the module's own body gate, because for these providers
    /// the HTTP status is not the answer: a Slack page that carries
    /// `{"ok": false}` is a failure of the walk even though it arrived as a
    /// `200`, and it is classified through the module's ordered error map.
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

/// One Batch E module whose whole deploy-time surface is a single bearer token
/// and whose responses are read through the module's own body gate.
macro_rules! body_gated_module {
    ($module:ident, $provider:ident, $pagination:expr) => {
        pub(crate) mod $module {
            use super::*;

            const RULES: ProviderRules = ProviderRules {
                module: $provider::NAME,
                credential: CredentialShape::SecretKey,
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
            }

            pub(crate) fn build_registered_instance(
                context: &mut ModuleContext<'_>,
            ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
                let invalid = invalid_configuration(context.instance);
                let token = resolve_secret_key(&context.instance.config).map_err(&invalid)?;
                let runtime = BodyGatedRuntime::compile(
                    context.connector.clone(),
                    Credential::secret(token),
                    &ConnectorConfiguration::default(),
                    $provider::decode,
                    $pagination,
                )
                .map_err(&invalid)?;
                build_provider_instance(context, Box::new(runtime))
            }
        }
    };
}

body_gated_module!(slack_module, slack, slack::pagination);
// Linear's `after` is a GraphQL variable inside the request body, which no
// plan in the SDK's closed set can spend. It is a declared input its Process
// carries, so one attempt is one page
// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
body_gated_module!(linear_module, linear, no_pagination);

/// One Batch E module whose whole deploy-time surface is a single bearer token
/// and whose failures arrive with an HTTP status, so its declaration is the
/// whole of its behaviour.
macro_rules! bearer_module {
    ($module:ident, $provider:ident, $pagination:expr) => {
        pub(crate) mod $module {
            use super::*;

            const RULES: ProviderRules = ProviderRules {
                module: $provider::NAME,
                credential: CredentialShape::SecretKey,
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
            }

            pub(crate) fn build_registered_instance(
                context: &mut ModuleContext<'_>,
            ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
                let token = resolve_secret_key(&context.instance.config)
                    .map_err(invalid_configuration(context.instance))?;
                build_declared_instance(
                    context,
                    Credential::secret(token),
                    [],
                    $provider::error_map(),
                    bind_nothing,
                    $pagination,
                )
            }
        }
    };
}

bearer_module!(notion_module, notion, notion::pagination);
bearer_module!(intercom_module, intercom, intercom::pagination);
bearer_module!(hubspot_module, hubspot, hubspot::pagination);

// ===========================================================================
// Batch J: payments and billing (spec 026)
// ===========================================================================

// Paddle's continuation is a URL it returns even when there is no next page, so
// no plan in the closed set can spend it and this instance is handed
// `no_pagination` explicitly ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]],
// [[058-a-declared-walk-is-the-executors-walk]]).
bearer_module!(paddle_module, paddle, no_pagination);
// Mercado Pago publishes `paging` in a search *response* and no offset or limit
// in the request, so there is no plan for the executor to walk either
// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
bearer_module!(mercado_pago_module, mercado_pago, no_pagination);

/// PayPal — the first module whose credential is an OAuth2 **client-credentials**
/// token the executor mints per attempt.
///
/// Its deploy-time surface is two ordinary `SecretRef`s. It declares no
/// `config.oauth2` block and holds no stored credential: the token is bought
/// from the client id and secret once per logical attempt and dropped when the
/// attempt ends ([[072-a-minted-credential-is-spent-inside-one-attempt]]), so
/// nothing in this instance can reach `donat.connector_credential`.
///
/// The one non-secret setting is the send horizon, checked here at startup
/// against PayPal's shortest documented key retention, exactly as Xero's is
/// ([[070-a-declared-idempotency-key-is-written-by-the-executor-and-a-window-is-a-startup-check]]).
pub(crate) mod paypal_module {
    use super::*;

    const SEND_HORIZON_MS: &str = "send_horizon_ms";
    /// The two halves of one PayPal REST app's credential, as PayPal names them.
    pub(crate) const CLIENT_ID: &str = "client_id";
    pub(crate) const CLIENT_SECRET: &str = "client_secret";

    const RULES: ProviderRules = ProviderRules {
        module: paypal::NAME,
        credential: CredentialShape::NamedSecrets,
        webhook: WebhookShape::None,
        settings: &[Key::optional(SEND_HORIZON_MS)],
        secrets: &[Key::required(CLIENT_ID), Key::required(CLIENT_SECRET)],
    };

    pub(crate) fn connector() -> &'static Connector {
        paypal::connector()
    }

    fn configuration(
        config: &ConnectorConfig,
    ) -> Result<paypal::PaypalConfiguration, (&'static str, String)> {
        let mut compiled = paypal::PaypalConfiguration::new();
        if let Some(milliseconds) = optional_usize_setting(config, SEND_HORIZON_MS)
            .map_err(|error| (SEND_HORIZON_MS, error))?
        {
            compiled = compiled
                .with_send_horizon(std::time::Duration::from_millis(milliseconds as u64))
                .map_err(|error| (SEND_HORIZON_MS, error.message().to_owned()))?;
        }
        Ok(compiled)
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Err((setting, message)) = configuration(&instance.config) {
            RULES.refuse_setting(path, setting, message, errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let invalid = invalid_configuration(context.instance);
        // The horizon is a startup check and nothing else reads it: past
        // PayPal's window the same key is a second order, not a replay.
        configuration(&context.instance.config)
            .map_err(|(setting, message)| invalid(format!("{setting}: {message}")))?;
        let config = &context.instance.config;
        let client_id = resolve_secret(config, CLIENT_ID).map_err(&invalid)?;
        let client_secret = resolve_secret(config, CLIENT_SECRET).map_err(&invalid)?;
        build_declared_instance(
            context,
            Credential::from_fields([
                (CLIENT_ID, Secret::new(client_id)),
                (CLIENT_SECRET, Secret::new(client_secret)),
            ]),
            [],
            paypal::error_map(),
            bind_nothing,
            paypal::pagination,
        )
    }
}

/// One compiled Xero instance.
///
/// It differs from [`DeclaredProvider`] in exactly two ways, and both are
/// properties of this provider rather than of this file. Xero serves every
/// organisation from one origin and selects the organisation with a required
/// `xero-tenant-id` header, so every request carries one deploy-time header; and
/// every Xero mutation is `ProviderIdempotent::ExplicitKey`, so every request
/// carries the durable activity's own stable key in the binding the class was
/// admitted on. Both are written here, where the instance's configuration is,
/// and neither is reachable from operation input.
struct XeroRuntime {
    connector: &'static Connector,
    origin: Origin,
    credential: Credential,
    configured: HeaderMap,
}

impl XeroRuntime {
    fn compile(tenant_id: &str) -> Result<Self, String> {
        let connector = xero::connector();
        let origin = connector
            .resolve_origin(&ConnectorConfiguration::default())
            .map_err(|error| error.message().to_owned())?;
        // The credential is the deployment's stored grant, so this instance
        // holds none — `Credential::from_fields([])` is the accurate statement
        // rather than a placeholder, exactly as the Google modules record.
        let credential = Credential::from_fields([]);
        connector
            .credential()
            .admits(&credential)
            .map_err(|missing| missing.to_string())?;
        let mut configured = HeaderMap::new();
        configured.insert(
            reqwest::header::HeaderName::from_static(xero::TENANT_HEADER),
            reqwest::header::HeaderValue::from_str(tenant_id)
                .map_err(|_| "tenant_id is not a valid header value".to_owned())?,
        );
        Ok(Self {
            connector,
            origin,
            credential,
            configured,
        })
    }
}

impl ProviderRuntime for XeroRuntime {
    fn origin(&self) -> &Origin {
        &self.origin
    }

    fn auth_plan(&self) -> Option<&AuthPlan> {
        self.connector.credential().plan()
    }

    fn credential(&self) -> &Credential {
        &self.credential
    }

    fn applies_stored_oauth2(&self) -> bool {
        true
    }

    fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        self.connector.admit_operation(id)
    }

    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let operation = operation(self.connector.operation(id))?;
        let mut request =
            operation.plan_configured_request(&self.origin, input, &self.configured)?;
        // A no-op for every read; for a mutation it is the whole reason the
        // operation is executable at all.
        operation.apply_idempotency_key(&mut request, idempotency_key)?;
        Ok(request)
    }

    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        let operation = operation(self.connector.operation(id))?;
        if !operation.is_success(status) {
            return Err(xero::error_map().classify(status, headers, body));
        }
        operation.decode_response(status, body)
    }

    fn pagination(&self, id: &str) -> Option<Pagination> {
        xero::pagination(id).cloned()
    }

    fn admit_page(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<(), ConnectorFailure> {
        if operation(self.connector.operation(id))?.is_success(status) {
            return Ok(());
        }
        Err(xero::error_map().classify(status, headers, body))
    }
}

pub(crate) mod xero_module {
    use super::*;

    const SEND_HORIZON_MS: &str = "send_horizon_ms";

    const RULES: ProviderRules = ProviderRules {
        module: xero::NAME,
        credential: CredentialShape::Oauth2,
        webhook: WebhookShape::None,
        settings: &[
            // Which organisation's books this instance writes to. It is not a
            // host, but it is the same kind of value one is, so it is
            // deploy-time by construction.
            Key::required(xero::TENANT_ID),
            // How long a durable activity may keep resending one idempotent
            // request. It must fit inside Xero's documented six-minute key
            // retention less the clock safety margin, and it is checked here
            // rather than at send time.
            Key::optional(SEND_HORIZON_MS),
        ],
        secrets: &[],
    };

    pub(crate) fn connector() -> &'static Connector {
        xero::connector()
    }

    fn configuration(
        config: &ConnectorConfig,
    ) -> Result<xero::XeroConfiguration, (&'static str, String)> {
        let tenant =
            required_setting(config, xero::TENANT_ID).map_err(|error| (xero::TENANT_ID, error))?;
        let mut compiled = xero::XeroConfiguration::new(tenant)
            .map_err(|error| (error.setting(), error.message().to_owned()))?;
        if let Some(milliseconds) = optional_usize_setting(config, SEND_HORIZON_MS)
            .map_err(|error| (SEND_HORIZON_MS, error))?
        {
            compiled = compiled
                .with_send_horizon(std::time::Duration::from_millis(milliseconds as u64))
                .map_err(|error| (SEND_HORIZON_MS, error.message().to_owned()))?;
        }
        Ok(compiled)
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        if let Err((setting, message)) = configuration(&instance.config) {
            RULES.refuse_setting(path, setting, message, errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let invalid = invalid_configuration(context.instance);
        let configuration = configuration(&context.instance.config)
            .map_err(|(setting, message)| invalid(format!("{setting}: {message}")))?;
        let runtime = XeroRuntime::compile(configuration.tenant_id()).map_err(&invalid)?;
        build_provider_instance(context, Box::new(runtime))
    }
}

// ===========================================================================
// jira — a templated host *and* a per-deployment declaration
// ===========================================================================

pub(crate) mod jira_module {
    use super::*;

    const RULES: ProviderRules = ProviderRules {
        module: jira::NAME,
        credential: CredentialShape::SecretKey,
        webhook: WebhookShape::None,
        // The site is the connector's *host* and the email is its HTTP Basic
        // username, so both are deploy-time by construction: an operation input
        // that could reach either would be an input choosing an authority or an
        // identity.
        settings: &[Key::required(jira::SITE), Key::required(jira::EMAIL)],
        secrets: &[],
    };

    /// This instance's declaration.
    ///
    /// `jira::connector` takes the account address because `AuthPlan::basic`
    /// takes its username where the plan is built — the same reason Twilio's
    /// declaration is built per deployment.
    pub(crate) fn declare(instance: &ConnectorInstance) -> Option<Connector> {
        let email = required_setting(&instance.config, jira::EMAIL).ok()?;
        jira::connector(email).ok()
    }

    pub(crate) fn validate_instance_metadata(
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        RULES.validate(instance, path, errors);
        // Atlassian's own grammar for the Basic username. Without a declaration
        // there are no operations to admit either, which is why the refusal has
        // to land here rather than in the operation loop.
        if let Ok(email) = required_setting(&instance.config, jira::EMAIL)
            && let Err(error) = jira::connector(email)
        {
            RULES.refuse_setting(path, jira::EMAIL, error.message(), errors);
        }
        // The templated host's own grammar, checked at deploy time rather than
        // at the first activity attempt.
        if let (Ok(site), Ok(connector)) = (
            required_setting(&instance.config, jira::SITE),
            jira::declaration_shape(),
        ) && let Err(error) = connector.resolve_origin(&ConnectorConfiguration::from_deployment(
            [(jira::SITE, site)],
        )) {
            RULES.refuse_setting(path, jira::SITE, error.message(), errors);
        }
    }

    pub(crate) fn build_registered_instance(
        context: &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
        let config = &context.instance.config;
        let invalid = invalid_configuration(context.instance);
        let api_token = resolve_secret_key(config).map_err(&invalid)?;
        let site = required_setting(config, jira::SITE)
            .map_err(&invalid)?
            .to_owned();
        let email = required_setting(config, jira::EMAIL)
            .map_err(&invalid)?
            .to_owned();
        build_declared_instance(
            context,
            // The address is Jira's declared non-secret credential field as well
            // as the Basic username its declaration was built with.
            Credential::from_fields([
                ("secret", Secret::new(api_token)),
                (jira::EMAIL, Secret::new(&email)),
            ]),
            [(jira::SITE, site.as_str())],
            jira::error_map(),
            bind_nothing,
            jira::pagination,
        )
    }
}

/// Every hand-written module, paired with the plan lookup its section wires.
///
/// It exists for one assertion — that a declared plan collects the items its
/// operation actually publishes — and it is written out rather than derived so
/// that a reviewer can see, in one place, which of the twenty-eight connectors
/// walk and which deliberately do not
/// ([[058-a-declared-walk-is-the-executors-walk]]).
#[cfg(test)]
fn wired_plans() -> Vec<(&'static Connector, PaginationLookup)> {
    use donat_connectors::providers::{
        freshdesk, google_calendar, google_drive, google_gmail, google_sheets, microsoft_excel,
        microsoft_onedrive, microsoft_outlook, microsoft_teams, pipedrive, salesforce, woocommerce,
        zendesk,
    };

    vec![
        (airtable::connector(), airtable::pagination),
        (sendgrid::connector(), sendgrid::pagination),
        (postmark::connector(), postmark::pagination),
        (openai::connector(), openai::pagination),
        (typeform::connector(), typeform::pagination),
        (github::connector(), github::pagination),
        (shopify::connector(), shopify::pagination),
        (calendly::connector(), calendly::pagination),
        (slack::connector(), slack::pagination),
        (notion::connector(), notion::pagination),
        (intercom::connector(), intercom::pagination),
        (hubspot::connector(), hubspot::pagination),
        (google_sheets::connector(), google_sheets::pagination),
        (google_drive::connector(), google_drive::pagination),
        (google_gmail::connector(), google_gmail::pagination),
        (google_calendar::connector(), google_calendar::pagination),
        (
            microsoft_outlook::connector(),
            microsoft_outlook::pagination,
        ),
        (microsoft_teams::connector(), microsoft_teams::pagination),
        (microsoft_excel::connector(), microsoft_excel::pagination),
        (
            microsoft_onedrive::connector(),
            microsoft_onedrive::pagination,
        ),
        // Batch G (spec 023). Four are declarations one deployment completes,
        // so their plans are asserted against a declaration built here.
        (pipedrive::connector(), pipedrive::pagination),
        (freshdesk::connector(), freshdesk::pagination),
        (salesforce::connector(), salesforce::pagination),
        (zendesk_shape(), zendesk::pagination),
        (woocommerce_shape(), woocommerce::pagination),
        // The ones that declare none at all.
        (linear::connector(), no_pagination),
        (sentry::connector(), no_pagination),
        (telegram::connector(), no_pagination),
        (paddle::connector(), no_pagination),
        (mercado_pago::connector(), no_pagination),
        (xero::connector(), xero::pagination),
        (paypal::connector(), paypal::pagination),
        // Zoho publishes a cursor whose empty page no plan in the closed set can
        // spend, so it declares none and every attempt is one request
        // ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
        (zoho_crm_shape(), no_pagination),
        // Batch I (spec 025). Box walks a marker and an offset, Mattermost and
        // Mailchimp walk the regimes their providers publish, and Zoom walks a
        // page token that ends on an empty string.
        (
            batch_i::box_platform::connector(),
            batch_i::box_platform::pagination,
        ),
        (
            batch_i::mattermost::connector(),
            batch_i::mattermost::pagination,
        ),
        (
            batch_i::mailchimp::connector(),
            batch_i::mailchimp::pagination,
        ),
        (batch_i::zoom::connector(), batch_i::zoom::pagination),
        // Dropbox spends its cursor on a *different route*, and Discord's
        // continuation is the id of the last item of a bare array; neither is a
        // plan in the SDK's closed set, so both declare none and every attempt
        // of theirs is one request.
        (batch_i::dropbox::connector(), no_pagination),
        (batch_i::dropbox_content::connector(), no_pagination),
        (batch_i::discord::connector(), no_pagination),
    ]
}

/// The Batch I modules, named once so the table above reads as one list.
#[cfg(test)]
mod batch_i {
    pub(super) use donat_connectors::providers::{
        box_platform, discord, dropbox, dropbox_content, mailchimp, mattermost, zoom,
    };
}

/// The two Batch G declarations a deployment completes, held for the plan
/// assertions below. A per-deployment declaration has no `&'static` form, so the
/// test leaks one placeholder each rather than rebuilding them per assertion.
#[cfg(test)]
fn zendesk_shape() -> &'static Connector {
    use donat_connectors::providers::zendesk;
    Box::leak(Box::new(
        zendesk::declaration_shape().expect("the Zendesk declaration shape is valid"),
    ))
}

#[cfg(test)]
fn woocommerce_shape() -> &'static Connector {
    use donat_connectors::providers::woocommerce;
    Box::leak(Box::new(
        woocommerce::declaration_shape().expect("the WooCommerce declaration shape is valid"),
    ))
}

#[cfg(test)]
fn zoho_crm_shape() -> &'static Connector {
    use donat_connectors::providers::zoho_crm;
    Box::leak(Box::new(
        zoho_crm::declaration_shape().expect("the Zoho CRM declaration shape is valid"),
    ))
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    /// Every declared plan collects the item list its own operation publishes.
    ///
    /// A walk writes its aggregate where the plan declared the items. If the
    /// operation reads its output from somewhere else, a deployment gets page
    /// one and no sign that the rest was fetched and dropped — which is the
    /// defect [[034-a-declaration-the-runtime-ignores-is-a-defect]] describes,
    /// one layer below the one this batch closed. The registry refuses such an
    /// instance at startup; this asserts it for every compiled module,
    /// including the operations no fixture happens to enable.
    #[test]
    fn every_declared_plan_collects_what_its_operation_publishes() {
        let mut walked = 0;
        for (connector, pagination) in wired_plans() {
            for operation in connector.operations() {
                let Some(plan) = pagination(operation.id()) else {
                    continue;
                };
                walked += 1;
                super::super::admits_a_walked_aggregate(plan, &operation.project())
                    .unwrap_or_else(|error| panic!("`{}`: {error}", connector.name()));
            }
        }
        assert!(
            walked >= 20,
            "the compiled table really does declare walks: {walked}"
        );
    }

    /// The three connectors of [[055-a-cursor-in-a-body-is-not-a-pagination-plan]]
    /// declare no plan for any operation, so every attempt of theirs is one
    /// request.
    #[test]
    fn the_connectors_with_a_body_carried_or_unendable_continuation_declare_no_plan() {
        for connector in [
            linear::connector(),
            sentry::connector(),
            telegram::connector(),
            paddle::connector(),
            mercado_pago::connector(),
        ] {
            for operation in connector.operations() {
                assert!(
                    no_pagination(operation.id()).is_none(),
                    "`{}.{}` is one request",
                    connector.name(),
                    operation.id()
                );
            }
        }
    }
}

#[cfg(test)]
mod batch_e_tests {
    use donat_connectors::sdk::{ConnectorErrorClass, Secret};

    use super::*;

    const TOKEN_SENTINEL: &str = "donat-provider-token-sentinel-do-not-log";

    fn body_gated(
        connector: &'static Connector,
        decode: fn(&Operation, u16, &HeaderMap, &[u8]) -> Result<JsonValue, ConnectorFailure>,
        pagination: PaginationLookup,
    ) -> BodyGatedRuntime {
        BodyGatedRuntime::compile(
            connector.clone(),
            Credential::from_fields([("secret", Secret::new(TOKEN_SENTINEL))]),
            &ConnectorConfiguration::default(),
            decode,
            pagination,
        )
        .expect("a configured instance compiles")
    }

    /// The runtime a deployment dispatches on walks exactly the plans its
    /// module declared — and Linear's, which declares none, walks nothing.
    ///
    /// This is the wiring rather than the declaration: `slack::pagination`
    /// having a plan proves nothing if the compiled runtime was handed
    /// `no_pagination` ([[058-a-declared-walk-is-the-executors-walk]]).
    #[test]
    fn a_batch_e_instance_walks_exactly_the_plans_its_module_declared() {
        let slack = body_gated(slack::connector(), slack::decode, slack::pagination);
        assert!(
            slack.pagination("conversation.list").is_some(),
            "a Slack collection is walked"
        );
        assert!(
            slack.pagination("user.info").is_none(),
            "a single-resource read is one request"
        );

        let linear = body_gated(linear::connector(), linear::decode, no_pagination);
        for operation in linear::connector().operations() {
            assert!(
                linear.pagination(operation.id()).is_none(),
                "Linear's cursor is a body variable, so `{}` is one page",
                operation.id()
            );
        }
    }

    /// The serving seam, not only the module: a `200` carrying a provider
    /// failure never becomes an activity success through the runtime a
    /// deployment actually dispatches on.
    #[test]
    fn a_body_gated_runtime_refuses_a_success_status_carrying_a_provider_failure() {
        let slack = body_gated(slack::connector(), slack::decode, slack::pagination);
        assert_eq!(
            slack
                .decode(
                    "conversation.info",
                    200,
                    &HeaderMap::new(),
                    br#"{"ok":true,"channel":{"id":"C1","name":"general"}}"#,
                )
                .expect("a documented success decodes"),
            json!({
                "id": "C1", "name": "general",
                "is_private": null, "is_archived": null,
            })
        );
        let failure = slack
            .decode(
                "conversation.info",
                200,
                &HeaderMap::new(),
                br#"{"ok":false,"error":"ratelimited"}"#,
            )
            .expect_err("a 200 with ok:false is never a success");
        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert_eq!(failure.provider_status(), Some(200));

        let linear = body_gated(linear::connector(), linear::decode, no_pagination);
        let failure = linear
            .decode(
                "issue.get",
                200,
                &HeaderMap::new(),
                br#"{"data":{"issue":null},"errors":[{"extensions":{"type":"forbidden"}}]}"#,
            )
            .expect_err("a 200 carrying GraphQL errors is never a success");
        assert_eq!(failure.class(), ConnectorErrorClass::Authentication);
        assert!(!failure.diagnostic().contains(TOKEN_SENTINEL));
    }

    /// Each Batch E instance applies the credential plan its provider publishes
    /// and nothing else — including Linear's, which sends the key as the whole
    /// `Authorization` value with no scheme.
    #[test]
    fn a_batch_e_instance_applies_only_its_declared_credential_plan() {
        let linear = body_gated(linear::connector(), linear::decode, no_pagination);
        let mut request = linear
            .plan("issue.get", &json!({ "id": "iss_1" }), "activity-1")
            .expect("the declared read renders");
        assert!(request.headers().get("authorization").is_none());
        linear
            .auth_plan()
            .expect("Linear declares a plan")
            .apply(linear.credential(), &mut request, None)
            .expect("the declared plan applies");
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(TOKEN_SENTINEL),
            "a Linear API key is the whole header value"
        );

        let slack = body_gated(slack::connector(), slack::decode, slack::pagination);
        let mut request = slack
            .plan("user.info", &json!({ "user": "W1" }), "activity-1")
            .expect("the declared read renders");
        slack
            .auth_plan()
            .expect("Slack declares a plan")
            .apply(slack.credential(), &mut request, None)
            .expect("the declared plan applies");
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some(format!("Bearer {TOKEN_SENTINEL}").as_str())
        );
    }

    /// Every Batch E module admits exactly the operations its declaration
    /// classified as executable, through the runtime the registry publishes.
    #[test]
    fn a_batch_e_instance_admits_only_executable_operations() {
        let slack = body_gated(slack::connector(), slack::decode, slack::pagination);
        assert!(slack.admit_operation("conversation.info").is_ok());
        // ADR 063: the post is admitted by the module and gated again at
        // process compilation, which is where the activity's opt-in is read.
        assert!(slack.admit_operation("message.post").is_ok());
        assert!(slack.admit_operation("message.update").is_err());
        assert!(slack.admit_operation("not.an.operation").is_err());

        let linear = body_gated(linear::connector(), linear::decode, no_pagination);
        assert!(linear.admit_operation("issue.list").is_ok());
        assert!(linear.admit_operation("issue.update").is_err());
    }

    /// Jira's declaration is completed by one deployment's account address, and
    /// its templated host comes only from that deployment's configuration.
    #[test]
    fn the_jira_declaration_carries_its_site_and_its_account_address() {
        let connector =
            jira::connector("integrations@example.test").expect("a valid address declares");
        let runtime = BodyGatedRuntime::compile(
            connector,
            Credential::from_fields([
                ("secret", Secret::new(TOKEN_SENTINEL)),
                (jira::EMAIL, Secret::new("integrations@example.test")),
            ]),
            &ConnectorConfiguration::from_deployment([(jira::SITE, "acme")]),
            |operation, status, headers, body| {
                if operation.is_success(status) {
                    operation.decode_response(status, body)
                } else {
                    Err(jira::error_map().classify(status, headers, body))
                }
            },
            jira::pagination,
        )
        .expect("a configured Jira instance compiles");

        let mut request = runtime
            .plan(
                "issue.get",
                &json!({ "issueIdOrKey": "ACM-42", "fields": "summary" }),
                "activity-1",
            )
            .expect("the declared read renders");
        assert_eq!(
            request.url().as_str(),
            "https://acme.atlassian.net/rest/api/3/issue/ACM%2D42?fields=summary"
        );
        runtime
            .auth_plan()
            .expect("Jira declares a basic plan")
            .apply(runtime.credential(), &mut request, None)
            .expect("the declared plan applies");
        let applied = request
            .headers()
            .get("authorization")
            .expect("the basic plan sets one")
            .to_str()
            .expect("a basic credential is visible ASCII")
            .to_owned();
        assert!(applied.starts_with("Basic "), "{applied}");
        assert!(!applied.contains(TOKEN_SENTINEL));

        assert!(jira::connector("not-an-email").is_err());
    }
}
