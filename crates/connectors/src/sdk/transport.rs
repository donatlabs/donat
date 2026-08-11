//! Bounded outbound transport.
//!
//! A transport receives an already-prepared request and a destination list its
//! caller resolved and vetted. It cannot be used as a caller-facing arbitrary
//! request API: `PreparedHttpRequest` has private fields, redirects are never
//! followed, proxies are cleared, and transport-level retries are disabled so
//! that a duplicate provider side effect can never happen below the engine's
//! own retry routing.

use std::net::{IpAddr, SocketAddr};

use futures_util::{StreamExt, future::BoxFuture};
use reqwest::{Client, Method, StatusCode, Url, header::HeaderMap};
use serde_json::Value as JsonValue;

/// The request, response, and raw webhook body ceiling shared by every
/// connector.
pub const MAX_HTTP_BODY_BYTES: usize = 1024 * 1024;

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
    /// The most of the response body this request will read before the
    /// connection is dropped.
    ///
    /// It defaults to [`MAX_HTTP_BODY_BYTES`], which is every provider request.
    /// A caller narrows it where the answer's shape is known and small and the
    /// read is expensive to leave open — the OAuth2 token exchange, whose body
    /// is a few hundred bytes and whose wait is spent inside one activity's
    /// deadline
    /// ([[061-a-locked-row-is-held-for-a-bounded-exchange-and-a-grant-may-not-narrow-under-it]]).
    response_max_bytes: usize,
}

impl PreparedHttpRequest {
    /// Built only by connector code that has already applied its compiled
    /// origin, path template, and static header set.
    pub fn new(method: Method, url: Url, headers: HeaderMap, body: Vec<u8>) -> Self {
        Self {
            method,
            url,
            headers,
            body,
            response_max_bytes: MAX_HTTP_BODY_BYTES,
        }
    }

    /// Read at most `ceiling` bytes of this request's response.
    ///
    /// The ceiling may only be narrowed: a caller cannot use this to read more
    /// than the shared [`MAX_HTTP_BODY_BYTES`] a connector response is bounded
    /// by.
    #[must_use]
    pub fn with_response_ceiling(mut self, ceiling: usize) -> Self {
        self.response_max_bytes = ceiling.min(MAX_HTTP_BODY_BYTES);
        self
    }

    /// The caller checks this against its own request ceiling before sending.
    pub fn body_len(&self) -> usize {
        self.body.len()
    }
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

    /// Set a response header on an injected response.  A pagination or error
    /// fixture needs one — a continuation and a correlation ID both arrive as
    /// headers — and an injected transport is how a test supplies a provider
    /// response without a socket.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .expect("an injected header name is valid");
        let value = reqwest::header::HeaderValue::from_str(value)
            .expect("an injected header value is valid");
        self.headers.insert(name, value);
        self
    }

    /// Set the connected peer for an injected transport.  The HTTP connector
    /// validates this against its second vetted DNS result before accepting a
    /// public-only response.
    pub fn with_peer(mut self, peer: SocketAddr) -> Self {
        self.peer = Some(peer);
        self
    }

    /// The address the transport actually connected to, when the transport
    /// records one.  The connector compares it against its vetted destination.
    pub fn peer(&self) -> Option<SocketAddr> {
        self.peer
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// The closed set of transport outcomes a connector maps into its own error
/// classes.  The transport never chooses a class itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportErrorKind {
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

    pub fn kind(&self) -> TransportErrorKind {
        self.kind
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
            let response_max_bytes = request.response_max_bytes;
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
                if body.len().saturating_add(chunk.len()) > response_max_bytes {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::Router;
    use axum::routing::any;

    use super::*;

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
        PreparedHttpRequest::new(
            Method::POST,
            Url::parse(&format!("{base_url}/operation")).expect("static test URL is valid"),
            HeaderMap::new(),
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn elapsed_deadline_before_client_construction_is_a_timeout() {
        let request = PreparedHttpRequest::new(
            Method::POST,
            Url::parse("https://provider.example.test/operation")
                .expect("static test URL is valid"),
            HeaderMap::new(),
            Vec::new(),
        );

        let error = ReqwestTransport::new()
            .execute(
                request,
                &[IpAddr::V4(std::net::Ipv4Addr::new(8, 8, 8, 8))],
                tokio::time::Instant::now() - Duration::from_millis(1),
            )
            .await
            .expect_err("a deadline that elapses before client setup is typed as timeout");

        assert_eq!(error.kind(), TransportErrorKind::Timeout);
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
        let request = PreparedHttpRequest::new(
            Method::GET,
            Url::parse(&format!("http://{address}/nack")).expect("test URL is valid"),
            HeaderMap::new(),
            Vec::new(),
        );

        let error = ReqwestTransport::new()
            .execute(
                request,
                &[IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("a protocol nack is surfaced to engine-owned retry routing");
        listener_task.await.expect("nack listener finishes");

        assert_eq!(error.kind(), TransportErrorKind::Transport);
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
            response.peer().map(|peer| peer.ip()),
            Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            "the production reqwest path records the actual connected peer"
        );
        assert_eq!(target_hits.load(Ordering::Relaxed), 1);
        assert_eq!(proxy_hits.load(Ordering::Relaxed), 0);
    }

    /// A narrowed response ceiling is enforced chunk by chunk, and it can only
    /// ever narrow: a caller cannot use it to read past the shared ceiling.
    #[tokio::test]
    async fn a_narrowed_response_ceiling_stops_the_read() {
        let target = LocalServer::start(
            Router::new().fallback(any(|| async { (StatusCode::OK, "x".repeat(4096)) })),
        )
        .await;

        let error = ReqwestTransport::new()
            .execute(
                request_to(&target.base_url).with_response_ceiling(64),
                &[IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
                tokio::time::Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect_err("a body past the narrowed ceiling is refused");
        assert_eq!(error.kind(), TransportErrorKind::ResponseTooLarge);

        let response = ReqwestTransport::new()
            .execute(
                request_to(&target.base_url).with_response_ceiling(MAX_HTTP_BODY_BYTES * 4),
                &[IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)],
                tokio::time::Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect("the same body is inside the shared ceiling");
        assert_eq!(response.body().len(), 4096);
    }
}
