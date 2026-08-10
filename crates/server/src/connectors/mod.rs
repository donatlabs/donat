//! Compiled connector registry.
//!
//! Connector instances are selected from deploy-time metadata.  This module
//! intentionally contains a fixed table of in-binary modules: it never loads
//! code, follows a package URL, starts a subprocess, or discovers anything on
//! the network.  Runtime construction resolves only the environment variables
//! named by that metadata and publishes an immutable registry before serving.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, LazyLock};

use donat_connector_abi::{OperationId, TriggerId, VerifiedInboundEvent};
use donat_connector_catalog::{OperationSpec, TriggerSpec};
use donat_connectors::sdk::Connector;
use donat_metadata::Metadata;
use futures_util::future::BoxFuture;
use serde_json::{Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::credentials::runtime::{CredentialRuntime, CredentialRuntimeError};
use crate::state::{ConnectorConfigError, ConnectorStartupError, validate_connector_startup};

mod catalog;
// The OAuth2 client-credentials exchange the executor makes per attempt. It is
// beside `credential` rather than inside it because the two credentials have
// opposite lifetimes: that one is stored and refreshed, this one is minted and
// dropped, and nothing here can reach the credential store.
mod client_credentials;
mod credential;
pub mod http;
mod provider;
pub mod stripe;

// The inbound verification failure is the SDK's, exactly as the activity
// failure and the transport are: one closed contract has one definition, and
// the ingress route, the connector module, and the verifier all name the same
// six reasons with the same codes.
pub use donat_connectors::sdk::WebhookRejection;

/// SHA-256 of a recursively key-sorted JSON value. Connector input is a JSON
/// object contract, so equivalent object order must never produce a different
/// durable activity identity.
pub(crate) fn canonical_json_sha256(input: &JsonValue) -> String {
    fn canonical(value: &JsonValue) -> JsonValue {
        match value {
            JsonValue::Object(object) => BTreeMap::from_iter(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value))),
            )
            .into_iter()
            .collect::<JsonMap<String, JsonValue>>()
            .into(),
            JsonValue::Array(values) => JsonValue::Array(values.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }

    let canonical = serde_json::to_vec(&canonical(input))
        .expect("canonical connector input JSON always serializes");
    format!("{:x}", Sha256::digest(canonical))
}

// The eight closed activity-failure classes and the provider-safe failure they
// carry are owned by the SDK, exactly as transport is.  Two definitions of one
// closed contract is a defect: a `retry_on` name a Process declares, a class a
// connector returns, and a class the journal records have to be the same thing.
//
// Deployment metadata and startup errors keep their own separate error types: a
// connector that cannot start never reaches activity retry routing.
//
// The SDK's `safe_message` is `&'static str` rather than `String`, and that is
// the point of the merge rather than a cost of it — a message borrowed from a
// provider response does not typecheck, so "no provider text is ever forwarded"
// is structural instead of reviewed.  Nothing here needs a message built at
// runtime: every message this crate produces is a literal written in this
// workspace, and the redacted diagnostic that carries provider *facts* (status,
// retry-after, correlation IDs) is assembled from typed fields.
pub use donat_connectors::sdk::{ConnectorErrorClass, ConnectorFailure};

/// The JSON value produced by a connector's declared response extraction.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorSuccess {
    pub output: JsonValue,
    /// SHA-256 of canonical JSON input, suitable for a future durable activity
    /// journal. The raw input is not retained by the connector boundary.
    pub request_fingerprint: String,
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

/// The runtime contract every compiled module in this binary speaks. It enters
/// each operation's configuration fingerprint, so a deployment can tell that a
/// pinned operation was compiled against a different runtime.
pub(crate) const CONNECTOR_RUNTIME_ABI: u32 = 1;

/// Identity of a connector module compiled into this exact binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectorDefinition {
    pub module_name: &'static str,
    pub semantic_version: &'static str,
    pub runtime_abi: u32,
}

impl ConnectorDefinition {
    /// The identity of a connector declaration.  The declaration is the single
    /// source of a module's name and contract version: a table entry does not
    /// get to disagree with the connector it publishes.
    pub(crate) const fn of(connector: &'static Connector) -> Self {
        Self {
            module_name: connector.name(),
            semantic_version: connector.version(),
            runtime_abi: CONNECTOR_RUNTIME_ABI,
        }
    }
}

/// Common minimum for compiled modules.  Execution remains module-specific so
/// callers cannot turn this into a raw HTTP request interface.
pub trait ConnectorModule: Send + Sync {
    fn definition(&self) -> ConnectorDefinition;
}

pub(crate) const HTTP_DEFINITION: ConnectorDefinition = ConnectorDefinition {
    module_name: "http",
    semantic_version: "0.1.0",
    runtime_abi: CONNECTOR_RUNTIME_ABI,
};
pub(crate) const STRIPE_DEFINITION: ConnectorDefinition = ConnectorDefinition {
    module_name: "stripe",
    semantic_version: "0.1.0",
    runtime_abi: CONNECTOR_RUNTIME_ABI,
};

/// One deployment-selected connector instance, as the registry sees it.
///
/// The registry knows only this trait: everything module-specific — which
/// operations exist, how one executes, whether the module has a verifier at all
/// — stays inside the module. Replacing the old per-module enum with it is what
/// makes adding a connector one module file and one line of
/// [`COMPILED_MODULES`], instead of a new variant in every lookup here.
pub(crate) trait RegisteredConnector: Send + Sync {
    fn execute<'a>(
        &'a self,
        operation: &'a str,
        input: JsonValue,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>>;

    /// Execute one operation with an applied `Authorization` header.
    ///
    /// A module that cannot apply an OAuth2 credential does not implement this,
    /// and never has to: a deployment that declares `config.oauth2` on such a
    /// module is refused before a listener opens, by that module's own metadata
    /// rules. The default is therefore a refusal rather than a silent fall back
    /// to the unauthenticated path — which would be exactly the defect
    /// [[034-a-declaration-the-runtime-ignores-is-a-defect]] names.
    fn execute_authorized<'a>(
        &'a self,
        _operation: &'a str,
        _input: JsonValue,
        _idempotency_key: &'a str,
        _deadline: tokio::time::Instant,
        _authorization: &'a str,
    ) -> BoxFuture<'a, Result<AuthorizedAttempt, ConnectorFailure>> {
        Box::pin(async {
            Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_credential_not_applicable",
                "connector module cannot apply an OAuth2 credential to its requests",
            ))
        })
    }

    /// The `Authorization` scheme this module's provider publishes for its
    /// stored OAuth2 access tokens.
    ///
    /// RFC 6750's `Bearer` is the default and is what every provider in this
    /// workspace but one publishes. Zoho CRM publishes `Zoho-oauthtoken` and
    /// uses it in every example on every endpoint page, so the scheme is a
    /// property of the connector's declaration rather than of the credential
    /// lifecycle: the lifecycle formats the header this answer names, and the
    /// module's own auth plan refuses a header in any other shape. A module
    /// whose credential is not a stored token never gets asked.
    fn oauth2_authorization_scheme(&self) -> &'static str {
        donat_connectors::sdk::BEARER_SCHEME
    }

    /// The immutable, non-secret deployment fingerprint of one compiled
    /// operation.
    fn configuration_fingerprint(&self, operation: &str) -> Option<&str>;

    /// The optional scalar input that serializes one compiled operation.
    fn serialization_key_input(&self, operation: &str) -> Option<&str>;

    /// The declarative HTTP connector behind this instance, if it is one.
    fn http_connector(&self) -> Option<&http::HttpConnector> {
        None
    }

    /// The one compiled inbound verifier this instance publishes, if any.
    fn webhook(&self) -> Option<WebhookInstance<'_>> {
        None
    }

    fn trigger_spec(&self, _source_name: &str, _trigger: TriggerId) -> Option<Arc<TriggerSpec>> {
        None
    }

    fn trigger_configuration_fingerprint(&self, _trigger: TriggerId) -> Option<&str> {
        None
    }
}

/// What one authorized provider attempt concluded.
///
/// The `401` case carries the failure the operation's own classification
/// produced, so that a refresh and a replay can be tried without the credential
/// seam discarding what the operation declared a `401` to be.
pub(crate) enum AuthorizedAttempt {
    Done(ConnectorSuccess),
    Unauthorized(ConnectorFailure),
}

/// Everything a module needs to compile one deployment-selected instance.
pub(crate) struct ModuleContext<'a> {
    pub(crate) metadata: &'a Metadata,
    pub(crate) instance: &'a donat_metadata::ConnectorInstance,
    pub(crate) definition: ConnectorDefinition,
    /// The module's own static declaration, which is where its origin,
    /// credential contract, operations, and effect classes come from.
    pub(crate) connector: &'a Connector,
    /// The executable, catalog-owned operations this instance publishes to its
    /// source. An inventory-only operation deliberately never lands here.
    pub(crate) executable_specs: &'a mut BTreeMap<OperationId, Arc<OperationSpec>>,
    /// The single Postgres source connector instances bind to.
    pub(crate) source_name: &'a str,
}

/// How the table holds one module's declaration.
///
/// Almost every connector is a constant: name, contract version, origin,
/// credential, operations with their effect classes, and triggers are all fixed
/// before any deployment exists, and the table holds a `&'static Connector`.
///
/// Twilio is not, and the difference is the provider's, not a design choice
/// here: Twilio's HTTP Basic username *is* the Account SID, and the SDK's
/// `AuthPlan::basic` takes its username where the plan is built. A `&'static`
/// declaration would therefore have to carry a placeholder username — a
/// credential contract that does not describe what reaches the wire. So the
/// table holds the module's *identity* plus a function that builds its
/// declaration from one deployment's own configuration.
///
/// This is not a dynamic registry. The variant is closed, the function is a
/// compiled `fn` item, its one input is deploy-time metadata this binary
/// already validated, and everything a declaration decides — the operation set,
/// every effect class, the origin, the auth plan — is still fixed at compile
/// time. What varies is one configured value inside it.
pub(crate) enum ModuleDeclaration {
    Static(&'static Connector),
    PerDeployment {
        name: &'static str,
        version: &'static str,
        declare: fn(&donat_metadata::ConnectorInstance) -> Option<Connector>,
    },
}

impl ModuleDeclaration {
    pub(crate) const fn name(&self) -> &'static str {
        match self {
            Self::Static(connector) => connector.name(),
            Self::PerDeployment { name, .. } => name,
        }
    }

    /// The declaration one instance is validated and compiled against.
    ///
    /// `None` means this deployment's configuration cannot complete the
    /// declaration at all — a missing or malformed Twilio Account SID — and the
    /// module's own validator has already reported which key earned it.
    pub(crate) fn resolve(
        &self,
        instance: &donat_metadata::ConnectorInstance,
    ) -> Option<Connector> {
        match self {
            Self::Static(connector) => Some((*connector).clone()),
            Self::PerDeployment { declare, .. } => declare(instance),
        }
    }

    /// The declaration when it is a constant. A per-deployment one has no
    /// `&'static` form by construction.
    pub(crate) const fn as_static(&self) -> Option<&'static Connector> {
        match self {
            Self::Static(connector) => Some(connector),
            Self::PerDeployment { .. } => None,
        }
    }
}

/// Everything a module contributes to the table.
///
/// The key is the declaration: the whole description of the provider — name,
/// contract version, origin, credential, operations with their effect classes,
/// triggers — and the two function pointers are the only module-specific
/// behaviour left. Adding a connector is one module file and one line of
/// [`compiled_modules`].
pub(crate) struct CompiledModule {
    declaration: ModuleDeclaration,
    build:
        fn(&mut ModuleContext<'_>) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError>,
    /// The module's own deploy-time metadata rules.
    ///
    /// This used to be a `match` on the module name inside `state.rs`, which
    /// meant adding a connector required editing a file that has nothing to do
    /// with connectors and forgetting to was a silent hole. A module now
    /// carries its rules with it.
    validate_metadata: fn(&donat_metadata::ConnectorInstance, &str, &mut Vec<ConnectorConfigError>),
}

impl CompiledModule {
    pub(crate) const fn declaration(&self) -> &ModuleDeclaration {
        &self.declaration
    }

    pub(crate) const fn definition(&self) -> ConnectorDefinition {
        match &self.declaration {
            // The declaration remains the single source of a module's identity.
            ModuleDeclaration::Static(connector) => ConnectorDefinition::of(connector),
            ModuleDeclaration::PerDeployment { name, version, .. } => ConnectorDefinition {
                module_name: name,
                semantic_version: version,
                runtime_abi: CONNECTOR_RUNTIME_ABI,
            },
        }
    }
}

/// One table line, for a module whose declaration is a constant.
const fn compiled(
    connector: &'static Connector,
    build: fn(
        &mut ModuleContext<'_>,
    ) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError>,
    validate_metadata: fn(&donat_metadata::ConnectorInstance, &str, &mut Vec<ConnectorConfigError>),
) -> CompiledModule {
    CompiledModule {
        declaration: ModuleDeclaration::Static(connector),
        build,
        validate_metadata,
    }
}

/// The complete module table.  Nothing is loaded, discovered, or resolved at
/// runtime: every entry is built once, from constants and from this
/// deployment's own already-validated metadata.
pub(crate) fn compiled_modules() -> &'static [CompiledModule] {
    use provider::modules;

    static TABLE: LazyLock<Vec<CompiledModule>> = LazyLock::new(|| {
        vec![
            compiled(
                http::connector(),
                http::build_registered_instance,
                http::validate_instance_metadata,
            ),
            compiled(
                stripe::connector(),
                stripe::build_registered_instance,
                stripe::validate_instance_metadata,
            ),
            // Batch A: the API-key REST connectors (spec 012).
            compiled(
                modules::airtable_module::connector(),
                modules::airtable_module::build_registered_instance,
                modules::airtable_module::validate_instance_metadata,
            ),
            compiled(
                modules::sendgrid_module::connector(),
                modules::sendgrid_module::build_registered_instance,
                modules::sendgrid_module::validate_instance_metadata,
            ),
            compiled(
                modules::postmark_module::connector(),
                modules::postmark_module::build_registered_instance,
                modules::postmark_module::validate_instance_metadata,
            ),
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::twilio::NAME,
                    version: donat_connectors::providers::twilio::VERSION,
                    declare: modules::twilio_module::declare,
                },
                build: modules::twilio_module::build_registered_instance,
                validate_metadata: modules::twilio_module::validate_instance_metadata,
            },
            compiled(
                modules::openai_module::connector(),
                modules::openai_module::build_registered_instance,
                modules::openai_module::validate_instance_metadata,
            ),
            compiled(
                modules::typeform_module::connector(),
                modules::typeform_module::build_registered_instance,
                modules::typeform_module::validate_instance_metadata,
            ),
            // Batch F: the AWS connectors (spec 017).
            compiled(
                modules::aws_s3_module::connector(),
                modules::aws_s3_module::build_registered_instance,
                modules::aws_s3_module::validate_instance_metadata,
            ),
            compiled(
                modules::aws_sqs_module::connector(),
                modules::aws_sqs_module::build_registered_instance,
                modules::aws_sqs_module::validate_instance_metadata,
            ),
            compiled(
                modules::aws_ses_module::connector(),
                modules::aws_ses_module::build_registered_instance,
                modules::aws_ses_module::validate_instance_metadata,
            ),
            // Batch B: the webhook-bearing connectors (spec 013).
            compiled(
                modules::github_module::connector(),
                modules::github_module::build_registered_instance,
                modules::github_module::validate_instance_metadata,
            ),
            compiled(
                modules::shopify_module::connector(),
                modules::shopify_module::build_registered_instance,
                modules::shopify_module::validate_instance_metadata,
            ),
            compiled(
                modules::telegram_module::connector(),
                modules::telegram_module::build_registered_instance,
                modules::telegram_module::validate_instance_metadata,
            ),
            compiled(
                modules::calendly_module::connector(),
                modules::calendly_module::build_registered_instance,
                modules::calendly_module::validate_instance_metadata,
            ),
            compiled(
                modules::sentry_module::connector(),
                modules::sentry_module::build_registered_instance,
                modules::sentry_module::validate_instance_metadata,
            ),
            // Batch C: the Google Workspace connectors (spec 014). These four
            // are the first whose credential is a stored authorization-code
            // OAuth2 token rather than deploy-time configuration.
            compiled(
                provider::google::google_sheets_module::connector(),
                provider::google::google_sheets_module::build_registered_instance,
                provider::google::google_sheets_module::validate_instance_metadata,
            ),
            compiled(
                provider::google::google_drive_module::connector(),
                provider::google::google_drive_module::build_registered_instance,
                provider::google::google_drive_module::validate_instance_metadata,
            ),
            compiled(
                provider::google::google_gmail_module::connector(),
                provider::google::google_gmail_module::build_registered_instance,
                provider::google::google_gmail_module::validate_instance_metadata,
            ),
            compiled(
                provider::google::google_calendar_module::connector(),
                provider::google::google_calendar_module::build_registered_instance,
                provider::google::google_calendar_module::validate_instance_metadata,
            ),
            // Batch E: the product SaaS connectors (spec 016).
            compiled(
                modules::slack_module::connector(),
                modules::slack_module::build_registered_instance,
                modules::slack_module::validate_instance_metadata,
            ),
            compiled(
                modules::linear_module::connector(),
                modules::linear_module::build_registered_instance,
                modules::linear_module::validate_instance_metadata,
            ),
            compiled(
                modules::notion_module::connector(),
                modules::notion_module::build_registered_instance,
                modules::notion_module::validate_instance_metadata,
            ),
            compiled(
                modules::intercom_module::connector(),
                modules::intercom_module::build_registered_instance,
                modules::intercom_module::validate_instance_metadata,
            ),
            compiled(
                modules::hubspot_module::connector(),
                modules::hubspot_module::build_registered_instance,
                modules::hubspot_module::validate_instance_metadata,
            ),
            // Batch J: the payments and billing connectors (spec 026).
            compiled(
                modules::paddle_module::connector(),
                modules::paddle_module::build_registered_instance,
                modules::paddle_module::validate_instance_metadata,
            ),
            compiled(
                modules::mercado_pago_module::connector(),
                modules::mercado_pago_module::build_registered_instance,
                modules::mercado_pago_module::validate_instance_metadata,
            ),
            // Xero's credential is a stored authorization-code OAuth2 token,
            // like Batch C and Batch D, and its organisation is a deploy-time
            // header rather than a host.
            compiled(
                modules::xero_module::connector(),
                modules::xero_module::build_registered_instance,
                modules::xero_module::validate_instance_metadata,
            ),
            // PayPal's credential is neither: it is an OAuth2 client-credentials
            // token the executor mints per attempt from two configured secrets
            // and never stores
            // ([[072-a-minted-credential-is-spent-inside-one-attempt]]).
            compiled(
                modules::paypal_module::connector(),
                modules::paypal_module::build_registered_instance,
                modules::paypal_module::validate_instance_metadata,
            ),
            // Jira's HTTP Basic username is the account's own email address,
            // and `AuthPlan::basic` takes it where the plan is built, so this
            // declaration is completed by one deployment exactly as Twilio's is.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::jira::NAME,
                    version: donat_connectors::providers::jira::VERSION,
                    declare: modules::jira_module::declare,
                },
                build: modules::jira_module::build_registered_instance,
                validate_metadata: modules::jira_module::validate_instance_metadata,
            },
            // Batch D: the Microsoft 365 connectors (spec 015). Like Batch C
            // their credential is a stored authorization-code OAuth2 token; the
            // identity platform returns a new refresh token on every exchange,
            // which is what makes the rotation path load-bearing here.
            compiled(
                provider::microsoft::microsoft_outlook_module::connector(),
                provider::microsoft::microsoft_outlook_module::build_registered_instance,
                provider::microsoft::microsoft_outlook_module::validate_instance_metadata,
            ),
            compiled(
                provider::microsoft::microsoft_teams_module::connector(),
                provider::microsoft::microsoft_teams_module::build_registered_instance,
                provider::microsoft::microsoft_teams_module::validate_instance_metadata,
            ),
            compiled(
                provider::microsoft::microsoft_excel_module::connector(),
                provider::microsoft::microsoft_excel_module::build_registered_instance,
                provider::microsoft::microsoft_excel_module::validate_instance_metadata,
            ),
            compiled(
                provider::microsoft::microsoft_onedrive_module::connector(),
                provider::microsoft::microsoft_onedrive_module::build_registered_instance,
                provider::microsoft::microsoft_onedrive_module::validate_instance_metadata,
            ),
            // Batch G: the CRM and helpdesk connectors (spec 023). Four of the
            // six have a per-tenant host, and four are declarations one
            // deployment completes — an account address, a consumer key, a data
            // centre — so they are per-deployment entries rather than constants.
            compiled(
                provider::crm::pipedrive_module::connector(),
                provider::crm::pipedrive_module::build_registered_instance,
                provider::crm::pipedrive_module::validate_instance_metadata,
            ),
            compiled(
                provider::crm::freshdesk_module::connector(),
                provider::crm::freshdesk_module::build_registered_instance,
                provider::crm::freshdesk_module::validate_instance_metadata,
            ),
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::zendesk::NAME,
                    version: donat_connectors::providers::zendesk::VERSION,
                    declare: provider::crm::zendesk_module::declare,
                },
                build: provider::crm::zendesk_module::build_registered_instance,
                validate_metadata: provider::crm::zendesk_module::validate_instance_metadata,
            },
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::woocommerce::NAME,
                    version: donat_connectors::providers::woocommerce::VERSION,
                    declare: provider::crm::woocommerce_module::declare,
                },
                build: provider::crm::woocommerce_module::build_registered_instance,
                validate_metadata: provider::crm::woocommerce_module::validate_instance_metadata,
            },
            compiled(
                provider::crm::salesforce_module::connector(),
                provider::crm::salesforce_module::build_registered_instance,
                provider::crm::salesforce_module::validate_instance_metadata,
            ),
            // Zoho's origin is one of eight data centres it publishes, chosen
            // from a compiled table rather than filled into a template, so its
            // declaration is built from one deployment's configured region.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::zoho_crm::NAME,
                    version: donat_connectors::providers::zoho_crm::VERSION,
                    declare: provider::crm::zoho_crm_module::declare,
                },
                build: provider::crm::zoho_crm_module::build_registered_instance,
                validate_metadata: provider::crm::zoho_crm_module::validate_instance_metadata,
            },
            // Batch H: the project-tracking and collaboration connectors
            // (spec 024). Five have a fixed origin and a configured credential;
            // Basecamp's declaration is one a deployment completes, because its
            // account id is the first path segment of every URL it renders and
            // the `User-Agent` its provider demands is on every request.
            compiled(
                provider::project::asana_module::connector(),
                provider::project::asana_module::build_registered_instance,
                provider::project::asana_module::validate_instance_metadata,
            ),
            compiled(
                provider::project::trello_module::connector(),
                provider::project::trello_module::build_registered_instance,
                provider::project::trello_module::validate_instance_metadata,
            ),
            compiled(
                provider::project::clickup_module::connector(),
                provider::project::clickup_module::build_registered_instance,
                provider::project::clickup_module::validate_instance_metadata,
            ),
            compiled(
                provider::project::monday_module::connector(),
                provider::project::monday_module::build_registered_instance,
                provider::project::monday_module::validate_instance_metadata,
            ),
            compiled(
                provider::project::todoist_module::connector(),
                provider::project::todoist_module::build_registered_instance,
                provider::project::todoist_module::validate_instance_metadata,
            ),
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::basecamp::NAME,
                    version: donat_connectors::providers::basecamp::VERSION,
                    declare: provider::project::basecamp_module::declare,
                },
                build: provider::project::basecamp_module::build_registered_instance,
                validate_metadata: provider::project::basecamp_module::validate_instance_metadata,
            },
            // Batch I: the storage and messaging connectors (spec 025).
            // `dropbox` and `dropbox_content` are one provider on two origins,
            // so they are two entries here: a connector's origin is a
            // compile-time constant, and a deployment that needs both names
            // both
            // ([[074-a-second-origin-is-a-second-connector-and-a-download-is-composed-under-its-bound]]).
            compiled(
                provider::storage::dropbox_module::connector(),
                provider::storage::dropbox_module::build_registered_instance,
                provider::storage::dropbox_module::validate_instance_metadata,
            ),
            compiled(
                provider::storage::dropbox_content_module::connector(),
                provider::storage::dropbox_content_module::build_registered_instance,
                provider::storage::dropbox_content_module::validate_instance_metadata,
            ),
            compiled(
                provider::storage::box_module::connector(),
                provider::storage::box_module::build_registered_instance,
                provider::storage::box_module::validate_instance_metadata,
            ),
            compiled(
                provider::storage::discord_module::connector(),
                provider::storage::discord_module::build_registered_instance,
                provider::storage::discord_module::validate_instance_metadata,
            ),
            compiled(
                provider::storage::mattermost_module::connector(),
                provider::storage::mattermost_module::build_registered_instance,
                provider::storage::mattermost_module::validate_instance_metadata,
            ),
            compiled(
                provider::storage::mailchimp_module::connector(),
                provider::storage::mailchimp_module::build_registered_instance,
                provider::storage::mailchimp_module::validate_instance_metadata,
            ),
            compiled(
                provider::storage::zoom_module::connector(),
                provider::storage::zoom_module::build_registered_instance,
                provider::storage::zoom_module::validate_instance_metadata,
            ),
            // Batch K: the development and monitoring connectors (spec 027).
            // Four have a fixed origin; GitLab's and Grafana's is the
            // deployment's own instance, named as a whole origin
            // ([[082-an-instance-a-deployment-operates-is-a-whole-origin-it-names]]).
            // Two are declarations one deployment completes: Bitbucket's HTTP
            // Basic username is an Atlassian account address, and PagerDuty's
            // `From` is the account user every write is attributed to.
            compiled(
                provider::devops::gitlab_module::connector(),
                provider::devops::gitlab_module::build_registered_instance,
                provider::devops::gitlab_module::validate_instance_metadata,
            ),
            compiled(
                provider::devops::grafana_module::connector(),
                provider::devops::grafana_module::build_registered_instance,
                provider::devops::grafana_module::validate_instance_metadata,
            ),
            compiled(
                provider::devops::uptimerobot_module::connector(),
                provider::devops::uptimerobot_module::build_registered_instance,
                provider::devops::uptimerobot_module::validate_instance_metadata,
            ),
            compiled(
                provider::devops::cloudflare_module::connector(),
                provider::devops::cloudflare_module::build_registered_instance,
                provider::devops::cloudflare_module::validate_instance_metadata,
            ),
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::bitbucket::NAME,
                    version: donat_connectors::providers::bitbucket::VERSION,
                    declare: provider::devops::bitbucket_module::declare,
                },
                build: provider::devops::bitbucket_module::build_registered_instance,
                validate_metadata: provider::devops::bitbucket_module::validate_instance_metadata,
            },
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::pagerduty::NAME,
                    version: donat_connectors::providers::pagerduty::VERSION,
                    declare: provider::devops::pagerduty_module::declare,
                },
                build: provider::devops::pagerduty_module::build_registered_instance,
                validate_metadata: provider::devops::pagerduty_module::validate_instance_metadata,
            },
            // Batch L, the forms half (spec 028). Jotform serves one account
            // from one of three published API URLs, chosen from a compiled
            // table rather than filled into a template, so its declaration is
            // built from one deployment's configured region.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::jotform::NAME,
                    version: donat_connectors::providers::jotform::VERSION,
                    declare: provider::forms::jotform_module::declare,
                },
                build: provider::forms::jotform_module::build_registered_instance,
                validate_metadata: provider::forms::jotform_module::validate_instance_metadata,
            },
            compiled(
                provider::forms::surveymonkey_module::connector(),
                provider::forms::surveymonkey_module::build_registered_instance,
                provider::forms::surveymonkey_module::validate_instance_metadata,
            ),
            compiled(
                provider::forms::cal_com_module::connector(),
                provider::forms::cal_com_module::build_registered_instance,
                provider::forms::cal_com_module::validate_instance_metadata,
            ),
            // Acuity's HTTP Basic username is the account's numeric User ID, so
            // its declaration is built from one deployment's configuration.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::acuity::NAME,
                    version: donat_connectors::providers::acuity::VERSION,
                    declare: provider::forms::acuity_module::declare,
                },
                build: provider::forms::acuity_module::build_registered_instance,
                validate_metadata: provider::forms::acuity_module::validate_instance_metadata,
            },
            // Batch L, the scheduling and people half (spec 028). Harvest's
            // declaration is one a deployment completes: its account id is a
            // `Harvest-Account-Id` header on every request and the `User-Agent`
            // its provider demands is on every request too. Neither is a
            // secret — the secret is the Personal Access Token, and it reaches
            // the wire only through the declared auth plan.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::harvest::NAME,
                    version: donat_connectors::providers::harvest::VERSION,
                    declare: provider::people::harvest_module::declare,
                },
                build: provider::people::harvest_module::build_registered_instance,
                validate_metadata: provider::people::harvest_module::validate_instance_metadata,
            },
            // BambooHR's per-company part is one host label, which the SDK
            // resolves from configuration, so its declaration stays a constant.
            compiled(
                provider::people::bamboohr_module::connector(),
                provider::people::bamboohr_module::build_registered_instance,
                provider::people::bamboohr_module::validate_instance_metadata,
            ),
            // Clockify's per-tenant part is a path segment rather than a host,
            // so its declaration is built with the workspace compiled in.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::clockify::NAME,
                    version: donat_connectors::providers::clockify::VERSION,
                    declare: provider::people::clockify_module::declare,
                },
                build: provider::people::clockify_module::build_registered_instance,
                validate_metadata: provider::people::clockify_module::validate_instance_metadata,
            },
            // Eventbrite's organization is a path segment of its event
            // collection and its event create, so its declaration is built with
            // the organization compiled in.
            CompiledModule {
                declaration: ModuleDeclaration::PerDeployment {
                    name: donat_connectors::providers::eventbrite::NAME,
                    version: donat_connectors::providers::eventbrite::VERSION,
                    declare: provider::people::eventbrite_module::declare,
                },
                build: provider::people::eventbrite_module::build_registered_instance,
                validate_metadata: provider::people::eventbrite_module::validate_instance_metadata,
            },
        ]
    });
    &TABLE
}

/// The `&'static [&'static Connector]` table of spec 010 §11.
///
/// It is every declaration that is a constant, which is every module except
/// `twilio` — see [`ModuleDeclaration`] for why that one has no `&'static`
/// form. [`ConnectorRegistry::built_in_module_names`] enumerates all of them.
pub fn connectors() -> Vec<&'static Connector> {
    compiled_modules()
        .iter()
        .filter_map(|module| module.declaration().as_static())
        .collect()
}

/// The compiled module a deployment's `module` field selects, if any.
pub(crate) fn compiled_module(module: &str) -> Option<&'static CompiledModule> {
    compiled_modules()
        .iter()
        .find(|compiled| compiled.declaration.name() == module)
}

/// Apply one module's own deploy-time metadata rules.
pub(crate) fn validate_module_metadata(
    module: &CompiledModule,
    instance: &donat_metadata::ConnectorInstance,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
) {
    (module.validate_metadata)(instance, path, errors);
}

/// The operations a deployment may enable on a connector whose operation set
/// is declared statically (spec 010 §11).
///
/// An unknown name and an inventory-only effect class are both refused here,
/// before a listener opens. A connector whose operations come from deployment
/// metadata instead — the declarative `http` module — declares none, and its
/// own validator classifies each compiled operation.
pub(crate) fn validate_enabled_operations(
    connector: &Connector,
    instance: &donat_metadata::ConnectorInstance,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
) {
    if connector.operations().is_empty() {
        return;
    }
    for (index, operation) in instance.operations.iter().enumerate() {
        if let Err(rejection) = connector.admit_operation(&operation.name) {
            errors.push(ConnectorConfigError::new(
                format!("{path}.operations[{index}].name"),
                format!(
                    "connector operation `{}` on module `{}`: {}",
                    operation.name,
                    connector.name(),
                    rejection.message()
                ),
            ));
        }
    }
}

pub(crate) struct CompiledWebhookTrigger {
    pub(crate) source_name: String,
    pub(crate) spec: Arc<TriggerSpec>,
    pub(crate) configuration_fingerprint: String,
}

/// One immutable deployment-selected webhook verifier. The verifier remains
/// module-owned, while its normalized behavior is described by the exact
/// catalog-owned trigger snapshot consumed by Process compilation.
pub struct WebhookInstance<'a> {
    pub(crate) source_name: &'a str,
    pub(crate) delivery: WebhookDelivery<'a>,
}

/// How far a verified delivery of this instance can currently travel.
///
/// The two arms are not a design choice, they are where the product is: the
/// Stripe module has a Process-owned inbound transaction to commit a verified
/// event into, and the connectors of spec 013 do not have one yet. Rather than
/// give the second kind a half-built correlation, the route answers `503` for
/// them and persists nothing — see
/// `knowledgebase/declarative-saas/decisions/053-*`.
pub(crate) enum WebhookDelivery<'a> {
    /// A trigger whose inbound transaction has landed: the route commits the
    /// verified event and acknowledges it.
    Correlated {
        trigger: &'a TriggerSpec,
        connector: &'a stripe::StripeConnector,
    },
    /// A trigger that verifies and rejects, and nothing else.
    Verified(&'a provider::ProviderWebhook),
}

/// What a successful verification produced.
pub enum VerifiedDelivery {
    /// A normalized event the Process-owned inbound transaction can commit.
    Correlated(Box<VerifiedInboundEvent>),
    /// The delivery is authentic and there is no inbound transaction to commit
    /// it to. Nothing has been parsed beyond what verification needed, nothing
    /// has been stored, and the route answers `503`.
    Unacknowledged,
}

impl WebhookInstance<'_> {
    pub fn source_name(&self) -> &str {
        self.source_name
    }

    /// The catalog-owned trigger snapshot, for the instance that publishes one.
    pub fn trigger(&self) -> Option<&TriggerSpec> {
        match &self.delivery {
            WebhookDelivery::Correlated { trigger, .. } => Some(trigger),
            WebhookDelivery::Verified(_) => None,
        }
    }

    /// The raw-body ceiling this route applies *before* it reads a body, so an
    /// oversized delivery is refused before any MAC is computed over it.
    pub fn raw_body_max_bytes(&self) -> usize {
        match &self.delivery {
            WebhookDelivery::Correlated { trigger, .. } => match trigger {
                TriggerSpec::Webhook {
                    raw_body_max_bytes, ..
                } => raw_body_max_bytes.get() as usize,
                TriggerSpec::Poll { .. } => {
                    unreachable!("an HTTP webhook route cannot retain a poll trigger")
                }
            },
            WebhookDelivery::Verified(webhook) => webhook.raw_body_max_bytes(),
        }
    }

    pub fn verify(
        &self,
        headers: &axum::http::HeaderMap,
        raw_body: &[u8],
    ) -> Result<VerifiedDelivery, WebhookRejection> {
        match &self.delivery {
            WebhookDelivery::Correlated { connector, .. } => connector
                .verify_completed_webhook(headers, raw_body)
                .map(|event| VerifiedDelivery::Correlated(Box::new(event))),
            WebhookDelivery::Verified(webhook) => webhook
                .verify(headers, raw_body)
                .map(|()| VerifiedDelivery::Unacknowledged),
        }
    }
}

type OperationSpecHandles =
    BTreeMap<String, BTreeMap<String, BTreeMap<OperationId, Arc<OperationSpec>>>>;

/// Immutable lookup table of deployment-selected connector instances.
pub struct ConnectorRegistry {
    instances: BTreeMap<String, Arc<dyn RegisteredConnector>>,
    operation_specs: OperationSpecHandles,
    /// The one Postgres source connector instances bind to, when any instance
    /// is declared. It is also the source that holds their credentials.
    source_name: Option<String>,
    /// Instances whose metadata declares `config.oauth2`.
    ///
    /// This is recorded from metadata rather than from the resolved runtime, so
    /// that a registry built without credentials still *knows* an instance
    /// declared one and can refuse to send its request. A declaration the
    /// runtime silently ignores is a defect
    /// ([[034-a-declaration-the-runtime-ignores-is-a-defect]]); a registry that
    /// forgets the declaration cannot even notice.
    oauth2_instances: BTreeSet<String>,
    credentials: Option<Arc<CredentialRuntime>>,
}

impl ConnectorRegistry {
    /// The complete module table compiled into the binary.
    pub fn built_in_module_names() -> Vec<&'static str> {
        compiled_modules()
            .iter()
            .map(|module| module.declaration().name())
            .collect()
    }

    /// Resolve and validate deployment configuration before a listener opens.
    /// Error messages contain metadata identities and variable *names* only;
    /// resolved values never enter an activity failure or a log message here.
    pub fn build(metadata: &Metadata) -> Result<Self, ConnectorRegistryError> {
        validate_connector_startup(metadata).map_err(ConnectorRegistryError::Startup)?;

        let source_name = if metadata.connectors.is_empty() {
            None
        } else {
            let postgres_sources = metadata
                .sources
                .iter()
                .filter(|source| source.kind == donat_metadata::SourceKind::Postgres)
                .collect::<Vec<_>>();
            if postgres_sources.len() != 1 {
                return Err(ConnectorRegistryError::ImplicitSourceBinding {
                    postgres_sources: postgres_sources.len(),
                });
            }
            Some(postgres_sources[0].name.clone())
        };
        let mut instances = BTreeMap::new();
        let mut operation_specs = BTreeMap::new();
        if let Some(source_name) = &source_name {
            operation_specs.insert(source_name.clone(), BTreeMap::new());
        }
        for instance in &metadata.connectors {
            // A `local.*` instance is a capability compiled into this binary,
            // not a provider: it has no origin and no credential, and it is
            // dispatched by `crate::local`. Skipping it here is what keeps the
            // two registries from having to know each other's shapes.
            if donat_metadata::is_local(&instance.module) {
                continue;
            }
            // The module table is the only way in. There is no dynamic
            // fallback: a name it does not carry is rejected, exactly as
            // startup validation already rejected it.
            let Some(module) = compiled_module(&instance.module) else {
                return Err(ConnectorRegistryError::UnknownModule {
                    instance: instance.name.clone(),
                    module: instance.module.clone(),
                });
            };
            let source_name = source_name
                .as_deref()
                .expect("connector instances have one Postgres source");
            let executable_specs = operation_specs
                .get_mut(source_name)
                .expect("the connector source operation table was initialized")
                .entry(instance.name.clone())
                .or_insert_with(BTreeMap::new);
            // The declaration this instance is compiled against. It is the
            // constant one for every module but `twilio`, whose declaration is
            // completed by this deployment's own Account SID; a configuration
            // that cannot complete one was already refused by that module's
            // validator, and is refused again here rather than assumed.
            let Some(connector) = module.declaration().resolve(instance) else {
                return Err(ConnectorRegistryError::InvalidConfiguration {
                    instance: instance.name.clone(),
                    message: format!(
                        "connector module `{}` cannot be declared from this instance's configuration",
                        instance.module
                    ),
                });
            };
            let registered = (module.build)(&mut ModuleContext {
                metadata,
                instance,
                definition: module.definition(),
                connector: &connector,
                executable_specs,
                source_name,
            })?;
            instances.insert(instance.name.clone(), Arc::from(registered));
        }
        Ok(Self {
            instances,
            operation_specs,
            source_name,
            oauth2_instances: metadata
                .connectors
                .iter()
                .filter(|instance| instance.config.oauth2.is_some())
                .map(|instance| instance.name.clone())
                .collect(),
            credentials: None,
        })
    }

    /// Empty immutable registry for existing server tests that deliberately do
    /// not load connector metadata.
    pub fn empty() -> Self {
        Self {
            instances: BTreeMap::new(),
            operation_specs: BTreeMap::new(),
            source_name: None,
            oauth2_instances: BTreeSet::new(),
            credentials: None,
        }
    }

    /// Resolve the OAuth2 credential half of this deployment, and prove it is
    /// usable, before the listener binds.
    ///
    /// A deployment that declares no `config.oauth2` block does nothing here and
    /// needs no sealing key. One that does must resolve its key and client
    /// identity, and must already hold a stored credential for every declared
    /// instance (spec 011 §7) — discovering that at the first activity attempt
    /// is a worse version of the same failure.
    pub async fn attach_credentials(
        &mut self,
        metadata: &Metadata,
        default_database_url: &str,
    ) -> Result<(), ConnectorRegistryError> {
        let Some(source_name) = self.source_name.clone() else {
            return Ok(());
        };
        if self.oauth2_instances.is_empty() {
            return Ok(());
        }
        // The metadata must be the one this registry was built from: the
        // credential store is source-local, and a different metadata would
        // resolve a different database.
        let source = metadata
            .sources
            .iter()
            .find(|source| source.name == source_name)
            .ok_or_else(|| ConnectorRegistryError::ForeignMetadata {
                source: source_name.clone(),
            })?;
        let pool = crate::state::make_pool(&crate::state::resolve_source_url(
            source,
            default_database_url,
        ))
        .map_err(|_| {
            ConnectorRegistryError::Credentials(CredentialRuntimeError::StoreUnreachable)
        })?;
        let runtime = CredentialRuntime::resolve(metadata, &source_name, pool)
            .map_err(ConnectorRegistryError::Credentials)?
            .ok_or_else(|| ConnectorRegistryError::ForeignMetadata {
                source: source_name.clone(),
            })?;
        runtime
            .validate_stored_credentials()
            .await
            .map_err(ConnectorRegistryError::Credentials)?;
        self.credentials = Some(Arc::new(runtime));
        Ok(())
    }

    /// Install an already-resolved credential runtime.
    ///
    /// Tests use this so no test needs a real token endpoint; production goes
    /// through [`ConnectorRegistry::attach_credentials`], which resolves the
    /// same runtime from metadata and the environment.
    pub fn with_credential_runtime(&mut self, credentials: Arc<CredentialRuntime>) {
        self.credentials = Some(credentials);
    }

    /// Resolve one accepted catalog-owned trigger within its exact deployment
    /// source. Inventory-only or module-local verifier details are absent.
    pub fn trigger_spec_handle(
        &self,
        source_name: &str,
        instance_name: &str,
        trigger: TriggerId,
    ) -> Option<Arc<TriggerSpec>> {
        self.instances
            .get(instance_name)?
            .trigger_spec(source_name, trigger)
    }

    /// Resolve one executable, catalog-owned operation descriptor within its
    /// deployment source. Runtime transport state, secret values, and resolved
    /// endpoint URLs are deliberately absent from the returned snapshot.
    pub fn operation_spec(
        &self,
        source_name: &str,
        instance_name: &str,
        operation: OperationId,
    ) -> Option<&OperationSpec> {
        self.operation_spec_entry(source_name, instance_name, operation)
            .map(Arc::as_ref)
    }

    /// Clone the shared immutable handle for a compiled dependency that must
    /// outlive a temporary registry borrow. This clones only the `Arc`, never
    /// the catalog-owned behavioral snapshot.
    pub fn operation_spec_handle(
        &self,
        source_name: &str,
        instance_name: &str,
        operation: OperationId,
    ) -> Option<Arc<OperationSpec>> {
        self.operation_spec_entry(source_name, instance_name, operation)
            .cloned()
    }

    fn operation_spec_entry(
        &self,
        source_name: &str,
        instance_name: &str,
        operation: OperationId,
    ) -> Option<&Arc<OperationSpec>> {
        self.operation_specs
            .get(source_name)?
            .get(instance_name)?
            .get(&operation)
    }

    pub fn http_instance(&self, name: &str) -> Option<&http::HttpConnector> {
        self.instances.get(name)?.http_connector()
    }

    /// Resolve the one compiled inbound verifier currently supported by the
    /// deployment. This returns `None` for both an unknown instance and a
    /// declared module without a webhook verifier, so the HTTP boundary never
    /// exposes connector configuration or capability details to an ingress
    /// caller.
    pub fn webhook_instance(&self, name: &str) -> Option<WebhookInstance<'_>> {
        self.instances.get(name)?.webhook()
    }

    pub fn trigger_configuration_fingerprint(
        &self,
        instance: &str,
        trigger: TriggerId,
    ) -> Option<&str> {
        self.instances
            .get(instance)?
            .trigger_configuration_fingerprint(trigger)
    }

    /// Return the immutable, non-secret deployment fingerprint for one
    /// compiled operation. It contains no resolved credential/header value or
    /// raw base URL, so a future process revision can retain it safely.
    pub fn configuration_fingerprint(&self, instance: &str, operation: &str) -> Option<&str> {
        self.instances
            .get(instance)?
            .configuration_fingerprint(operation)
    }

    /// Return the optional scalar input that serializes this exact operation.
    /// An unknown operation and an operation without serialization both return
    /// `None`; callers already resolve the operation spec before consulting
    /// this refinement.
    pub fn serialization_key_input(&self, instance: &str, operation: &str) -> Option<&str> {
        self.instances
            .get(instance)?
            .serialization_key_input(operation)
    }

    /// Execute only a named operation compiled from deployed metadata. This
    /// deliberately accepts neither a raw URL/method/header nor a caller-owned
    /// HTTP client. The future process worker supplies the stable idempotency
    /// key after acquiring its durable capacity reservation.
    pub async fn execute(
        &self,
        instance: &str,
        operation: &str,
        input: JsonValue,
        idempotency_key: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        let Some(registered) = self.instances.get(instance) else {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "connector instance is not declared",
            ));
        };
        // An instance that declared `config.oauth2` never takes the
        // unauthenticated path. If the credential runtime is absent the attempt
        // fails here, before a socket is opened: a declaration the runtime
        // ignores is a defect, and a request that quietly leaves without the
        // header is the shape that defect takes.
        if self.oauth2_instances.contains(instance) {
            let Some(credentials) = &self.credentials else {
                return Err(credential::NO_CREDENTIAL_RUNTIME);
            };
            return credential::execute_with_credential(
                credentials,
                Arc::clone(registered),
                instance,
                operation,
                input,
                idempotency_key,
                deadline,
            )
            .await;
        }
        registered
            .execute(operation, input, idempotency_key, deadline)
            .await
    }
}

#[derive(Debug)]
pub enum ConnectorRegistryError {
    Startup(ConnectorStartupError),
    ImplicitSourceBinding {
        postgres_sources: usize,
    },
    UnknownModule {
        instance: String,
        module: String,
    },
    InvalidConfiguration {
        instance: String,
        message: String,
    },
    Credentials(CredentialRuntimeError),
    /// Credentials were attached with metadata this registry was not built
    /// from, so the source-local store it would read is not this deployment's.
    ForeignMetadata {
        source: String,
    },
}

impl fmt::Display for ConnectorRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ImplicitSourceBinding { postgres_sources } => write!(
                formatter,
                "connector instances without an explicit source require exactly one Postgres source; found {postgres_sources}"
            ),
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
            Self::Credentials(error) => error.fmt(formatter),
            Self::ForeignMetadata { source } => write!(
                formatter,
                "connector credentials were resolved against metadata that does not declare source `{source}`"
            ),
        }
    }
}

impl std::error::Error for ConnectorRegistryError {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::http::http_configuration_fingerprint;
    use super::{
        CONNECTOR_RUNTIME_ABI, ConnectorDefinition, HTTP_DEFINITION, STRIPE_DEFINITION, connectors,
        validate_enabled_operations,
    };
    use crate::state::{ConnectorConfigError, validate_connector_metadata};

    fn metadata(connectors: serde_json::Value) -> donat_metadata::Metadata {
        serde_json::from_value(json!({ "version": 3, "connectors": connectors }))
            .expect("connector metadata deserializes")
    }

    fn messages(metadata: &donat_metadata::Metadata) -> String {
        validate_connector_metadata(metadata)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `registry_is_static`: an unknown connector, an unenabled operation, and
    /// an `InventoryOnly` operation are all rejected by metadata validation.
    ///
    /// All three answers come from the compiled table and the declarations in
    /// it — no lookup, no fallback, no runtime discovery — and all three land
    /// before a listener opens.
    #[test]
    fn registry_is_static() {
        // 1. A connector the table does not carry. There is no dynamic path
        //    that could find one: the table is the whole world.
        assert!(
            messages(&metadata(json!([{
                "name": "unsupported",
                "module": "does_not_exist",
                "config": {
                    "endpoint_identity": "static_connector_test",
                    "credential_identity": "static_connector_credential"
                },
                "operations": []
            }])))
            .contains("unknown connector module `does_not_exist`"),
            "only a module in the compiled table may be selected"
        );
        assert!(
            super::compiled_module("does_not_exist").is_none(),
            "an unknown module resolves to nothing rather than to a default"
        );

        // 2. An operation the connector's declaration does not carry. The
        //    deployment names a real module, and still cannot enable a
        //    capability this binary was not built with.
        let unenabled = messages(&metadata(json!([{
            "name": "payments",
            "module": "stripe",
            "config": {
                "endpoint_identity": "stripe_test",
                "credential_identity": "stripe_test_credential",
                "secret_key": { "value_from_env": "DONAT_REGISTRY_STATIC_TEST_KEY" },
                "webhook_secret": { "value_from_env": "DONAT_REGISTRY_STATIC_TEST_WHSEC" },
                "api_version": "2026-07-27"
            },
            "operations": [{
                "name": "checkout.refund",
                "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                }
            }]
        }])));
        assert!(
            unenabled
                .contains("connector operation `checkout.refund` on module `stripe`: connector operation is not compiled into this binary"),
            "an operation the declaration does not carry cannot be enabled: {unenabled}"
        );

        // 3. An operation the declaration carries and classifies
        //    `InventoryOnly`. It is declared, typed, and testable, and a
        //    deployment still cannot enable it.
        //
        //    The two compiled connectors declare no inventory-only operation
        //    today — that is the point of the gate — so the rejection is
        //    exercised on a declaration built here and run through the same
        //    production validator the table uses.
        let inventory = inventory_only_connector();
        let instance: donat_metadata::ConnectorInstance = serde_json::from_value(json!({
            "name": "provider",
            "module": "provider",
            "config": {
                "endpoint_identity": "provider_test",
                "credential_identity": "provider_test_credential"
            },
            "operations": [
                { "name": "item.get", "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 } } },
                { "name": "item.create", "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 } } }
            ]
        }))
        .expect("instance metadata deserializes");
        let mut errors: Vec<ConnectorConfigError> = Vec::new();
        validate_enabled_operations(inventory, &instance, "connectors.yaml[0]", &mut errors);
        let rendered = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            errors.len(),
            1,
            "only the inventory-only one is refused: {rendered}"
        );
        assert!(rendered.contains("item.create"), "{rendered}");
        assert!(
            rendered.contains("inventory-only and cannot be enabled by a deployment"),
            "{rendered}"
        );
        assert_eq!(
            errors[0].path, "connectors.yaml[0].operations[1].name",
            "the refusal names the exact metadata path"
        );
    }

    /// A declaration with one executable read and one inventory-only mutation,
    /// leaked once so the production validator sees the `&'static Connector` it
    /// takes.
    fn inventory_only_connector() -> &'static donat_connectors::sdk::Connector {
        use donat_connectors::sdk::{
            AuthPlan, Connector, CredentialSpec, Effect, Operation, OriginSpec,
        };
        use reqwest::StatusCode;

        static CONNECTOR: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
            Connector::declare("provider", "1.0.0")
                .origin(
                    OriginSpec::fixed("https://provider.example.test").expect("a static origin"),
                )
                .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
                .operation(
                    Operation::get("item.get", "/v1/items")
                        .version("1.0.0")
                        .success_statuses([StatusCode::OK])
                        .effect(Effect::read_only())
                        .build()
                        .expect("a static read declaration is valid"),
                )
                .operation(
                    Operation::post("item.create", "/v1/items")
                        .version("1.0.0")
                        .success_statuses([StatusCode::CREATED])
                        .effect(
                            Effect::inventory_only(
                                "the provider publishes no idempotency key for this create",
                            )
                            .expect("a recorded reason is required"),
                        )
                        .build()
                        .expect("an inventory-only declaration is valid"),
                )
                .build()
                .expect("a complete declaration is valid")
        });
        &CONNECTOR
    }

    /// The table's entries and the declarations in them are one identity: a
    /// module cannot be listed under a name its connector does not carry.
    #[test]
    fn the_module_table_is_keyed_on_its_connector_declarations() {
        let statics = connectors()
            .iter()
            .map(|connector| (connector.name(), connector.version()))
            .collect::<Vec<_>>();
        assert_eq!(
            &statics[..2],
            [
                (
                    HTTP_DEFINITION.module_name,
                    HTTP_DEFINITION.semantic_version
                ),
                (
                    STRIPE_DEFINITION.module_name,
                    STRIPE_DEFINITION.semantic_version
                )
            ]
        );
        assert!(
            !statics.iter().any(|(name, _)| *name == "twilio"),
            "twilio's declaration is completed by a deployment and has no static form"
        );
        for module in super::compiled_modules() {
            let declaration = module.declaration();
            assert_eq!(module.definition().module_name, declaration.name());
            assert_eq!(module.definition().runtime_abi, CONNECTOR_RUNTIME_ABI);
            if let Some(connector) = declaration.as_static() {
                assert_eq!(module.definition(), ConnectorDefinition::of(connector));
            }
            assert_eq!(
                super::compiled_module(declaration.name()).map(|found| found.declaration().name()),
                Some(declaration.name()),
                "every entry is reachable by the name its declaration carries"
            );
        }
    }

    /// Every Batch B connector is reachable under its own name, publishes the
    /// trigger set its module declares, and shares one verification across that
    /// set — which is the invariant one HTTP route per instance rests on.
    #[test]
    fn every_webhook_bearing_module_publishes_one_route_for_its_whole_trigger_set() {
        use donat_connectors::providers::{calendly, github, sentry, shopify, telegram, typeform};

        for (name, expected) in [
            (
                github::NAME,
                vec!["issues", "pull_request", "push", "release"],
            ),
            (
                shopify::NAME,
                vec![
                    "orders/create",
                    "orders/updated",
                    "orders/paid",
                    "products/update",
                ],
            ),
            (telegram::NAME, vec!["message", "callback_query"]),
            (calendly::NAME, vec!["invitee.created", "invitee.canceled"]),
            (sentry::NAME, vec!["issue.created", "issue.resolved"]),
            (typeform::NAME, vec!["form_response"]),
        ] {
            let module = super::compiled_module(name)
                .unwrap_or_else(|| panic!("the module table carries `{name}`"));
            let connector = module
                .declaration()
                .as_static()
                .unwrap_or_else(|| panic!("`{name}` declares a constant connector"));
            assert_eq!(
                connector
                    .triggers()
                    .iter()
                    .map(donat_connectors::sdk::Trigger::name)
                    .collect::<Vec<_>>(),
                expected,
                "`{name}` publishes its declared trigger set"
            );

            // The compiled inbound route holds the same set and shares one
            // scheme, which is what `ProviderWebhook::compile` refuses to build
            // without.
            let webhook = super::provider::ProviderWebhook::compile(
                connector,
                "a-deployment-secret".to_owned(),
            )
            .unwrap_or_else(|error| panic!("`{name}` compiles one inbound route: {error}"));
            assert_eq!(webhook.trigger_names(), expected);
            assert_eq!(
                webhook.raw_body_max_bytes(),
                donat_connectors::sdk::MAX_HTTP_BODY_BYTES,
                "`{name}` reads a delivery to the shared ceiling and no further"
            );
        }

        // A module with no trigger at all cannot compile an inbound route, so a
        // deployment cannot configure a webhook secret into one.
        assert!(
            super::provider::ProviderWebhook::compile(
                donat_connectors::providers::openai::connector(),
                "a-deployment-secret".to_owned(),
            )
            .is_err(),
            "a connector that declares no trigger publishes no route"
        );
    }

    #[test]
    fn http_configuration_fingerprint_changes_when_runtime_abi_changes() {
        let operation: donat_metadata::ConnectorOperation = serde_json::from_value(json!({
            "name": "create_shipment",
            "version": "v1",
            "method": "POST",
            "path": "/v1/shipments/{input.order_id}",
            "success_statuses": [200],
            "idempotency": { "header": "Idempotency-Key" },
            "capacity": {
                "max_in_flight": 1,
                "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
            }
        }))
        .expect("fingerprint operation metadata deserializes");
        let config: donat_metadata::ConnectorConfig = serde_json::from_value(json!({
            "endpoint_identity": "logistics_test",
            "credential_identity": "logistics_test_credential",
            "base_url": "https://provider.example.test"
        }))
        .expect("fingerprint config metadata deserializes");

        let baseline =
            http_configuration_fingerprint(HTTP_DEFINITION, &config, &operation, "base-url-digest");
        let changed_abi = http_configuration_fingerprint(
            ConnectorDefinition {
                runtime_abi: HTTP_DEFINITION.runtime_abi + 1,
                ..HTTP_DEFINITION
            },
            &config,
            &operation,
            "base-url-digest",
        );

        assert_ne!(baseline, changed_abi);
    }
}
