//! The local provider stub every connector test runs against.
//!
//! A connector test declares, in order, the exact requests it expects the
//! connector to make — method, path, query, headers, body — and the fixture to
//! answer each with. A request that differs in any of those is recorded as a
//! mismatch and reported by [`ProviderStub::assert_satisfied`], so a connector
//! whose request shape drifted fails loudly instead of passing because the
//! stub was permissive.
//!
//! Two rules the surrounding specification places on these tests are worth
//! restating here, because this module is what makes them practical: no test
//! reaches a live provider, and no test carries a real credential —
//! [`SECRET_SENTINEL`] stands in for one and doubles as the value a redaction
//! assertion looks for.
//!
//! Response fixtures may contain `{base_url}`, which is substituted with the
//! stub's own origin when the response is served. A pagination fixture needs
//! that: the continuation URL it offers has to be on an origin that does not
//! exist until the listener binds.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap as AxumHeaderMap, Method as AxumMethod, StatusCode as AxumStatus};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use serde_json::Value as JsonValue;

use crate::sdk::errors::ConnectorFailure;
use crate::sdk::operation::{Origin, RequestPlan};
use crate::sdk::transport::{HttpTransport, RawHttpResponse, ReqwestTransport, TransportErrorKind};

/// The stand-in for a credential in every connector test.  A test asserts both
/// that it reaches the wire where the auth plan puts it, and that it reaches
/// nothing else.
pub const SECRET_SENTINEL: &str = "donat-secret-sentinel-do-not-log";

/// One expected request and the fixture that answers it.
#[derive(Debug, Clone)]
pub struct Expectation {
    method: String,
    path: String,
    query: Option<String>,
    headers: Vec<(String, String)>,
    absent_headers: Vec<String>,
    body: Option<ExpectedBody>,
    delay: Duration,
    status: u16,
    response_headers: Vec<(String, String)>,
    response_body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpectedBody {
    Json(JsonValue),
    Empty,
}

impl Expectation {
    /// An expected request.  Method and path are always asserted; every other
    /// facet is asserted once the test declares it.
    pub fn new(method: &str, path: &str) -> Self {
        Self {
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
            query: None,
            headers: Vec::new(),
            absent_headers: Vec::new(),
            body: None,
            delay: Duration::ZERO,
            status: 200,
            response_headers: Vec::new(),
            response_body: b"{}".to_vec(),
        }
    }

    /// The exact query string, already percent-encoded as the connector sends
    /// it.  `None` in the declaration means "not asserted"; an expected empty
    /// string asserts there is no query.
    #[must_use]
    pub fn query(mut self, query: &str) -> Self {
        self.query = Some(query.to_owned());
        self
    }

    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers
            .push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    /// Assert a header is absent — how a test proves a credential did not
    /// travel somewhere it should not.
    #[must_use]
    pub fn without_header(mut self, name: &str) -> Self {
        self.absent_headers.push(name.to_ascii_lowercase());
        self
    }

    #[must_use]
    pub fn json_body(mut self, body: JsonValue) -> Self {
        self.body = Some(ExpectedBody::Json(body));
        self
    }

    #[must_use]
    pub fn no_body(mut self) -> Self {
        self.body = Some(ExpectedBody::Empty);
        self
    }

    /// Hold the answer back, so a test can prove a deadline is enforced.
    #[must_use]
    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    #[must_use]
    pub fn respond_json(mut self, status: u16, body: JsonValue) -> Self {
        self.status = status;
        self.response_body = serde_json::to_vec(&body).expect("a fixture body serializes");
        self
    }

    /// A non-JSON fixture: a malformed body, an oversized body, or an error
    /// page a provider actually returns.
    #[must_use]
    pub fn respond_bytes(mut self, status: u16, body: impl Into<Vec<u8>>) -> Self {
        self.status = status;
        self.response_body = body.into();
        self
    }

    #[must_use]
    pub fn respond_header(mut self, name: &str, value: &str) -> Self {
        self.response_headers
            .push((name.to_owned(), value.to_owned()));
        self
    }
}

#[derive(Debug, Default)]
struct StubState {
    expectations: Vec<Expectation>,
    received: usize,
    recorded: Vec<RecordedRequest>,
    mismatches: Vec<String>,
    base_url: String,
}

/// One request the stub actually received.
///
/// [`Expectation`] asserts the values a test can write down. This is for the
/// ones it cannot: a signature, a timestamp, a nonce. A test reads them back
/// and asserts about them — that two pages of a walk were signed differently,
/// and that each signature is the one its own request earns.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// The exact query string as it arrived, without the `?`.
    pub query: String,
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// One received header, by lowercase name.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(received, _)| *received == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A bound loopback HTTP server standing in for a provider.
pub struct ProviderStub {
    base_url: String,
    origin: Origin,
    state: Arc<Mutex<StubState>>,
    task: tokio::task::JoinHandle<()>,
}

impl ProviderStub {
    pub async fn start(expectations: impl IntoIterator<Item = Expectation>) -> Self {
        let state = Arc::new(Mutex::new(StubState {
            expectations: expectations.into_iter().collect(),
            ..StubState::default()
        }));
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("the provider stub binds a loopback port");
        let address = listener
            .local_addr()
            .expect("the provider stub has an address");
        let base_url = format!("http://{address}");
        state
            .lock()
            .expect("the stub state is not poisoned")
            .base_url = base_url.clone();
        let router = Router::new().fallback(any(serve)).with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("the provider stub serves");
        });
        Self {
            origin: Origin::parse(&base_url).expect("a loopback origin is valid"),
            base_url,
            state,
            task,
        }
    }

    /// The stub's compiled origin, as a connector declaration would carry it.
    pub fn origin(&self) -> Origin {
        self.origin.clone()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Send one rendered request through the real SDK transport.  Connector
    /// tests use this rather than a bare HTTP client so that request bounds,
    /// the no-redirect policy, and peer pinning are exercised too.
    pub async fn send(&self, request: RequestPlan) -> Result<RawHttpResponse, ConnectorFailure> {
        self.send_until(
            request,
            tokio::time::Instant::now() + Duration::from_secs(5),
        )
        .await
    }

    pub async fn send_until(
        &self,
        request: RequestPlan,
        deadline: tokio::time::Instant,
    ) -> Result<RawHttpResponse, ConnectorFailure> {
        let prepared = request.into_prepared()?;
        ReqwestTransport::new()
            .execute(prepared, &[IpAddr::V4(Ipv4Addr::LOCALHOST)], deadline)
            .await
            .map_err(|error| match error.kind() {
                TransportErrorKind::Timeout => ConnectorFailure::timeout(),
                TransportErrorKind::ResponseTooLarge => ConnectorFailure::validation(
                    "connector provider response exceeds the declared ceiling",
                ),
                TransportErrorKind::Transport => ConnectorFailure::transport(),
            })
    }

    /// How many requests actually reached the stub.
    ///
    /// A walk is only a walk if the provider was asked more than once, so a
    /// pagination test asserts this number rather than inferring it: an
    /// executor that quietly sent one request and returned one page would
    /// otherwise satisfy every assertion about the *content* of the aggregate.
    pub fn received(&self) -> usize {
        self.state
            .lock()
            .expect("the stub state is not poisoned")
            .received
    }

    /// Every request that arrived, in order.
    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.state
            .lock()
            .expect("the stub state is not poisoned")
            .recorded
            .clone()
    }

    /// Every difference between what was expected and what arrived, plus any
    /// expectation that was never received and any request beyond the last
    /// expectation.
    pub fn mismatches(&self) -> Vec<String> {
        let state = self.state.lock().expect("the stub state is not poisoned");
        let mut mismatches = state.mismatches.clone();
        for (index, expectation) in state.expectations.iter().enumerate().skip(state.received) {
            mismatches.push(format!(
                "expectation {index} {} {} was never received",
                expectation.method, expectation.path
            ));
        }
        mismatches
    }

    /// The assertion every connector test ends with.
    pub fn assert_satisfied(&self) {
        let mismatches = self.mismatches();
        assert!(
            mismatches.is_empty(),
            "the provider stub saw requests it did not expect:\n  {}",
            mismatches.join("\n  ")
        );
    }
}

impl Drop for ProviderStub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    State(state): State<Arc<Mutex<StubState>>>,
    method: AxumMethod,
    OriginalUri(uri): OriginalUri,
    headers: AxumHeaderMap,
    body: Bytes,
) -> Response {
    // The guard is confined to this block: an `axum` handler future has to stay
    // `Send`, and the delay below is an await point.
    let matched = {
        let mut state = state.lock().expect("the stub state is not poisoned");
        let index = state.received;
        state.received += 1;
        state.recorded.push(RecordedRequest {
            method: method.to_string(),
            path: uri.path().to_owned(),
            query: uri.query().unwrap_or_default().to_owned(),
            headers: headers
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_ascii_lowercase(),
                        String::from_utf8_lossy(value.as_bytes()).into_owned(),
                    )
                })
                .collect(),
            body: body.to_vec(),
        });
        match state.expectations.get(index).cloned() {
            None => {
                state.mismatches.push(format!(
                    "unexpected request {index}: {method} {}",
                    uri.path()
                ));
                None
            }
            Some(expectation) => {
                let mismatches = compare(index, &expectation, &method, &uri, &headers, &body);
                state.mismatches.extend(mismatches);
                Some((expectation, state.base_url.clone()))
            }
        }
    };
    let Some((expectation, base_url)) = matched else {
        return (AxumStatus::INTERNAL_SERVER_ERROR, "unexpected request").into_response();
    };

    if !expectation.delay.is_zero() {
        tokio::time::sleep(expectation.delay).await;
    }

    let mut response = Response::builder().status(
        AxumStatus::from_u16(expectation.status).expect("a fixture status is a valid HTTP status"),
    );
    for (name, value) in &expectation.response_headers {
        response = response.header(name, value.replace("{base_url}", &base_url));
    }
    let body = match String::from_utf8(expectation.response_body.clone()) {
        Ok(text) => text.replace("{base_url}", &base_url).into_bytes(),
        Err(_) => expectation.response_body.clone(),
    };
    response
        .body(axum::body::Body::from(body))
        .expect("a fixture response is well formed")
}

/// Every way the received request differs from the one that was expected.
fn compare(
    index: usize,
    expectation: &Expectation,
    method: &AxumMethod,
    uri: &axum::http::Uri,
    headers: &AxumHeaderMap,
    body: &Bytes,
) -> Vec<String> {
    let mut mismatches = Vec::new();
    if method.as_str() != expectation.method {
        mismatches.push(format!(
            "request {index} method: expected {}, received {method}",
            expectation.method
        ));
    }
    if uri.path() != expectation.path {
        mismatches.push(format!(
            "request {index} path: expected {}, received {}",
            expectation.path,
            uri.path()
        ));
    }
    if let Some(expected) = &expectation.query {
        let received = uri.query().unwrap_or_default();
        if received != expected {
            mismatches.push(format!(
                "request {index} query: expected {expected}, received {received}"
            ));
        }
    }
    for (name, expected) in &expectation.headers {
        let received = headers.get(name).and_then(|value| value.to_str().ok());
        if received != Some(expected.as_str()) {
            mismatches.push(format!(
                "request {index} header {name}: expected {expected}, received {}",
                received.unwrap_or("<absent>")
            ));
        }
    }
    for name in &expectation.absent_headers {
        if headers.contains_key(name) {
            mismatches.push(format!("request {index} header {name}: expected absent"));
        }
    }
    match &expectation.body {
        Some(ExpectedBody::Empty) if !body.is_empty() => {
            mismatches.push(format!("request {index} body: expected no body"));
        }
        Some(ExpectedBody::Json(expected)) => match serde_json::from_slice::<JsonValue>(body) {
            Ok(received) if &received == expected => {}
            Ok(received) => mismatches.push(format!(
                "request {index} body: expected {expected}, received {received}"
            )),
            Err(_) => mismatches.push(format!("request {index} body: expected JSON")),
        },
        Some(ExpectedBody::Empty) | None => {}
    }
    mismatches
}

#[cfg(test)]
mod tests {
    use donat_value_contract::ValueScalar;
    use reqwest::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::sdk::auth::{AuthPlan, Credential};
    use crate::sdk::operation::{JsonTemplate, Operation, Required};

    #[tokio::test]
    async fn the_provider_stub_asserts_the_exact_request_it_received() {
        let stub = ProviderStub::start([Expectation::new("POST", "/v1/items")
            .query("api-version=2026%2D01%2D01&state=open")
            .header("x-api-key", SECRET_SENTINEL)
            .header("content-type", "application/json")
            .json_body(json!({ "name": "widget" }))
            .respond_json(201, json!({ "id": "item_1" }))])
        .await;

        let operation = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .query_static("api-version", "2026-01-01")
            .query_input("state", "state")
            .body(JsonTemplate::object([(
                "name",
                JsonTemplate::input("name"),
            )]))
            .success_statuses([StatusCode::CREATED])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .build()
            .expect("static declaration is valid");
        let mut request = operation
            .plan_request(
                &stub.origin(),
                &json!({ "state": "open", "name": "widget" }),
            )
            .expect("request renders");
        AuthPlan::api_key_header("X-Api-Key")
            .expect("static header name")
            .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
            .expect("the api key applies");

        let response = stub.send(request).await.expect("the stub answers");
        assert_eq!(response.status, StatusCode::CREATED);
        assert_eq!(
            operation
                .decode_response(response.status.as_u16(), response.body())
                .expect("the declared response is satisfied"),
            json!({ "id": "item_1" })
        );
        stub.assert_satisfied();
    }

    #[tokio::test]
    async fn the_provider_stub_reports_a_mismatch_instead_of_passing_silently() {
        let stub = ProviderStub::start([Expectation::new("POST", "/v1/items")
            .header("x-api-key", SECRET_SENTINEL)
            .json_body(json!({ "name": "widget" }))])
        .await;

        let operation = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .body(JsonTemplate::object([(
                "name",
                JsonTemplate::literal(json!("something-else")),
            )]))
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");
        let request = operation
            .plan_request(&stub.origin(), &json!({}))
            .expect("request renders");
        stub.send(request).await.expect("the stub answers");

        let mismatches = stub.mismatches();
        assert_eq!(mismatches.len(), 2, "{mismatches:?}");
        assert!(mismatches.iter().any(|entry| entry.contains("header")));
        assert!(mismatches.iter().any(|entry| entry.contains("body")));
    }

    #[tokio::test]
    async fn an_unconsumed_or_surplus_expectation_is_a_mismatch() {
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/items")]).await;
        assert_eq!(
            stub.mismatches(),
            vec!["expectation 0 GET /v1/items was never received".to_owned()]
        );

        let operation = Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");
        for _ in 0..2 {
            let request = operation
                .plan_request(&stub.origin(), &json!({}))
                .expect("request renders");
            stub.send(request).await.expect("the stub answers");
        }
        let mismatches = stub.mismatches();
        assert_eq!(mismatches.len(), 1, "{mismatches:?}");
        assert!(mismatches[0].contains("unexpected request 1"));
    }

    #[tokio::test]
    async fn a_response_fixture_may_name_the_stub_origin_it_does_not_know_yet() {
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/items")
            .respond_header("link", "<{base_url}/v1/items?page=2>; rel=\"next\"")
            .respond_json(200, json!({ "self": "{base_url}/v1/items" }))])
        .await;

        let operation = Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");
        let response = stub
            .send(
                operation
                    .plan_request(&stub.origin(), &json!({}))
                    .expect("request renders"),
            )
            .await
            .expect("the stub answers");

        let base_url = stub.base_url().to_owned();
        assert_eq!(
            response
                .headers()
                .get("link")
                .and_then(|value| value.to_str().ok()),
            Some(format!("<{base_url}/v1/items?page=2>; rel=\"next\"").as_str())
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(response.body()).expect("body is JSON"),
            json!({ "self": format!("{base_url}/v1/items") })
        );
        stub.assert_satisfied();
    }
}
