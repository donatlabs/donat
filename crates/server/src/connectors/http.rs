//! The deploy-time declarative HTTP connector.
//!
//! This module receives a static operation template and JSON values for its
//! declared slots.  There is intentionally no API accepting a caller-supplied
//! URL, method, header name, redirect policy, or TLS policy.
//!
//! The request itself is an SDK [`donat_connectors::sdk::Operation`]: the path
//! template and its per-value encoding, the query, the JSON body template, the
//! declared success statuses, and the declared response pointers are all the
//! SDK's, so this connector and every hand-written one render a request through
//! the same code.  What stays here is what is specific to being *declarative*:
//! compiling v2 metadata into that declaration, the deploy-time configured
//! headers, the idempotency binding, and the operation's own error map.
//!
//! The one spelling difference is deliberate.  Deployment metadata writes a
//! path slot as `{input.order_id}` and the SDK writes it as `{order_id}`, so
//! the builder translates as it compiles.  Metadata is a published format; the
//! SDK declaration is ours.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use donat_connectors::sdk::{
    Connector, CredentialSpec, Effect, ExplicitKeyEvidence, IdempotencyBinding,
    Operation as SdkOperation, OperationBuilder as SdkOperationBuilder, Origin, OriginSpec,
    Required,
};
use donat_ir::ValueScalar;
use donat_metadata::{ConnectorBaseUrl, ConnectorConfig, ConnectorOperation};
use reqwest::{
    StatusCode, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use futures_util::future::BoxFuture;
use serde::Serialize;

use super::{
    ConnectorDefinition, ConnectorErrorClass, ConnectorFailure, ConnectorModule,
    ConnectorRegistryError, ConnectorSuccess, ExecutionContext, HTTP_DEFINITION, ModuleContext,
    RegisteredConnector, canonical_json_sha256, catalog,
};
use crate::state::ConnectorConfigError;

/// This module's static declaration (spec 010 §4).
///
/// It is the declarative connector, so two of its parts are deployment
/// material rather than constants: the origin is whatever `config.base_url`
/// names, and the credential is whatever headers the deployment declared. Its
/// operation list is empty for the same reason — a deployment authors the
/// operations, and each one is classified as it is compiled, in
/// [`HttpOperationBuilder::build`].
pub(crate) fn connector() -> &'static Connector {
    static CONNECTOR: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        Connector::declare(
            HTTP_DEFINITION.module_name,
            HTTP_DEFINITION.semantic_version,
        )
        .origin(
            OriginSpec::deployment_origin("base_url").expect("a static configuration key is valid"),
        )
        .credential(CredentialSpec::deployment_declared_headers())
        .build()
        .expect("the declarative connector's declaration is valid")
    });
    &CONNECTOR
}

/// This module's own deploy-time metadata rules.
pub(crate) fn validate_instance_metadata(
    instance: &donat_metadata::ConnectorInstance,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
) {
    if instance.config.base_url.is_none() {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.base_url"),
            "base_url is required for the http connector",
        ));
    }
    if let Err(error) = validate_http_config_metadata(&instance.config) {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.base_url"),
            error.to_string(),
        ));
    }
    if let Err(error) = validate_http_instance_metadata(&instance.config, &instance.operations) {
        errors.push(ConnectorConfigError::new(
            format!("{path}.operations"),
            error.to_string(),
        ));
    }
}

// Which destinations a connector may reach is a network-layer concern: the
// engine resolves and pins the host it was configured to call, and the
// deployment's egress rules decide what that host is allowed to be.

/// One static credential or integration header resolved from deploy-time
/// configuration.  Input can never select its name or value.
#[derive(Clone)]
pub struct ConfiguredHeader {
    name: HeaderName,
    value: HeaderValue,
}

impl ConfiguredHeader {
    pub fn new(name: &str, value: &str) -> Result<Self, HttpConfigError> {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| HttpConfigError::new("configured header name is invalid"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| HttpConfigError::new("configured header value is invalid"))?;
        Ok(Self { name, value })
    }
}

/// Resolved, immutable HTTP instance configuration.
#[derive(Clone)]
pub struct HttpConnectorConfig {
    base_url: Url,
    /// The compiled origin every request of this instance is built on. A base
    /// URL path has never reached the wire — the operation path replaces it —
    /// so the origin is derived rather than parsed, and a deployment that
    /// carries one keeps starting exactly as it does today.
    origin: Origin,
    headers: HeaderMap,
}

impl HttpConnectorConfig {
    pub fn new(base_url: &str, headers: Vec<ConfiguredHeader>) -> Result<Self, HttpConfigError> {
        let base_url = validate_http_base_url(base_url)?;
        let origin = {
            let mut origin = base_url.clone();
            origin.set_path("/");
            origin.set_query(None);
            origin.set_fragment(None);
            Origin::parse(origin.as_str())
                .map_err(|_| HttpConfigError::new("base_url must be an absolute HTTP(S) URL"))?
        };
        let mut header_names = HashSet::new();
        let mut resolved_headers = HeaderMap::new();
        for header in headers {
            if !header_names.insert(header.name.clone()) {
                return Err(HttpConfigError::new(
                    "configured header names must not collide",
                ));
            }
            resolved_headers.insert(header.name, header.value);
        }
        Ok(Self {
            base_url,
            origin,
            headers: resolved_headers,
        })
    }
}

#[derive(Debug, Clone)]
pub struct HttpConfigError {
    message: &'static str,
}

impl HttpConfigError {
    fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for HttpConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for HttpConfigError {}

fn validate_http_base_url(base_url: &str) -> Result<Url, HttpConfigError> {
    let base_url = Url::parse(base_url)
        .map_err(|_| HttpConfigError::new("base_url must be an absolute HTTP(S) URL"))?;
    if !matches!(base_url.scheme(), "http" | "https") || base_url.host_str().is_none() {
        return Err(HttpConfigError::new(
            "base_url must be an absolute HTTP(S) URL",
        ));
    }
    if !base_url.username().is_empty() || base_url.password().is_some() {
        return Err(HttpConfigError::new("base URL must not contain userinfo"));
    }
    if base_url.query().is_some() || base_url.fragment().is_some() {
        return Err(HttpConfigError::new(
            "base_url must not contain query or fragment",
        ));
    }
    Ok(base_url)
}

/// Validate every HTTP instance setting that can be checked without resolving
/// an environment value.  `validate` and `migrate` use this before opening a
/// database connection; startup reuses it before reading configuration env.
pub(crate) fn validate_http_config_metadata(
    config: &ConnectorConfig,
) -> Result<(), HttpConfigError> {
    if config.network_policy.is_some() {
        return Err(HttpConfigError::new(
            "http connector does not accept network_policy",
        ));
    }
    if let Some(ConnectorBaseUrl::Literal(base_url)) = &config.base_url {
        validate_http_base_url(base_url)?;
    }
    Ok(())
}

// The fixed method set and the JSON request template are the SDK's: a
// declarative deployment and a hand-written connector describe the same request
// in the same words.
pub use donat_connectors::sdk::{HttpMethod, JsonTemplate};

/// The SemVer core of the *declarative request shape* this module compiles.
///
/// A deployment's own `version` field is a free-form metadata identity (`v1`)
/// rather than a SemVer core, and it already enters the operation's
/// configuration fingerprint. What the SDK declaration versions is the shape
/// this module renders, which changes only when this module does.
const DECLARATIVE_REQUEST_SHAPE_VERSION: &str = "1.0.0";

#[derive(Debug, Clone)]
struct StaticHeader {
    name: HeaderName,
    value: HeaderValue,
}

#[derive(Debug, Clone)]
struct ResponsePointer {
    output_name: String,
    pointer: String,
    required: bool,
}

/// Validated static operation profile.  Its builder is intentionally internal
/// configuration-shaped rather than a raw request builder.
///
/// The request shape itself is an SDK declaration; what surrounds it here is
/// the deploy-time material the SDK deliberately does not own: the configured
/// headers, the idempotency binding, and the operation's own error map.
#[derive(Debug, Clone)]
pub struct HttpOperation {
    operation: SdkOperation,
    headers: Vec<StaticHeader>,
    declared_5xx: BTreeSet<u16>,
    /// The operation's own `error_map`: which class each status belongs to.
    /// It is what decides whether a failure is retryable, so it has to reach
    /// the runtime rather than stay a deploy-time description.
    declared_classes: BTreeMap<u16, ConnectorErrorClass>,
    /// The operation's `error_map.fallback`: the class for a status no rule
    /// names. `None` means the operation declared no error map at all, and
    /// the built-in handling answers.
    declared_fallback: Option<ConnectorErrorClass>,
    idempotency_header: Option<HeaderName>,
    /// `(slot, input)` for every path binding a deployment spent more than
    /// once, after the first (see [`sdk_path_bindings`]). Empty for almost
    /// every operation, and it stays out of the declared input contract: these
    /// slots are filled from the declared input at render, never by a caller.
    repeated_path_slots: Vec<(String, String)>,
}

pub struct HttpOperationBuilder {
    name: String,
    method: HttpMethod,
    path_template: String,
    query_inputs: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<JsonTemplate>,
    success_statuses: BTreeSet<u16>,
    response_pointers: Vec<ResponsePointer>,
    declared_5xx: BTreeSet<u16>,
    declared_classes: BTreeMap<u16, ConnectorErrorClass>,
    declared_fallback: Option<ConnectorErrorClass>,
    idempotency_header: Option<String>,
    /// The operation's effect class (spec 010 §7).
    ///
    /// A deployment authors these operations, so the class is compiled from
    /// the metadata `effect` contract in [`declarative_effect`] rather than
    /// written by hand. `None` means the caller built this operation directly
    /// rather than from metadata, and the conservative default applies: a read
    /// for a `GET`, inventory-only for anything that mutates.
    effect: Option<Effect>,
}

impl HttpOperation {
    pub fn builder(name: &str, method: HttpMethod, path_template: &str) -> HttpOperationBuilder {
        HttpOperationBuilder {
            name: name.to_owned(),
            method,
            path_template: path_template.to_owned(),
            query_inputs: Vec::new(),
            headers: Vec::new(),
            declared_classes: BTreeMap::new(),
            declared_fallback: None,
            body: None,
            success_statuses: BTreeSet::new(),
            response_pointers: Vec::new(),
            declared_5xx: BTreeSet::new(),
            idempotency_header: None,
            effect: None,
        }
    }

    fn header_names(&self) -> impl Iterator<Item = &HeaderName> {
        self.headers
            .iter()
            .map(|header| &header.name)
            .chain(self.idempotency_header.iter())
    }

    /// The render input, with every repeated path slot filled from the one
    /// declared input it repeats (see [`sdk_path_bindings`]).
    ///
    /// `None` when there is nothing to fill, which is every operation whose
    /// path spends each binding once. A missing or non-object input is left
    /// exactly as it came, so the renderer reports it as it always has.
    fn filled_repeated_path_slots(&self, input: &JsonValue) -> Option<JsonValue> {
        if self.repeated_path_slots.is_empty() {
            return None;
        }
        let JsonValue::Object(fields) = input else {
            return None;
        };
        let mut filled = fields.clone();
        for (slot, declared) in &self.repeated_path_slots {
            let Some(value) = fields.get(declared) else {
                continue;
            };
            filled.insert(slot.clone(), value.clone());
        }
        Some(JsonValue::Object(filled))
    }

    /// Whether a Process may reference this operation (spec 010 §7).
    pub(crate) fn is_executable(&self) -> bool {
        self.operation.is_executable()
    }
}

impl HttpOperationBuilder {
    pub fn query_input(mut self, name: &str, input_name: &str) -> Self {
        self.query_inputs
            .push((name.to_owned(), input_name.to_owned()));
        self
    }

    pub fn static_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn body(mut self, body: JsonTemplate) -> Self {
        self.body = Some(body);
        self
    }

    pub fn success_statuses(mut self, statuses: impl IntoIterator<Item = StatusCode>) -> Self {
        self.success_statuses = statuses.into_iter().map(|status| status.as_u16()).collect();
        self
    }

    pub fn response_pointer(mut self, output_name: &str, pointer: &str, required: bool) -> Self {
        self.response_pointers.push(ResponsePointer {
            output_name: output_name.to_owned(),
            pointer: pointer.to_owned(),
            required,
        });
        self
    }

    pub fn declared_5xx(mut self, statuses: impl IntoIterator<Item = StatusCode>) -> Self {
        self.declared_5xx = statuses.into_iter().map(|status| status.as_u16()).collect();
        self
    }

    /// Bind the operation's declared error map: status to failure class.
    /// The class a status no rule names falls back to.
    pub fn declared_fallback(mut self, class: ConnectorErrorClass) -> Self {
        self.declared_fallback = Some(class);
        self
    }

    pub fn declared_classes(
        mut self,
        classes: impl IntoIterator<Item = (u16, ConnectorErrorClass)>,
    ) -> Self {
        self.declared_classes = classes.into_iter().collect();
        self
    }

    pub fn idempotency_header(mut self, name: &str) -> Self {
        self.idempotency_header = Some(name.to_owned());
        self
    }

    /// The effect class compiled from this operation's metadata contract.
    pub(crate) fn effect(mut self, effect: Effect) -> Self {
        self.effect = Some(effect);
        self
    }

    pub fn build(self) -> Result<HttpOperation, HttpConfigError> {
        if self.name.is_empty() {
            return Err(HttpConfigError::new("connector operation name is required"));
        }
        // Validated by this module before anything else reads it, so a
        // deployment's refusal is this module's own (see
        // `validate_path_template`).
        let (sdk_path, path_bindings) = sdk_path_bindings(&self.path_template)?;
        if self.success_statuses.is_empty() {
            return Err(HttpConfigError::new(
                "connector operation must declare a success status",
            ));
        }
        // A query key is a literal on the wire, so the characters it may carry
        // are the SDK's closed set — stated here for the same reason the path
        // rule is (`knowledgebase/declarative-saas/decisions/059-*`). None of
        // them can end a key or start a second parameter, which is what makes
        // the set safe to emit unencoded.
        if self
            .query_inputs
            .iter()
            .any(|(name, input)| input.is_empty() || !is_declarable_query_key(name))
        {
            return Err(HttpConfigError::new(
                "query names and input bindings must be static and valid",
            ));
        }
        if self
            .response_pointers
            .iter()
            .any(|field| field.output_name.is_empty() || !field.pointer.starts_with('/'))
        {
            return Err(HttpConfigError::new(
                "response JSON pointers must be static and valid",
            ));
        }
        if self
            .declared_5xx
            .iter()
            .any(|status| !(500..=599).contains(status))
        {
            return Err(HttpConfigError::new(
                "declared http_5xx statuses must be 5xx",
            ));
        }
        let mut header_names = HashSet::new();
        let headers = self
            .headers
            .into_iter()
            .map(|(name, value)| {
                let name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| HttpConfigError::new("operation header name is invalid"))?;
                if !header_names.insert(name.clone()) {
                    return Err(HttpConfigError::new(
                        "operation header names must not collide",
                    ));
                }
                Ok(StaticHeader {
                    name,
                    value: HeaderValue::from_str(&value)
                        .map_err(|_| HttpConfigError::new("operation header value is invalid"))?,
                })
            })
            .collect::<Result<Vec<_>, HttpConfigError>>()?;
        let idempotency_header = self
            .idempotency_header
            .map(|name| {
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| HttpConfigError::new("idempotency header name is invalid"))
            })
            .transpose()?;
        if idempotency_header
            .as_ref()
            .is_some_and(|name| header_names.contains(name))
        {
            return Err(HttpConfigError::new(
                "operation header names must not collide",
            ));
        }

        // Everything above is the deploy-time surface. What follows is the same
        // request every hand-written connector declares, so the SDK compiles it:
        // the metadata `{input.name}` spelling becomes the SDK's `{name}`, and
        // each slot is declared untyped because the v2 metadata format types an
        // operation's contract rather than its individual path segments.
        let mut declaration = sdk_builder(&self.name, self.method, &sdk_path)
        .version(DECLARATIVE_REQUEST_SHAPE_VERSION)
        .success_statuses(self.success_statuses.iter().map(|status| {
            StatusCode::from_u16(*status).expect("a declared success status came from a StatusCode")
        }))
        // Spec 010 §7: an operation compiled from metadata carries the class
        // its declared contract earns, and one built directly here — no
        // metadata, so no declared contract — takes the conservative default.
        .effect(self.effect.unwrap_or_else(|| {
            if self.method.mutates() {
                Effect::inventory_only(
                    "a mutating operation with no declared effect contract cannot be sent twice safely",
                )
                .expect("the reason is a non-empty literal")
            } else {
                Effect::read_only()
            }
        }));
        for (slot, _) in &path_bindings {
            declaration = declaration.untyped_path_param(slot);
        }
        for (name, input) in &self.query_inputs {
            declaration = declaration.query_input(name, input);
        }
        if let Some(body) = self.body {
            declaration = declaration.body(body);
        }
        for pointer in &self.response_pointers {
            // A declarative response binding carries the metadata type of the
            // whole field, which this module has never checked against the
            // provider value; declaring the slot as `Json` keeps that exactly
            // so rather than rejecting deployments that work today.
            declaration = declaration.output_pointer(
                &pointer.output_name,
                &pointer.pointer,
                ValueScalar::Json,
                if pointer.required {
                    Required::Yes
                } else {
                    Required::No
                },
            );
        }

        Ok(HttpOperation {
            operation: declaration
                .build()
                .map_err(|error| HttpConfigError::new(error.message()))?,
            headers,
            declared_5xx: self.declared_5xx,
            declared_classes: self.declared_classes,
            declared_fallback: self.declared_fallback,
            idempotency_header,
            repeated_path_slots: path_bindings
                .into_iter()
                .filter(|(slot, input)| slot != input)
                .collect(),
        })
    }
}

fn sdk_builder(name: &str, method: HttpMethod, path: &str) -> SdkOperationBuilder {
    match method {
        HttpMethod::Get => SdkOperation::get(name, path),
        // The declarative method set is closed by `parse_method`, which does
        // not admit `HEAD`; the arm exists because the SDK's set is the one
        // hand-written connectors share, and a metadata deployment does not get
        // to decide what that set is.
        HttpMethod::Head => SdkOperation::head(name, path),
        HttpMethod::Post => SdkOperation::post(name, path),
        HttpMethod::Put => SdkOperation::put(name, path),
        HttpMethod::Patch => SdkOperation::patch(name, path),
        HttpMethod::Delete => SdkOperation::delete(name, path),
    }
}

/// Rewrite a metadata path template into the SDK's spelling, and name the slot
/// each occurrence renders from: `{input.order_id}` is the deployment format,
/// `{order_id}` is the SDK's.
///
/// A deployment may spend one binding in more than one segment —
/// `/orgs/{input.org}/repos/{input.org}` is an ordinary provider path, and this
/// module has always admitted it. The SDK's request node declares each slot
/// once, which is the right rule for a hand-written connector (a name written
/// twice there is a mistake) and would be a compatibility break here: metadata
/// that deployed yesterday would stop the engine from booting.
///
/// So the repetition is translated rather than refused. The first occurrence of
/// a binding keeps the input's own name; every later one gets a distinct slot,
/// and [`HttpOperation::repeated_path_slots`] fills those slots from the one
/// declared input at render. The operation's declared input contract is
/// unchanged — it is taken from the metadata names, not from these slots — so
/// nothing a Process binds moves.
///
/// Returns the SDK template and the `(slot, input)` pairs in occurrence order.
fn sdk_path_bindings(template: &str) -> Result<(String, Vec<(String, String)>), HttpConfigError> {
    let declared = path_input_names(template)?;
    let mut rendered = String::with_capacity(template.len());
    let mut bindings: Vec<(String, String)> = Vec::new();
    let mut remaining = template;
    while let Some(index) = remaining.find("{input.") {
        rendered.push_str(&remaining[..index]);
        let after = &remaining[index + "{input.".len()..];
        let end = after
            .find('}')
            .expect("a validated path template has closed input bindings");
        let input = &after[..end];
        let mut slot = input.to_owned();
        // A synthetic slot may collide neither with a slot already taken nor
        // with another declared input — a collision with the latter would fill
        // a caller's own value from a different binding. It grows by
        // underscores, which keep it a valid input name, until it collides
        // with neither. The first occurrence keeps the input's own name, which
        // is the one collision with `declared` that is intended.
        while bindings.iter().any(|(taken, _)| taken == &slot)
            || (slot != input && declared.contains(&slot))
        {
            slot.push('_');
        }
        rendered.push('{');
        rendered.push_str(&slot);
        rendered.push('}');
        bindings.push((slot, input.to_owned()));
        remaining = &after[end + 1..];
    }
    rendered.push_str(remaining);
    Ok((rendered, bindings))
}

/// The characters a declared query key may carry.
///
/// The set is the SDK's: alphanumerics and `_ - . [ ] $`. None of them can end
/// a key or start a second parameter, which is what makes a key safe to emit on
/// the wire exactly as it was declared. It is stated here because a deployment
/// is refused by this module's rule, not by the SDK builder's message.
fn is_declarable_query_key(key: &str) -> bool {
    !key.is_empty()
        && key.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '_' | '-' | '.' | '[' | ']' | '$')
        })
}

/// The runtime class an operation's `error_map` names.
fn metadata_error_class(class: &donat_metadata::ConnectorErrorClass) -> ConnectorErrorClass {
    use donat_metadata::ConnectorErrorClass as Declared;

    match class {
        Declared::Authentication => ConnectorErrorClass::Authentication,
        Declared::Transport => ConnectorErrorClass::Transport,
        Declared::Timeout => ConnectorErrorClass::Timeout,
        Declared::Http429 => ConnectorErrorClass::Http429,
        Declared::Http5xx => ConnectorErrorClass::Http5xx,
        Declared::Validation => ConnectorErrorClass::Validation,
        Declared::Permanent => ConnectorErrorClass::Permanent,
        Declared::Invariant => ConnectorErrorClass::Invariant,
    }
}

/// The effect class of one operation a *deployment* authored (spec 010 §7).
///
/// The rule is the effect contract the catalog already requires of an
/// executable declarative operation, said once in the SDK's words:
///
/// * a `GET` is read-only by its method;
/// * a mutation-shaped method the deployment declared `read_only` — a quote, a
///   lookup, a search posted with a body — is read-only on the deployment's own
///   assertion, because the deployment authored the operation;
/// * a mutating operation with a complete `provider_idempotent` contract
///   (binding, scope, retention, and a strictly smaller clock margin) is
///   `ProviderIdempotent::ExplicitKey`;
/// * anything else — including the legacy bare `idempotency: { header }`, which
///   names a binding but publishes no retention to keep a margin under — is
///   inventory-only. It stays declared, typed, and deployable, and it is never
///   published as an executable operation a Process can reference.
fn declarative_effect(method: HttpMethod, http: &donat_metadata::HttpConnectorOperation) -> Effect {
    let inventory = |reason: &'static str| {
        Effect::inventory_only(reason).expect("the reason is a non-empty literal")
    };
    match (&http.effect, method.mutates()) {
        (Some(donat_metadata::ConnectorEffect::ReadOnly(_)), false) | (None, false) => {
            Effect::read_only()
        }
        (Some(donat_metadata::ConnectorEffect::ReadOnly(_)), true) => {
            Effect::read_only_declared_by_deployment()
        }
        (
            Some(donat_metadata::ConnectorEffect::ProviderIdempotent {
                provider_idempotent,
            }),
            _,
        ) => {
            let Some(step) = provider_idempotent.side_effect_steps.first() else {
                return inventory(
                    "a provider-idempotent effect with no side-effecting step declares no binding",
                );
            };
            let evidence =
                IdempotencyBinding::header(&step.fixed_binding.header).and_then(|binding| {
                    ExplicitKeyEvidence::documented(
                        binding,
                        &step.scope,
                        Duration::from_millis(step.minimum_retention_ms),
                        Duration::from_millis(step.clock_safety_margin_ms),
                        &step.evidence.source_record_id,
                    )
                });
            match evidence {
                Ok(evidence) => Effect::provider_idempotent_explicit_key(evidence),
                // The catalog rejects an incomplete contract with its own
                // message; classifying it inventory-only here keeps this module
                // from inventing a second one.
                Err(_) => inventory(
                    "a provider-idempotent effect whose evidence is incomplete is not idempotency evidence",
                ),
            }
        }
        (None, true) => inventory(
            "a mutating operation that declares no provider idempotency contract cannot be sent twice safely",
        ),
    }
}

/// The header a provider-idempotent effect binds its stable key to.
fn fixed_idempotency_header(http: &donat_metadata::HttpConnectorOperation) -> Option<String> {
    let donat_metadata::ConnectorEffect::ProviderIdempotent {
        provider_idempotent,
    } = http.effect.as_ref()?
    else {
        return None;
    };
    provider_idempotent
        .side_effect_steps
        .first()
        .map(|step| step.fixed_binding.header.clone())
}

/// The failure an operation's own error map declares for a status.
fn declared_failure(class: ConnectorErrorClass, headers: &HeaderMap) -> ConnectorFailure {
    match class {
        ConnectorErrorClass::Http429 => ConnectorFailure::new(
            ConnectorErrorClass::Http429,
            "connector_http_429",
            "connector provider rate limited the request",
        )
        .with_retry_after(retry_after(headers)),
        ConnectorErrorClass::Http5xx => ConnectorFailure::new(
            ConnectorErrorClass::Http5xx,
            "connector_declared_http_5xx",
            "connector provider returned a declared server error",
        ),
        ConnectorErrorClass::Timeout => timeout_failure(),
        ConnectorErrorClass::Transport => ConnectorFailure::new(
            ConnectorErrorClass::Transport,
            "connector_transport_failure",
            "connector provider was unreachable",
        ),
        ConnectorErrorClass::Authentication => ConnectorFailure::new(
            ConnectorErrorClass::Authentication,
            "connector_http_authentication",
            "connector provider rejected connector authentication",
        ),
        ConnectorErrorClass::Validation => {
            validation_failure("connector provider rejected the declared request")
        }
        ConnectorErrorClass::Invariant => {
            invariant_failure("connector provider answered outside its declared contract")
        }
        ConnectorErrorClass::Permanent => ConnectorFailure::new(
            ConnectorErrorClass::Permanent,
            "connector_unsupported_http_status",
            "connector provider returned an unsupported HTTP status",
        ),
    }
}

/// One attempt's failure, plus the one fact a credential seam needs from it.
///
/// It exists so that "the provider answered 401" can travel out of the request
/// path without the classified failure being replaced by a credential-shaped
/// one. Everything that does not care converts back with
/// [`HttpAttemptFailure::into_failure`], which is what the two public entry
/// points do.
pub(crate) struct HttpAttemptFailure {
    pub(crate) failure: ConnectorFailure,
    /// The provider answered `401`, so a refreshed credential may change the
    /// answer. It is `false` for everything that never reached the provider.
    pub(crate) unauthorized: bool,
}

impl HttpAttemptFailure {
    pub(crate) fn into_failure(self) -> ConnectorFailure {
        self.failure
    }
}

impl From<ConnectorFailure> for HttpAttemptFailure {
    fn from(failure: ConnectorFailure) -> Self {
        Self {
            failure,
            unauthorized: false,
        }
    }
}

/// An HTTP operation compiled from typed deploy-time metadata. It retains the
/// static request shape and the exact set of allowed job input names.
#[derive(Debug, Clone)]
pub struct ValidatedHttpOperation {
    operation: HttpOperation,
    declared_inputs: BTreeSet<String>,
    serialize_by: Option<String>,
}

impl ValidatedHttpOperation {
    pub fn from_metadata(operation: &ConnectorOperation) -> Result<Self, HttpConfigError> {
        let http = operation.http().ok_or_else(|| {
            HttpConfigError::new("http connector operations must declare an HTTP operation profile")
        })?;
        if http.version.is_empty() {
            return Err(HttpConfigError::new(
                "connector operation version is required",
            ));
        }
        let method = parse_method(&http.method)?;
        let mut declared_inputs = path_input_names(&http.path)?;
        let mut builder = HttpOperation::builder(&operation.name, method, &http.path);
        for (name, binding) in &http.query {
            validate_input_name(&binding.input)?;
            declared_inputs.insert(binding.input.clone());
            builder = builder.query_input(name, &binding.input);
        }
        for header in &http.headers {
            builder = builder.static_header(&header.name, &header.value);
        }
        if let Some(body) = &http.body {
            let template = json_template_from_metadata(body, &mut declared_inputs)?;
            builder = builder.body(template);
        }
        let statuses = http
            .success_statuses
            .iter()
            .copied()
            .map(|status| {
                let status = StatusCode::from_u16(status).map_err(|_| {
                    HttpConfigError::new("success statuses must be valid HTTP statuses")
                })?;
                if !status.is_success() {
                    return Err(HttpConfigError::new("success statuses must be 2xx"));
                }
                Ok(status)
            })
            .collect::<Result<Vec<_>, _>>()?;
        builder = builder.success_statuses(statuses);
        for (name, response) in &http.response {
            builder = builder.response_pointer(
                name,
                &response.json_pointer,
                response.type_.ends_with('!'),
            );
        }
        let declared_5xx = http
            .error_classification
            .http_5xx
            .iter()
            .copied()
            .map(|status| {
                StatusCode::from_u16(status).map_err(|_| {
                    HttpConfigError::new("http_5xx statuses must be valid HTTP statuses")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        builder = builder.declared_5xx(declared_5xx);
        if let Some(error_map) = &http.error_map {
            builder = builder.declared_classes(error_map.rules.iter().flat_map(|rule| {
                let class = metadata_error_class(&rule.class_);
                rule.statuses.iter().map(move |status| (*status, class))
            }));
            builder = builder.declared_fallback(metadata_error_class(&error_map.fallback.class_));
        }
        // The header the provider deduplicates on is declared in one of two
        // places. The legacy field names it directly; the effect names it as
        // the binding of a provider-idempotent step, which is the form the
        // catalog contract uses. Reading only the first left every operation
        // declared the second way sending no key at all.
        let idempotency_header = http
            .idempotency
            .as_ref()
            .map(|idempotency| idempotency.header.clone())
            .or_else(|| fixed_idempotency_header(http));
        if let Some(header) = idempotency_header {
            builder = builder.idempotency_header(&header);
        }
        builder = builder.effect(declarative_effect(method, http));
        let capacity = operation.capacity().ok_or_else(|| {
            HttpConfigError::new("capacity is required for every connector operation")
        })?;
        if capacity.max_in_flight == 0
            || capacity.rate_limit.permits == 0
            || capacity.rate_limit.burst == 0
            || !valid_rate_period(&capacity.rate_limit.per)
        {
            return Err(HttpConfigError::new(
                "connector operation capacity is invalid",
            ));
        }
        let serialize_by = capacity
            .serialize_by
            .as_ref()
            .map(|binding| {
                validate_input_name(&binding.input)?;
                if !declared_inputs.contains(&binding.input) {
                    return Err(HttpConfigError::new(
                        "capacity serialize_by must name a declared operation input",
                    ));
                }
                Ok(binding.input.clone())
            })
            .transpose()?;
        Ok(Self {
            operation: builder.build()?,
            declared_inputs,
            serialize_by,
        })
    }

    fn validate_dispatch_input(&self, input: &JsonValue) -> Result<(), ConnectorFailure> {
        let JsonValue::Object(input) = input else {
            return Err(invariant_failure(
                "connector operation input must be an object",
            ));
        };
        if input
            .keys()
            .any(|name| !self.declared_inputs.contains(name))
        {
            return Err(invariant_failure(
                "connector operation input contains an undeclared value",
            ));
        }
        if self
            .declared_inputs
            .iter()
            .any(|name| !input.contains_key(name))
        {
            return Err(invariant_failure(
                "a declared connector input value is missing",
            ));
        }
        if let Some(name) = &self.serialize_by {
            scalar_input(&JsonValue::Object(input.clone()), name)?;
        }
        Ok(())
    }

    pub(crate) fn serialization_key_input(&self) -> Option<&str> {
        self.serialize_by.as_deref()
    }

    pub(crate) fn is_executable(&self) -> bool {
        self.operation.is_executable()
    }
}

/// Validate static HTTP headers and operations without resolving any
/// environment value. This is shared by deploy-time metadata validation and
/// registry construction so the same profile and header rules apply before a
/// database connection or a listener can be opened.
pub(crate) fn validate_http_instance_metadata(
    config: &ConnectorConfig,
    operations: &[ConnectorOperation],
) -> Result<(), HttpConfigError> {
    let mut configured_headers = HashSet::new();
    for header in &config.headers {
        let name = HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| HttpConfigError::new("configured header name is invalid"))?;
        if !configured_headers.insert(name) {
            return Err(HttpConfigError::new(
                "configured header names must not collide",
            ));
        }
    }

    let mut declared_operations = HashSet::new();
    for operation in operations {
        if !declared_operations.insert(operation.name.as_str()) {
            return Err(HttpConfigError::new(
                "http connector operation is declared more than once",
            ));
        }
        let compiled = ValidatedHttpOperation::from_metadata(operation)?;
        if compiled
            .operation
            .header_names()
            .any(|name| configured_headers.contains(name))
        {
            return Err(HttpConfigError::new(
                "connector operation header names must not collide with configured headers",
            ));
        }
    }
    Ok(())
}

fn parse_method(method: &str) -> Result<HttpMethod, HttpConfigError> {
    match method {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "PATCH" => Ok(HttpMethod::Patch),
        "DELETE" => Ok(HttpMethod::Delete),
        _ => Err(HttpConfigError::new(
            "method must be one of GET, POST, PUT, PATCH, or DELETE",
        )),
    }
}

fn validate_input_name(name: &str) -> Result<(), HttpConfigError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(HttpConfigError::new("connector input binding is invalid"));
    }
    Ok(())
}

fn path_input_names(path: &str) -> Result<BTreeSet<String>, HttpConfigError> {
    validate_path_template(path)?;
    let mut inputs = BTreeSet::new();
    let mut remaining = path;
    while let Some(index) = remaining.find("{input.") {
        let after = &remaining[index + "{input.".len()..];
        let end = after
            .find('}')
            .expect("validated path template has a closed input binding");
        let name = &after[..end];
        validate_input_name(name)?;
        inputs.insert(name.to_owned());
        remaining = &after[end + 1..];
    }
    Ok(inputs)
}

fn json_template_from_metadata(
    value: &JsonValue,
    inputs: &mut BTreeSet<String>,
) -> Result<JsonTemplate, HttpConfigError> {
    match value {
        JsonValue::Object(fields) if fields.len() == 1 && fields.contains_key("input") => {
            let Some(JsonValue::String(name)) = fields.get("input") else {
                return Err(HttpConfigError::new(
                    "body input binding must name an input",
                ));
            };
            validate_input_name(name)?;
            inputs.insert(name.clone());
            Ok(JsonTemplate::Input(name.clone()))
        }
        JsonValue::Object(fields) => fields
            .iter()
            .map(|(name, value)| {
                json_template_from_metadata(value, inputs).map(|value| (name.clone(), value))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(JsonTemplate::Object),
        JsonValue::Array(values) => values
            .iter()
            .map(|value| json_template_from_metadata(value, inputs))
            .collect::<Result<Vec<_>, _>>()
            .map(JsonTemplate::Array),
        value => Ok(JsonTemplate::Literal(value.clone())),
    }
}

fn valid_rate_period(value: &str) -> bool {
    let Some(unit) = value.chars().last() else {
        return false;
    };
    matches!(unit, 's' | 'm' | 'h')
        && value
            .strip_suffix(unit)
            .and_then(|number| number.parse::<u64>().ok())
            .is_some_and(|number| number > 0)
}

// Transport, DNS resolution, and the shared payload ceiling live in the SDK
// so every connector gets the same no-redirect, no-proxy, no-transport-retry
// behaviour.  They are re-exported here because this module is still their
// only caller and the existing tests address them through this path.
pub use donat_connectors::sdk::transport::{
    HostResolver, HttpTransport, MAX_HTTP_BODY_BYTES, PreparedHttpRequest, RawHttpResponse,
    ReqwestTransport, ResolveError, SystemResolver, TransportError, TransportErrorKind,
};

/// Immutable HTTP connector instance.  The resolver and transport seams are
/// production dependencies; they are exposed to tests to prove SSRF controls
/// rather than providing a user-configurable transport.
pub struct HttpConnector {
    config: HttpConnectorConfig,
    resolver: Arc<dyn HostResolver>,
    transport: Arc<dyn HttpTransport>,
}

impl HttpConnector {
    pub fn with_components(
        config: HttpConnectorConfig,
        resolver: Arc<dyn HostResolver>,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        Self {
            config,
            resolver,
            transport,
        }
    }

    pub fn from_metadata_config(config: &ConnectorConfig) -> Result<Self, HttpConfigError> {
        validate_http_config_metadata(config)?;
        let base_url = match config.base_url.as_ref() {
            Some(ConnectorBaseUrl::Literal(value)) => value.clone(),
            Some(ConnectorBaseUrl::FromEnv(reference)) => std::env::var(&reference.value_from_env)
                .map_err(|_| HttpConfigError::new("base_url environment value is unavailable"))?,
            None => {
                return Err(HttpConfigError::new(
                    "base_url is required for the http connector",
                ));
            }
        };
        let headers = config
            .headers
            .iter()
            .map(|header| {
                let value = std::env::var(&header.value_from_env).map_err(|_| {
                    HttpConfigError::new("configured header environment value is unavailable")
                })?;
                ConfiguredHeader::new(&header.name, &value)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let config = HttpConnectorConfig::new(&base_url, headers)?;
        Ok(Self::with_components(
            config,
            Arc::new(SystemResolver),
            Arc::new(ReqwestTransport::new()),
        ))
    }

    /// A non-reversible identity for the resolved endpoint. The raw resolved
    /// URL is intentionally kept inside the connector and never placed in a
    /// deployment fingerprint or process record.
    pub(crate) fn base_url_digest(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(self.config.base_url.as_str().as_bytes())
        )
    }

    pub async fn execute(
        &self,
        operation: &HttpOperation,
        input: JsonValue,
        context: ExecutionContext,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        let fingerprint = canonical_json_sha256(&input);
        let mut success = self
            .execute_internal(operation, input, None, None, context)
            .await
            .map_err(HttpAttemptFailure::into_failure)?;
        success.request_fingerprint = fingerprint;
        Ok(success)
    }

    pub(crate) async fn execute_validated(
        &self,
        operation: &ValidatedHttpOperation,
        input: JsonValue,
        idempotency_key: &str,
        deadline: tokio::time::Instant,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        self.execute_validated_authorized(operation, input, idempotency_key, deadline, None)
            .await
            .map_err(HttpAttemptFailure::into_failure)
    }

    /// One attempt, optionally carrying an applied `Authorization` header.
    ///
    /// The header is deploy-time material like every other one this connector
    /// sends: its name is fixed here, its value comes from the credential store,
    /// and no operation input can select either. The failure it returns says
    /// whether the provider answered `401`, because that is the one answer a
    /// refreshed credential can change.
    pub(crate) async fn execute_validated_authorized(
        &self,
        operation: &ValidatedHttpOperation,
        input: JsonValue,
        idempotency_key: &str,
        deadline: tokio::time::Instant,
        authorization: Option<&str>,
    ) -> Result<ConnectorSuccess, HttpAttemptFailure> {
        // Validate the job boundary before DNS or template rendering. A caller
        // therefore cannot smuggle raw transport controls into an operation
        // whose request shape was fixed at deployment time.
        operation.validate_dispatch_input(&input)?;
        let fingerprint = canonical_json_sha256(&input);
        let mut success = self
            .execute_internal(
                &operation.operation,
                input,
                Some(idempotency_key),
                authorization,
                ExecutionContext::with_deadline(deadline),
            )
            .await?;
        success.request_fingerprint = fingerprint;
        Ok(success)
    }

    async fn execute_internal(
        &self,
        operation: &HttpOperation,
        input: JsonValue,
        idempotency_key: Option<&str>,
        authorization: Option<&str>,
        context: ExecutionContext,
    ) -> Result<ConnectorSuccess, HttpAttemptFailure> {
        if context.deadline <= tokio::time::Instant::now() {
            return Err(timeout_failure().into());
        }
        // Reject an unsafe initial lookup before rendering path, query, or body
        // templates.  This keeps untrusted input out of a request whose
        // declared destination is already disallowed.
        self.resolve_under_deadline(context.deadline).await?;
        // `prepare_request` refuses an oversized request before it is prepared.
        let request = self.prepare_request(operation, &input, idempotency_key, authorization)?;

        // Check resolution at request start and again directly before handing
        // the pinned result to reqwest.  A DNS rebinding between the two is
        // rejected; a new per-request reqwest client avoids connection reuse.
        let destination = self.resolve_under_deadline(context.deadline).await?;
        let response = tokio::time::timeout_at(
            context.deadline,
            self.transport
                .execute(request, &destination, context.deadline),
        )
        .await
        .map_err(|_| timeout_failure())?
        .map_err(|error| match error.kind() {
            TransportErrorKind::Transport => transport_failure(),
            TransportErrorKind::Timeout => timeout_failure(),
            TransportErrorKind::ResponseTooLarge => {
                validation_failure("connector response exceeds the 1 MiB limit")
            }
        })?;
        self.validate_connected_peer(&destination, response.peer())?;
        self.decode_response(operation, response)
    }

    async fn resolve_under_deadline(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<IpAddr>, ConnectorFailure> {
        let host = self
            .config
            .base_url
            .host_str()
            .expect("validated base URL has a host");
        let port = self
            .config
            .base_url
            .port_or_known_default()
            .expect("validated base URL has a known port");
        let addresses = tokio::time::timeout_at(deadline, self.resolver.resolve(host, port))
            .await
            .map_err(|_| timeout_failure())?
            .map_err(|_| transport_failure())?;
        if addresses.is_empty() {
            return Err(transport_failure());
        }
        Ok(addresses)
    }

    /// The connection must land on one of the addresses this request already
    /// resolved. Egress reachability itself is a network-layer concern, but
    /// pinning the peer still keeps one request on one resolved host, so a name
    /// cannot resolve to one address for validation and another for transport.
    fn validate_connected_peer(
        &self,
        destination: &[IpAddr],
        peer: Option<SocketAddr>,
    ) -> Result<(), ConnectorFailure> {
        let Some(peer) = peer else {
            return Err(invariant_failure(
                "connector transport could not verify the connected peer",
            ));
        };
        if !destination.contains(&peer.ip()) {
            return Err(invariant_failure(
                "connector transport connected to an unresolved peer",
            ));
        }
        Ok(())
    }

    fn prepare_request(
        &self,
        operation: &HttpOperation,
        input: &JsonValue,
        idempotency_key: Option<&str>,
        authorization: Option<&str>,
    ) -> Result<PreparedHttpRequest, ConnectorFailure> {
        // The deploy-time header set: the instance's configured headers, the
        // operation's declared ones, and the engine's own idempotency key. Every
        // name here comes from metadata and every value from an environment
        // variable or the activity identity, so none of it is caller input.
        let mut headers = self.config.headers.clone();
        for header in &operation.headers {
            if headers.contains_key(&header.name) {
                return Err(invariant_failure(
                    "connector operation header names must not collide with configured headers",
                ));
            }
            headers.insert(header.name.clone(), header.value.clone());
        }
        if let Some(name) = &operation.idempotency_header {
            let key = idempotency_key.ok_or_else(|| {
                invariant_failure("connector operation requires a stable idempotency key")
            })?;
            let value = HeaderValue::from_str(key)
                .map_err(|_| invariant_failure("connector activity idempotency key is invalid"))?;
            if headers.contains_key(name) {
                return Err(invariant_failure(
                    "connector operation header names must not collide with configured headers",
                ));
            }
            headers.insert(name.clone(), value);
        }
        // The applied OAuth2 credential. It is the last header set and it never
        // overwrites one the deployment declared: an instance that declares both
        // `config.oauth2` and its own `Authorization` header has said two
        // different things about the same header, and the request must not
        // silently pick one.
        if let Some(authorization) = authorization {
            if headers.contains_key(AUTHORIZATION) {
                return Err(invariant_failure(
                    "connector operation header names must not collide with an applied credential",
                ));
            }
            let mut value = HeaderValue::from_str(authorization).map_err(|_| {
                invariant_failure("an applied connector credential is not a valid header value")
            })?;
            // A header dump redacts it, exactly as the SDK's own auth plans do.
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        // A binding the deployment spent twice is rendered from one declared
        // input: the extra slots exist only between here and the renderer, so
        // they are added to a copy of the input and never to the contract a
        // caller is validated against.
        let repeated = operation.filled_repeated_path_slots(input);
        let input = repeated.as_ref().unwrap_or(input);
        let request =
            operation
                .operation
                .plan_configured_request(&self.config.origin, input, &headers)?;
        if request.body().len() > MAX_HTTP_BODY_BYTES {
            return Err(invariant_failure(
                "connector request exceeds the 1 MiB limit",
            ));
        }
        request.into_prepared()
    }

    fn decode_response(
        &self,
        operation: &HttpOperation,
        response: RawHttpResponse,
    ) -> Result<ConnectorSuccess, HttpAttemptFailure> {
        if response.body().len() > MAX_HTTP_BODY_BYTES {
            return Err(validation_failure("connector response exceeds the 1 MiB limit").into());
        }
        let status = response.status.as_u16();
        if !operation.operation.is_success(status) {
            // Whatever the failure turns out to be, a `401` is the one answer a
            // refreshed credential can change. The class is still the
            // operation's own — the credential seam replays once and then
            // returns exactly this failure.
            let unauthorized = status == 401;
            let classified = |failure: ConnectorFailure| HttpAttemptFailure {
                failure,
                unauthorized,
            };
            // What the operation declared about this status wins: the built-in
            // handling below is the answer for an operation that declared
            // nothing, not an override of one that did.
            if let Some(class) = operation.declared_classes.get(&status) {
                return Err(classified(declared_failure(*class, response.headers())));
            }
            // ...and an operation that declared an error map also declared what
            // a status none of its rules name should be. Ignoring it meant a
            // 502 under `fallback: { class: http_5xx }` was classified
            // `Permanent`, so an activity declaring `retry_on: [http_5xx]`
            // refused to retry the very failure it was written for.
            if let Some(class) = operation.declared_fallback {
                return Err(classified(declared_failure(class, response.headers())));
            }
            return Err(classified(match status {
                408 => timeout_failure(),
                429 => ConnectorFailure::new(
                    ConnectorErrorClass::Http429,
                    "connector_http_429",
                    "connector provider rate limited the request",
                )
                .with_retry_after(retry_after(response.headers())),
                401 | 403 => ConnectorFailure::new(
                    ConnectorErrorClass::Authentication,
                    "connector_http_authentication",
                    "connector provider rejected connector authentication",
                ),
                400 => validation_failure("connector provider rejected the declared request"),
                _ if operation.declared_5xx.contains(&status) => ConnectorFailure::new(
                    ConnectorErrorClass::Http5xx,
                    "connector_declared_http_5xx",
                    "connector provider returned a declared server error",
                ),
                _ => ConnectorFailure::new(
                    ConnectorErrorClass::Permanent,
                    "connector_unsupported_http_status",
                    "connector provider returned an unsupported HTTP status",
                ),
            }));
        }
        // The declared response is the activity's output schema, so every
        // declared field appears in the output: a provider that omits an
        // optional field yields an explicit null rather than a missing key.
        // That, the JSON contract, and the response ceiling are all the SDK's.
        Ok(ConnectorSuccess {
            output: operation
                .operation
                .decode_response(status, response.body())?,
            request_fingerprint: String::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// The deployment-selected instance this module publishes to the registry.
// ---------------------------------------------------------------------------

/// The immutable compiled contract for one deployed HTTP operation. The
/// fingerprint is intentionally non-secret and therefore safe for a future
/// process-definition revision to retain.
struct CompiledHttpOperation {
    operation: ValidatedHttpOperation,
    configuration_fingerprint: String,
}

/// One deployment-selected instance of the `http` module.
pub(crate) struct HttpInstance {
    connector: HttpConnector,
    operations: BTreeMap<String, CompiledHttpOperation>,
}

/// Compile one instance of this module from validated deployment metadata.
/// The registry reaches this through the compiled module table and knows
/// nothing else about the module.
pub(crate) fn build_registered_instance(
    context: &mut ModuleContext<'_>,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    let instance = context.instance;
    let invalid = |message: String| ConnectorRegistryError::InvalidConfiguration {
        instance: instance.name.clone(),
        message,
    };
    let connector = HttpConnector::from_metadata_config(&instance.config)
        .map_err(|error| invalid(error.to_string()))?;
    let base_url_digest = connector.base_url_digest();
    let mut operations = BTreeMap::new();
    for operation in &instance.operations {
        let validated = ValidatedHttpOperation::from_metadata(operation)
            .map_err(|error| invalid(error.to_string()))?;
        if let Some(spec) = catalog::compile_http_operation_spec(
            context.metadata,
            context.definition,
            instance,
            operation,
        )
        .map_err(invalid)?
        {
            // The catalog snapshot and the SDK effect class are two
            // descriptions of one operation, and they must agree: publishing an
            // operation this module classifies inventory-only would let a
            // Process reference a mutation with no admitted idempotency
            // evidence, which is the one thing spec 010 §7 exists to prevent.
            if !validated.is_executable() {
                return Err(invalid(format!(
                    "connector operation `{}` is published as executable but carries no admitted effect class",
                    operation.name
                )));
            }
            if context
                .executable_specs
                .insert(spec.operation, Arc::new(spec))
                .is_some()
            {
                return Err(invalid(format!(
                    "executable catalog operation `{}` is declared more than once",
                    operation.name
                )));
            }
        }
        let compiled = CompiledHttpOperation {
            configuration_fingerprint: http_configuration_fingerprint(
                context.definition,
                &instance.config,
                operation,
                &base_url_digest,
            ),
            operation: validated,
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
    Ok(Box::new(HttpInstance {
        connector,
        operations,
    }))
}

impl RegisteredConnector for HttpInstance {
    fn execute<'a>(
        &'a self,
        operation: &'a str,
        input: JsonValue,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            let Some(operation) = self.operations.get(operation) else {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "connector_invariant",
                    "connector operation is not declared",
                ));
            };
            self.connector
                .execute_validated(&operation.operation, input, idempotency_key, deadline)
                .await
        })
    }

    fn execute_authorized<'a>(
        &'a self,
        operation: &'a str,
        input: JsonValue,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
        authorization: &'a str,
    ) -> BoxFuture<'a, Result<super::AuthorizedAttempt, ConnectorFailure>> {
        Box::pin(async move {
            let Some(operation) = self.operations.get(operation) else {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "connector_invariant",
                    "connector operation is not declared",
                ));
            };
            match self
                .connector
                .execute_validated_authorized(
                    &operation.operation,
                    input,
                    idempotency_key,
                    deadline,
                    Some(authorization),
                )
                .await
            {
                Ok(success) => Ok(super::AuthorizedAttempt::Done(success)),
                Err(attempt) if attempt.unauthorized => {
                    Ok(super::AuthorizedAttempt::Unauthorized(attempt.failure))
                }
                Err(attempt) => Err(attempt.failure),
            }
        })
    }

    fn configuration_fingerprint(&self, operation: &str) -> Option<&str> {
        self.operations
            .get(operation)
            .map(|operation| operation.configuration_fingerprint.as_str())
    }

    fn serialization_key_input(&self, operation: &str) -> Option<&str> {
        self.operations
            .get(operation)
            .and_then(|operation| operation.operation.serialization_key_input())
    }

    fn http_connector(&self) -> Option<&HttpConnector> {
        Some(&self.connector)
    }
}

#[derive(Serialize)]
struct HttpConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    operation_name: &'a str,
    operation_version: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    network_policy: &'a Option<String>,
    credential_header_declarations: &'a [donat_metadata::ConnectorHeader],
    operation_profile: &'a donat_metadata::HttpConnectorOperation,
    capacity: &'a donat_metadata::ConnectorCapacity,
    base_url_source: HttpBaseUrlSourceIdentity<'a>,
    base_url_sha256: &'a str,
}

/// The configured origin of a base URL, deliberately excluding its resolved
/// value. An environment-variable name is deploy-time identity, while the
/// resolved endpoint itself is represented only by `base_url_sha256`.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HttpBaseUrlSourceIdentity<'a> {
    Literal,
    Environment { variable: &'a str },
}

pub(super) fn http_configuration_fingerprint(
    definition: ConnectorDefinition,
    config: &ConnectorConfig,
    operation: &ConnectorOperation,
    base_url_digest: &str,
) -> String {
    let profile = operation
        .http()
        .expect("HTTP operation profile was validated before fingerprinting");
    let capacity = operation
        .capacity()
        .expect("HTTP operation capacity was validated before fingerprinting");
    let base_url_source = match config
        .base_url
        .as_ref()
        .expect("HTTP base URL was validated before fingerprinting")
    {
        ConnectorBaseUrl::Literal(_) => HttpBaseUrlSourceIdentity::Literal,
        ConnectorBaseUrl::FromEnv(reference) => HttpBaseUrlSourceIdentity::Environment {
            variable: &reference.value_from_env,
        },
    };
    let canonical = HttpConfigurationFingerprint {
        module_name: definition.module_name,
        module_semantic_version: definition.semantic_version,
        runtime_abi: definition.runtime_abi,
        operation_name: &operation.name,
        operation_version: &profile.version,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        network_policy: &config.network_policy,
        credential_header_declarations: &config.headers,
        operation_profile: profile,
        capacity,
        base_url_source,
        base_url_sha256: base_url_digest,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated connector fingerprint fields always serialize to JSON");
    format!("{:x}", Sha256::digest(bytes))
}

impl ConnectorModule for HttpConnector {
    fn definition(&self) -> ConnectorDefinition {
        HTTP_DEFINITION
    }
}

/// What a deployment's `path` may contain.
///
/// This is the module's own rule and the only one a deployment is refused by:
/// the SDK checks the template again when the request node is built, but a
/// deployment must never first learn what it may declare from a message the
/// SDK's builder wrote for hand-written connectors. Whatever the two say, they
/// say the same thing here.
///
/// The `@` is the one character this rule gained when the declarative request
/// became an SDK declaration. It carries no meaning a path needs — the origin
/// is deploy-time configuration, so a path literal cannot reach an authority —
/// and one engine answering one question about what a connector URL may
/// contain is worth more than a second, wider grammar for deployments only.
/// The set widens once, in the SDK, on provider evidence
/// (`knowledgebase/declarative-saas/decisions/047-*` and `/059-*`).
///
/// A binding repeated across segments is *not* refused: see
/// [`sdk_path_bindings`].
fn validate_path_template(template: &str) -> Result<(), HttpConfigError> {
    if !template.starts_with('/')
        || template.starts_with("//")
        || template.contains('?')
        || template.contains('#')
        || template.contains("://")
        || template.contains('@')
        || template
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(HttpConfigError::new(
            "path must be a static absolute path without authority, userinfo, query, fragment, or dot segments",
        ));
    }
    let mut remaining = template;
    while let Some(index) = remaining.find("{input.") {
        let before = &remaining[..index];
        let after = &remaining[index + "{input.".len()..];
        let Some(end) = after.find('}') else {
            return Err(HttpConfigError::new("path input binding is not closed"));
        };
        let name = &after[..end];
        if name.is_empty()
            || !name
                .chars()
                .all(|character| character == '_' || character.is_ascii_alphanumeric())
        {
            return Err(HttpConfigError::new("path input binding is invalid"));
        }
        if before.contains('{') {
            return Err(HttpConfigError::new(
                "path template contains an unsupported binding",
            ));
        }
        remaining = &after[end + 1..];
    }
    if remaining.contains(['{', '}']) {
        return Err(HttpConfigError::new(
            "path template contains an unsupported binding",
        ));
    }
    Ok(())
}

/// Read one declared scalar input slot.  Path and query values are rendered by
/// the SDK; this remains for the deploy-time `serialize_by` binding, which
/// names an input the engine reads without sending it anywhere.
fn scalar_input(input: &JsonValue, name: &str) -> Result<String, ConnectorFailure> {
    match input.get(name) {
        Some(JsonValue::String(value)) => Ok(value.clone()),
        Some(JsonValue::Number(value)) => Ok(value.to_string()),
        Some(JsonValue::Bool(value)) => Ok(value.to_string()),
        Some(JsonValue::Null) | Some(JsonValue::Array(_)) | Some(JsonValue::Object(_)) => Err(
            invariant_failure("a declared connector input value must be scalar"),
        ),
        None => Err(invariant_failure(
            "a declared connector input value is missing",
        )),
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
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

fn validation_failure(message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Validation,
        "connector_validation",
        message,
    )
}

fn invariant_failure(message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Invariant,
        "connector_invariant",
        message,
    )
}

// `reqwest` 0.12 exposes the connected peer through `Response::remote_addr`.
// We validate DNS twice, pin the second result with `resolve_to_addrs`, disable
// proxies and redirects, and reject a response unless that observed peer is a
// public member of the second vetted destination set.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{
        Json, Router,
        extract::{OriginalUri, Path, State},
        http::HeaderMap,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use serde_json::{Value as JsonValue, json};

    struct LocalServer {
        base_url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl LocalServer {
        async fn start(router: Router) -> Self {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("local test listener binds");
            let address = listener
                .local_addr()
                .expect("local test listener has an address");
            let task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .await
                    .expect("local test server serves");
            });
            Self {
                base_url: format!("http://{address}"),
                task,
            }
        }
    }

    impl Drop for LocalServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    /// Spec 010 §7 for the connector a *deployment* authors: every compiled
    /// operation carries a class, and a mutation is executable only on a
    /// complete provider-idempotency contract.
    ///
    /// The classes match the effect contract the catalog already requires
    /// before it publishes an operation to process compilation, said in the
    /// SDK's words: an operation this test calls inventory-only is exactly one
    /// `crates/server/src/connectors/catalog.rs` refuses to publish.
    #[test]
    fn every_declarative_operation_carries_an_effect_class() {
        use donat_connectors::sdk::EffectClass;

        let operation = |method: &str, effect: JsonValue, idempotency: JsonValue| {
            let mut value = json!({
                "name": "operation",
                "version": "v1",
                "method": method,
                "path": "/v1/things",
                "success_statuses": [200],
                "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                }
            });
            if !effect.is_null() {
                value["effect"] = effect;
            }
            if !idempotency.is_null() {
                value["idempotency"] = idempotency;
            }
            let metadata: ConnectorOperation =
                serde_json::from_value(value).expect("operation metadata deserializes");
            ValidatedHttpOperation::from_metadata(&metadata)
                .expect("the operation compiles")
                .operation
                .operation
                .effect_class()
                .expect("every compiled declarative operation carries a class")
        };

        let complete = || {
            json!({
                "provider_idempotent": {
                    "side_effect_steps": [{
                        "step": "request",
                        "fixed_binding": { "header": "Idempotency-Key" },
                        "scope": "provider-account-v1",
                        "minimum_retention_ms": 604_800_000u64,
                        "clock_safety_margin_ms": 300_000u64,
                        "evidence": {
                            "source_record_id": "source.provider.v1",
                            "fact_ids": ["fact.provider.fixed-idempotency-header"]
                        }
                    }]
                }
            })
        };

        // A read by method, and a read a deployment asserts for a provider that
        // publishes its quotes and lookups as POST.
        assert_eq!(
            operation("GET", JsonValue::Null, JsonValue::Null),
            EffectClass::ReadOnly
        );
        assert_eq!(
            operation("GET", json!("read_only"), JsonValue::Null),
            EffectClass::ReadOnly
        );
        assert_eq!(
            operation("POST", json!("read_only"), JsonValue::Null),
            EffectClass::ReadOnly
        );

        // A mutation with the complete contract is executable...
        assert_eq!(
            operation("POST", complete(), JsonValue::Null),
            EffectClass::ProviderIdempotentExplicitKey
        );

        // ...and every incomplete shape is not. A bare `idempotency` header
        // names a binding but publishes no retention to keep a margin under, a
        // margin that is not strictly smaller than the retention is no margin,
        // and silence is not evidence.
        for (method, effect, idempotency) in [
            ("POST", JsonValue::Null, JsonValue::Null),
            (
                "POST",
                JsonValue::Null,
                json!({ "header": "Idempotency-Key" }),
            ),
            ("PUT", JsonValue::Null, JsonValue::Null),
            ("DELETE", JsonValue::Null, JsonValue::Null),
            ("PATCH", JsonValue::Null, JsonValue::Null),
            (
                "POST",
                {
                    let mut effect = complete();
                    effect["provider_idempotent"]["side_effect_steps"][0]["clock_safety_margin_ms"] =
                        json!(604_800_000u64);
                    effect
                },
                JsonValue::Null,
            ),
            (
                "POST",
                {
                    let mut effect = complete();
                    effect["provider_idempotent"]["side_effect_steps"][0]["scope"] = json!("");
                    effect
                },
                JsonValue::Null,
            ),
            (
                "POST",
                json!({ "provider_idempotent": { "side_effect_steps": [] } }),
                JsonValue::Null,
            ),
        ] {
            assert_eq!(
                operation(method, effect, idempotency),
                EffectClass::InventoryOnly,
                "{method} without a complete idempotency contract is not executable"
            );
        }
    }

    /// An operation built here rather than compiled from metadata has no
    /// declared contract at all, so it takes the conservative class.
    #[test]
    fn a_directly_built_operation_defaults_to_the_conservative_class() {
        let read = HttpOperation::builder("read", HttpMethod::Get, "/v1/things")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a static declaration is valid");
        assert!(read.is_executable());

        let write = HttpOperation::builder("write", HttpMethod::Post, "/v1/things")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a static declaration is valid");
        assert!(
            !write.is_executable(),
            "an undeclared mutation is never executable"
        );
    }

    fn local_connector(base_url: &str) -> HttpConnector {
        let config = HttpConnectorConfig::new(
            base_url,
            vec![
                ConfiguredHeader::new("Authorization", "test-credential")
                    .expect("fixed credential header is valid"),
            ],
        )
        .expect("local static base URL is valid");
        HttpConnector::with_components(
            config,
            Arc::new(SystemResolver),
            Arc::new(ReqwestTransport::new()),
        )
    }

    fn context() -> ExecutionContext {
        ExecutionContext::with_deadline(tokio::time::Instant::now() + Duration::from_secs(1))
    }

    fn operation(path: &str) -> HttpOperation {
        HttpOperation::builder("test_operation", HttpMethod::Post, path)
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static operation declaration is valid")
    }

    fn declared_operation(path: &str, body: JsonValue) -> ValidatedHttpOperation {
        let operation: ConnectorOperation = serde_json::from_value(json!({
            "name": "create_shipment",
            "version": "v1",
            "method": "POST",
            "path": path,
            "body": body,
            "success_statuses": [200],
            "idempotency": { "header": "Idempotency-Key" },
            "capacity": {
                "max_in_flight": 1,
                "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
            }
        }))
        .expect("declarative operation metadata deserializes");
        ValidatedHttpOperation::from_metadata(&operation)
            .expect("declarative operation compiles before execution")
    }

    /// The same operation declared through the catalog fields: the header is
    /// bound by a provider-idempotent effect, and the retryable statuses come
    /// from an error map. Metadata written this way must behave identically —
    /// reading only the legacy fields left it sending no key and treating a
    /// declared 500 as permanent.
    fn catalog_declared_operation(path: &str) -> ValidatedHttpOperation {
        catalog_declared_operation_with_fallback(path, "permanent")
    }

    /// The same, with the error map's fallback class chosen by the caller.
    fn catalog_declared_operation_with_fallback(
        path: &str,
        fallback: &str,
    ) -> ValidatedHttpOperation {
        let operation: ConnectorOperation = serde_json::from_value(json!({
            "name": "create_shipment",
            "version": "v1",
            "method": "POST",
            "path": path,
            "body": { "order_id": { "input": "order_id" } },
            "success_statuses": [200],
            "effect": {
                "provider_idempotent": {
                    "side_effect_steps": [{
                        "step": "request",
                        "fixed_binding": { "header": "Idempotency-Key" },
                        "scope": "unit-test-v1",
                        "minimum_retention_ms": 600000,
                        "clock_safety_margin_ms": 1000,
                        "evidence": {
                            "source_record_id": "source.unit.test.v1",
                            "fact_ids": ["fact.unit.fixed-idempotency-key"]
                        }
                    }]
                }
            },
            "error_map": {
                "rules": [
                    { "statuses": [503], "class": "http_5xx", "code": "unavailable" },
                    { "statuses": [409], "class": "validation", "code": "conflict" }
                ],
                "fallback": { "class": fallback, "code": "provider_error" }
            },
            "capacity": {
                "max_in_flight": 1,
                "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
            }
        }))
        .expect("catalog-declared operation metadata deserializes");
        ValidatedHttpOperation::from_metadata(&operation)
            .expect("catalog-declared operation compiles before execution")
    }

    #[tokio::test]
    async fn an_effect_declared_binding_puts_the_idempotency_key_on_the_wire() {
        async fn record(headers: HeaderMap) -> Json<JsonValue> {
            Json(json!({
                "idempotency_key": headers
                    .get("idempotency-key")
                    .and_then(|value| value.to_str().ok()),
            }))
        }

        let server = LocalServer::start(Router::new().fallback(post(record))).await;
        let operation = catalog_declared_operation("/v1/orders");

        let result = local_connector(&server.base_url)
            .execute_validated(
                &operation,
                json!({ "order_id": "order-42" }),
                "logical-activity-42",
                context().deadline,
            )
            .await
            .expect("catalog-declared operation succeeds");

        assert_eq!(
            result.output["idempotency_key"], "logical-activity-42",
            "the effect's fixed binding names the header the key travels in"
        );
    }

    #[tokio::test]
    async fn a_declared_error_map_decides_the_failure_class() {
        for (status, expected) in [
            (
                StatusCode::SERVICE_UNAVAILABLE,
                ConnectorErrorClass::Http5xx,
            ),
            (StatusCode::CONFLICT, ConnectorErrorClass::Validation),
            // Unmapped: the built-in handling still answers for it.
            (StatusCode::IM_A_TEAPOT, ConnectorErrorClass::Permanent),
        ] {
            let server = LocalServer::start(Router::new().fallback(post(move || async move {
                (status, Json(json!({ "error": "no" })))
            })))
            .await;
            let operation = catalog_declared_operation("/v1/orders");

            let failure = local_connector(&server.base_url)
                .execute_validated(
                    &operation,
                    json!({ "order_id": "order-42" }),
                    "logical-activity-42",
                    context().deadline,
                )
                .await
                .expect_err("the provider refused");

            assert_eq!(
                failure.class(),
                expected,
                "status {status} is classified by what the operation declared"
            );
        }
    }

    /// A provider cannot park a durable activity for an arbitrary time.
    ///
    /// The failure type is now the SDK's, and it clamps `Retry-After` at its
    /// declared ceiling on construction. Before the merge this module had its
    /// own failure type with no ceiling at all, so a provider answering
    /// `Retry-After: 999999` proposed an eleven-day delay to retry routing.
    #[tokio::test]
    async fn a_provider_retry_after_is_clamped_to_the_declared_ceiling() {
        let server = LocalServer::start(Router::new().fallback(post(|| async {
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "999999")],
                Json(json!({ "error": "slow down" })),
            )
        })))
        .await;

        let failure = local_connector(&server.base_url)
            .execute(&operation("/v1/orders"), json!({}), context())
            .await
            .expect_err("the provider rate limited the request");

        assert_eq!(failure.class(), ConnectorErrorClass::Http429);
        assert_eq!(
            failure.retry_after(),
            Some(Duration::from_secs(86_400)),
            "a provider delay is honoured only up to the declared ceiling"
        );
    }

    /// A status no rule names takes the error map's own `fallback`.
    ///
    /// `fallback` is required by the metadata shape, and nothing read it: a
    /// provider returning 502 under `fallback: { class: http_5xx }` was
    /// classified `Permanent` by the built-in handling, so an activity that
    /// declared `retry_on: [http_5xx]` refused to retry the exact failure it
    /// was written for — and the operation's own declaration was the thing
    /// being overruled.
    #[tokio::test]
    async fn an_unmapped_status_takes_the_error_map_fallback() {
        let server = LocalServer::start(Router::new().fallback(post(|| async {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "upstream" })),
            )
        })))
        .await;

        // The same operation, but its fallback says a status it did not name is
        // a server error rather than a permanent one.
        let operation = catalog_declared_operation_with_fallback("/v1/orders", "http_5xx");

        let failure = local_connector(&server.base_url)
            .execute_validated(
                &operation,
                json!({ "order_id": "order-42" }),
                "logical-activity-42",
                context().deadline,
            )
            .await
            .expect_err("the provider refused");

        assert_eq!(
            failure.class(),
            ConnectorErrorClass::Http5xx,
            "502 names no rule, so the operation's fallback decides it"
        );
    }

    #[tokio::test]
    async fn declared_idempotency_header_uses_the_stable_logical_activity_key_and_canonical_input_fingerprint()
     {
        async fn record(headers: HeaderMap) -> Json<JsonValue> {
            Json(json!({
                "idempotency_key": headers
                    .get("idempotency-key")
                    .and_then(|value| value.to_str().ok()),
            }))
        }

        let server = LocalServer::start(Router::new().fallback(post(record))).await;
        let operation = declared_operation(
            "/v1/orders/{input.order_id}",
            json!({
                "order_id": { "input": "order_id" },
                "address": { "input": "address" }
            }),
        );

        let connector = local_connector(&server.base_url);
        let result = connector
            .execute_validated(
                &operation,
                json!({ "order_id": "order-42", "address": "first street" }),
                "logical-activity-42",
                context().deadline,
            )
            .await
            .expect("declared operation succeeds");

        assert_eq!(
            result.output,
            json!({ "idempotency_key": "logical-activity-42" }),
            "the deployed header receives the stable activity key, not a job input value"
        );

        let reordered = connector
            .execute_validated(
                &operation,
                json!({ "address": "first street", "order_id": "order-42" }),
                "logical-activity-42",
                context().deadline,
            )
            .await
            .expect("the same declared input values remain executable in a different JSON order");
        assert_eq!(
            result.request_fingerprint, reordered.request_fingerprint,
            "request fingerprints use canonical JSON rather than object insertion order"
        );
    }

    #[tokio::test]
    async fn declarative_optional_response_binding_publishes_an_absent_provider_field_as_null() {
        async fn response() -> Json<JsonValue> {
            Json(json!({ "id": "ship_123" }))
        }

        let server = LocalServer::start(Router::new().route("/shipment", post(response))).await;
        let operation: ConnectorOperation = serde_json::from_value(json!({
            "name": "read_shipment",
            "version": "v1",
            "method": "POST",
            "path": "/shipment",
            "success_statuses": [200],
            "idempotency": { "header": "Idempotency-Key" },
            "response": {
                "tracking_url": { "json_pointer": "/tracking_url", "type": "string" }
            },
            "capacity": {
                "max_in_flight": 1,
                "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
            }
        }))
        .expect("optional response declaration deserializes");
        let operation = ValidatedHttpOperation::from_metadata(&operation)
            .expect("optional response declaration compiles");

        let result = local_connector(&server.base_url)
            .execute_validated(
                &operation,
                json!({}),
                "logical-activity-optional-response",
                context().deadline,
            )
            .await
            .expect("an optional provider field may be absent");

        assert_eq!(
            result.output,
            json!({ "tracking_url": null }),
            "the declared output keeps every declared field, so a downstream \
             binding can read an optional field that the provider omitted"
        );
    }

    #[tokio::test]
    async fn declarative_required_response_binding_rejects_an_explicit_provider_null() {
        async fn response() -> Json<JsonValue> {
            Json(json!({ "shipment_id": null }))
        }

        let server = LocalServer::start(Router::new().route("/shipment", post(response))).await;
        let operation = HttpOperation::builder("read", HttpMethod::Post, "/shipment")
            .success_statuses([StatusCode::OK])
            .response_pointer("shipment_id", "/shipment_id", true)
            .build()
            .expect("response declaration is valid");

        let failure = local_connector(&server.base_url)
            .execute(&operation, json!({}), context())
            .await
            .expect_err("a declared non-null field is not satisfied by an explicit null");

        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    }

    #[tokio::test]
    async fn local_test_connector_encodes_static_paths_queries_headers_and_json_without_host_input()
    {
        async fn echo_item(
            OriginalUri(uri): OriginalUri,
            headers: HeaderMap,
            Json(body): Json<JsonValue>,
        ) -> Json<JsonValue> {
            Json(json!({
                "uri": uri.to_string(),
                "authorization": headers.get("authorization").and_then(|value| value.to_str().ok()),
                "operation_header": headers.get("x-operation").and_then(|value| value.to_str().ok()),
                "body": body,
            }))
        }

        let server =
            LocalServer::start(Router::new().route("/v1/items/{id}", post(echo_item))).await;
        let operation = HttpOperation::builder(
            "create_item",
            HttpMethod::Post,
            "/v1/items/{input.order_id}",
        )
        .query_input("search", "search")
        .static_header("X-Operation", "fixed")
        .body(JsonTemplate::object([(
            "order_id",
            JsonTemplate::input("order_id"),
        )]))
        .success_statuses([StatusCode::OK])
        .response_pointer("uri", "/uri", true)
        .response_pointer("authorization", "/authorization", true)
        .response_pointer("operation_header", "/operation_header", true)
        .response_pointer("body_order_id", "/body/order_id", true)
        .build()
        .expect("declared operation is valid");

        let result = local_connector(&server.base_url)
            .execute(
                &operation,
                json!({
                    "order_id": "a/b ? #",
                    "search": "one/two & three",
                    "unbound_url": "https://attacker.invalid/should-not-be-used",
                }),
                context(),
            )
            .await
            .expect("static endpoint request succeeds");

        assert_eq!(
            result.output,
            json!({
                "uri": "/v1/items/a%2Fb%20%3F%20%23?search=one%2Ftwo%20%26%20three",
                "authorization": "test-credential",
                "operation_header": "fixed",
                "body_order_id": "a/b ? #",
            }),
            "declared path and query slots are percent-encoded while the input cannot choose a host"
        );
    }

    /// A declared query key reaches the provider exactly as it was declared.
    ///
    /// This module used to percent-encode the *key* as well as the value, so an
    /// operation declaring `api-version` put `api%2Dversion=2026-01-01` on the
    /// wire. RFC 3986 makes `%2D` and `-` equivalent only after normalization,
    /// and query parsers do not normalize: a provider matching on `api-version`
    /// saw an unknown parameter and a missing required one. The SDK validates
    /// the key when the operation is built and then emits it literally, which
    /// is the wire form the provider documents. Values are still encoded.
    #[tokio::test]
    async fn a_declared_query_key_reaches_the_provider_as_written() {
        async fn echo(OriginalUri(uri): OriginalUri) -> Json<JsonValue> {
            Json(json!({ "uri": uri.to_string() }))
        }

        let server = LocalServer::start(Router::new().route("/v1/items", post(echo))).await;
        let operation = HttpOperation::builder("list_items", HttpMethod::Post, "/v1/items")
            .query_input("api-version", "api_version")
            .query_input("filter[status]", "status")
            .success_statuses([StatusCode::OK])
            .response_pointer("uri", "/uri", true)
            .build()
            .expect("a declared query key is valid");

        let result = local_connector(&server.base_url)
            .execute(
                &operation,
                json!({ "api_version": "2026-01-01", "status": "one/two" }),
                context(),
            )
            .await
            .expect("the query renders");

        assert_eq!(
            result.output,
            json!({
                "uri": "/v1/items?api-version=2026%2D01%2D01&filter[status]=one%2Ftwo",
            }),
            "a declared key travels literally while its value stays encoded"
        );
    }

    #[tokio::test]
    async fn local_test_connector_extracts_declared_json_pointers() {
        async fn response() -> Json<JsonValue> {
            Json(
                json!({"id": "ship_123", "tracking": {"url": "https://tracking.example.test/123"}}),
            )
        }

        let server = LocalServer::start(Router::new().route("/response", post(response))).await;
        let operation = HttpOperation::builder("extract", HttpMethod::Post, "/response")
            .success_statuses([StatusCode::OK])
            .response_pointer("shipment_id", "/id", true)
            .response_pointer("tracking_url", "/tracking/url", false)
            .build()
            .expect("response declaration is valid");
        let result = local_connector(&server.base_url)
            .execute(&operation, json!({}), context())
            .await
            .expect("JSON response satisfies the declared extraction");

        assert_eq!(
            result.output,
            json!({
                "shipment_id": "ship_123",
                "tracking_url": "https://tracking.example.test/123",
            })
        );
    }

    #[tokio::test]
    async fn local_test_connector_never_follows_redirects() {
        async fn redirect() -> impl IntoResponse {
            (StatusCode::FOUND, [("location", "/redirect-target")])
        }
        async fn redirect_target(State(hits): State<Arc<AtomicUsize>>) -> impl IntoResponse {
            hits.fetch_add(1, Ordering::Relaxed);
            Json(json!({"unexpected": true}))
        }

        let target_hits = Arc::new(AtomicUsize::new(0));
        let server = LocalServer::start(
            Router::new()
                .route("/redirect", post(redirect))
                .route("/redirect-target", get(redirect_target))
                .with_state(target_hits.clone()),
        )
        .await;
        let failure = local_connector(&server.base_url)
            .execute(&operation("/redirect"), json!({}), context())
            .await
            .expect_err("redirect status is not followed");

        assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
        assert_eq!(target_hits.load(Ordering::Relaxed), 0);
        assert!(!failure.safe_message().contains("redirect-target"));
    }

    #[tokio::test]
    async fn local_test_connector_bounds_request_and_response_bodies() {
        async fn oversized_response() -> Response {
            (StatusCode::OK, "x".repeat(MAX_HTTP_BODY_BYTES + 1)).into_response()
        }

        let request_failure = local_connector("http://127.0.0.1:9")
            .execute(
                &HttpOperation::builder("oversized_request", HttpMethod::Post, "/unused")
                    .body(JsonTemplate::literal(
                        json!({"payload": "x".repeat(MAX_HTTP_BODY_BYTES)}),
                    ))
                    .success_statuses([StatusCode::OK])
                    .build()
                    .expect("declared request is valid"),
                json!({}),
                context(),
            )
            .await
            .expect_err("the module refuses an oversized request before connecting");
        assert_eq!(request_failure.class(), ConnectorErrorClass::Invariant);

        let server =
            LocalServer::start(Router::new().route("/large", post(oversized_response))).await;
        let response_failure = local_connector(&server.base_url)
            .execute(&operation("/large"), json!({}), context())
            .await
            .expect_err("the module refuses an oversized provider response");
        assert_eq!(response_failure.class(), ConnectorErrorClass::Validation);
    }

    #[tokio::test]
    async fn local_test_connector_enforces_deadline_and_closed_status_classes() {
        async fn slow() -> Json<JsonValue> {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Json(json!({"ok": true}))
        }
        async fn status(Path(status): Path<u16>) -> Response {
            (
                StatusCode::from_u16(status).expect("test status is valid"),
                "provider response body must not leak",
            )
                .into_response()
        }
        async fn malformed() -> impl IntoResponse {
            (StatusCode::OK, "not JSON")
        }

        let server = LocalServer::start(
            Router::new()
                .route("/slow", post(slow))
                .route("/status/{status}", post(status))
                .route("/malformed", post(malformed)),
        )
        .await;
        let connector = local_connector(&server.base_url);
        let deadline_failure = connector
            .execute(
                &operation("/slow"),
                json!({}),
                ExecutionContext::with_deadline(
                    tokio::time::Instant::now() + Duration::from_millis(10),
                ),
            )
            .await
            .expect_err("the finite activity deadline is enforced");
        assert_eq!(deadline_failure.class(), ConnectorErrorClass::Timeout);

        for (path, operation, expected) in [
            (
                "/status/429",
                operation("/status/429"),
                ConnectorErrorClass::Http429,
            ),
            (
                "/status/503",
                HttpOperation::builder("declared_5xx", HttpMethod::Post, "/status/503")
                    .success_statuses([StatusCode::OK])
                    .declared_5xx([StatusCode::SERVICE_UNAVAILABLE])
                    .build()
                    .expect("5xx declaration is valid"),
                ConnectorErrorClass::Http5xx,
            ),
            (
                "/status/401",
                operation("/status/401"),
                ConnectorErrorClass::Authentication,
            ),
            (
                "/status/403",
                operation("/status/403"),
                ConnectorErrorClass::Authentication,
            ),
            (
                "/status/400",
                operation("/status/400"),
                ConnectorErrorClass::Validation,
            ),
            (
                "/status/404",
                operation("/status/404"),
                ConnectorErrorClass::Permanent,
            ),
        ] {
            let failure = connector
                .execute(&operation, json!({}), context())
                .await
                .expect_err("non-success provider status fails the operation");
            assert_eq!(failure.class(), expected, "status path {path}");
            assert!(
                !failure
                    .safe_message()
                    .contains("provider response body must not leak")
            );
        }
        let malformed_failure = connector
            .execute(&operation("/malformed"), json!({}), context())
            .await
            .expect_err("successful response must still satisfy the JSON contract");
        assert_eq!(malformed_failure.class(), ConnectorErrorClass::Validation);
    }

    /// A path binding a deployment declared twice still loads, and still
    /// renders twice.
    ///
    /// The declarative path template is the deployment's, and this module has
    /// always admitted a binding that appears in more than one segment — a
    /// provider that repeats an owner in `/orgs/{org}/repos/{org}` is
    /// ordinary. Compiling the request through the SDK re-validated the
    /// template against the rule written for hand-written connectors, which
    /// declares each slot once, so metadata that deployed yesterday failed
    /// `from_metadata` and the engine refused to boot. The repetition is a
    /// spelling difference, not a contract difference: it is translated into
    /// distinct SDK slots, the operation's declared inputs are unchanged, and
    /// the wire form is the one the deployment wrote.
    #[tokio::test]
    async fn a_repeated_path_binding_loads_and_renders_every_occurrence() {
        async fn echo(OriginalUri(uri): OriginalUri) -> Json<JsonValue> {
            Json(json!({ "uri": uri.to_string() }))
        }

        let server =
            LocalServer::start(Router::new().route("/v1/orgs/{org}/repos/{repo}", get(echo))).await;
        let metadata: ConnectorOperation = serde_json::from_value(json!({
            "name": "list_repos",
            "version": "v1",
            "method": "GET",
            "path": "/v1/orgs/{input.org}/repos/{input.org}",
            "query": { "since": { "input": "org" } },
            "success_statuses": [200],
            "response": { "uri": { "json_pointer": "/uri", "type": "string!" } },
            "capacity": {
                "max_in_flight": 1,
                "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
            }
        }))
        .expect("operation metadata deserializes");
        let operation = ValidatedHttpOperation::from_metadata(&metadata)
            .expect("a repeated path binding is still declarative metadata");

        // The declared input contract is the deployment's own: one binding,
        // however many times the path spends it. `validate_dispatch_input`
        // refuses both an undeclared name and a missing one, so this call
        // passing is the assertion.
        let result = local_connector(&server.base_url)
            .execute_validated(
                &operation,
                json!({ "org": "acme" }),
                "logical-activity-repeated-binding",
                context().deadline,
            )
            .await
            .expect("the repeated binding renders");

        assert_eq!(
            result.output,
            json!({ "uri": "/v1/orgs/acme/repos/acme?since=acme" }),
            "every occurrence renders the value the one declared input carries"
        );
    }

    /// A synthetic slot is not allowed to land on a name the deployment
    /// declared: filling it would then overwrite one binding's value with
    /// another's, which is a wrong request rather than a refused one.
    #[test]
    fn a_repeated_bindings_synthetic_slot_never_takes_a_declared_name() {
        let (path, bindings) = sdk_path_bindings("/v1/{input.org}/{input.org_}/{input.org}")
            .expect("the template is valid metadata");
        assert_eq!(path, "/v1/{org}/{org_}/{org__}");
        assert_eq!(
            bindings,
            vec![
                ("org".to_owned(), "org".to_owned()),
                ("org_".to_owned(), "org_".to_owned()),
                ("org__".to_owned(), "org".to_owned()),
            ],
            "each occurrence renders from the binding it was written as"
        );
    }

    /// What this module admits in a URL is this module's own rule, stated here.
    ///
    /// A literal `@` in a path and a query key outside the SDK's character set
    /// are refused — deliberately, because one engine should have one answer to
    /// what a connector request's URL may contain, and that answer is widened
    /// once, with provider evidence, in the SDK
    /// (`knowledgebase/declarative-saas/decisions/047-*`). The refusal must be
    /// *this module's*, at the metadata field, in words an operator can act on:
    /// inheriting it from the SDK's builder would make the deployment surface
    /// change by accident, and a rule nobody wrote is a rule nobody can read.
    #[test]
    fn the_declarative_url_grammar_is_this_modules_own() {
        let compile = |path: &str, query: JsonValue| {
            let metadata: ConnectorOperation = serde_json::from_value(json!({
                "name": "read",
                "version": "v1",
                "method": "GET",
                "path": path,
                "query": query,
                "success_statuses": [200],
                "capacity": {
                    "max_in_flight": 1,
                    "rate_limit": { "permits": 1, "per": "1s", "burst": 1 }
                }
            }))
            .expect("operation metadata deserializes");
            ValidatedHttpOperation::from_metadata(&metadata).map(|_| ())
        };

        assert!(compile("/v1/users/me", json!({})).is_ok());
        let path_error =
            compile("/v1/users/@me", json!({})).expect_err("a literal userinfo marker is refused");
        assert_eq!(
            path_error.to_string(),
            "path must be a static absolute path without authority, userinfo, query, fragment, or dot segments"
        );
        // And it is refused *here*, by this module's own validator, rather
        // than by whatever the SDK's builder happens to check today.
        assert_eq!(
            validate_path_template("/v1/users/@me")
                .expect_err("this module's own path rule refuses a literal `@`")
                .to_string(),
            path_error.to_string()
        );

        assert!(
            compile(
                "/v1/items",
                json!({ "filter[status]": { "input": "status" } })
            )
            .is_ok()
        );
        let query_error = compile(
            "/v1/items",
            json!({ "filter:status": { "input": "status" } }),
        )
        .expect_err("a query key outside the declared set is refused");
        assert_eq!(
            query_error.to_string(),
            "query names and input bindings must be static and valid"
        );
    }
}
