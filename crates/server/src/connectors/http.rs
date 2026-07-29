//! Safe, declarative outbound HTTP transport.
//!
//! This module receives a static operation template and JSON values for its
//! declared slots.  There is intentionally no API accepting a caller-supplied
//! URL, method, header name, redirect policy, or TLS policy.

use std::collections::BTreeSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use donat_metadata::{ConnectorBaseUrl, ConnectorConfig};
use futures_util::{StreamExt, future::BoxFuture};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{
    Client, Method, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
};
use serde_json::{Map as JsonMap, Value as JsonValue};

use super::{
    ConnectorDefinition, ConnectorErrorClass, ConnectorFailure, ConnectorModule, ConnectorSuccess,
    ExecutionContext,
};

pub const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;

/// The deploy-time network policy.  `public_only` is the safe default;
/// private access needs an explicit configuration choice and is used by local
/// test servers only through the same capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    PublicOnly,
    PrivateAllowed,
}

impl NetworkPolicy {
    fn from_metadata(value: Option<&str>) -> Result<Self, HttpConfigError> {
        match value.unwrap_or("public_only") {
            "public_only" => Ok(Self::PublicOnly),
            "private_allowed" => Ok(Self::PrivateAllowed),
            _ => Err(HttpConfigError::new(
                "network_policy must be public_only or private_allowed",
            )),
        }
    }
}

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
    network_policy: NetworkPolicy,
    headers: HeaderMap,
}

impl HttpConnectorConfig {
    pub fn new(
        base_url: &str,
        network_policy: NetworkPolicy,
        headers: Vec<ConfiguredHeader>,
    ) -> Result<Self, HttpConfigError> {
        let base_url = Url::parse(base_url)
            .map_err(|_| HttpConfigError::new("base_url must be an absolute HTTP(S) URL"))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(HttpConfigError::new(
                "base_url must be an absolute HTTP(S) URL without userinfo, query, or fragment",
            ));
        }
        let mut resolved_headers = HeaderMap::new();
        for header in headers {
            resolved_headers.insert(header.name, header.value);
        }
        Ok(Self {
            base_url,
            network_policy,
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
}

impl HttpOperation {
    pub fn builder(name: &str, method: HttpMethod, path_template: &str) -> HttpOperationBuilder {
        HttpOperationBuilder {
            name: name.to_owned(),
            method,
            path_template: path_template.to_owned(),
            query_inputs: Vec::new(),
            headers: Vec::new(),
            body: None,
            success_statuses: BTreeSet::new(),
            response_pointers: Vec::new(),
            declared_5xx: BTreeSet::new(),
        }
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
        let headers = self
            .headers
            .into_iter()
            .map(|(name, value)| {
                Ok(StaticHeader {
                    name: HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| HttpConfigError::new("operation header name is invalid"))?,
                    value: HeaderValue::from_str(&value)
                        .map_err(|_| HttpConfigError::new("operation header value is invalid"))?,
                })
            })
            .collect::<Result<Vec<_>, HttpConfigError>>()?;
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
        })
    }
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
    headers: HeaderMap,
    body: Vec<u8>,
}

impl RawHttpResponse {
    pub fn json(status: StatusCode, value: JsonValue) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: serde_json::to_vec(&value).expect("JSON test response serializes"),
        }
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

pub struct ReqwestTransport;

impl ReqwestTransport {
    pub fn new() -> Self {
        Self
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
                return Err(TransportError::new("deadline elapsed"));
            }
            let addresses = destination
                .into_iter()
                .map(|address| SocketAddr::new(address, port))
                .collect::<Vec<_>>();
            let client = Client::builder()
                // Redirects would require a fresh static-destination policy
                // check and could become an SSRF bypass, so never follow them.
                .redirect(reqwest::redirect::Policy::none())
                .timeout(remaining)
                // Pin reqwest's lookup to the second, immediately-before-
                // connect resolver result that this module just vetted.
                .resolve_to_addrs(host, &addresses)
                .build()
                .map_err(|_| TransportError::new("client construction failed"))?;
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
        let config = HttpConnectorConfig::new(
            &base_url,
            NetworkPolicy::from_metadata(config.network_policy.as_deref())?,
            headers,
        )?;
        Ok(Self::with_components(
            config,
            Arc::new(SystemResolver),
            Arc::new(ReqwestTransport::new()),
        ))
    }

    pub async fn execute(
        &self,
        operation: &HttpOperation,
        input: JsonValue,
        context: ExecutionContext,
    ) -> Result<ConnectorSuccess, ConnectorFailure> {
        if context.deadline <= tokio::time::Instant::now() {
            return Err(timeout_failure());
        }
        let request = self.prepare_request(operation, &input)?;
        if request.body.len() > MAX_HTTP_BODY_BYTES {
            return Err(invariant_failure(
                "connector request exceeds the 1 MiB limit",
            ));
        }

        // Check resolution at request start and again directly before handing
        // the pinned result to reqwest.  A DNS rebinding between the two is
        // rejected; a new per-request reqwest client avoids connection reuse.
        self.resolve_under_deadline(context.deadline).await?;
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
        if self.config.network_policy == NetworkPolicy::PublicOnly
            && addresses.iter().any(|address| !is_public_address(*address))
        {
            return Err(invariant_failure(
                "connector network policy rejected a non-public destination",
            ));
        }
        Ok(addresses)
    }

    fn prepare_request(
        &self,
        operation: &HttpOperation,
        input: &JsonValue,
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
            headers.insert(header.name.clone(), header.value.clone());
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
            return Ok(ConnectorSuccess { output: value });
        }
        let mut output = JsonMap::new();
        for field in &operation.response_pointers {
            match value.pointer(&field.pointer) {
                Some(value) => {
                    output.insert(field.output_name.clone(), value.clone());
                }
                None if field.required => {
                    return Err(validation_failure(
                        "connector provider response did not satisfy the declared contract",
                    ));
                }
                None => {}
            }
        }
        Ok(ConnectorSuccess {
            output: JsonValue::Object(output),
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

fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, fourth] = address.octets();
            !(first == 0
                || first == 10
                || first == 100 && (64..=127).contains(&second)
                || first == 127
                || first == 169 && second == 254
                || first == 172 && (16..=31).contains(&second)
                || first == 192 && second == 0 && third == 0
                || first == 192 && second == 0 && third == 2
                || first == 192 && second == 88 && third == 99
                || first == 192 && second == 168
                || first == 198 && (second == 18 || second == 19)
                || first == 198 && second == 51 && third == 100
                || first == 203 && second == 0 && third == 113
                || first >= 224
                || fourth == 255 && first == 255 && second == 255 && third == 255)
        }
        IpAddr::V6(address) => {
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_public_address(IpAddr::V4(mapped));
            }
            let segments = address.segments();
            !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !address.is_unicast_link_local()
                && !(segments[0] & 0xfe00 == 0xfc00) // unique-local fc00::/7
                && !(segments[0] & 0xffc0 == 0xfec0) // deprecated site-local fec0::/10
                && !segments[..6].iter().all(|segment| *segment == 0) // IPv4-compatible ::/96
                && !(segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
                && !(segments[0] == 0x2001 && segments[1] == 0x0002) // benchmarking
                && !(segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0) // discard-only 100::/64
        }
    }
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

// `reqwest` 0.12 exposes response metadata but not the connected peer socket
// through its high-level API.  We therefore validate DNS twice and pin the
// second result with `resolve_to_addrs`, create a fresh client per request, and
// disable redirects.  A future transport that exposes the peer must add a
// post-connect check before making the request body observable to a provider.
