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

/// Private network access is never a deployment capability.  This test-only
/// switch exists solely so unit tests can exercise a local Axum server through
/// the real transport.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkPolicy {
    PublicOnly,
    #[cfg(test)]
    PrivateAllowed,
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
    #[cfg(test)]
    network_policy: NetworkPolicy,
    headers: HeaderMap,
}

impl HttpConnectorConfig {
    pub fn new(base_url: &str, headers: Vec<ConfiguredHeader>) -> Result<Self, HttpConfigError> {
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
            #[cfg(test)]
            network_policy: NetworkPolicy::PublicOnly,
            headers: resolved_headers,
        })
    }

    fn permits_private_destinations(&self) -> bool {
        #[cfg(test)]
        {
            self.network_policy == NetworkPolicy::PrivateAllowed
        }
        #[cfg(not(test))]
        {
            false
        }
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
        if config
            .network_policy
            .as_deref()
            .is_some_and(|policy| policy != "public_only")
        {
            return Err(HttpConfigError::new("network_policy must be public_only"));
        }
        let config = HttpConnectorConfig::new(&base_url, headers)?;
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
        // Reject an unsafe initial lookup before rendering path, query, or body
        // templates.  This keeps untrusted input out of a request whose
        // declared destination is already disallowed.
        self.resolve_under_deadline(context.deadline).await?;
        let request = self.prepare_request(operation, &input)?;
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
        if !self.config.permits_private_destinations()
            && addresses.iter().any(|address| !is_public_address(*address))
        {
            return Err(invariant_failure(
                "connector network policy rejected a non-public destination",
            ));
        }
        Ok(addresses)
    }

    fn validate_connected_peer(
        &self,
        destination: &[IpAddr],
        peer: Option<SocketAddr>,
    ) -> Result<(), ConnectorFailure> {
        if self.config.permits_private_destinations() {
            return Ok(());
        }
        let Some(peer) = peer else {
            return Err(invariant_failure(
                "connector network policy could not verify the connected peer",
            ));
        };
        if !is_public_address(peer.ip()) || !destination.contains(&peer.ip()) {
            return Err(invariant_failure(
                "connector network policy rejected the connected peer",
            ));
        }
        Ok(())
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
        let mut config = HttpConnectorConfig::new(
            base_url,
            vec![
                ConfiguredHeader::new("Authorization", "test-credential")
                    .expect("fixed credential header is valid"),
            ],
        )
        .expect("local static base URL is valid");
        config.network_policy = NetworkPolicy::PrivateAllowed;
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
