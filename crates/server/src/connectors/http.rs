//! Safe, declarative outbound HTTP transport.
//!
//! This module receives a static operation template and JSON values for its
//! declared slots.  There is intentionally no API accepting a caller-supplied
//! URL, method, header name, redirect policy, or TLS policy.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use donat_metadata::{ConnectorBaseUrl, ConnectorConfig, ConnectorOperation};
use futures_util::{StreamExt, future::BoxFuture};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{
    Client, Method, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
};
use serde_json::{Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};

use super::{
    ConnectorDefinition, ConnectorErrorClass, ConnectorFailure, ConnectorModule, ConnectorSuccess,
    ExecutionContext, canonical_json_sha256,
};

pub const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;

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
    headers: HeaderMap,
}

impl HttpConnectorConfig {
    pub fn new(base_url: &str, headers: Vec<ConfiguredHeader>) -> Result<Self, HttpConfigError> {
        let base_url = validate_http_base_url(base_url)?;
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

/// A fixed HTTP method from the deployment operation declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    fn as_reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Post => Method::POST,
            Self::Put => Method::PUT,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
        }
    }
}

/// A JSON request template whose only dynamic leaves are static, named input
/// slots.  Task 3 maps metadata declarations onto this representation.
#[derive(Debug, Clone)]
pub enum JsonTemplate {
    Literal(JsonValue),
    Input(String),
    Object(Vec<(String, JsonTemplate)>),
    Array(Vec<JsonTemplate>),
}

impl JsonTemplate {
    pub fn literal(value: JsonValue) -> Self {
        Self::Literal(value)
    }

    pub fn input(name: &str) -> Self {
        Self::Input(name.to_owned())
    }

    pub fn object<const N: usize>(fields: [(&str, JsonTemplate); N]) -> Self {
        Self::Object(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }

    fn render(&self, input: &JsonValue) -> Result<JsonValue, ConnectorFailure> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Input(name) => input
                .get(name)
                .cloned()
                .ok_or_else(|| invariant_failure("a declared connector input value is missing")),
            Self::Object(fields) => {
                let mut object = JsonMap::new();
                for (name, value) in fields {
                    object.insert(name.clone(), value.render(input)?);
                }
                Ok(JsonValue::Object(object))
            }
            Self::Array(values) => values
                .iter()
                .map(|value| value.render(input))
                .collect::<Result<Vec<_>, _>>()
                .map(JsonValue::Array),
        }
    }
}

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
#[derive(Debug, Clone)]
pub struct HttpOperation {
    _name: String,
    method: HttpMethod,
    path_template: String,
    query_inputs: Vec<(String, String)>,
    headers: Vec<StaticHeader>,
    body: Option<JsonTemplate>,
    success_statuses: BTreeSet<u16>,
    response_pointers: Vec<ResponsePointer>,
    declared_5xx: BTreeSet<u16>,
    /// The operation's own `error_map`: which class each status belongs to.
    /// It is what decides whether a failure is retryable, so it has to reach
    /// the runtime rather than stay a deploy-time description.
    declared_classes: BTreeMap<u16, ConnectorErrorClass>,
    idempotency_header: Option<HeaderName>,
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
    idempotency_header: Option<String>,
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
            body: None,
            success_statuses: BTreeSet::new(),
            response_pointers: Vec::new(),
            declared_5xx: BTreeSet::new(),
            idempotency_header: None,
        }
    }

    fn header_names(&self) -> impl Iterator<Item = &HeaderName> {
        self.headers
            .iter()
            .map(|header| &header.name)
            .chain(self.idempotency_header.iter())
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

    pub fn build(self) -> Result<HttpOperation, HttpConfigError> {
        if self.name.is_empty() {
            return Err(HttpConfigError::new("connector operation name is required"));
        }
        validate_path_template(&self.path_template)?;
        if self.success_statuses.is_empty() {
            return Err(HttpConfigError::new(
                "connector operation must declare a success status",
            ));
        }
        if self.query_inputs.iter().any(|(name, input)| {
            name.is_empty() || input.is_empty() || name.contains(['&', '=', '#'])
        }) {
            return Err(HttpConfigError::new(
                "query names and input bindings must be static and valid",
            ));
        }
        if self.response_pointers.iter().any(|field| {
            field.output_name.is_empty()
                || (!field.pointer.is_empty() && !field.pointer.starts_with('/'))
        }) {
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
        Ok(HttpOperation {
            _name: self.name,
            method: self.method,
            path_template: self.path_template,
            query_inputs: self.query_inputs,
            headers,
            body: self.body,
            success_statuses: self.success_statuses,
            response_pointers: self.response_pointers,
            declared_5xx: self.declared_5xx,
            declared_classes: self.declared_classes,
            idempotency_header,
        })
    }
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

/// A DNS resolver seam.  Production uses [`SystemResolver`]; tests use a
/// sequence resolver to prove that every pre-connect lookup is checked.
pub trait HostResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<Vec<IpAddr>, ResolveError>>;
}

pub struct SystemResolver;

impl HostResolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> BoxFuture<'a, Result<Vec<IpAddr>, ResolveError>> {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| ResolveError::new("system lookup failed"))?
                .map(|address| address.ip())
                .collect::<Vec<_>>();
            if addresses.is_empty() {
                return Err(ResolveError::new("system lookup returned no addresses"));
            }
            Ok(addresses)
        })
    }
}

#[derive(Debug, Clone)]
pub struct ResolveError {
    _private: (),
}

impl ResolveError {
    pub fn new(_detail: &str) -> Self {
        // Resolver diagnostics can contain host details.  Retain no diagnostic
        // string because activity errors must be provider-safe.
        Self { _private: () }
    }
}

/// A prepared request has only private fields so a transport cannot be used as
/// a caller-facing arbitrary-request API.
pub struct PreparedHttpRequest {
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct RawHttpResponse {
    pub status: StatusCode,
    peer: Option<SocketAddr>,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl RawHttpResponse {
    pub fn json(status: StatusCode, value: JsonValue) -> Self {
        Self {
            status,
            peer: None,
            headers: HeaderMap::new(),
            body: serde_json::to_vec(&value).expect("JSON test response serializes"),
        }
    }

    /// Set the connected peer for an injected transport.  The HTTP connector
    /// validates this against its second vetted DNS result before accepting a
    /// public-only response.
    pub fn with_peer(mut self, peer: SocketAddr) -> Self {
        self.peer = Some(peer);
        self
    }
}

#[derive(Debug, Clone)]
enum TransportErrorKind {
    Transport,
    Timeout,
    ResponseTooLarge,
}

#[derive(Debug, Clone)]
pub struct TransportError {
    kind: TransportErrorKind,
}

impl TransportError {
    pub fn new(_detail: &str) -> Self {
        // As with resolver errors, omit raw provider/host details from the
        // connector boundary.
        Self {
            kind: TransportErrorKind::Transport,
        }
    }

    fn response_too_large() -> Self {
        Self {
            kind: TransportErrorKind::ResponseTooLarge,
        }
    }

    fn timeout() -> Self {
        Self {
            kind: TransportErrorKind::Timeout,
        }
    }
}

/// An outbound transport seam.  It is injected solely by the compiled module
/// constructor and receives a vetted destination list, never caller values.
pub trait HttpTransport: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: PreparedHttpRequest,
        destination: &'a [IpAddr],
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<RawHttpResponse, TransportError>>;
}

pub struct ReqwestTransport {
    #[cfg(test)]
    test_proxy: Option<reqwest::Proxy>,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self {
            #[cfg(test)]
            test_proxy: None,
        }
    }

    #[cfg(test)]
    fn with_proxy_for_test(proxy: reqwest::Proxy) -> Self {
        Self {
            test_proxy: Some(proxy),
        }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport for ReqwestTransport {
    fn execute<'a>(
        &'a self,
        request: PreparedHttpRequest,
        destination: &'a [IpAddr],
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<RawHttpResponse, TransportError>> {
        let destination = destination.to_vec();
        Box::pin(async move {
            let host = request
                .url
                .host_str()
                .ok_or_else(|| TransportError::new("missing host"))?;
            let port = request
                .url
                .port_or_known_default()
                .ok_or_else(|| TransportError::new("missing port"))?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::timeout());
            }
            let addresses = destination
                .into_iter()
                .map(|address| SocketAddr::new(address, port))
                .collect::<Vec<_>>();
            let client = {
                let builder = Client::builder();
                #[cfg(test)]
                let builder = match &self.test_proxy {
                    Some(proxy) => builder.proxy(proxy.clone()),
                    None => builder,
                };
                builder
                    // Redirects would require a fresh static-destination policy
                    // check and could become an SSRF bypass, so never follow them.
                    .redirect(reqwest::redirect::Policy::none())
                    // System and environment proxy configuration would create an
                    // unchecked connection hop outside the vetted destination set.
                    .no_proxy()
                    // Activity retries are owned by the engine.  A transport retry
                    // could duplicate a provider-side effect before routing sees it.
                    .retry(reqwest::retry::never())
                    .timeout(remaining)
                    // Pin reqwest's lookup to the second, immediately-before-
                    // connect resolver result that this module just vetted.
                    .resolve_to_addrs(host, &addresses)
                    .build()
                    .map_err(|_| TransportError::new("client construction failed"))?
            };
            let response = client
                .request(request.method, request.url)
                .headers(request.headers)
                .body(request.body)
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() {
                        TransportError::timeout()
                    } else {
                        TransportError::new("request failed")
                    }
                })?;
            let status = response.status();
            let headers = response.headers().clone();
            let peer = response.remote_addr();
            let mut stream = response.bytes_stream();
            let mut body = Vec::new();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    if error.is_timeout() {
                        TransportError::timeout()
                    } else {
                        TransportError::new("response read failed")
                    }
                })?;
                if body.len().saturating_add(chunk.len()) > MAX_HTTP_BODY_BYTES {
                    return Err(TransportError::response_too_large());
                }
                body.extend_from_slice(&chunk);
            }
            Ok(RawHttpResponse {
                status,
                peer,
                headers,
                body,
            })
        })
    }
}

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
            .execute_internal(operation, input, None, context)
            .await?;
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
        context: ExecutionContext,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        if context.deadline <= tokio::time::Instant::now() {
            return Err(timeout_failure());
        }
        // Reject an unsafe initial lookup before rendering path, query, or body
        // templates.  This keeps untrusted input out of a request whose
        // declared destination is already disallowed.
        self.resolve_under_deadline(context.deadline).await?;
        let request = self.prepare_request(operation, &input, idempotency_key)?;
        if request.body.len() > MAX_HTTP_BODY_BYTES {
            return Err(invariant_failure(
                "connector request exceeds the 1 MiB limit",
            ));
        }

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
        .map_err(|error| match error.kind {
            TransportErrorKind::Transport => transport_failure(),
            TransportErrorKind::Timeout => timeout_failure(),
            TransportErrorKind::ResponseTooLarge => {
                validation_failure("connector response exceeds the 1 MiB limit")
            }
        })?;
        self.validate_connected_peer(&destination, response.peer)?;
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
    ) -> Result<PreparedHttpRequest, ConnectorFailure> {
        let path = render_path(&operation.path_template, input)?;
        let mut url = self.config.base_url.clone();
        url.set_path(&path);
        if operation.query_inputs.is_empty() {
            url.set_query(None);
        } else {
            let query = operation
                .query_inputs
                .iter()
                .map(|(name, input_name)| {
                    let value = scalar_input(input, input_name)?;
                    Ok(format!(
                        "{}={}",
                        utf8_percent_encode(name, NON_ALPHANUMERIC),
                        utf8_percent_encode(&value, NON_ALPHANUMERIC)
                    ))
                })
                .collect::<Result<Vec<_>, ConnectorFailure>>()?
                .join("&");
            url.set_query(Some(&query));
        }
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
        let body = operation
            .body
            .as_ref()
            .map(|template| {
                template.render(input).and_then(|value| {
                    serde_json::to_vec(&value).map_err(|_| {
                        invariant_failure("connector request JSON serialization failed")
                    })
                })
            })
            .transpose()?
            .unwrap_or_default();
        if operation.body.is_some() && !headers.contains_key(CONTENT_TYPE) {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        Ok(PreparedHttpRequest {
            method: operation.method.as_reqwest(),
            url,
            headers,
            body,
        })
    }

    fn decode_response(
        &self,
        operation: &HttpOperation,
        response: RawHttpResponse,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        if response.body.len() > MAX_HTTP_BODY_BYTES {
            return Err(validation_failure(
                "connector response exceeds the 1 MiB limit",
            ));
        }
        let status = response.status.as_u16();
        if !operation.success_statuses.contains(&status) {
            // What the operation declared about this status wins: the built-in
            // handling below is the answer for an operation that declared
            // nothing, not an override of one that did.
            if let Some(class) = operation.declared_classes.get(&status) {
                return Err(declared_failure(*class, &response.headers));
            }
            return Err(match status {
                408 => timeout_failure(),
                429 => ConnectorFailure::new(
                    ConnectorErrorClass::Http429,
                    "connector_http_429",
                    "connector provider rate limited the request",
                )
                .with_retry_after(retry_after(&response.headers)),
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
            });
        }
        let value: JsonValue = serde_json::from_slice(&response.body)
            .map_err(|_| validation_failure("connector provider returned malformed JSON"))?;
        if operation.response_pointers.is_empty() {
            return Ok(ConnectorSuccess {
                output: value,
                request_fingerprint: String::new(),
            });
        }
        // The declared response is the activity's output schema, so every
        // declared field appears in the output. A provider that omits an
        // optional field yields an explicit null rather than a missing key:
        // downstream bindings read declared fields by name, and a key that
        // exists only when the provider felt like sending it would make an
        // optional field unreadable exactly when it is absent.
        let mut output = JsonMap::new();
        for field in &operation.response_pointers {
            match value.pointer(&field.pointer) {
                Some(value) if field.required && value.is_null() => {
                    return Err(validation_failure(
                        "connector provider response did not satisfy the declared contract",
                    ));
                }
                Some(value) => {
                    output.insert(field.output_name.clone(), value.clone());
                }
                None if field.required => {
                    return Err(validation_failure(
                        "connector provider response did not satisfy the declared contract",
                    ));
                }
                None => {
                    output.insert(field.output_name.clone(), JsonValue::Null);
                }
            }
        }
        Ok(ConnectorSuccess {
            output: JsonValue::Object(output),
            request_fingerprint: String::new(),
        })
    }
}

impl ConnectorModule for HttpConnector {
    fn definition(&self) -> ConnectorDefinition {
        ConnectorDefinition {
            module_name: "http",
            semantic_version: "0.1.0",
            runtime_abi: 1,
        }
    }
}

fn validate_path_template(template: &str) -> Result<(), HttpConfigError> {
    if !template.starts_with('/')
        || template.starts_with("//")
        || template.contains('?')
        || template.contains('#')
        || template.contains("://")
        || template
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(HttpConfigError::new(
            "path must be a static absolute path without authority, query, fragment, or dot segments",
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

fn render_path(template: &str, input: &JsonValue) -> Result<String, ConnectorFailure> {
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some(index) = remaining.find("{input.") {
        rendered.push_str(&remaining[..index]);
        let after = &remaining[index + "{input.".len()..];
        let end = after
            .find('}')
            .expect("HttpOperation validates every path binding");
        let name = &after[..end];
        let value = scalar_input(input, name)?;
        rendered.push_str(&utf8_percent_encode(&value, NON_ALPHANUMERIC).to_string());
        remaining = &after[end + 1..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

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
        routing::{any, get, post},
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

    fn request_to(base_url: &str) -> PreparedHttpRequest {
        PreparedHttpRequest {
            method: Method::POST,
            url: Url::parse(&format!("{base_url}/operation")).expect("static test URL is valid"),
            headers: HeaderMap::new(),
            body: Vec::new(),
        }
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
                "fallback": { "class": "permanent", "code": "provider_error" }
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
                failure.class, expected,
                "status {status} is classified by what the operation declared"
            );
        }
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

        assert_eq!(failure.class, ConnectorErrorClass::Validation);
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

        assert_eq!(failure.class, ConnectorErrorClass::Permanent);
        assert_eq!(target_hits.load(Ordering::Relaxed), 0);
        assert!(!failure.safe_message.contains("redirect-target"));
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
        assert_eq!(request_failure.class, ConnectorErrorClass::Invariant);

        let server =
            LocalServer::start(Router::new().route("/large", post(oversized_response))).await;
        let response_failure = local_connector(&server.base_url)
            .execute(&operation("/large"), json!({}), context())
            .await
            .expect_err("the module refuses an oversized provider response");
        assert_eq!(response_failure.class, ConnectorErrorClass::Validation);
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
        assert_eq!(deadline_failure.class, ConnectorErrorClass::Timeout);

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
            assert_eq!(failure.class, expected, "status path {path}");
            assert!(
                !failure
                    .safe_message
                    .contains("provider response body must not leak")
            );
        }
        let malformed_failure = connector
            .execute(&operation("/malformed"), json!({}), context())
            .await
            .expect_err("successful response must still satisfy the JSON contract");
        assert_eq!(malformed_failure.class, ConnectorErrorClass::Validation);
    }

    #[tokio::test]
    async fn elapsed_deadline_before_client_construction_is_a_timeout() {
        let request = PreparedHttpRequest {
            method: Method::POST,
            url: Url::parse("https://provider.example.test/operation")
                .expect("static test URL is valid"),
            headers: HeaderMap::new(),
            body: Vec::new(),
        };

        let error = ReqwestTransport::new()
            .execute(
                request,
                &[IpAddr::V4(std::net::Ipv4Addr::new(8, 8, 8, 8))],
                tokio::time::Instant::now() - Duration::from_millis(1),
            )
            .await
            .expect_err("a deadline that elapses before client setup is typed as timeout");

        assert!(matches!(error.kind, TransportErrorKind::Timeout));
    }

    #[tokio::test]
    async fn reqwest_transport_does_not_retry_a_protocol_nack() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("nack test listener binds");
        let address = listener
            .local_addr()
            .expect("nack test listener has an address");
        let connections = Arc::new(AtomicUsize::new(0));
        let observed_connections = connections.clone();
        let listener_task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.expect("initial connection arrives");
            observed_connections.fetch_add(1, Ordering::Relaxed);
            drop(socket);
            if let Ok(Ok((_socket, _))) =
                tokio::time::timeout(Duration::from_millis(100), listener.accept()).await
            {
                observed_connections.fetch_add(1, Ordering::Relaxed);
            }
        });
        let request = PreparedHttpRequest {
            method: Method::GET,
            url: Url::parse(&format!("http://{address}/nack")).expect("test URL is valid"),
            headers: HeaderMap::new(),
            body: Vec::new(),
        };

        let error = ReqwestTransport::new()
            .execute(
                request,
                &[IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("a protocol nack is surfaced to engine-owned retry routing");
        listener_task.await.expect("nack listener finishes");

        assert!(matches!(error.kind, TransportErrorKind::Transport));
        assert_eq!(connections.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn reqwest_transport_discards_a_configured_proxy_before_connecting() {
        let proxy_hits = Arc::new(AtomicUsize::new(0));
        let proxy_counter = proxy_hits.clone();
        let proxy = LocalServer::start(Router::new().fallback(any(move || {
            let proxy_counter = proxy_counter.clone();
            async move {
                proxy_counter.fetch_add(1, Ordering::Relaxed);
                (StatusCode::BAD_GATEWAY, "proxy was contacted")
            }
        })))
        .await;
        let target_hits = Arc::new(AtomicUsize::new(0));
        let target_counter = target_hits.clone();
        let target = LocalServer::start(Router::new().fallback(any(move || {
            let target_counter = target_counter.clone();
            async move {
                target_counter.fetch_add(1, Ordering::Relaxed);
                (StatusCode::OK, "direct target")
            }
        })))
        .await;
        // The test seam installs a proxy immediately before production policy
        // is applied.  `no_proxy()` must clear it without mutating global
        // process environment shared by parallel tests.
        let response = ReqwestTransport::with_proxy_for_test(
            reqwest::Proxy::all(&proxy.base_url).expect("test proxy URL is valid"),
        )
        .execute(
            request_to(&target.base_url),
            &[IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("transport reaches the vetted destination directly");

        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.peer.map(|peer| peer.ip()),
            Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "the production reqwest path records the actual connected peer"
        );
        assert_eq!(target_hits.load(Ordering::Relaxed), 1);
        assert_eq!(proxy_hits.load(Ordering::Relaxed), 0);
    }
}
