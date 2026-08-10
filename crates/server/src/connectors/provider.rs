//! The serving half of the hand-written connectors (specs 012 and 017).
//!
//! A provider module in `donat-connectors` is a declaration plus, where the
//! provider demands it, the deploy-time configuration type its instances
//! compile from. None of that knows anything about deployment metadata,
//! environment variables, or the registry — deliberately, because a connector
//! must be testable against the SDK's own stub without a server.
//!
//! This module is the seam. It reads one instance's deploy-time configuration
//! out of `connectors.yaml`, resolves the `SecretRef`s it names, hands the
//! provider module the values it declared, and publishes the compiled result to
//! the registry behind [`RegisteredConnector`]. Everything module-specific —
//! which operations exist, which of them this deployment may enable, how a
//! request is rendered and a response decoded — stays inside the provider
//! module, behind [`ProviderRuntime`].
//!
//! Four rules hold for every module wired up here.
//!
//! * The origin is the connector's own. A hand-written connector never accepts
//!   `base_url`, `network_policy`, or a configured header: those describe the
//!   declarative `http` module, and accepting them here would quietly turn a
//!   fixed-origin connector into a generic HTTP client.
//! * Deploy-time material comes from `config.settings` (non-secret) and
//!   `config.secrets`/`config.secret_key` (`SecretRef`s), and a key a module
//!   does not read is refused rather than ignored
//!   ([[034-a-declaration-the-runtime-ignores-is-a-defect]]).
//! * A startup failure names the metadata path or the environment variable, and
//!   never a resolved value.
//! * A module's continuation plan is executed, not merely declared. An
//!   operation whose module declares one is walked here, under one budget
//!   shared with the attempt; an operation whose module declares none is one
//!   request. Every module names its plan lookup explicitly — its own or
//!   [`no_pagination`] — so a connector cannot acquire or lose a walk by
//!   omission ([[058-a-declared-walk-is-the-executors-walk]]).

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, Credential, ErrorMap, HostResolver, HttpTransport,
    MAX_HTTP_BODY_BYTES, Operation, OperationRejection, Origin, Pagination, PaginationBudget,
    RawHttpResponse, RequestPlan, ReqwestTransport, Secret, SystemResolver, TransportErrorKind,
    Trigger, WebhookRejection,
};
use donat_metadata::{ConnectorConfig, ConnectorInstance, ConnectorOperationProfile};
use futures_util::future::BoxFuture;
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use super::{
    ConnectorDefinition, ConnectorErrorClass, ConnectorFailure, ConnectorRegistryError,
    ConnectorSuccess, ModuleContext, RegisteredConnector, canonical_json_sha256,
};
use crate::state::{ConnectorConfigError, validate_connector_operation_defaults};

pub(crate) mod crm;
pub(crate) mod google;
pub(crate) mod microsoft;
pub(crate) mod modules;
pub(crate) mod project;
// Batch I: storage and messaging (spec 025).
pub(crate) mod storage;
// Batch K: development and monitoring (spec 027).
pub(crate) mod devops;
// Batch L, the scheduling and people half (spec 028).
pub(crate) mod people;
// Batch L, the forms half: forms and surveys (spec 028).
pub(crate) mod forms;

/// One compiled provider instance, as the shared executor sees it.
///
/// The three questions here are the only ones a request needs answered, and
/// every one of them is module-owned: which operations this *deployment* may
/// reach, how one renders, and what a response means. The AWS modules answer
/// the first from their own configuration — a standard queue's send and a
/// versioned bucket's delete are inventory-only for that deployment and
/// executable for another (`knowledgebase/declarative-saas/decisions/046-*`).
pub(crate) trait ProviderRuntime: Send + Sync {
    /// The one resolved origin every request of this instance renders against.
    fn origin(&self) -> &Origin;

    /// The credential plan the connector declared, and the resolved credential
    /// it applies. The executor never formats an `Authorization` header itself.
    fn auth_plan(&self) -> Option<&AuthPlan>;

    fn credential(&self) -> &Credential;

    /// The gate this deployment meets, for one operation name.
    ///
    /// It answers with the compiled operation rather than with `()`, because
    /// the same answer is what the registry publishes: the declaration this
    /// deployment admitted is the one it projects to process compilation, so
    /// there is no second lookup that could resolve a different one.
    fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection>;

    /// Whether this module's credential is a stored authorization-code OAuth2
    /// token the credential seam supplies per attempt (spec 011).
    ///
    /// It is `false` by default, and that default is the refusal
    /// [[043-the-credential-seam-refuses-before-it-sends]] asks for: a module
    /// that authenticates with a configured key never accepts an applied
    /// `Authorization` header, even if a deployment somehow reached the
    /// authorized path with one.
    fn applies_stored_oauth2(&self) -> bool {
        false
    }

    /// Render one request. `idempotency_key` is the durable activity's stable
    /// key; a module that binds one reads it here and nowhere else.
    fn plan(
        &self,
        id: &str,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure>;

    /// The declared output of one response, or the classified failure.
    fn decode(
        &self,
        id: &str,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure>;

    /// The continuation plan this module declared for one operation.
    ///
    /// `None` is the answer for every operation a provider does not paginate,
    /// and for the connectors that deliberately declare no plan at all — a
    /// cursor a provider takes in the request *body* is a declared input and a
    /// Process walks it, and a `Link` header that always offers a next page is
    /// not a walk the SDK's plan can end
    /// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]). Those connectors
    /// send exactly one request, which is what their declaration says.
    ///
    /// The plan is returned by value because one module builds its plans per
    /// compiled instance rather than as statics; the clone is a handful of
    /// short strings on a path that is about to make an HTTP request.
    fn pagination(&self, _id: &str) -> Option<Pagination> {
        None
    }

    /// The ceilings one logical attempt may spend walking that plan.
    ///
    /// The returned budget's deadline is a placeholder — the executor binds the
    /// attempt's own deadline with `with_deadline` before spending it — so the
    /// four ceilings here are the whole of what a module declares. A module
    /// that publishes its own page limits overrides this; everything else
    /// spends the SDK's default.
    fn pagination_budget(&self, _id: &str) -> PaginationBudget {
        PaginationBudget::default_ceilings()
    }

    /// Whether one page of a walk carries a provider failure.
    ///
    /// It is the same question [`Self::decode`] answers first, which is why the
    /// default asks `decode` itself: an operation's own `ErrorMap` classifies a
    /// non-success status, and a module whose provider reports failure inside a
    /// `2xx` applies its own body gate. A page's declared output is discarded —
    /// the aggregate is decoded once, at the end of the walk — but the page
    /// still has to satisfy the contract that the aggregate will be read
    /// through.
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

/// The plan lookup of a connector that declares no continuation plan anywhere.
///
/// It is spelled out per module rather than defaulted, so that adding a
/// connector is a decision about its pagination rather than an omission
/// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
pub(crate) fn no_pagination(_id: &str) -> Option<&'static Pagination> {
    None
}

// ---------------------------------------------------------------------------
// The declaration-driven runtime, which is most of Batch A.
// ---------------------------------------------------------------------------

/// A connector whose whole behaviour is its SDK declaration: the operation
/// renders from the declaration, the response decodes through the declared
/// output pointers, and a failure is classified by the module's error map.
///
/// `bind` is the one module-specific step: an account-scoped identifier — an
/// Airtable base, a Twilio account — is deploy-time configuration, so the
/// module merges it into the input and refuses an input that carries one of its
/// own.
pub(crate) struct DeclaredProvider {
    /// This instance's own copy of the declaration it was validated against.
    /// It is a clone rather than a `&'static` because Twilio's declaration is
    /// built per deployment, and one runtime type that holds both is simpler
    /// than two that differ only in a lifetime.
    connector: Connector,
    origin: Origin,
    credential: Credential,
    configuration: ConnectorConfiguration,
    error_map: &'static ErrorMap,
    bind: fn(&ConnectorConfiguration, &JsonValue) -> Result<JsonValue, ConnectorFailure>,
    /// The module's own continuation plans, by operation. It is a required
    /// constructor argument rather than an option with a default, because a
    /// module that forgot to wire one would be a declared walk the runtime
    /// silently does not make
    /// ([[034-a-declaration-the-runtime-ignores-is-a-defect]]).
    pagination: PaginationLookup,
}

/// One module's plan table: the operation's declared continuation plan, or
/// `None` where the provider publishes none this connector can walk.
pub(crate) type PaginationLookup = fn(&str) -> Option<&'static Pagination>;

/// The binder of a connector with no account-scoped identifier in its paths.
pub(crate) fn bind_nothing(
    _configuration: &ConnectorConfiguration,
    input: &JsonValue,
) -> Result<JsonValue, ConnectorFailure> {
    Ok(input.clone())
}

impl DeclaredProvider {
    pub(crate) fn compile(
        connector: Connector,
        credential: Credential,
        configuration: ConnectorConfiguration,
        error_map: &'static ErrorMap,
        bind: fn(&ConnectorConfiguration, &JsonValue) -> Result<JsonValue, ConnectorFailure>,
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
        Ok(Self {
            connector,
            origin,
            credential,
            configuration,
            error_map,
            bind,
            pagination,
        })
    }

    fn operation(&self, id: &str) -> Result<&Operation, ConnectorFailure> {
        self.connector.operation(id).ok_or_else(|| {
            ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "connector operation is not compiled into this binary",
            )
        })
    }
}

impl ProviderRuntime for DeclaredProvider {
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
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let operation = self.operation(id)?;
        let bound = (self.bind)(&self.configuration, input)?;
        let mut request = operation.plan_request(&self.origin, &bound)?;
        // A no-op for every class that binds no key, and the whole of the
        // safety for one that does. The binding is the SDK's own header — a
        // declaration that names it does not build — so this is the only place
        // it can reach the wire, and an operation admitted as
        // `ProviderIdempotent::ExplicitKey` that was sent without it would be a
        // declaration the runtime ignores
        // ([[034-a-declaration-the-runtime-ignores-is-a-defect]]) on the one
        // class where ignoring it means sending a payment twice.
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
        let operation = self.operation(id)?;
        if !operation.is_success(status) {
            return Err(self.error_map.classify(status, headers, body));
        }
        operation.decode_response(status, body)
    }

    fn pagination(&self, id: &str) -> Option<Pagination> {
        (self.pagination)(id).cloned()
    }

    /// The page gate of a declaration-driven connector: the declared success
    /// statuses decide, and the operation's own map classifies everything else.
    /// It is `decode` without the output extraction, because the aggregate —
    /// not each page — is what this connector's declared output describes.
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

// ---------------------------------------------------------------------------
// The registry instance every hand-written module publishes.
// ---------------------------------------------------------------------------

/// One selected operation of a compiled provider instance.
struct CompiledProviderOperation {
    configuration_fingerprint: String,
    serialization_key_input: Option<String>,
}

/// One deployment-selected inbound route of a hand-written connector (spec 013).
///
/// A connector declares one trigger per provider event, and every one of them
/// arrives on the same route with the same scheme — GitHub signs an `issues` and
/// a `push` delivery identically, and the event is named by a header. This type
/// therefore holds the declared trigger set and refuses at startup to compile an
/// instance whose triggers disagree about the verification or the ceiling: one
/// instance is one route, so one route has one answer.
///
/// It holds the resolved signing secret and gives nothing back: `Secret` has no
/// `Display`, no `Serialize`, and a `Debug` that prints `Secret(<redacted>)`.
pub(crate) struct ProviderWebhook {
    triggers: Vec<Trigger>,
    secret: Secret,
    raw_body_max_bytes: usize,
}

impl ProviderWebhook {
    /// Compile one instance's inbound route from its declaration and its
    /// configured secret.
    pub(crate) fn compile(connector: &Connector, secret: String) -> Result<Self, String> {
        let triggers = connector.triggers().to_vec();
        let Some(first) = triggers.first() else {
            return Err(format!(
                "connector module `{}` declares no inbound trigger",
                connector.name()
            ));
        };
        if triggers.iter().any(|trigger| {
            trigger.verification() != first.verification()
                || trigger.raw_body_max_bytes() != first.raw_body_max_bytes()
        }) {
            return Err(format!(
                "connector module `{}` declares inbound triggers that do not share one \
                 verification scheme, so one route could not answer for all of them",
                connector.name()
            ));
        }
        Ok(Self {
            raw_body_max_bytes: first.raw_body_max_bytes(),
            triggers,
            secret: Secret::new(secret),
        })
    }

    pub(crate) const fn raw_body_max_bytes(&self) -> usize {
        self.raw_body_max_bytes
    }

    /// The declared trigger names, in declaration order.
    ///
    /// Nothing on the request path reads this: the route answers for the whole
    /// set with one scheme. It exists so the module-table test can assert that
    /// a compiled route carries exactly the trigger set its declaration does.
    #[cfg(test)]
    pub(crate) fn trigger_names(&self) -> Vec<&'static str> {
        self.triggers.iter().map(Trigger::name).collect()
    }

    /// Verify one delivery's exact raw bytes.
    ///
    /// The trigger applies its own ceiling first and then the declared scheme,
    /// so an oversized body is refused before a MAC is computed over it and a
    /// body that fails the scheme is never parsed. A successful answer is `()`:
    /// this batch verifies and rejects, and produces no event.
    pub(crate) fn verify(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> Result<(), WebhookRejection> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| WebhookRejection::TimestampOutOfTolerance)?
            .as_secs()
            .try_into()
            .map_err(|_| WebhookRejection::TimestampOutOfTolerance)?;
        self.verify_at(headers, raw_body, now)
    }

    /// The same verification at a caller-chosen clock, so a timestamped scheme
    /// can be proven against a fixed vector.
    pub(crate) fn verify_at(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
        now_unix_seconds: i64,
    ) -> Result<(), WebhookRejection> {
        self.triggers
            .first()
            .expect("a compiled inbound route declares at least one trigger")
            .verify(headers, raw_body, &self.secret, now_unix_seconds)
    }
}

/// One deployment-selected instance of a hand-written connector module.
pub(crate) struct ProviderInstance {
    runtime: Box<dyn ProviderRuntime>,
    operations: BTreeMap<String, CompiledProviderOperation>,
    /// The inbound route this instance publishes, for a connector that declares
    /// one. A connector with no trigger has none, and its instance name is
    /// therefore indistinguishable from an absent route at the ingress boundary.
    webhook: Option<ProviderWebhook>,
    /// The one Postgres source this instance's inbound route belongs to.
    source_name: String,
    resolver: Arc<dyn HostResolver>,
    transport: Arc<dyn HttpTransport>,
}

/// Compile one instance from validated deployment metadata and a module's own
/// compiled runtime.
///
/// The declaration is the gate at build as well as at validation: an operation
/// the deployment enabled that this instance does not admit never becomes a
/// registry entry, so a Process cannot reach it even if metadata validation
/// were somehow skipped.
pub(crate) fn build_registered_instance(
    context: &mut ModuleContext<'_>,
    runtime: Box<dyn ProviderRuntime>,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    build_registered_instance_with_webhook(context, runtime, None)
}

/// The same, for a module that also publishes an inbound route.
pub(crate) fn build_registered_instance_with_webhook(
    context: &mut ModuleContext<'_>,
    runtime: Box<dyn ProviderRuntime>,
    webhook: Option<ProviderWebhook>,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    let instance = context.instance;
    let invalid = |message: String| ConnectorRegistryError::InvalidConfiguration {
        instance: instance.name.clone(),
        message,
    };
    let mut operations = BTreeMap::new();
    for operation in &instance.operations {
        let declared = runtime
            .admit_operation(&operation.name)
            .map_err(|rejection| invalid(rejection.message().to_owned()))?;
        // The declaration this deployment admitted is the one it publishes.
        // Everything the catalog snapshot needs is derived from it here, so a
        // request shape has exactly one description in this workspace
        // ([[049-a-connector-publishes-the-declaration-it-was-admitted-on]]).
        let capacity = operation
            .capacity()
            .ok_or_else(|| invalid("connector operation declares no capacity".to_owned()))?;
        // The bounds this operation publishes are the ones its executor will
        // spend: an operation whose module declared a continuation plan is a
        // walk, and one that declared none is a single request.
        let projection = declared.project();
        let walk = match runtime.pagination(&operation.name) {
            None => None,
            Some(plan) => {
                admits_a_walked_aggregate(&plan, &projection).map_err(&invalid)?;
                Some(runtime.pagination_budget(&operation.name))
            }
        };
        let spec = super::catalog::compile_provider_operation_spec(
            context.definition,
            instance,
            runtime.origin(),
            &projection,
            capacity,
            walk.as_ref(),
        )
        .map_err(&invalid)?;
        context
            .executable_specs
            .insert(spec.operation, std::sync::Arc::new(spec));
        let compiled = CompiledProviderOperation {
            configuration_fingerprint: provider_configuration_fingerprint(
                context.definition,
                context.connector,
                &instance.config,
                operation,
            ),
            serialization_key_input: operation
                .capacity()
                .and_then(|capacity| capacity.serialize_by.as_ref())
                .map(|binding| binding.input.clone()),
        };
        if operations
            .insert(operation.name.clone(), compiled)
            .is_some()
        {
            return Err(invalid(format!(
                "connector operation `{}` is declared more than once",
                operation.name
            )));
        }
    }
    admits_its_own_credential(runtime.as_ref()).map_err(&invalid)?;
    Ok(Box::new(ProviderInstance {
        runtime,
        operations,
        webhook,
        source_name: context.source_name.to_owned(),
        resolver: Arc::new(SystemResolver),
        transport: Arc::new(ReqwestTransport::new()),
    }))
}

/// Whether this compiled instance can actually apply the credential plan it
/// declares — asked here, before a listener opens, rather than at the first
/// activity attempt.
///
/// [[043-the-credential-seam-refuses-before-it-sends]] made this the rule for
/// the stored OAuth2 credential: "a module that cannot apply the plan refuses at
/// deploy time". The client-credentials plan is the case that needs it checked
/// by the *registry* rather than by each module, because the plan's credential
/// is not a header the module writes — it is a token exchange the executor
/// makes on the module's behalf, from fields the module had to wire into its own
/// `credential()`. A module that declared the plan and returned a credential
/// without them would compile, deploy, and then fail every attempt with
/// `connector_credential_missing_field`; rendering the exchange once here turns
/// that into a startup failure naming the instance.
///
/// Every other plan answers immediately: `token_request` is `None` for all of
/// them and there is nothing to render.
pub(crate) fn admits_its_own_credential(runtime: &dyn ProviderRuntime) -> Result<(), String> {
    let Some(plan) = runtime.auth_plan() else {
        return Ok(());
    };
    if !plan.issues_its_own_token() {
        return Ok(());
    }
    match plan.token_request(runtime.credential()) {
        Ok(Some(_)) => Ok(()),
        // The message names the plan and never a resolved value: a token
        // request that failed to render did so over a credential.
        Ok(None) | Err(_) => Err(
            "this connector declares an OAuth2 client-credentials plan and its configured \
             credential cannot render the token request that plan requires"
                .to_owned(),
        ),
    }
}

/// Whether a walked aggregate would be readable through the operation's own
/// declared output.
///
/// A completed walk writes every collected item where the plan declared the
/// item list. If the operation publishes its output from a *different* pointer,
/// a deployment would receive the first page's list and no sign that fifteen
/// more were fetched and discarded — the same silence
/// [[034-a-declaration-the-runtime-ignores-is-a-defect]] is about, one layer
/// down. So the two are checked against each other at startup rather than
/// trusted to agree.
///
/// An operation with no pointer-based output publishes the whole response
/// document, which is what a provider that answers with a bare collection needs
/// (GitHub), so that case admits any plan.
fn admits_a_walked_aggregate(
    plan: &Pagination,
    projection: &donat_connectors::sdk::OperationProjection,
) -> Result<(), String> {
    let items = plan.items_pointer();
    let mut published = projection
        .outputs()
        .iter()
        .filter_map(donat_connectors::sdk::OutputProjection::pointer)
        .peekable();
    if published.peek().is_none() {
        return Ok(());
    }
    if published.clone().any(|pointer| {
        pointer.is_empty() || pointer == items || items.starts_with(&format!("{pointer}/"))
    }) {
        return Ok(());
    }
    Err(format!(
        "connector operation `{}` declares a continuation plan collecting `{items}`, which none \
         of its declared outputs reads; a walked aggregate would be discarded",
        projection.id()
    ))
}

/// Compile one instance of a connector whose behaviour is its declaration.
///
/// The declaration is the one this instance was *validated* against — for
/// Twilio, the one built from this deployment's own Account SID — so a
/// validated instance and a compiled one can never describe different requests.
pub(crate) fn build_declared_instance<'a>(
    context: &mut ModuleContext<'_>,
    credential: Credential,
    configuration: impl IntoIterator<Item = (&'a str, &'a str)>,
    error_map: &'static ErrorMap,
    bind: fn(&ConnectorConfiguration, &JsonValue) -> Result<JsonValue, ConnectorFailure>,
    pagination: PaginationLookup,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    build_declared_instance_with_webhook(
        context,
        credential,
        configuration,
        error_map,
        bind,
        pagination,
        None,
    )
}

/// The same, for a declaration-driven connector that also publishes an inbound
/// route (spec 013). `webhook_secret` is the resolved signing secret of this
/// instance; the module has already refused to start without one.
pub(crate) fn build_declared_instance_with_webhook<'a>(
    context: &mut ModuleContext<'_>,
    credential: Credential,
    configuration: impl IntoIterator<Item = (&'a str, &'a str)>,
    error_map: &'static ErrorMap,
    bind: fn(&ConnectorConfiguration, &JsonValue) -> Result<JsonValue, ConnectorFailure>,
    pagination: PaginationLookup,
    webhook_secret: Option<String>,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    let invalid = invalid_configuration(context.instance);
    let webhook = webhook_secret
        .map(|secret| ProviderWebhook::compile(context.connector, secret))
        .transpose()
        .map_err(&invalid)?;
    let runtime = DeclaredProvider::compile(
        context.connector.clone(),
        credential,
        ConnectorConfiguration::from_deployment(configuration),
        error_map,
        bind,
        pagination,
    )
    .map_err(&invalid)?;
    build_registered_instance_with_webhook(context, Box::new(runtime), webhook)
}

impl RegisteredConnector for ProviderInstance {
    fn execute<'a>(
        &'a self,
        operation: &'a str,
        input: JsonValue,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            if !self.operations.contains_key(operation) {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "connector_invariant",
                    "connector operation is not declared",
                ));
            }
            let fingerprint = canonical_json_sha256(&input);
            let output = self
                .attempt(operation, &input, idempotency_key, deadline, None)
                .await
                .map_err(|attempt| attempt.failure)?;
            Ok(ConnectorSuccess {
                output,
                request_fingerprint: fingerprint,
            })
        })
    }

    /// One attempt under a live `Authorization` header (spec 011 §6).
    ///
    /// Only a module whose declared credential *is* a stored OAuth2 token
    /// accepts one: for every other module this is the trait's own refusal,
    /// because a connector that authenticates with a configured key has no
    /// place to put an applied header and would otherwise send its own
    /// credential alongside one it does not understand.
    ///
    /// The `401` case returns the failure the operation's own error map
    /// produced rather than a credential-shaped one, so the refresh-and-replay
    /// in `crate::connectors::credential` can happen without discarding what
    /// the operation declared a `401` to be
    /// ([[043-the-credential-seam-refuses-before-it-sends]]).
    fn execute_authorized<'a>(
        &'a self,
        operation: &'a str,
        input: JsonValue,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
        authorization: &'a str,
    ) -> BoxFuture<'a, Result<super::AuthorizedAttempt, ConnectorFailure>> {
        Box::pin(async move {
            if !self.runtime.applies_stored_oauth2() {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "connector_credential_not_applicable",
                    "connector module cannot apply an OAuth2 credential to its requests",
                ));
            }
            if !self.operations.contains_key(operation) {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "connector_invariant",
                    "connector operation is not declared",
                ));
            }
            let fingerprint = canonical_json_sha256(&input);
            match self
                .attempt(
                    operation,
                    &input,
                    idempotency_key,
                    deadline,
                    Some(authorization),
                )
                .await
            {
                Ok(output) => Ok(super::AuthorizedAttempt::Done(ConnectorSuccess {
                    output,
                    request_fingerprint: fingerprint,
                })),
                Err(attempt) if attempt.unauthorized => {
                    Ok(super::AuthorizedAttempt::Unauthorized(attempt.failure))
                }
                Err(attempt) => Err(attempt.failure),
            }
        })
    }

    /// The scheme this instance's provider publishes for its stored OAuth2
    /// tokens, read off the declaration's own auth plan.
    ///
    /// RFC 6750's `Bearer` is the answer for every module whose credential is
    /// deploy-time configuration — those never receive an applied header at all
    /// — and for every stored-credential module but Zoho CRM, which publishes
    /// `Zoho-oauthtoken`. Reading it here rather than storing a second copy is
    /// what keeps the header the lifecycle formats and the header the connector
    /// admits one decision.
    fn oauth2_authorization_scheme(&self) -> &'static str {
        self.runtime
            .auth_plan()
            .and_then(AuthPlan::oauth2_authorization_scheme)
            .unwrap_or(donat_connectors::sdk::BEARER_SCHEME)
    }

    fn configuration_fingerprint(&self, operation: &str) -> Option<&str> {
        self.operations
            .get(operation)
            .map(|operation| operation.configuration_fingerprint.as_str())
    }

    fn serialization_key_input(&self, operation: &str) -> Option<&str> {
        self.operations
            .get(operation)
            .and_then(|operation| operation.serialization_key_input.as_deref())
    }

    /// The inbound route this instance publishes, if it has one.
    ///
    /// A verified delivery of one of these connectors is `Unacknowledged`: it is
    /// authentic, nothing was stored, and the route answers `503` until the
    /// Process-owned inbound transaction lands (spec 013 §0).
    fn webhook(&self) -> Option<super::WebhookInstance<'_>> {
        let webhook = self.webhook.as_ref()?;
        Some(super::WebhookInstance {
            source_name: &self.source_name,
            delivery: super::WebhookDelivery::Verified(webhook),
        })
    }
}

impl ProviderInstance {
    /// One compiled instance with a caller-supplied resolver and transport, for
    /// the unit tests that need a loopback stub rather than a provider.
    ///
    /// It builds the same value [`build_registered_instance`] does, minus the
    /// catalog projection a deployment's metadata supplies, so an executor test
    /// exercises the production `attempt` path exactly.
    #[cfg(test)]
    pub(crate) fn for_test(
        runtime: Box<dyn ProviderRuntime>,
        operations: impl IntoIterator<Item = &'static str>,
        resolver: Arc<dyn HostResolver>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            runtime,
            operations: operations
                .into_iter()
                .map(|name| {
                    (
                        name.to_owned(),
                        CompiledProviderOperation {
                            configuration_fingerprint: format!("test-{name}"),
                            serialization_key_input: None,
                        },
                    )
                })
                .collect(),
            webhook: None,
            source_name: "default".to_owned(),
            resolver,
            transport,
        }
    }

    /// One provider attempt: render, authenticate, send — once, or once per
    /// declared page — and decode.
    ///
    /// The host is resolved before the request is rendered and again
    /// immediately before connecting, and the connected peer must be one of the
    /// addresses this attempt already resolved — the same rule the declarative
    /// connector and the Stripe module hold to, so a name cannot resolve to one
    /// address for validation and another for transport. A walk repeats that
    /// for every page: each continuation resolves and pins again.
    ///
    /// `authorization` is the complete `Authorization` header one attempt was
    /// given by the credential seam, for the modules whose credential is a
    /// stored OAuth2 token; every other module passes `None` and applies its
    /// own configured credential through its declared plan.
    ///
    /// The credential is applied per *request*, not per attempt. For a static
    /// header — a bearer token, an API key, HTTP Basic — that is the same
    /// thing, but an AWS SigV4 signature covers the canonical query string, and
    /// a continuation is a different query. A walk that signed once and then
    /// changed the URL would send page two with page one's signature, which AWS
    /// answers `SignatureDoesNotMatch` and the error map classifies
    /// `authentication` — a permanent failure with no cause a Process could
    /// act on.
    async fn attempt(
        &self,
        operation: &str,
        input: &JsonValue,
        idempotency_key: &str,
        deadline: tokio::time::Instant,
        authorization: Option<&str>,
    ) -> Result<JsonValue, ProviderAttemptFailure> {
        if deadline <= tokio::time::Instant::now() {
            return Err(timeout_failure().into());
        }
        self.resolve_under_deadline(deadline).await?;
        let request = self.render(operation, input, idempotency_key)?;
        // A module with no credential plan cannot send anything, and finding
        // that out here rather than per page keeps the refusal one answer.
        let plan = self.runtime.auth_plan().ok_or_else(no_credential_plan)?;

        // Every plan but one spends deploy-time configuration, or the stored
        // token the credential seam handed this attempt.
        if !plan.issues_its_own_token() {
            let stored = authorization.map(donat_connectors::sdk::AccessToken::new);
            return self
                .dispatch(operation, request, deadline, stored.as_ref())
                .await;
        }

        // Client credentials (spec 011 §6): one token per logical attempt,
        // minted under this attempt's own deadline, never persisted, and
        // dropped when this frame ends. A `401` buys exactly one
        // re-acquisition and exactly one replay — the same contract the stored
        // path has ([[043-the-credential-seam-refuses-before-it-sends]]) — and
        // the replay's failure is returned unchanged, so what a `401` *means*
        // stays the operation's `error_map`'s answer.
        let issued = self.issue_token(plan, deadline).await?;
        match self
            .dispatch(operation, request, deadline, Some(&issued))
            .await
        {
            Err(failed) if failed.unauthorized => {
                drop(issued);
                let reissued = self.issue_token(plan, deadline).await?;
                let replay = self.render(operation, input, idempotency_key)?;
                self.dispatch(operation, replay, deadline, Some(&reissued))
                    .await
            }
            outcome => outcome,
        }
    }

    /// Render one request and hold it to the shared body ceiling.
    fn render(
        &self,
        operation: &str,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let request = self.runtime.plan(operation, input, idempotency_key)?;
        if request.body().len() > MAX_HTTP_BODY_BYTES {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "connector_invariant",
                "connector request exceeds the 1 MiB limit",
            ));
        }
        Ok(request)
    }

    /// Mint one access token for this attempt, through the same resolver and
    /// transport its provider requests use.
    async fn issue_token(
        &self,
        plan: &AuthPlan,
        deadline: tokio::time::Instant,
    ) -> Result<donat_connectors::sdk::AccessToken, ProviderAttemptFailure> {
        super::client_credentials::issue(
            plan,
            self.runtime.credential(),
            self.resolver.as_ref(),
            self.transport.as_ref(),
            deadline,
        )
        .await
        .map_err(ProviderAttemptFailure::from)
    }

    /// One rendered request through this operation's declared shape: a single
    /// request, or the walk its module declared.
    async fn dispatch(
        &self,
        operation: &str,
        request: RequestPlan,
        deadline: tokio::time::Instant,
        applied: Option<&donat_connectors::sdk::AccessToken>,
    ) -> Result<JsonValue, ProviderAttemptFailure> {
        match self.runtime.pagination(operation) {
            None => self.one_page(operation, request, deadline, applied).await,
            Some(pagination) => {
                self.walk(operation, &pagination, request, deadline, applied)
                    .await
            }
        }
    }

    /// Apply this module's declared credential plan to one rendered request.
    ///
    /// The applied header travels as the plan's issued token, so there is
    /// exactly one place in this workspace that writes an `Authorization`
    /// header onto a rendered request — and one place that signs one.
    fn authenticate(
        &self,
        request: &mut RequestPlan,
        applied: Option<&donat_connectors::sdk::AccessToken>,
    ) -> Result<(), ConnectorFailure> {
        let plan = self.runtime.auth_plan().ok_or_else(no_credential_plan)?;
        plan.apply(self.runtime.credential(), request, applied)
    }

    /// The whole attempt of an operation whose module declared no continuation
    /// plan: one request, one response, one decode.
    async fn one_page(
        &self,
        operation: &str,
        mut request: RequestPlan,
        deadline: tokio::time::Instant,
        applied: Option<&donat_connectors::sdk::AccessToken>,
    ) -> Result<JsonValue, ProviderAttemptFailure> {
        self.authenticate(&mut request, applied)?;
        let response = self.send(request, deadline).await?;
        let status = response.status.as_u16();
        self.runtime
            .decode(operation, status, response.headers(), response.body())
            .map_err(|failure| ProviderAttemptFailure {
                // What a `401` *means* stays the operation's; all this records
                // is that the provider sent one, which is the only thing the
                // credential seam is allowed to act on.
                unauthorized: status == 401,
                failure,
            })
    }

    /// The whole attempt of an operation whose module declared one.
    ///
    /// One logical attempt is one walk sharing one budget: the plan's four
    /// ceilings plus this attempt's own deadline, spent across every page. A
    /// budget failure is a failure of the attempt — the items already collected
    /// go with it, because a truncated aggregate is indistinguishable from a
    /// complete one downstream — and a page the operation classifies as a
    /// failure ends the walk with exactly that classification.
    ///
    /// What the aggregate decodes as is the plan's business: the final page
    /// with every collected item written where the plan declared the item list.
    /// The declared output pointers then read it exactly as they read a single
    /// page, so the operation's output contract is the same whether its
    /// provider answered in one page or in nine.
    ///
    /// The request handed to the plan carries no credential, and each page is
    /// authenticated from it immediately before that page is sent. That is what
    /// makes a signed connector paginable at all: the plan derives the next
    /// request from the *unsigned* one, so every page is signed over its own
    /// URL and query rather than inheriting page one's signature.
    async fn walk(
        &self,
        operation: &str,
        pagination: &Pagination,
        request: RequestPlan,
        deadline: tokio::time::Instant,
        applied: Option<&donat_connectors::sdk::AccessToken>,
    ) -> Result<JsonValue, ProviderAttemptFailure> {
        let budget = self
            .runtime
            .pagination_budget(operation)
            .with_deadline(deadline);
        // A `401` on any page is the one fact the credential seam may act on,
        // and it has to survive the gate that classified it.
        let unauthorized = AtomicBool::new(false);
        let walked = pagination
            .collect_pages(
                request,
                self.runtime.origin(),
                &budget,
                |status, headers, body| {
                    if status == 401 {
                        unauthorized.store(true, Ordering::Relaxed);
                    }
                    self.runtime.admit_page(operation, status, headers, body)
                },
                |mut request| async move {
                    self.authenticate(&mut request, applied)?;
                    self.send(request, deadline).await
                },
            )
            .await
            .map_err(|failure| ProviderAttemptFailure {
                unauthorized: unauthorized.load(Ordering::Relaxed),
                failure,
            })?;

        let status = walked.status();
        let headers = walked.headers().clone();
        let aggregate = serde_json::to_vec(&pagination.aggregate(walked))
            .expect("a walked aggregate of parsed provider pages serializes");
        self.runtime
            .decode(operation, status, &headers, &aggregate)
            .map_err(|failure| ProviderAttemptFailure {
                unauthorized: unauthorized.load(Ordering::Relaxed),
                failure,
            })
    }

    /// Send one prepared request under this attempt's deadline, resolving the
    /// host again immediately before connecting and refusing a peer this
    /// attempt did not resolve.
    async fn send(
        &self,
        request: RequestPlan,
        deadline: tokio::time::Instant,
    ) -> Result<RawHttpResponse, ConnectorFailure> {
        let destination = self.resolve_under_deadline(deadline).await?;
        let response = tokio::time::timeout_at(
            deadline,
            self.transport
                .execute(request.into_prepared()?, &destination, deadline),
        )
        .await
        .map_err(|_| timeout_failure())?
        .map_err(|error| match error.kind() {
            TransportErrorKind::Transport => transport_failure(),
            TransportErrorKind::Timeout => timeout_failure(),
            TransportErrorKind::ResponseTooLarge => ConnectorFailure::new(
                ConnectorErrorClass::Validation,
                "connector_validation",
                "connector response exceeds the 1 MiB limit",
            ),
        })?;
        validate_connected_peer(&destination, response.peer())?;
        Ok(response)
    }

    async fn resolve_under_deadline(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<IpAddr>, ConnectorFailure> {
        let url = self.runtime.origin().as_url();
        let host = url.host_str().expect("a resolved origin has a host");
        let port = url
            .port_or_known_default()
            .expect("a resolved origin has a known port");
        let addresses = tokio::time::timeout_at(deadline, self.resolver.resolve(host, port))
            .await
            .map_err(|_| timeout_failure())?
            .map_err(|_| transport_failure())?;
        if addresses.is_empty() {
            return Err(transport_failure());
        }
        Ok(addresses)
    }
}

/// One failed provider attempt, and the one fact about it the credential seam
/// may act on.
///
/// `unauthorized` is deliberately not a class: the failure it carries is
/// whatever the operation's own error map produced for the response, and a
/// `401` that an operation declared `permanent` stays permanent. All this flag
/// says is "the provider answered 401", which is what
/// [[043-the-credential-seam-refuses-before-it-sends]] allows one refresh and
/// one replay to be triggered by.
pub(crate) struct ProviderAttemptFailure {
    pub(crate) failure: ConnectorFailure,
    pub(crate) unauthorized: bool,
}

impl From<ConnectorFailure> for ProviderAttemptFailure {
    fn from(failure: ConnectorFailure) -> Self {
        Self {
            failure,
            unauthorized: false,
        }
    }
}

fn validate_connected_peer(
    destination: &[IpAddr],
    peer: Option<SocketAddr>,
) -> Result<(), ConnectorFailure> {
    let Some(peer) = peer else {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "connector_invariant",
            "connector transport could not verify the connected peer",
        ));
    };
    if !destination.contains(&peer.ip()) {
        return Err(ConnectorFailure::new(
            ConnectorErrorClass::Invariant,
            "connector_invariant",
            "connector transport connected to an unresolved peer",
        ));
    }
    Ok(())
}

/// A module that declares no credential plan cannot send anything at all.
fn no_credential_plan() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Invariant,
        "connector_invariant",
        "connector module declares no credential plan",
    )
}

fn transport_failure() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Transport,
        "connector_transport",
        "connector transport failed",
    )
}

fn timeout_failure() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Timeout,
        "connector_timeout",
        "connector activity deadline elapsed",
    )
}

// ---------------------------------------------------------------------------
// The deployment fingerprint.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ProviderConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    connector_contract_version: &'a str,
    operation_name: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    /// The non-secret deploy-time values themselves: a Region, a bucket, a
    /// queue and its type all change what a pinned operation does.
    settings: &'a BTreeMap<String, String>,
    /// The *names* of the environment variables behind this instance's
    /// secrets. No resolved value enters a fingerprint.
    secret_environment: BTreeMap<&'a str, &'a str>,
    capacity: Option<&'a donat_metadata::ConnectorCapacity>,
}

/// The immutable, non-secret deployment identity of one compiled operation.
fn provider_configuration_fingerprint(
    definition: ConnectorDefinition,
    connector: &Connector,
    config: &ConnectorConfig,
    operation: &donat_metadata::ConnectorOperation,
) -> String {
    let mut secret_environment = BTreeMap::new();
    if let Some(reference) = &config.secret_key {
        secret_environment.insert("secret_key", reference.value_from_env.as_str());
    }
    for (name, reference) in &config.secrets {
        secret_environment.insert(name.as_str(), reference.value_from_env.as_str());
    }
    let canonical = ProviderConfigurationFingerprint {
        module_name: definition.module_name,
        module_semantic_version: definition.semantic_version,
        runtime_abi: definition.runtime_abi,
        connector_contract_version: connector.version(),
        operation_name: &operation.name,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        settings: &config.settings,
        secret_environment,
        capacity: operation.capacity(),
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated provider fingerprint fields always serialize to JSON");
    format!("{:x}", Sha256::digest(bytes))
}

// ---------------------------------------------------------------------------
// Reading one instance's deploy-time configuration.
// ---------------------------------------------------------------------------

/// A non-secret deploy-time value, or a startup failure naming its key.
pub(crate) fn required_setting<'a>(
    config: &'a ConnectorConfig,
    name: &str,
) -> Result<&'a str, String> {
    match config.settings.get(name) {
        Some(value) if !value.is_empty() => Ok(value.as_str()),
        _ => Err(format!("config.settings.{name} is required")),
    }
}

pub(crate) fn optional_setting<'a>(config: &'a ConnectorConfig, name: &str) -> Option<&'a str> {
    config
        .settings
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

/// One optional numeric bound, parsed where it is read rather than where it is
/// used, so a deployment that mistypes one is refused by name.
pub(crate) fn optional_usize_setting(
    config: &ConnectorConfig,
    name: &str,
) -> Result<Option<usize>, String> {
    match optional_setting(config, name) {
        None => Ok(None),
        Some(value) => value
            .parse::<usize>()
            .map(Some)
            .map_err(|_| format!("config.settings.{name} must be a positive whole number")),
    }
}

/// The single API key of a connector that authenticates with one.
pub(crate) fn resolve_secret_key(config: &ConnectorConfig) -> Result<String, String> {
    let reference = config
        .secret_key
        .as_ref()
        .ok_or_else(|| "config.secret_key is required".to_owned())?;
    resolve_environment(&reference.value_from_env)
}

/// The inbound signing secret of a connector that publishes a webhook route.
pub(crate) fn resolve_webhook_secret(config: &ConnectorConfig) -> Result<String, String> {
    let reference = config
        .webhook_secret
        .as_ref()
        .ok_or_else(|| "config.webhook_secret is required".to_owned())?;
    resolve_environment(&reference.value_from_env)
}

/// One further named secret, such as an AWS access key.
pub(crate) fn resolve_secret(config: &ConnectorConfig, name: &str) -> Result<String, String> {
    let reference = config
        .secrets
        .get(name)
        .ok_or_else(|| format!("config.secrets.{name} is required"))?;
    resolve_environment(&reference.value_from_env)
}

pub(crate) fn resolve_optional_secret(
    config: &ConnectorConfig,
    name: &str,
) -> Result<Option<String>, String> {
    match config.secrets.get(name) {
        None => Ok(None),
        Some(reference) => resolve_environment(&reference.value_from_env).map(Some),
    }
}

/// Resolve one environment variable, reporting its *name* and never its value.
/// Availability was already checked by startup validation; this is the second
/// half of the same answer.
fn resolve_environment(variable: &str) -> Result<String, String> {
    std::env::var(variable)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("environment variable `{variable}` is unavailable"))
}

// ---------------------------------------------------------------------------
// One module's deploy-time metadata rules.
// ---------------------------------------------------------------------------

/// How a module's credential is configured.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CredentialShape {
    /// One API key or token in `config.secret_key`.
    SecretKey,
    /// Several named secrets in `config.secrets`, and no `secret_key`.
    NamedSecrets,
    /// Authorization-code OAuth2: `config.oauth2` and no configured secret at
    /// all. The token is the source-local credential store's, written by
    /// `donat connector authorize` and refreshed on use (spec 011).
    Oauth2,
}

/// Whether a module publishes an inbound route, and therefore whether it reads
/// `config.webhook_secret`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookShape {
    /// The module declares no trigger. A `webhook_secret` here is configuration
    /// nothing reads, so it is refused rather than ignored.
    None,
    /// The module declares triggers and signs or compares against one secret.
    RequiredSecret,
}

/// One configuration key a module reads.
pub(crate) struct Key {
    pub(crate) name: &'static str,
    pub(crate) required: bool,
}

impl Key {
    pub(crate) const fn required(name: &'static str) -> Self {
        Self {
            name,
            required: true,
        }
    }

    pub(crate) const fn optional(name: &'static str) -> Self {
        Self {
            name,
            required: false,
        }
    }
}

/// The closed deploy-time configuration surface of one hand-written connector.
///
/// It is the module's own rules, carried with the module: the compiled table
/// reaches them, and nothing in `state.rs` has to remember that this connector
/// exists.
pub(crate) struct ProviderRules {
    pub(crate) module: &'static str,
    pub(crate) credential: CredentialShape,
    pub(crate) webhook: WebhookShape,
    pub(crate) settings: &'static [Key],
    pub(crate) secrets: &'static [Key],
}

impl ProviderRules {
    /// Everything about one instance that can be decided from metadata alone.
    pub(crate) fn validate(
        &self,
        instance: &ConnectorInstance,
        path: &str,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        let config = &instance.config;
        let module = self.module;
        let mut refuse = |field: &str, message: String| {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.{field}"),
                message,
            ));
        };
        // A hand-written connector has one fixed provider origin and one
        // declared credential plan. Each of these fields describes the
        // declarative `http` module instead, so accepting one would be
        // configuration nothing reads.
        if config.base_url.is_some() {
            refuse(
                "base_url",
                format!(
                    "the `{module}` connector has a fixed provider origin and does not accept base_url"
                ),
            );
        }
        if config.network_policy.is_some() {
            refuse(
                "network_policy",
                format!("the `{module}` connector does not accept network_policy"),
            );
        }
        if !config.headers.is_empty() {
            refuse(
                "headers",
                format!(
                    "the `{module}` connector applies its own declared credential and does not accept configured headers"
                ),
            );
        }
        if config.api_version.is_some() {
            refuse(
                "api_version",
                format!(
                    "the `{module}` connector pins its own provider contract version and does not accept api_version"
                ),
            );
        }
        match self.webhook {
            WebhookShape::None if config.webhook_secret.is_some() => refuse(
                "webhook_secret",
                format!(
                    "the `{module}` connector declares no inbound trigger and does not accept webhook_secret"
                ),
            ),
            WebhookShape::RequiredSecret if config.webhook_secret.is_none() => refuse(
                "webhook_secret",
                format!(
                    "webhook_secret is required for the `{module}` connector, which publishes a signed inbound route"
                ),
            ),
            _ => {}
        }
        match (self.credential, config.oauth2.is_some()) {
            (CredentialShape::Oauth2, false) => refuse(
                "oauth2",
                format!(
                    "`oauth2` is required for the `{module}` connector, which authenticates with an authorization-code OAuth2 credential"
                ),
            ),
            (CredentialShape::SecretKey | CredentialShape::NamedSecrets, true) => refuse(
                "oauth2",
                format!(
                    "the `{module}` connector authenticates with a configured key and cannot apply an OAuth2 credential; remove `config.oauth2`"
                ),
            ),
            _ => {}
        }
        match self.credential {
            CredentialShape::SecretKey if config.secret_key.is_none() => refuse(
                "secret_key",
                format!("secret_key is required for the `{module}` connector"),
            ),
            CredentialShape::NamedSecrets if config.secret_key.is_some() => refuse(
                "secret_key",
                format!(
                    "the `{module}` connector reads its credential from `config.secrets`; remove `config.secret_key`"
                ),
            ),
            // The stored token is not deploy-time configuration, so a
            // configured secret here is a value nothing reads.
            CredentialShape::Oauth2 if config.secret_key.is_some() => refuse(
                "secret_key",
                format!(
                    "the `{module}` connector reads its credential from the source-local OAuth2 credential store; remove `config.secret_key`"
                ),
            ),
            _ => {}
        }
        validate_keys(
            self.settings,
            config.settings.keys().map(String::as_str),
            |name| required_setting(config, name).is_ok(),
            module,
            "settings",
            path,
            errors,
        );
        validate_keys(
            self.secrets,
            config.secrets.keys().map(String::as_str),
            |name| config.secrets.contains_key(name),
            module,
            "secrets",
            path,
            errors,
        );

        validate_connector_operation_defaults(instance, path, errors);
        for (index, operation) in instance.operations.iter().enumerate() {
            if !matches!(&operation.profile, ConnectorOperationProfile::Undeclared(_)) {
                errors.push(ConnectorConfigError::new(
                    format!("{path}.operations[{index}]"),
                    format!(
                        "the `{module}` connector compiles its own request shape; an operation here \
                         has no configurable HTTP profile"
                    ),
                ));
            }
            if instance.operations[..index]
                .iter()
                .any(|earlier| earlier.name == operation.name)
            {
                errors.push(ConnectorConfigError::new(
                    format!("{path}.operations[{index}].name"),
                    format!(
                        "connector operation `{}` is declared more than once",
                        operation.name
                    ),
                ));
            }
        }
    }

    /// Report one module-specific refusal — an identifier a provider's own
    /// grammar rejects, a queue whose type disagrees with its name — against
    /// the metadata key that carries it.
    pub(crate) fn refuse_setting(
        &self,
        path: &str,
        setting: &str,
        message: impl std::fmt::Display,
        errors: &mut Vec<ConnectorConfigError>,
    ) {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.settings.{setting}"),
            format!(
                "the `{}` connector refuses this value: {message}",
                self.module
            ),
        ));
    }
}

/// Required keys must be present, and a key the module does not read is
/// refused rather than ignored.
fn validate_keys<'a>(
    declared: &'static [Key],
    configured: impl Iterator<Item = &'a str>,
    present: impl Fn(&str) -> bool,
    module: &str,
    section: &str,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
) {
    for key in declared {
        if key.required && !present(key.name) {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.{section}.{}", key.name),
                format!("`{}` is required for the `{module}` connector", key.name),
            ));
        }
    }
    for name in configured {
        if !declared.iter().any(|key| key.name == name) {
            errors.push(ConnectorConfigError::new(
                format!("{path}.config.{section}.{name}"),
                format!("the `{module}` connector reads no `{name}` {section} value"),
            ));
        }
    }
}

/// Turn one module's configuration failure into a registry startup failure.
pub(crate) fn invalid_configuration(
    instance: &ConnectorInstance,
) -> impl Fn(String) -> ConnectorRegistryError + use<'_> {
    move |message| ConnectorRegistryError::InvalidConfiguration {
        instance: instance.name.clone(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use donat_connectors::providers::{airtable, aws_s3, twilio};
    use donat_connectors::sdk::Secret;
    use serde_json::json;

    use super::*;

    const TOKEN_SENTINEL: &str = "donat-provider-token-sentinel-do-not-log";
    const ACCOUNT_SID: &str = "AC00000000000000000000000000000042";

    fn airtable_provider() -> DeclaredProvider {
        DeclaredProvider::compile(
            airtable::connector().clone(),
            Credential::from_fields([
                ("secret", Secret::new(TOKEN_SENTINEL)),
                (airtable::BASE_ID, Secret::new("appDeployTimeBase")),
            ]),
            ConnectorConfiguration::from_deployment([(airtable::BASE_ID, "appDeployTimeBase")]),
            airtable::error_map(),
            airtable::base_scoped_input,
            airtable::pagination,
        )
        .expect("a configured Airtable instance compiles")
    }

    /// The account-scoped identifier is deploy-time material: it reaches the
    /// request from configuration, and an input that names one of its own is
    /// refused rather than overwritten.
    #[test]
    fn a_declared_provider_renders_the_configured_account_scope_and_refuses_one_from_input() {
        let provider = airtable_provider();
        let request = provider
            .plan(
                "record.list",
                &json!({ "table": "Grid view" }),
                "activity-1",
            )
            .expect("the declared read renders");

        assert_eq!(
            request.url().as_str(),
            "https://api.airtable.com/v0/appDeployTimeBase/Grid%20view"
        );

        let refused = provider
            .plan(
                "record.list",
                &json!({ "table": "Grid", "base_id": "appAttacker" }),
                "activity-1",
            )
            .expect_err("an input that names deploy-time configuration is refused");
        assert_eq!(refused.class(), ConnectorErrorClass::Invariant);
    }

    /// The credential reaches the wire in exactly the form the connector
    /// declared, and nothing this module can print carries its value.
    #[test]
    fn a_declared_provider_applies_only_its_declared_credential_plan() {
        let provider = airtable_provider();
        let mut request = provider
            .plan("record.list", &json!({ "table": "Grid" }), "activity-1")
            .expect("the declared read renders");
        assert!(
            request.headers().get("authorization").is_none(),
            "a rendered request carries no credential until the plan applies one"
        );

        provider
            .auth_plan()
            .expect("Airtable declares a bearer plan")
            .apply(provider.credential(), &mut request, None)
            .expect("the declared plan applies");
        assert_eq!(
            request
                .headers()
                .get("authorization")
                .expect("the bearer plan sets one")
                .to_str()
                .ok(),
            Some(format!("Bearer {TOKEN_SENTINEL}").as_str())
        );

        let failure = provider
            .decode(
                "record.list",
                429,
                &HeaderMap::new(),
                br#"{"error":{"type":"RATE_LIMITED","message":"do not forward this"}}"#,
            )
            .expect_err("a documented failure status is classified");
        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert!(!failure.diagnostic().contains("do not forward this"));
        assert!(!failure.diagnostic().contains(TOKEN_SENTINEL));
    }

    /// Twilio's declaration is completed by one deployment's Account SID, and
    /// both halves of it — the Basic username and the path — come from that one
    /// configured value.
    #[test]
    fn the_twilio_declaration_carries_its_account_into_the_path_and_the_credential() {
        let connector = twilio::connector(ACCOUNT_SID).expect("a valid account SID declares");
        let provider = DeclaredProvider::compile(
            connector,
            Credential::from_fields([
                ("secret", Secret::new(TOKEN_SENTINEL)),
                (twilio::ACCOUNT_SID, Secret::new(ACCOUNT_SID)),
            ]),
            ConnectorConfiguration::from_deployment([(twilio::ACCOUNT_SID, ACCOUNT_SID)]),
            twilio::error_map(),
            twilio::account_scoped_input,
            twilio::pagination,
        )
        .expect("a configured Twilio instance compiles");

        let mut request = provider
            .plan("message.list", &json!({}), "activity-1")
            .expect("the declared read renders");
        assert_eq!(
            request.url().path(),
            format!("/2010-04-01/Accounts/{ACCOUNT_SID}/Messages.json")
        );
        provider
            .auth_plan()
            .expect("Twilio declares a basic plan")
            .apply(provider.credential(), &mut request, None)
            .expect("the declared plan applies");
        let applied = request
            .headers()
            .get("authorization")
            .expect("the basic plan sets one")
            .to_str()
            .expect("a basic credential is visible ASCII")
            .to_owned();
        assert!(applied.starts_with("Basic "), "{applied}");
        assert!(
            !applied.contains(TOKEN_SENTINEL),
            "the auth token is encoded, never echoed"
        );
    }

    /// An AWS instance renders the bucket it was configured with, and an input
    /// that names one is refused before anything is signed.
    #[test]
    fn an_aws_instance_renders_its_configured_target() {
        let configuration = aws_s3::S3Configuration::new(
            "eu-west-1",
            "donat-test-bucket",
            aws_s3::BucketVersioning::Unversioned,
        )
        .expect("a valid S3 configuration");
        let instance =
            aws_s3::S3Instance::compile(&configuration).expect("a configured S3 instance compiles");
        let operation = instance
            .operation("object.get")
            .expect("the declared read exists");

        let request = instance
            .plan(operation, &json!({ "key": "reports/2026.json" }))
            .expect("the declared read renders");
        assert_eq!(
            request.url().as_str(),
            "https://s3.eu-west-1.amazonaws.com/donat-test-bucket/reports%2F2026%2Ejson"
        );
        assert!(
            instance
                .plan(operation, &json!({ "key": "a", "bucket": "attacker" }))
                .is_err(),
            "an input that names deploy-time configuration is refused"
        );
    }

    /// The registry entry refuses an operation the deployment never enabled,
    /// before any name resolution or request rendering.
    #[tokio::test]
    async fn an_unenabled_operation_is_refused_at_dispatch() {
        let instance = ProviderInstance {
            runtime: Box::new(airtable_provider()),
            operations: BTreeMap::new(),
            webhook: None,
            source_name: "default".to_owned(),
            resolver: Arc::new(SystemResolver),
            transport: Arc::new(ReqwestTransport::new()),
        };
        let failure = instance
            .execute(
                "record.list",
                json!({ "table": "Grid" }),
                "activity-1",
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .await
            .expect_err("an operation the deployment did not enable is not dispatched");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        assert_eq!(
            failure.safe_message(),
            "connector operation is not declared"
        );
    }
}

/// The serving executor walking a declared plan, end to end, against the SDK's
/// own provider stub.
///
/// Everything here goes through `RegisteredConnector::execute` — the entry
/// point a durable activity dispatches on — and every case asserts how many
/// requests the provider actually received. That assertion is the point: an
/// executor that ignored the declaration and sent one request would satisfy
/// every assertion about the *content* of a single page, which is exactly how
/// the defect [[034-a-declaration-the-runtime-ignores-is-a-defect]] names
/// survived twenty-eight connector modules
/// (`knowledgebase/declarative-saas/decisions/058-*`).
#[cfg(test)]
mod walk_tests {
    use std::sync::LazyLock;
    use std::time::Duration;

    use donat_connectors::providers::{aws, aws_ses};
    use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
    use donat_connectors::sdk::{
        CredentialSpec, Effect, ErrorMap, OriginSpec, Pagination, Required,
    };
    use donat_ir::ValueScalar;
    use reqwest::StatusCode;
    use serde_json::json;

    use super::*;

    /// The plans the test connector declares, in the shape a provider module
    /// declares its own: a lookup from operation name to a static plan.
    fn pagination(operation_id: &str) -> Option<&'static Pagination> {
        static CURSOR: LazyLock<Pagination> = LazyLock::new(|| {
            Pagination::cursor("/data", "cursor", "/next", "limit", 2).expect("a valid plan")
        });
        static OFFSET: LazyLock<Pagination> = LazyLock::new(|| {
            Pagination::offset_limit("/data", "offset", "limit", 2).expect("a valid plan")
        });
        static PAGE: LazyLock<Pagination> = LazyLock::new(|| {
            Pagination::page_number("/data", "page", "per_page", 2).expect("a valid plan")
        });
        static LINK: LazyLock<Pagination> =
            LazyLock::new(|| Pagination::link_header("/data", "next").expect("a valid plan"));
        static TOKEN: LazyLock<Pagination> = LazyLock::new(|| {
            Pagination::token_in_body("/data", "/next_token", "page_token").expect("a valid plan")
        });
        static URI: LazyLock<Pagination> =
            LazyLock::new(|| Pagination::next_uri_in_body("/data", "/next").expect("a valid plan"));
        match operation_id {
            "cursor.list" => Some(&CURSOR),
            "offset.list" => Some(&OFFSET),
            "page.list" => Some(&PAGE),
            "link.list" => Some(&LINK),
            "token.list" => Some(&TOKEN),
            "uri.list" => Some(&URI),
            // `single.get` declares none, which is the case ADR 055 records.
            _ => None,
        }
    }

    /// The test connector's own error map: a `429` is retryable and everything
    /// else is permanent, so a mid-walk `429` that came out `permanent` would
    /// be the built-in fallback rather than this declaration.
    fn error_map() -> &'static ErrorMap {
        static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
            ErrorMap::builder(ConnectorErrorClass::Permanent)
                .on_status(429, ConnectorErrorClass::Http429)
                .on_status(401, ConnectorErrorClass::Authentication)
                .build()
                .expect("a static error map is valid")
        });
        &MAP
    }

    fn listing(id: &str, path: &str) -> Operation {
        Operation::get(id, path)
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .output_pointer("items", "/data", ValueScalar::Json, Required::Yes)
            .effect(Effect::read_only())
            .build()
            .expect("a static declaration is valid")
    }

    /// One connector whose origin is the stub's, declaring one operation per
    /// plan family.
    fn connector(origin: &str) -> Connector {
        Connector::declare("testwalk", "1.0.0")
            .origin(OriginSpec::fixed(origin).expect("a loopback origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations([
                listing("cursor.list", "/v1/cursor"),
                listing("offset.list", "/v1/offset"),
                listing("page.list", "/v1/page"),
                listing("link.list", "/v1/link"),
                listing("token.list", "/v1/token"),
                listing("uri.list", "/v1/uri"),
                listing("single.get", "/v1/single"),
            ])
            .build()
            .expect("a static declaration is valid")
    }

    /// One compiled instance of it, with every operation enabled.
    fn instance(stub: &ProviderStub) -> ProviderInstance {
        runtime_instance(Box::new(
            DeclaredProvider::compile(
                connector(stub.base_url()),
                Credential::secret(SECRET_SENTINEL),
                ConnectorConfiguration::default(),
                error_map(),
                bind_nothing,
                pagination,
            )
            .expect("a configured instance compiles"),
        ))
    }

    fn runtime_instance(runtime: Box<dyn ProviderRuntime>) -> ProviderInstance {
        let operations = [
            "cursor.list",
            "offset.list",
            "page.list",
            "link.list",
            "token.list",
            "uri.list",
            "single.get",
            "template.list",
        ]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                CompiledProviderOperation {
                    configuration_fingerprint: String::new(),
                    serialization_key_input: None,
                },
            )
        })
        .collect();
        ProviderInstance {
            runtime,
            operations,
            webhook: None,
            source_name: "default".to_owned(),
            resolver: Arc::new(SystemResolver),
            transport: Arc::new(ReqwestTransport::new()),
        }
    }

    async fn execute(
        instance: &ProviderInstance,
        operation: &str,
    ) -> Result<JsonValue, ConnectorFailure> {
        instance
            .execute(
                operation,
                json!({}),
                "activity-1",
                tokio::time::Instant::now() + Duration::from_secs(10),
            )
            .await
            .map(|success| success.output)
    }

    /// Every plan family walks at serve time, and every one of them is asserted
    /// by the number of requests the provider received.
    #[tokio::test]
    async fn the_executor_walks_every_declared_plan_family() {
        // Cursor: the token is echoed back as a query value until it is absent.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/cursor")
                .query("limit=2")
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, json!({ "data": [1, 2], "next": "c2" })),
            Expectation::new("GET", "/v1/cursor")
                .query("limit=2&cursor=c2")
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            execute(&instance(&stub), "cursor.list")
                .await
                .expect("the cursor walk completes"),
            json!({ "items": [1, 2, 3] })
        );
        assert_eq!(stub.received(), 2, "a cursor walk is two requests");
        stub.assert_satisfied();

        // Offset/limit: a short page ends the walk.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/offset")
                .query("offset=0&limit=2")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/offset")
                .query("offset=2&limit=2")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            execute(&instance(&stub), "offset.list")
                .await
                .expect("the offset walk completes"),
            json!({ "items": [1, 2, 3] })
        );
        assert_eq!(stub.received(), 2, "an offset walk is two requests");
        stub.assert_satisfied();

        // Page number: the number is derived from the walk, never the provider.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/page")
                .query("page=1&per_page=2")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/page")
                .query("page=2&per_page=2")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            execute(&instance(&stub), "page.list")
                .await
                .expect("the page-number walk completes"),
            json!({ "items": [1, 2, 3] })
        );
        assert_eq!(stub.received(), 2, "a page-number walk is two requests");
        stub.assert_satisfied();

        // Link header: the continuation is a URL on the compiled origin.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/link")
                .respond_header("link", "<{base_url}/v1/link?page=2>; rel=\"next\"")
                .respond_json(200, json!({ "data": [1, 2] })),
            Expectation::new("GET", "/v1/link")
                .query("page=2")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            execute(&instance(&stub), "link.list")
                .await
                .expect("the link walk completes"),
            json!({ "items": [1, 2, 3] })
        );
        assert_eq!(stub.received(), 2, "a link walk is two requests");
        stub.assert_satisfied();

        // Token in body: the token is a query value and only ever that.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/token")
                .query("")
                .respond_json(200, json!({ "data": [1, 2], "next_token": "t2" })),
            Expectation::new("GET", "/v1/token")
                .query("page_token=t2")
                .respond_json(200, json!({ "data": [3], "next_token": null })),
        ])
        .await;
        assert_eq!(
            execute(&instance(&stub), "token.list")
                .await
                .expect("the token walk completes"),
            json!({ "items": [1, 2, 3] })
        );
        assert_eq!(stub.received(), 2, "a token walk is two requests");
        stub.assert_satisfied();

        // Next URI in body: a relative continuation resolved against the origin.
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/uri")
                .respond_json(200, json!({ "data": [1, 2], "next": "/v1/uri?Page=1" })),
            Expectation::new("GET", "/v1/uri")
                .query("Page=1")
                .respond_json(200, json!({ "data": [3] })),
        ])
        .await;
        assert_eq!(
            execute(&instance(&stub), "uri.list")
                .await
                .expect("the next-URI walk completes"),
            json!({ "items": [1, 2, 3] })
        );
        assert_eq!(stub.received(), 2, "a next-URI walk is two requests");
        stub.assert_satisfied();
    }

    /// A connector that declares no plan sends exactly one request, and a body
    /// that looks like a continuation changes nothing
    /// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
    #[tokio::test]
    async fn an_operation_with_no_declared_plan_is_exactly_one_request() {
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/single")
            .query("")
            .respond_json(
                200,
                json!({ "data": [1, 2], "next": "c2", "next_token": "t2", "has_more": true }),
            )])
        .await;
        assert_eq!(
            execute(&instance(&stub), "single.get")
                .await
                .expect("the single request completes"),
            json!({ "items": [1, 2] }),
            "the page is returned as it arrived, cursor and all"
        );
        assert_eq!(
            stub.received(),
            1,
            "an operation with no declared plan is one request"
        );
        stub.assert_satisfied();
    }

    /// One walk, one budget: the ceilings and the attempt's own deadline are
    /// spent across every page, and a walk that exhausts one returns nothing.
    #[tokio::test]
    async fn one_walk_shares_one_budget_and_a_budget_failure_yields_no_partial_output() {
        // An endless provider: sixteen pages is the SDK's default call ceiling,
        // so the seventeenth request is never made and the sixteen pages
        // already collected are thrown away with the failure.
        let stub = ProviderStub::start((0..64).map(|index| {
            Expectation::new("GET", "/v1/cursor")
                .respond_json(200, json!({ "data": [index], "next": "more" }))
        }))
        .await;
        let failure = execute(&instance(&stub), "cursor.list")
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "connector_pagination_budget");
        assert_eq!(
            stub.received(),
            PaginationBudget::DEFAULT_MAX_CALLS as usize,
            "the call ceiling is spent across the walk, not per page"
        );

        // The activity's deadline is the walk's deadline: it is not restarted
        // per page, so a walk whose pages are each fast enough still stops.
        let stub = ProviderStub::start((0..8).map(|index| {
            Expectation::new("GET", "/v1/cursor")
                .delay(Duration::from_millis(60))
                .respond_json(200, json!({ "data": [index], "next": "more" }))
        }))
        .await;
        let failure = instance(&stub)
            .execute(
                "cursor.list",
                json!({}),
                "activity-1",
                tokio::time::Instant::now() + Duration::from_millis(150),
            )
            .await
            .expect_err("the shared deadline stops the walk");
        assert_eq!(failure.class(), ConnectorErrorClass::Timeout);
        assert!(
            stub.received() < 8,
            "the walk stopped at the deadline it shared with the attempt"
        );
    }

    /// A page that fails is classified by the operation's own error map, not by
    /// the walk's built-in fallback — and the provider's retry hint survives.
    ///
    /// A `429` on page three of a listing is the same retryable failure it
    /// would be on page one. Before this, the walk answered every non-2xx page
    /// with one `permanent` classification and discarded `Retry-After`, so an
    /// activity declaring `retry_on: [http_429]` refused to retry the failure
    /// it was written for.
    #[tokio::test]
    async fn a_failing_page_is_classified_by_the_operations_own_error_map() {
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/cursor")
                .respond_json(200, json!({ "data": [1, 2], "next": "c2" })),
            Expectation::new("GET", "/v1/cursor")
                .respond_json(200, json!({ "data": [3, 4], "next": "c3" })),
            Expectation::new("GET", "/v1/cursor")
                .respond_header("retry-after", "30")
                .respond_json(429, json!({ "error": "slow down" })),
        ])
        .await;
        let failure = execute(&instance(&stub), "cursor.list")
            .await
            .expect_err("a rate-limited page fails the walk");
        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Http429,
            "the operation's own map classifies a mid-walk failure"
        );
        assert_eq!(failure.provider_status(), Some(429));
        assert_eq!(
            failure.retry_after(),
            Some(Duration::from_secs(30)),
            "the provider's retry hint survives the walk"
        );
        assert_eq!(stub.received(), 3, "the walk stopped at the failing page");
        stub.assert_satisfied();
    }

    /// A continuation that would leave the compiled origin is refused, and the
    /// other origin is never contacted.
    #[tokio::test]
    async fn a_continuation_off_the_compiled_origin_is_refused_before_any_request() {
        let elsewhere = ProviderStub::start([
            Expectation::new("GET", "/v1/link").respond_json(200, json!({ "data": [99] }))
        ])
        .await;
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/link")
            .respond_header(
                "link",
                &format!("<{}/v1/link>; rel=\"next\"", elsewhere.base_url()),
            )
            .respond_json(200, json!({ "data": [1, 2] }))])
        .await;

        let failure = execute(&instance(&stub), "link.list")
            .await
            .expect_err("a cross-origin continuation is not followed");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
        assert_eq!(failure.code(), "connector_pagination_cross_origin");
        assert_eq!(stub.received(), 1);
        assert_eq!(
            elsewhere.received(),
            0,
            "the other origin was never contacted"
        );
    }

    /// Every page of a signed walk is signed over the request that page is
    /// actually sending.
    ///
    /// AWS SigV4 covers the canonical query string, so a continuation carries a
    /// different canonical request. An executor that authenticated once, before
    /// the walk, would send page two with page one's signature and earn
    /// `SignatureDoesNotMatch` — classified `authentication`, which no Process
    /// retries. The two signatures below must therefore differ, and both must
    /// be well formed.
    #[tokio::test]
    async fn every_page_of_a_signed_walk_carries_its_own_signature() {
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v1/token")
                .query("")
                .respond_json(200, json!({ "data": [1], "next_token": "t2" })),
            Expectation::new("GET", "/v1/token")
                .query("page_token=t2")
                .respond_json(200, json!({ "data": [2] })),
        ])
        .await;
        let runtime = SignedRuntime {
            connector: connector(stub.base_url()),
            origin: Origin::parse(stub.base_url()).expect("a loopback origin is valid"),
            credential: aws::credential("AKIDEXAMPLE", SECRET_SENTINEL, "eu-west-1", None),
            plan: AuthPlan::aws_sigv4(aws_ses::SERVICE).expect("a static service code is valid"),
        };
        assert_eq!(
            execute(&runtime_instance(Box::new(runtime)), "token.list")
                .await
                .expect("the signed walk completes"),
            json!({ "items": [1, 2] })
        );
        assert_eq!(stub.received(), 2, "a signed walk is still two requests");
        stub.assert_satisfied();

        let recorded = stub.recorded();
        let authorization = |index: usize| {
            recorded[index]
                .header("authorization")
                .expect("every page is signed")
                .to_owned()
        };
        for index in 0..2 {
            assert!(
                authorization(index).starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"),
                "page {index} is signed: {}",
                authorization(index)
            );
            assert!(
                authorization(index).contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"),
                "page {index} signs its own host and date"
            );
            assert!(
                !authorization(index).contains(SECRET_SENTINEL),
                "a signature never carries the secret it was derived from"
            );
        }
        assert_ne!(
            authorization(0),
            authorization(1),
            "page two must be signed over page two's own query"
        );
    }

    /// A signed connector, which is what makes the per-page signature
    /// observable: everything else about it is the declaration.
    struct SignedRuntime {
        connector: Connector,
        origin: Origin,
        credential: Credential,
        plan: AuthPlan,
    }

    impl ProviderRuntime for SignedRuntime {
        fn origin(&self) -> &Origin {
            &self.origin
        }

        fn auth_plan(&self) -> Option<&AuthPlan> {
            Some(&self.plan)
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
            self.operation(id)?.plan_request(&self.origin, input)
        }

        fn decode(
            &self,
            id: &str,
            status: u16,
            headers: &HeaderMap,
            body: &[u8],
        ) -> Result<JsonValue, ConnectorFailure> {
            let operation = self.operation(id)?;
            if !operation.is_success(status) {
                return Err(error_map().classify(status, headers, body));
            }
            operation.decode_response(status, body)
        }

        fn pagination(&self, id: &str) -> Option<Pagination> {
            pagination(id).cloned()
        }
    }

    impl SignedRuntime {
        fn operation(&self, id: &str) -> Result<&Operation, ConnectorFailure> {
            self.connector
                .operation(id)
                .ok_or_else(|| ConnectorFailure::invariant("connector operation is not compiled"))
        }
    }
}
