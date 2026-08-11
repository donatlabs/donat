//! The closed connector error classes and the provider-safe failure they carry.
//!
//! Two properties are structural rather than reviewed:
//!
//! * `safe_message` and `code` are `&'static str`, so no provider response text
//!   can reach either of them — a borrowed provider string does not typecheck.
//! * `retry_after` is clamped to [`MAX_RETRY_AFTER_SECONDS`] at construction,
//!   so a provider cannot park a durable activity for an arbitrary time.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, RETRY_AFTER};
use serde_json::Value as JsonValue;

use crate::sdk::operation::{OperationError, validate_json_pointer};
use crate::sdk::transport::RawHttpResponse;

/// The `Retry-After` ceiling, matching `donat_connector_abi`'s
/// `MAXIMUM_RETRY_AFTER_SECONDS`.
pub const MAX_RETRY_AFTER_SECONDS: u64 = 86_400;

/// The longest correlation identifier the SDK will retain from a provider
/// header.  A correlation ID is a support handle, not a payload.
pub const MAX_CORRELATION_ID_BYTES: usize = 128;

/// Every activity execution failure belongs to this closed set.  It is closed
/// on purpose: a Process declares `retry_on` against these names, so a ninth
/// class would be a class no deployed Process can route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConnectorErrorClass {
    Transport,
    Timeout,
    Http429,
    Http5xx,
    Authentication,
    Validation,
    Permanent,
    Invariant,
}

impl ConnectorErrorClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Timeout => "timeout",
            Self::Http429 => "http_429",
            Self::Http5xx => "http_5xx",
            Self::Authentication => "authentication",
            Self::Validation => "validation",
            Self::Permanent => "permanent",
            Self::Invariant => "invariant",
        }
    }
}

impl fmt::Display for ConnectorErrorClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A provider-safe, typed activity execution failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorFailure {
    class: ConnectorErrorClass,
    code: &'static str,
    safe_message: &'static str,
    retry_after: Option<Duration>,
    provider_status: Option<u16>,
    correlation_ids: BTreeMap<&'static str, String>,
}

impl ConnectorFailure {
    pub const fn new(
        class: ConnectorErrorClass,
        code: &'static str,
        safe_message: &'static str,
    ) -> Self {
        Self {
            class,
            code,
            safe_message,
            retry_after: None,
            provider_status: None,
            correlation_ids: BTreeMap::new(),
        }
    }

    /// A provider `Retry-After` never exceeds [`MAX_RETRY_AFTER_SECONDS`].
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after =
            retry_after.map(|value| value.min(Duration::from_secs(MAX_RETRY_AFTER_SECONDS)));
        self
    }

    #[must_use]
    pub fn with_provider_status(mut self, status: u16) -> Self {
        self.provider_status = Some(status);
        self
    }

    /// Correlation identifiers a connector declared as safe to retain.  Values
    /// are truncated and stripped of anything but printable ASCII, because a
    /// diagnostic is written to an operator log.
    #[must_use]
    pub fn with_correlation_ids(
        mut self,
        ids: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        self.correlation_ids = ids
            .into_iter()
            .map(|(name, value)| {
                let value = value
                    .chars()
                    .filter(|character| character.is_ascii_graphic())
                    .take(MAX_CORRELATION_ID_BYTES)
                    .collect();
                (name, value)
            })
            .collect();
        self
    }

    pub const fn class(&self) -> ConnectorErrorClass {
        self.class
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub const fn safe_message(&self) -> &'static str {
        self.safe_message
    }

    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    pub const fn provider_status(&self) -> Option<u16> {
        self.provider_status
    }

    pub fn correlation_ids(&self) -> &BTreeMap<&'static str, String> {
        &self.correlation_ids
    }

    /// The redacted operator diagnostic: class, provider status, retry-after,
    /// and the safe correlation IDs.  It is built from typed fields only, so
    /// there is no path by which provider text joins it.
    pub fn diagnostic(&self) -> String {
        let mut diagnostic = format!("class={} code={}", self.class, self.code);
        if let Some(status) = self.provider_status {
            diagnostic.push_str(&format!(" provider_status={status}"));
        }
        if let Some(retry_after) = self.retry_after {
            diagnostic.push_str(&format!(" retry_after_s={}", retry_after.as_secs()));
        }
        for (name, value) in &self.correlation_ids {
            diagnostic.push_str(&format!(" {name}={value}"));
        }
        diagnostic
    }

    pub const fn transport() -> Self {
        Self::new(
            ConnectorErrorClass::Transport,
            "connector_transport",
            "connector transport failed",
        )
    }

    pub const fn timeout() -> Self {
        Self::new(
            ConnectorErrorClass::Timeout,
            "connector_timeout",
            "connector activity deadline elapsed",
        )
    }

    pub const fn validation(safe_message: &'static str) -> Self {
        Self::new(
            ConnectorErrorClass::Validation,
            "connector_validation",
            safe_message,
        )
    }

    pub const fn invariant(safe_message: &'static str) -> Self {
        Self::new(
            ConnectorErrorClass::Invariant,
            "connector_invariant",
            safe_message,
        )
    }
}

/// One ordered rule in a connector's error map.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ErrorRule {
    Status {
        status: u16,
        class: ConnectorErrorClass,
    },
    /// A provider's own stable machine-readable code, read from the declared
    /// code pointer.  Provider message *text* is never matched on: it is not a
    /// contract, it is prose that changes.
    Code {
        code: String,
        class: ConnectorErrorClass,
    },
    StatusAndCode {
        status: u16,
        code: String,
        class: ConnectorErrorClass,
    },
}

impl ErrorRule {
    fn class(&self) -> ConnectorErrorClass {
        match self {
            Self::Status { class, .. }
            | Self::Code { class, .. }
            | Self::StatusAndCode { class, .. } => *class,
        }
    }

    fn matches(&self, status: u16, code: Option<&str>) -> bool {
        match self {
            Self::Status {
                status: declared, ..
            } => *declared == status,
            Self::Code { code: declared, .. } => code == Some(declared.as_str()),
            Self::StatusAndCode {
                status: declared_status,
                code: declared_code,
                ..
            } => *declared_status == status && code == Some(declared_code.as_str()),
        }
    }
}

/// A connector's ordered map from a provider response to one of the eight
/// closed classes.
///
/// The map is total: the first matching rule wins, and everything unmatched
/// takes the declared fallback, so there is no response a connector answers
/// with "unclassified". What the map never does is carry the provider's own
/// words across the boundary — the class selects a Donat-owned message, and the
/// only provider material retained is the status, the `Retry-After` seconds,
/// and the correlation IDs the connector declared as safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorMap {
    code_pointer: Option<String>,
    rules: Vec<ErrorRule>,
    fallback: ConnectorErrorClass,
    correlation_headers: Vec<(&'static str, HeaderName)>,
}

impl ErrorMap {
    /// Every map declares the class an unmatched response takes.
    pub fn builder(fallback: ConnectorErrorClass) -> ErrorMapBuilder {
        ErrorMapBuilder {
            code_pointer: None,
            rules: Vec::new(),
            fallback,
            correlation_headers: Vec::new(),
            error: None,
        }
    }

    pub fn classify_response(&self, response: &RawHttpResponse) -> ConnectorFailure {
        self.classify(
            response.status.as_u16(),
            response.headers(),
            response.body(),
        )
    }

    pub fn classify(&self, status: u16, headers: &HeaderMap, body: &[u8]) -> ConnectorFailure {
        let code = self.provider_code(body);
        let class = self
            .rules
            .iter()
            .find(|rule| rule.matches(status, code.as_deref()))
            .map_or(self.fallback, ErrorRule::class);
        failure_for(class)
            .with_provider_status(status)
            .with_retry_after(retry_after(headers))
            .with_correlation_ids(self.correlation_ids(headers))
    }

    /// The provider's machine-readable code, when the connector declared where
    /// it lives and the body is JSON that has one there.
    fn provider_code(&self, body: &[u8]) -> Option<String> {
        let pointer = self.code_pointer.as_ref()?;
        let value: JsonValue = serde_json::from_slice(body).ok()?;
        match value.pointer(pointer)? {
            JsonValue::String(code) => Some(code.clone()),
            JsonValue::Number(code) => Some(code.to_string()),
            _ => None,
        }
    }

    fn correlation_ids(&self, headers: &HeaderMap) -> Vec<(&'static str, String)> {
        self.correlation_headers
            .iter()
            .filter_map(|(name, header)| {
                headers
                    .get(header)
                    .and_then(|value| value.to_str().ok())
                    .map(|value| (*name, value.to_owned()))
            })
            .collect()
    }
}

pub struct ErrorMapBuilder {
    code_pointer: Option<String>,
    rules: Vec<ErrorRule>,
    fallback: ConnectorErrorClass,
    correlation_headers: Vec<(&'static str, HeaderName)>,
    error: Option<OperationError>,
}

impl ErrorMapBuilder {
    /// Where the provider publishes its stable machine-readable code.
    #[must_use]
    pub fn code_pointer(mut self, pointer: &str) -> Self {
        if let Err(error) = validate_json_pointer(pointer) {
            self.error.get_or_insert(error);
        }
        self.code_pointer = Some(pointer.to_owned());
        self
    }

    #[must_use]
    pub fn on_status(mut self, status: u16, class: ConnectorErrorClass) -> Self {
        self.rules.push(ErrorRule::Status { status, class });
        self
    }

    #[must_use]
    pub fn on_statuses(
        mut self,
        statuses: impl IntoIterator<Item = u16>,
        class: ConnectorErrorClass,
    ) -> Self {
        for status in statuses {
            self.rules.push(ErrorRule::Status { status, class });
        }
        self
    }

    #[must_use]
    pub fn on_code(mut self, code: &str, class: ConnectorErrorClass) -> Self {
        self.rules.push(ErrorRule::Code {
            code: code.to_owned(),
            class,
        });
        self
    }

    #[must_use]
    pub fn on_status_and_code(
        mut self,
        status: u16,
        code: &str,
        class: ConnectorErrorClass,
    ) -> Self {
        self.rules.push(ErrorRule::StatusAndCode {
            status,
            code: code.to_owned(),
            class,
        });
        self
    }

    /// A response header safe to retain as a support handle, under the name the
    /// diagnostic will use for it.
    #[must_use]
    pub fn correlation_header(mut self, name: &'static str, header: &str) -> Self {
        match HeaderName::from_bytes(header.as_bytes()) {
            Ok(header) => self.correlation_headers.push((name, header)),
            Err(_) => {
                self.error.get_or_insert(OperationError::new(
                    "a correlation header name must be static and valid",
                ));
            }
        }
        self
    }

    pub fn build(self) -> Result<ErrorMap, OperationError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        let names_a_code = self.rules.iter().any(|rule| {
            matches!(
                rule,
                ErrorRule::Code { .. } | ErrorRule::StatusAndCode { .. }
            )
        });
        if names_a_code && self.code_pointer.is_none() {
            return Err(OperationError::new(
                "a provider code rule requires a declared code pointer",
            ));
        }
        if self.rules.iter().any(|rule| match rule {
            ErrorRule::Status { status, .. } | ErrorRule::StatusAndCode { status, .. } => {
                (200..=299).contains(status)
            }
            ErrorRule::Code { .. } => false,
        }) {
            return Err(OperationError::new(
                "a success status is classified by the operation, not by the error map",
            ));
        }
        Ok(ErrorMap {
            code_pointer: self.code_pointer,
            rules: self.rules,
            fallback: self.fallback,
            correlation_headers: self.correlation_headers,
        })
    }
}

/// The Donat-owned failure each class carries.  This is the only place a
/// message text is chosen, and every one of them is a `&'static str` written
/// here rather than anything a provider sent.
fn failure_for(class: ConnectorErrorClass) -> ConnectorFailure {
    match class {
        ConnectorErrorClass::Transport => ConnectorFailure::transport(),
        ConnectorErrorClass::Timeout => ConnectorFailure::timeout(),
        ConnectorErrorClass::Http429 => ConnectorFailure::new(
            ConnectorErrorClass::Http429,
            "connector_http_429",
            "connector provider rate limited the request",
        ),
        ConnectorErrorClass::Http5xx => ConnectorFailure::new(
            ConnectorErrorClass::Http5xx,
            "connector_declared_http_5xx",
            "connector provider returned a declared server error",
        ),
        ConnectorErrorClass::Authentication => ConnectorFailure::new(
            ConnectorErrorClass::Authentication,
            "connector_http_authentication",
            "connector provider rejected connector authentication",
        ),
        ConnectorErrorClass::Validation => {
            ConnectorFailure::validation("connector provider rejected the declared request")
        }
        ConnectorErrorClass::Permanent => ConnectorFailure::new(
            ConnectorErrorClass::Permanent,
            "connector_unsupported_http_status",
            "connector provider returned an unsupported HTTP status",
        ),
        ConnectorErrorClass::Invariant => {
            ConnectorFailure::invariant("connector provider answered outside its declared contract")
        }
    }
}

/// `Retry-After` in delta-seconds.  The HTTP-date form is deliberately not
/// honoured: it needs a trusted clock comparison against a provider clock, and
/// getting that wrong parks a durable activity.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u128>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(u128::from(MAX_RETRY_AFTER_SECONDS)) as u64))
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::sdk::operation::Operation;
    use crate::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};

    /// What a provider actually sends back: prose, an internal hostname, and —
    /// because providers do this — the credential it just rejected.
    fn leaky_body(code: &str) -> serde_json::Value {
        json!({
            "error": {
                "code": code,
                "message": format!(
                    "too many requests from tenant acme on shard db-7.internal using key {SECRET_SENTINEL}"
                ),
                "request_id": "req_01H",
            }
        })
    }

    fn map() -> ErrorMap {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/code")
            .on_code("rate_limit_exceeded", ConnectorErrorClass::Http429)
            .on_code("invalid_api_key", ConnectorErrorClass::Authentication)
            .on_status(400, ConnectorErrorClass::Validation)
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses(500..=504, ConnectorErrorClass::Http5xx)
            .on_status(408, ConnectorErrorClass::Timeout)
            .correlation_header("request_id", "x-request-id")
            .build()
            .expect("a static error map is valid")
    }

    /// `sdk_error_map_is_closed_and_redacted`: every mapped and unmapped case
    /// reaches one of the eight classes with a Donat-owned message and no
    /// provider text or secret.
    #[tokio::test]
    async fn sdk_error_map_is_closed_and_redacted() {
        let map = map();
        let operation = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");

        // Mapped by provider code, mapped by status, and unmapped.
        for (status, code, expected) in [
            // Mapped by the provider's own code, whatever the status.
            (429, "rate_limit_exceeded", ConnectorErrorClass::Http429),
            (200, "rate_limit_exceeded", ConnectorErrorClass::Http429),
            // Mapped by status, with a code no rule names.
            (400, "generic_error", ConnectorErrorClass::Validation),
            (401, "generic_error", ConnectorErrorClass::Authentication),
            (503, "generic_error", ConnectorErrorClass::Http5xx),
            // Unmapped in both dimensions: the declared fallback answers.
            (418, "generic_error", ConnectorErrorClass::Permanent),
        ] {
            let stub = ProviderStub::start([Expectation::new("GET", "/v1/items")
                .respond_header("retry-after", "120")
                .respond_header("x-request-id", "req_01H")
                .respond_json(status, leaky_body(code))])
            .await;
            let response = stub
                .send(
                    operation
                        .plan_request(&stub.origin(), &json!({}))
                        .expect("request renders"),
                )
                .await
                .expect("the stub answers");
            assert!(
                status == 200
                    || operation
                        .decode_response(response.status.as_u16(), response.body())
                        .is_err(),
                "status {status} is not a declared success"
            );

            let failure = map.classify_response(&response);
            assert_eq!(failure.class(), expected, "status {status}");
            assert_eq!(failure.provider_status(), Some(status));
            assert_eq!(failure.retry_after(), Some(Duration::from_secs(120)));
            assert_eq!(
                failure
                    .correlation_ids()
                    .get("request_id")
                    .map(String::as_str),
                Some("req_01H")
            );

            // Nothing the provider wrote reaches the failure or its diagnostic.
            let surface = format!(
                "{} {} {} {failure:?}",
                failure.code(),
                failure.safe_message(),
                failure.diagnostic()
            );
            for leaked in [
                SECRET_SENTINEL,
                "too many requests",
                "acme",
                "db-7.internal",
                "rate_limit_exceeded",
            ] {
                assert!(
                    !surface.contains(leaked),
                    "status {status} leaked {leaked} in {surface}"
                );
            }
            stub.assert_satisfied();
        }

        // Every status reaches exactly one of the eight closed classes: there
        // is no response a connector cannot classify.
        let headers = reqwest::header::HeaderMap::new();
        for status in 100_u16..=599 {
            let failure = map.classify(status, &headers, b"not json at all");
            assert!(
                [
                    ConnectorErrorClass::Transport,
                    ConnectorErrorClass::Timeout,
                    ConnectorErrorClass::Http429,
                    ConnectorErrorClass::Http5xx,
                    ConnectorErrorClass::Authentication,
                    ConnectorErrorClass::Validation,
                    ConnectorErrorClass::Permanent,
                    ConnectorErrorClass::Invariant,
                ]
                .contains(&failure.class()),
                "status {status}"
            );
            assert_eq!(failure.provider_status(), Some(status));
        }
        // A body that is not JSON simply matches no code rule.
        assert_eq!(
            map.classify(429, &headers, b"<html>gateway</html>").class(),
            ConnectorErrorClass::Permanent
        );
    }

    #[test]
    fn an_error_map_is_ordered_and_the_first_matching_rule_wins() {
        let headers = reqwest::header::HeaderMap::new();
        let body = br#"{"error":{"code":"busy"}}"#;

        let code_first = ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/code")
            .on_code("busy", ConnectorErrorClass::Http429)
            .on_status(503, ConnectorErrorClass::Http5xx)
            .build()
            .expect("a static error map is valid");
        assert_eq!(
            code_first.classify(503, &headers, body).class(),
            ConnectorErrorClass::Http429
        );

        let status_first = ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/code")
            .on_status(503, ConnectorErrorClass::Http5xx)
            .on_code("busy", ConnectorErrorClass::Http429)
            .build()
            .expect("a static error map is valid");
        assert_eq!(
            status_first.classify(503, &headers, body).class(),
            ConnectorErrorClass::Http5xx
        );

        // A status-and-code rule matches only when both agree.
        let both = ErrorMap::builder(ConnectorErrorClass::Permanent)
            .code_pointer("/error/code")
            .on_status_and_code(503, "busy", ConnectorErrorClass::Http5xx)
            .build()
            .expect("a static error map is valid");
        assert_eq!(
            both.classify(503, &headers, body).class(),
            ConnectorErrorClass::Http5xx
        );
        assert_eq!(
            both.classify(500, &headers, body).class(),
            ConnectorErrorClass::Permanent
        );
    }

    #[test]
    fn retry_after_is_read_as_seconds_and_clamped() {
        fn retry_after(value: &str) -> Option<Duration> {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::RETRY_AFTER,
                reqwest::header::HeaderValue::from_str(value).expect("a test header is valid"),
            );
            map().classify(429, &headers, b"{}").retry_after()
        }

        assert_eq!(retry_after("30"), Some(Duration::from_secs(30)));
        assert_eq!(
            retry_after("86400"),
            Some(Duration::from_secs(MAX_RETRY_AFTER_SECONDS))
        );
        assert_eq!(
            retry_after("86401"),
            Some(Duration::from_secs(MAX_RETRY_AFTER_SECONDS)),
            "one second over the ceiling is clamped, not honoured"
        );
        assert_eq!(
            retry_after("99999999999999999999"),
            Some(Duration::from_secs(MAX_RETRY_AFTER_SECONDS))
        );
        assert_eq!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
        assert_eq!(retry_after("-5"), None);
    }

    #[test]
    fn only_declared_correlation_headers_are_retained_and_they_are_bounded() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-request-id",
            reqwest::header::HeaderValue::from_str(&format!(
                "req_{}",
                "9".repeat(MAX_CORRELATION_ID_BYTES * 2)
            ))
            .expect("a test header is valid"),
        );
        headers.insert(
            "x-internal-debug",
            reqwest::header::HeaderValue::from_static("shard=db-7.internal"),
        );

        let failure = map().classify(500, &headers, b"{}");
        let ids = failure.correlation_ids();
        assert_eq!(ids.len(), 1, "an undeclared header is never retained");
        assert_eq!(ids["request_id"].len(), MAX_CORRELATION_ID_BYTES);
        assert!(!failure.diagnostic().contains("db-7.internal"));
    }

    #[test]
    fn an_error_map_declaration_is_static() {
        assert!(
            ErrorMap::builder(ConnectorErrorClass::Permanent)
                .on_code("busy", ConnectorErrorClass::Http429)
                .build()
                .is_err(),
            "a code rule without a declared code pointer cannot match anything"
        );
        assert!(
            ErrorMap::builder(ConnectorErrorClass::Permanent)
                .code_pointer("error/code")
                .build()
                .is_err()
        );
        assert!(
            ErrorMap::builder(ConnectorErrorClass::Permanent)
                .on_status(200, ConnectorErrorClass::Validation)
                .build()
                .is_err(),
            "a success status is decided by the operation, not by the error map"
        );
        assert!(
            ErrorMap::builder(ConnectorErrorClass::Permanent)
                .correlation_header("request_id", "x-request-{tenant}")
                .build()
                .is_err()
        );
    }
}
