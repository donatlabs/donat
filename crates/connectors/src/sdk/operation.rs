//! The operation declaration and its builder.
//!
//! An operation is a `&'static`-shaped declaration of one provider request. It
//! is deliberately not a request builder: a caller supplies JSON values for
//! declared, typed slots and nothing else. There is no API here that accepts a
//! method, a header name, a query key, a body key, a host, or a URL from
//! input — those are the parts an operation fixes at construction, and
//! [`OperationBuilder::build`] refuses a declaration that leaves any of them
//! open.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{
    Method, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::sdk::effect::{Effect, EffectClass, IdempotencyBinding};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};
use crate::sdk::projection::{
    HeaderProjection, InputProjection, OperationProjection, OutputProjection, QueryProjection,
    RequestBodyProjection, ValueSource,
};
use crate::sdk::transport::{MAX_HTTP_BODY_BYTES, PreparedHttpRequest};

/// The largest value a single request header may carry, matching
/// `donat_connector_abi::MAXIMUM_HEADER_VALUE_BYTES`.
pub const MAX_HEADER_VALUE_BYTES: usize = 8_192;

/// The largest assembled request header block, matching
/// `donat_connector_abi::MAXIMUM_RETAINED_HEADER_BYTES`.
pub const MAX_REQUEST_HEADER_BYTES: usize = 32_768;

/// The deadline one attempt of an operation declares unless the operation
/// declares its own.
///
/// It is the operation's *own* deadline rather than a ceiling the SDK applies:
/// the activity that calls a connector supplies the deadline the attempt
/// actually runs under, and a Process may not give an activity less time than
/// the operation declares it needs. Five seconds is one provider HTTP request
/// against a fixed origin; a provider that documents a longer one declares it
/// with [`OperationBuilder::deadline`].
pub const DEFAULT_OPERATION_DEADLINE: Duration = Duration::from_secs(5);

/// The deepest JSON document the response decoder will parse.
///
/// This is `serde_json`'s own recursion limit, which the decoder inherits: a
/// document nested deeper is a parse failure, classified `validation`. It is
/// stated here because a catalog snapshot has to publish the bound, and the
/// bound has to be one the decoder really holds.
pub const MAX_RESPONSE_JSON_DEPTH: u32 = 128;

/// Headers the SDK applies itself.  An operation that declared one of these
/// could overwrite an applied credential or desynchronise the framing, so the
/// builder refuses them.
const SDK_OWNED_HEADERS: [&str; 3] = ["authorization", "content-length", "host"];

/// Whether the SDK, rather than a declaration, owns this header name.  The
/// idempotency binding is the fourth such header, and it is owned per
/// operation rather than globally, so it is checked where the effect is.
pub(in crate::sdk) fn is_sdk_owned_header(name: &str) -> bool {
    SDK_OWNED_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

/// A declaration defect.  It is a separate type from [`ConnectorFailure`] on
/// purpose: a connector that cannot be declared never reaches activity retry
/// routing, so its errors must not be classifiable as retryable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationError {
    message: &'static str,
}

impl OperationError {
    pub(crate) const fn new(message: &'static str) -> Self {
        Self { message }
    }

    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for OperationError {}

/// The closed method set.  There is no constructor taking a caller string:
/// [`HttpMethod::parse`] exists for deploy-time metadata and admits only these
/// six spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    /// A read that returns only the response headers: Amazon's `HeadObject` is
    /// "the `HEAD` operation retrieves metadata from an object without
    /// returning the object itself".
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn parse(value: &str) -> Result<Self, OperationError> {
        match value {
            "GET" => Ok(Self::Get),
            "HEAD" => Ok(Self::Head),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "PATCH" => Ok(Self::Patch),
            "DELETE" => Ok(Self::Delete),
            _ => Err(OperationError::new(
                "method must be one of GET, HEAD, POST, PUT, PATCH, or DELETE",
            )),
        }
    }

    /// Whether this method carries a provider-side mutation.  It is what the
    /// effect gate reads: a mutating method cannot be described as a read.
    ///
    /// A `HEAD` is a read for exactly the reason a `GET` is — it retrieves and
    /// returns, and the provider's contract has it change nothing — so it needs
    /// no provider assertion to be classified `ReadOnly`.
    pub const fn mutates(self) -> bool {
        !matches!(self, Self::Get | Self::Head)
    }

    /// The wire spelling, which is also the spelling a catalog snapshot holds.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Head => "HEAD",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    pub fn as_reqwest(self) -> Method {
        match self {
            Self::Get => Method::GET,
            Self::Head => Method::HEAD,
            Self::Post => Method::POST,
            Self::Put => Method::PUT,
            Self::Patch => Method::PATCH,
            Self::Delete => Method::DELETE,
        }
    }
}

/// A connector's compiled origin: scheme, host, and port and nothing else.
///
/// Every URL the SDK produces is built from an `Origin` plus a declared path,
/// and every continuation URL a provider offers is checked against it. Input,
/// credentials, cursors, `Link` headers, and response bodies cannot reach it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    url: Url,
}

impl Origin {
    pub fn parse(value: &str) -> Result<Self, OperationError> {
        let url = Url::parse(value)
            .map_err(|_| OperationError::new("origin must be an absolute HTTP(S) URL"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(OperationError::new(
                "origin must be an absolute HTTP(S) URL",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(OperationError::new("origin must not contain userinfo"));
        }
        if url.query().is_some() || url.fragment().is_some() || url.path() != "/" {
            return Err(OperationError::new(
                "origin must not contain a path, query, or fragment",
            ));
        }
        Ok(Self { url })
    }

    /// Whether a URL — typically one a provider offered as a continuation —
    /// is on this exact origin.
    pub fn contains(&self, url: &Url) -> bool {
        url.scheme() == self.url.scheme()
            && url.host_str() == self.url.host_str()
            && url.port_or_known_default() == self.url.port_or_known_default()
            && url.username().is_empty()
            && url.password().is_none()
    }

    pub const fn as_url(&self) -> &Url {
        &self.url
    }
}

/// A JSON request template whose only dynamic leaves are declared, named input
/// slots.  Object keys are always literal, so input can add a value but never a
/// key.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    pub fn array<const N: usize>(values: [JsonTemplate; N]) -> Self {
        Self::Array(values.into_iter().collect())
    }

    fn validate(&self) -> Result<(), OperationError> {
        match self {
            Self::Literal(_) => Ok(()),
            Self::Input(name) => validate_input_name(name),
            Self::Object(fields) => fields.iter().try_for_each(|(name, value)| {
                if name.is_empty() || name.contains(['{', '}']) {
                    return Err(OperationError::new("body keys must be static"));
                }
                value.validate()
            }),
            Self::Array(values) => values.iter().try_for_each(Self::validate),
        }
    }

    /// The named input slots this template reads, in declaration order and
    /// without repeats.
    pub(in crate::sdk) fn input_names(&self) -> Vec<String> {
        fn visit(template: &JsonTemplate, names: &mut Vec<String>) {
            match template {
                JsonTemplate::Literal(_) => {}
                JsonTemplate::Input(name) => {
                    if !names.iter().any(|seen| seen == name) {
                        names.push(name.clone());
                    }
                }
                JsonTemplate::Object(fields) => {
                    for (_, value) in fields {
                        visit(value, names);
                    }
                }
                JsonTemplate::Array(values) => {
                    for value in values {
                        visit(value, names);
                    }
                }
            }
        }

        let mut names = Vec::new();
        visit(self, &mut names);
        names
    }

    fn render(&self, input: &JsonValue) -> Result<JsonValue, ConnectorFailure> {
        match self {
            Self::Literal(value) => Ok(value.clone()),
            Self::Input(name) => input.get(name).cloned().ok_or_else(|| {
                ConnectorFailure::invariant("a declared connector input value is missing")
            }),
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

/// Where an operation's request body comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BodySource {
    None,
    /// The normal case: a static template whose only dynamic leaves are
    /// declared input slots.
    Json(JsonTemplate),
    /// Bytes assembled by a named Rust processor in this workspace, in the
    /// media type the provider documents.  It exists because some providers do
    /// not take JSON — Stripe's Checkout API takes form-encoded pairs with
    /// indexed repeated keys, which no static JSON template can express.  A
    /// processor supplies bytes and nothing else: the method, origin, path,
    /// query, and every header name stay with the declaration.
    Processor {
        content_type: HeaderValue,
    },
}

/// Whether a declared output field must be present and non-null.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Required {
    Yes,
    No,
}

impl Required {
    const fn is_required(self) -> bool {
        matches!(self, Self::Yes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum QueryEntry {
    Static { key: String, value: String },
    Input { key: String, input: String },
}

/// A declared header whose value binds from a declared, typed input slot.
///
/// The name is `&'static`-shaped declaration material like every other header
/// name; only the value binds, exactly as a [`QueryEntry::Input`] value does.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HeaderEntry {
    name: HeaderName,
    input: String,
    scalar: ValueScalar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputPointer {
    name: String,
    pointer: String,
    scalar: ValueScalar,
    required: Required,
}

/// A validated static operation profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    id: String,
    version: String,
    method: HttpMethod,
    path_template: String,
    path_params: BTreeMap<String, ValueScalar>,
    /// The subset of `path_params` rendered inside an OData quoted string
    /// literal; see [`OperationBuilder::odata_literal_path_param`].
    odata_literals: BTreeSet<String>,
    query: Vec<QueryEntry>,
    headers: Vec<(HeaderName, HeaderValue)>,
    bound_headers: Vec<HeaderEntry>,
    body: BodySource,
    success_statuses: BTreeSet<u16>,
    /// The success statuses whose documented response carries no body.
    ///
    /// It is a subset of `success_statuses` rather than a flag, because a
    /// provider routinely documents both spellings for one operation: SendGrid
    /// answers a list delete with `200` and a job identifier, or with `204` and
    /// nothing at all.
    no_content_statuses: BTreeSet<u16>,
    outputs: Vec<OutputPointer>,
    /// The effect class this operation was admitted on.
    ///
    /// `None` is not a class: it is an operation that has not been classified,
    /// and an unclassified operation is never executable and can never be part
    /// of a [`crate::sdk::connector::Connector`]. Rendering a request is
    /// deliberately still possible without one, so an SDK test can exercise
    /// transport, auth, and pagination without inventing a classification for
    /// a fixture.
    effect: Option<Effect>,
    /// The declared input contract: the slots a *Process* binds, in name order.
    ///
    /// It is derived at build from the declaration's own slots, minus the ones
    /// the connector fills itself, plus the ones a module consumes without
    /// rendering them into the request. See
    /// [`OperationBuilder::declared_input`].
    contract_inputs: Vec<InputProjection>,
    /// The declared output contract: the activity's output schema
    /// (`knowledgebase/declarative-saas/decisions/029-*`), in name order.
    contract_outputs: Vec<OutputProjection>,
    /// The deadline one attempt of this operation declares.
    deadline: Duration,
}

impl Operation {
    pub fn get(id: &str, path_template: &str) -> OperationBuilder {
        OperationBuilder::new(id, HttpMethod::Get, path_template)
    }

    /// A metadata read: the same request as a `GET` with the body left off the
    /// response.
    pub fn head(id: &str, path_template: &str) -> OperationBuilder {
        OperationBuilder::new(id, HttpMethod::Head, path_template)
    }

    pub fn post(id: &str, path_template: &str) -> OperationBuilder {
        OperationBuilder::new(id, HttpMethod::Post, path_template)
    }

    pub fn put(id: &str, path_template: &str) -> OperationBuilder {
        OperationBuilder::new(id, HttpMethod::Put, path_template)
    }

    pub fn patch(id: &str, path_template: &str) -> OperationBuilder {
        OperationBuilder::new(id, HttpMethod::Patch, path_template)
    }

    pub fn delete(id: &str, path_template: &str) -> OperationBuilder {
        OperationBuilder::new(id, HttpMethod::Delete, path_template)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub const fn method(&self) -> HttpMethod {
        self.method
    }

    /// The declared effect class, or `None` for an operation that has not been
    /// classified.
    pub const fn effect(&self) -> Option<&Effect> {
        self.effect.as_ref()
    }

    pub const fn effect_class(&self) -> Option<EffectClass> {
        match &self.effect {
            Some(effect) => Some(effect.class()),
            None => None,
        }
    }

    /// Whether a Process may reference this operation.  An unclassified
    /// operation is never executable: spec 010 §7 admits classes, not silence.
    pub const fn is_executable(&self) -> bool {
        match &self.effect {
            Some(effect) => effect.is_executable(),
            None => false,
        }
    }

    /// Where a durable activity writes its stable idempotency key, when the
    /// operation's class binds one.
    pub const fn idempotency_binding(&self) -> Option<&IdempotencyBinding> {
        match &self.effect {
            Some(effect) => effect.idempotency_binding(),
            None => None,
        }
    }

    /// The deadline one attempt of this operation declares.
    pub const fn deadline(&self) -> Duration {
        self.deadline
    }

    /// The behavioural snapshot a catalog `OperationSpec` is built from.
    ///
    /// It is a *derivation* of this declaration rather than a second
    /// description of it, which is the whole point: the connector module stays
    /// the one place the provider is described, and nothing downstream has to
    /// restate a path, a status, or a contract field
    /// (`knowledgebase/declarative-saas/decisions/049-*`).
    ///
    /// The result is inert: it carries no credential, no resolved origin, no
    /// value, and no constructor back into an [`Operation`] or a
    /// [`RequestPlan`]. It cannot be used to compose a request.
    pub fn project(&self) -> OperationProjection {
        OperationProjection {
            id: self.id.clone(),
            version: self.version.clone(),
            method: self.method.as_str(),
            path_template: self.path_template.clone(),
            query: self
                .query
                .iter()
                .map(|entry| match entry {
                    QueryEntry::Static { key, value } => QueryProjection {
                        key: key.clone(),
                        value: ValueSource::Static(value.clone()),
                    },
                    QueryEntry::Input { key, input } => QueryProjection {
                        key: key.clone(),
                        value: ValueSource::Input(input.clone()),
                    },
                })
                .collect(),
            headers: self
                .headers
                .iter()
                .map(|(name, value)| HeaderProjection {
                    name: name.as_str().to_owned(),
                    value: ValueSource::Static(value.to_str().unwrap_or_default().to_owned()),
                })
                .chain(self.bound_headers.iter().map(|entry| HeaderProjection {
                    name: entry.name.as_str().to_owned(),
                    value: ValueSource::Input(entry.input.clone()),
                }))
                .collect(),
            body: match &self.body {
                BodySource::None => RequestBodyProjection::None,
                BodySource::Json(template) => RequestBodyProjection::Json {
                    inputs: template.input_names(),
                },
                BodySource::Processor { content_type } => RequestBodyProjection::Processor {
                    content_type: content_type.to_str().unwrap_or_default().to_owned(),
                },
            },
            success_statuses: self.success_statuses.iter().copied().collect(),
            inputs: self.contract_inputs.clone(),
            outputs: self.contract_outputs.clone(),
            effect_class: self.effect_class(),
            explicit_key: self
                .effect
                .as_ref()
                .and_then(Effect::explicit_key_evidence)
                .cloned(),
            deadline: self.deadline,
        }
    }

    pub fn is_success(&self, status: u16) -> bool {
        self.success_statuses.contains(&status)
    }

    /// Whether the provider documents this success status as carrying no body.
    pub fn is_no_content_success(&self, status: u16) -> bool {
        self.no_content_statuses.contains(&status)
    }

    /// Render this operation against its compiled origin and one set of input
    /// values.  The result carries no credential: an auth plan applies that.
    pub fn plan_request(
        &self,
        origin: &Origin,
        input: &JsonValue,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.render(origin, input, &HeaderMap::new(), None)
    }

    /// Render this operation for a deployment that resolved its own headers for
    /// this connector instance.
    ///
    /// Those headers are deploy-time material: their names come from deployment
    /// metadata and their values from named environment variables, so operation
    /// input still cannot choose either. A declared operation header may not
    /// collide with one of them, because a deployment's credential header must
    /// never be silently replaced by a declaration.
    pub fn plan_configured_request(
        &self,
        origin: &Origin,
        input: &JsonValue,
        configured: &HeaderMap,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.render(origin, input, configured, None)
    }

    /// Write a durable activity's stable key into the binding this operation's
    /// effect class was admitted on.
    ///
    /// It is the SDK's own header, not the declaration's — a builder that names
    /// it does not compile — so this is the only way it reaches the wire, and it
    /// is a *no-op* for every class that binds no key. An operation whose class
    /// is [`crate::sdk::EffectClass::ProviderIdempotentExplicitKey`] with a
    /// header binding therefore cannot be sent without its key, and one whose
    /// binding is a body pointer is refused here rather than silently sent
    /// without deduplication: a body binding is assembled where the body is
    /// (`aws_sqs`), and a caller that reached this method for one is describing
    /// an operation it cannot render.
    pub fn apply_idempotency_key(
        &self,
        request: &mut RequestPlan,
        key: &str,
    ) -> Result<(), ConnectorFailure> {
        let Some(binding) = self.idempotency_binding() else {
            return Ok(());
        };
        match binding {
            IdempotencyBinding::Header(name) => {
                let value = HeaderValue::from_str(key).map_err(|_| {
                    ConnectorFailure::invariant("connector activity idempotency key is invalid")
                })?;
                if value.len() > MAX_HEADER_VALUE_BYTES {
                    return Err(ConnectorFailure::invariant(
                        "connector activity idempotency key exceeds the declared header ceiling",
                    ));
                }
                request.set_header(name.clone(), value, false);
                Ok(())
            }
            IdempotencyBinding::BodyPointer(_) => Err(ConnectorFailure::invariant(
                "connector operation binds its idempotency key in the request body, which only \
                 the module that assembles the body can fill",
            )),
        }
    }

    /// Render this operation and bind the durable activity's stable key into
    /// the header its effect declared for it.
    ///
    /// The binding is part of the *evidence* an `ExplicitKey` class was admitted
    /// on, so the same declaration that publishes the header is what writes it —
    /// a runtime that rendered the request and forgot the key would be sending
    /// an operation of a class it does not have
    /// ([[034-a-declaration-the-runtime-ignores-is-a-defect]]). An operation
    /// whose class binds nothing, or binds a body pointer its own module fills,
    /// renders exactly as [`Self::plan_request`] does.
    ///
    /// The key is a Donat-owned activity identifier rather than a credential, so
    /// it is not marked sensitive: a redacted diagnostic that could not name the
    /// key would make a duplicate impossible to trace.
    pub fn plan_keyed_request(
        &self,
        origin: &Origin,
        input: &JsonValue,
        idempotency_key: &str,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let mut request = self.plan_request(origin, input)?;
        if let Some(name) = self
            .effect
            .as_ref()
            .and_then(Effect::idempotency_binding)
            .and_then(IdempotencyBinding::as_header)
        {
            let value = HeaderValue::from_str(idempotency_key).map_err(|_| {
                ConnectorFailure::invariant(
                    "a durable activity's idempotency key is not a valid header value",
                )
            })?;
            request.set_header(name.clone(), value, false);
        }
        Ok(request)
    }

    /// Render an operation whose body a processor assembled, for a provider
    /// whose wire format the JSON template cannot express.
    pub fn plan_processor_request(
        &self,
        origin: &Origin,
        input: &JsonValue,
        configured: &HeaderMap,
        body: Vec<u8>,
    ) -> Result<RequestPlan, ConnectorFailure> {
        self.render(origin, input, configured, Some(body))
    }

    fn render(
        &self,
        origin: &Origin,
        input: &JsonValue,
        configured: &HeaderMap,
        processor_body: Option<Vec<u8>>,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let mut url = origin.as_url().clone();
        url.set_path(&self.render_path(input)?);
        if self.query.is_empty() {
            url.set_query(None);
        } else {
            let query = self
                .query
                .iter()
                .map(|entry| match entry {
                    QueryEntry::Static { key, value } => Ok(format!(
                        "{key}={}",
                        utf8_percent_encode(value, NON_ALPHANUMERIC)
                    )),
                    QueryEntry::Input { key, input: name } => {
                        let value = scalar_input(input, name, ValueScalar::Json)?;
                        Ok(format!(
                            "{key}={}",
                            utf8_percent_encode(&value, NON_ALPHANUMERIC)
                        ))
                    }
                })
                .collect::<Result<Vec<_>, ConnectorFailure>>()?
                .join("&");
            url.set_query(Some(&query));
        }

        let mut headers = configured.clone();
        for (name, value) in &self.headers {
            if headers.contains_key(name) {
                return Err(ConnectorFailure::invariant(
                    "connector operation header names must not collide with configured headers",
                ));
            }
            headers.insert(name.clone(), value.clone());
        }
        for entry in &self.bound_headers {
            if headers.contains_key(&entry.name) {
                return Err(ConnectorFailure::invariant(
                    "connector operation header names must not collide with configured headers",
                ));
            }
            let value = scalar_input(input, &entry.input, entry.scalar.clone())?;
            // The declared ceiling is applied to the value, and `HeaderValue`
            // refuses the control characters a second header line would need,
            // so a bound value is one header value and only ever one.
            if value.len() > MAX_HEADER_VALUE_BYTES {
                return Err(ConnectorFailure::invariant(
                    "a connector request header exceeds the declared ceiling",
                ));
            }
            let value = HeaderValue::from_str(&value).map_err(|_| {
                ConnectorFailure::invariant(
                    "a declared connector input value is not a valid header value",
                )
            })?;
            headers.insert(entry.name.clone(), value);
        }
        let body = match (&self.body, processor_body) {
            (BodySource::None, None) => Vec::new(),
            (BodySource::Json(template), None) => {
                let rendered = template.render(input)?;
                let body = serde_json::to_vec(&rendered).map_err(|_| {
                    ConnectorFailure::invariant("connector request JSON serialization failed")
                })?;
                // A connector that declared its own media type keeps it: not
                // every provider body is `application/json`.
                if !headers.contains_key(CONTENT_TYPE) {
                    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                }
                body
            }
            (BodySource::Processor { content_type }, Some(body)) => {
                if !headers.contains_key(CONTENT_TYPE) {
                    headers.insert(CONTENT_TYPE, content_type.clone());
                }
                body
            }
            (BodySource::Processor { .. }, None) => {
                return Err(ConnectorFailure::invariant(
                    "connector operation requires a processor-assembled body",
                ));
            }
            (BodySource::None | BodySource::Json(_), Some(_)) => {
                return Err(ConnectorFailure::invariant(
                    "connector operation does not declare a processor-assembled body",
                ));
            }
        };
        Ok(RequestPlan {
            method: self.method.as_reqwest(),
            url,
            headers,
            body,
            url_carries_credential: false,
        })
    }

    fn render_path(&self, input: &JsonValue) -> Result<String, ConnectorFailure> {
        let mut rendered = String::new();
        let mut remaining = self.path_template.as_str();
        while let Some(index) = remaining.find('{') {
            rendered.push_str(&remaining[..index]);
            let after = &remaining[index + 1..];
            let end = after
                .find('}')
                .expect("a built operation has closed path bindings");
            let name = &after[..end];
            let scalar = self
                .path_params
                .get(name)
                .expect("a built operation declares every path binding");
            let value = scalar_input(input, name, scalar.clone())?;
            // A slot inside an OData quoted literal is escaped the way OData
            // escapes one — by doubling the quote — *before* it is encoded, so
            // that a receiver which decodes `%27` back to `'` still reads the
            // whole value as one literal rather than as expression syntax.
            let value = if self.odata_literals.contains(name) {
                value.replace('\'', "''")
            } else {
                value
            };
            // Encode every non-alphanumeric byte, so a value carrying `/`,
            // `..`, `%`, `?`, `#`, or NUL cannot leave its own segment or add
            // a query, fragment, or authority.
            rendered.push_str(&utf8_percent_encode(&value, NON_ALPHANUMERIC).to_string());
            remaining = &after[end + 1..];
        }
        rendered.push_str(remaining);
        Ok(rendered)
    }

    /// Decode a provider response for a declared success status.
    ///
    /// A non-success status is a failure here rather than a classification: an
    /// operation's [`crate::sdk::errors::ErrorMap`] decides which of the eight
    /// classes it belongs to, and this is the answer for a connector that
    /// declared no rule at all.
    pub fn decode_response(&self, status: u16, body: &[u8]) -> Result<JsonValue, ConnectorFailure> {
        if body.len() > MAX_HTTP_BODY_BYTES {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Validation,
                "connector_response_too_large",
                "connector provider response exceeds the declared ceiling",
            )
            .with_provider_status(status));
        }
        if !self.is_success(status) {
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Permanent,
                "connector_unsupported_http_status",
                "connector provider returned an undeclared HTTP status",
            )
            .with_provider_status(status));
        }
        // A documented empty-bodied success is a success. Nothing else changes:
        // the status still has to be one the declaration admits, and an
        // operation that did not declare this status as no-content still needs
        // a body it can read.
        if self.is_no_content_success(status) && body.iter().all(u8::is_ascii_whitespace) {
            return self.extract_output(&JsonValue::Object(JsonMap::new()));
        }
        let value: JsonValue = serde_json::from_slice(body).map_err(|_| {
            ConnectorFailure::validation("connector provider returned malformed JSON")
        })?;
        self.extract_output(&value)
    }

    /// The declared response is the activity's output schema, not a filter over
    /// whatever the provider sent (ADR declarative-saas/029).
    pub fn extract_output(&self, value: &JsonValue) -> Result<JsonValue, ConnectorFailure> {
        if self.outputs.is_empty() {
            return Ok(value.clone());
        }
        let mut output = JsonMap::new();
        for field in &self.outputs {
            let found = value.pointer(&field.pointer);
            match found {
                Some(JsonValue::Null) | None if field.required.is_required() => {
                    return Err(ConnectorFailure::validation(
                        "connector provider response did not satisfy the declared contract",
                    ));
                }
                Some(JsonValue::Null) | None => {
                    output.insert(field.name.clone(), JsonValue::Null);
                }
                Some(found) => {
                    if !scalar_admits(&field.scalar, found) {
                        return Err(ConnectorFailure::validation(
                            "connector provider response did not satisfy the declared contract",
                        ));
                    }
                    output.insert(field.name.clone(), found.clone());
                }
            }
        }
        Ok(JsonValue::Object(output))
    }
}

/// The builder.  Every method takes static declaration material; the only
/// caller-supplied thing an `Operation` ever sees is a JSON value for a
/// declared slot.
pub struct OperationBuilder {
    id: String,
    version: Option<String>,
    method: HttpMethod,
    path_template: String,
    /// `None` is a slot whose declaration does not fix a type; see
    /// [`OperationBuilder::untyped_path_param`].
    path_params: Vec<(String, Option<ValueScalar>)>,
    /// The subset of `path_params` declared as OData string literals.
    odata_literals: BTreeSet<String>,
    query: Vec<QueryEntry>,
    headers: Vec<(String, String)>,
    bound_headers: Vec<(String, String, ValueScalar)>,
    body: BuilderBody,
    success_statuses: BTreeSet<u16>,
    no_content_statuses: BTreeSet<u16>,
    outputs: Vec<OutputPointer>,
    effect: Option<Effect>,
    declared_inputs: Vec<(String, ValueScalar, Required)>,
    supplied_inputs: Vec<String>,
    declared_outputs: Vec<(String, ValueScalar, Required)>,
    deadline: Duration,
}

/// A declared body before validation.  The media type of a processor body is
/// kept as written so `build` reports an invalid one rather than panicking.
enum BuilderBody {
    None,
    Json(JsonTemplate),
    Processor(String),
}

impl OperationBuilder {
    fn new(id: &str, method: HttpMethod, path_template: &str) -> Self {
        Self {
            id: id.to_owned(),
            version: None,
            method,
            path_template: path_template.to_owned(),
            path_params: Vec::new(),
            odata_literals: BTreeSet::new(),
            query: Vec::new(),
            headers: Vec::new(),
            bound_headers: Vec::new(),
            body: BuilderBody::None,
            success_statuses: BTreeSet::new(),
            no_content_statuses: BTreeSet::new(),
            outputs: Vec::new(),
            effect: None,
            declared_inputs: Vec::new(),
            supplied_inputs: Vec::new(),
            declared_outputs: Vec::new(),
            deadline: DEFAULT_OPERATION_DEADLINE,
        }
    }

    /// Declare, or retype, one field of the input contract a Process binds.
    ///
    /// Two cases need it. The first is a slot whose declaration does not carry
    /// a type — a query key or a body leaf — where the provider documents one:
    /// without this the contract admits any scalar. The second is an input the
    /// *module* consumes without rendering it into the request at all, such as
    /// the object bytes of an S3 `PUT` or the source key a copy composes its
    /// `x-amz-copy-source` from; those are part of the operation's contract even
    /// though no template leaf reads them.
    #[must_use]
    pub fn declared_input(mut self, name: &str, scalar: ValueScalar, required: Required) -> Self {
        self.declared_inputs
            .push((name.to_owned(), scalar, required));
        self
    }

    /// Declare that the connector fills this slot itself, so no Process may
    /// bind it.
    ///
    /// Three things fill a slot this way: deploy-time configuration (an
    /// Airtable base, a Twilio Account SID), the durable activity's own stable
    /// key (a FIFO `MessageDeduplicationId`), and a value the module composes
    /// from other declared inputs (an S3 copy source). All three are refused if
    /// they arrive as input, so publishing them in the contract a Process binds
    /// would publish a field whose only possible value is a failure.
    ///
    /// Naming a slot the template does not read is a no-op rather than a
    /// defect: one declaration is built both with its configured value written
    /// in as a literal and, before any deployment exists, with the slot left
    /// open, and only the second build has the slot.
    #[must_use]
    pub fn supplied_input(mut self, name: &str) -> Self {
        self.supplied_inputs.push(name.to_owned());
        self
    }

    /// Declare one output-contract field the *module* composes rather than
    /// reads from a JSON pointer.
    ///
    /// [`Self::output_pointer`] covers the normal case, where the field is a
    /// pointer into the provider's JSON body and the SDK extracts it. This
    /// covers the rest: an `ETag` that only ever arrives as a response header,
    /// a key list lifted out of an XML document. The field is part of the
    /// activity's output schema either way
    /// (`knowledgebase/declarative-saas/decisions/029-*`), and a module that
    /// composes one still has to say so here or a Process cannot read it.
    #[must_use]
    pub fn declared_output(mut self, name: &str, scalar: ValueScalar, required: Required) -> Self {
        self.declared_outputs
            .push((name.to_owned(), scalar, required));
        self
    }

    /// Declare this operation's own deadline, when the provider documents one
    /// longer than [`DEFAULT_OPERATION_DEADLINE`].
    #[must_use]
    pub const fn deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    /// Classify this operation (spec 010 §7).  Every operation a connector
    /// publishes carries one; the class is checked against the method here, so
    /// a mutation cannot be spelled as a read.
    #[must_use]
    pub fn effect(mut self, effect: Effect) -> Self {
        self.effect = Some(effect);
        self
    }

    /// The SemVer core of this operation's contract.
    #[must_use]
    pub fn version(mut self, version: &str) -> Self {
        self.version = Some(version.to_owned());
        self
    }

    #[must_use]
    pub fn path_param(mut self, name: &str, scalar: ValueScalar) -> Self {
        self.path_params.push((name.to_owned(), Some(scalar)));
        self
    }

    /// A path slot whose declaration does not fix a scalar type: any scalar
    /// value is accepted and rendered, and a null, array, or object is still
    /// refused at render.
    ///
    /// A hand-written connector declares [`OperationBuilder::path_param`] with
    /// the type the provider documents. This exists for the deploy-time
    /// declarative `http` connector, whose v2 metadata format types the
    /// operation's *contract* but not the individual path slot, and where
    /// inventing a type would reject deployments that work today.
    #[must_use]
    pub fn untyped_path_param(mut self, name: &str) -> Self {
        self.path_params.push((name.to_owned(), None));
        self
    }

    /// A path slot that renders inside an OData quoted string literal.
    ///
    /// Some providers put an argument inside a function call in the path rather
    /// than in a query: Microsoft Graph publishes
    /// `range(address='<address>')` and `search(q='<search-text>')`, and there
    /// is no other spelling of either. Percent-encoding alone is *not* enough
    /// there, and this is the whole reason the kind exists: a receiver decodes
    /// `%27` back to `'` before it parses the expression, so a value carrying a
    /// quote would end the literal and the rest of it would be read as syntax.
    ///
    /// OData's own escape is to double the quote — `ABNF: SQUOTE-in-string =
    /// SQUOTE SQUOTE` — so that is what this does, before the same
    /// `NON_ALPHANUMERIC` encoding every other path value gets. The two are
    /// complementary: doubling keeps the value inside the literal, and encoding
    /// keeps it inside the path segment.
    #[must_use]
    pub fn odata_literal_path_param(mut self, name: &str, scalar: ValueScalar) -> Self {
        self.odata_literals.insert(name.to_owned());
        self.path_params.push((name.to_owned(), Some(scalar)));
        self
    }

    /// A static query key whose value binds from a named input slot.
    #[must_use]
    pub fn query_input(mut self, key: &str, input: &str) -> Self {
        self.query.push(QueryEntry::Input {
            key: key.to_owned(),
            input: input.to_owned(),
        });
        self
    }

    /// A static query key with a static value, such as an API version pin.
    #[must_use]
    pub fn query_static(mut self, key: &str, value: &str) -> Self {
        self.query.push(QueryEntry::Static {
            key: key.to_owned(),
            value: value.to_owned(),
        });
        self
    }

    #[must_use]
    pub fn static_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// A declared header name whose *value* binds from a declared, typed input
    /// slot.
    ///
    /// The name is declaration material exactly as [`Self::static_header`]'s
    /// is — there is no API here that takes a header name from input — and
    /// obeys the same rules: it may not be a header the SDK applies, it may not
    /// collide with another declared header or with a deployment's configured
    /// one, and the value it renders is bounded and validated as a single
    /// header value. Amazon's `CopyObject` is the case: `x-amz-copy-source`
    /// names the object being copied, and only that name can come from input.
    #[must_use]
    pub fn header_input(mut self, name: &str, input: &str, scalar: ValueScalar) -> Self {
        self.bound_headers
            .push((name.to_owned(), input.to_owned(), scalar));
        self
    }

    #[must_use]
    pub fn body(mut self, body: JsonTemplate) -> Self {
        self.body = BuilderBody::Json(body);
        self
    }

    /// Declare that this operation's body is assembled by a processor in this
    /// workspace, in the media type the provider documents.  The bytes are
    /// supplied to [`Operation::plan_processor_request`]; everything else about
    /// the request stays in the declaration.
    #[must_use]
    pub fn processor_body(mut self, content_type: &str) -> Self {
        self.body = BuilderBody::Processor(content_type.to_owned());
        self
    }

    #[must_use]
    pub fn success_statuses(mut self, statuses: impl IntoIterator<Item = StatusCode>) -> Self {
        self.success_statuses = statuses.into_iter().map(|status| status.as_u16()).collect();
        self
    }

    /// Declare the success statuses whose documented response carries no body.
    ///
    /// Each one must already be a declared success status: this narrows an
    /// admitted success, it never adds one. An empty body at such a status
    /// decodes exactly as an empty JSON object would, so an operation that
    /// declares one may not also declare a *required* output pointer — silence
    /// cannot satisfy a required field, and quietly publishing it as absent
    /// would be the null the SDK refuses everywhere else.
    #[must_use]
    pub fn no_content_statuses(mut self, statuses: impl IntoIterator<Item = StatusCode>) -> Self {
        self.no_content_statuses = statuses.into_iter().map(|status| status.as_u16()).collect();
        self
    }

    #[must_use]
    pub fn output_pointer(
        mut self,
        name: &str,
        pointer: &str,
        scalar: ValueScalar,
        required: Required,
    ) -> Self {
        self.outputs.push(OutputPointer {
            name: name.to_owned(),
            pointer: pointer.to_owned(),
            scalar,
            required,
        });
        self
    }

    pub fn build(self) -> Result<Operation, OperationError> {
        if self.id.is_empty() {
            return Err(OperationError::new("connector operation id is required"));
        }
        let version = self
            .version
            .ok_or_else(|| OperationError::new("connector operation version is required"))?;
        validate_semver_core(&version)?;

        let bound = validate_path_template(&self.path_template)?;
        let mut path_params = BTreeMap::new();
        for (name, scalar) in self.path_params {
            validate_input_name(&name)?;
            if matches!(scalar, Some(ValueScalar::Json)) {
                return Err(OperationError::new(
                    "a path parameter must be a scalar, not JSON",
                ));
            }
            // An untyped slot is stored as `Json`, which admits every scalar
            // spelling and still refuses a null, array, or object at render.
            if path_params
                .insert(name, scalar.unwrap_or(ValueScalar::Json))
                .is_some()
            {
                return Err(OperationError::new(
                    "a path parameter is declared more than once",
                ));
            }
        }
        if bound.iter().any(|name| !path_params.contains_key(name)) {
            return Err(OperationError::new(
                "every path binding must declare a typed path parameter",
            ));
        }
        if self
            .odata_literals
            .iter()
            .any(|name| !path_params.contains_key(name))
        {
            return Err(OperationError::new(
                "an OData literal slot must be a declared path parameter",
            ));
        }
        if path_params.keys().any(|name| !bound.contains(name)) {
            return Err(OperationError::new(
                "a declared path parameter must appear in the path",
            ));
        }

        let mut query_keys = BTreeSet::new();
        for entry in &self.query {
            let key = match entry {
                QueryEntry::Static { key, .. } => key,
                QueryEntry::Input { key, input } => {
                    validate_input_name(input)?;
                    key
                }
            };
            validate_query_key(key)?;
            if !query_keys.insert(key.clone()) {
                return Err(OperationError::new(
                    "a query key is declared more than once",
                ));
            }
        }

        let mut header_names = HashSet::new();
        let mut headers = Vec::new();
        for (name, value) in self.headers {
            let lowercase = name.to_ascii_lowercase();
            if SDK_OWNED_HEADERS.contains(&lowercase.as_str()) {
                return Err(OperationError::new(
                    "an operation must not declare a header the SDK applies",
                ));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| OperationError::new("operation header names must be static"))?;
            if !header_names.insert(name.clone()) {
                return Err(OperationError::new(
                    "operation header names must not collide",
                ));
            }
            if value.len() > MAX_HEADER_VALUE_BYTES {
                return Err(OperationError::new(
                    "an operation header value exceeds the declared ceiling",
                ));
            }
            let value = HeaderValue::from_str(&value)
                .map_err(|_| OperationError::new("operation header value is invalid"))?;
            headers.push((name, value));
        }
        let mut bound_headers = Vec::new();
        for (name, input, scalar) in self.bound_headers {
            let lowercase = name.to_ascii_lowercase();
            if SDK_OWNED_HEADERS.contains(&lowercase.as_str()) {
                return Err(OperationError::new(
                    "an operation must not declare a header the SDK applies",
                ));
            }
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| OperationError::new("operation header names must be static"))?;
            if !header_names.insert(name.clone()) {
                return Err(OperationError::new(
                    "operation header names must not collide",
                ));
            }
            validate_input_name(&input)?;
            if matches!(scalar, ValueScalar::Json) {
                return Err(OperationError::new(
                    "a bound header value must be a scalar, not JSON",
                ));
            }
            bound_headers.push(HeaderEntry {
                name,
                input,
                scalar,
            });
        }

        let body = match self.body {
            BuilderBody::None => BodySource::None,
            BuilderBody::Json(template) => {
                template.validate()?;
                BodySource::Json(template)
            }
            BuilderBody::Processor(content_type) => BodySource::Processor {
                content_type: HeaderValue::from_str(&content_type).map_err(|_| {
                    OperationError::new("a processor body media type must be static and valid")
                })?,
            },
        };

        if self.success_statuses.is_empty() {
            return Err(OperationError::new(
                "connector operation must declare a success status",
            ));
        }
        if self
            .success_statuses
            .iter()
            .any(|status| !(200..=299).contains(status))
        {
            return Err(OperationError::new("success statuses must be 2xx"));
        }
        if self
            .no_content_statuses
            .iter()
            .any(|status| !self.success_statuses.contains(status))
        {
            return Err(OperationError::new(
                "a no-content success status must be a declared success status",
            ));
        }
        if !self.no_content_statuses.is_empty()
            && self
                .outputs
                .iter()
                .any(|output| output.required.is_required())
        {
            return Err(OperationError::new(
                "an operation with a no-content success must not declare a required output pointer",
            ));
        }

        // The effect gate: a class is admitted against the method it was
        // declared for, and the header a class binds its key to is the SDK's
        // for this operation, so a declaration cannot overwrite it.
        if let Some(effect) = &self.effect {
            effect.admit_method(self.method)?;
            if let Some(bound) = effect
                .idempotency_binding()
                .and_then(IdempotencyBinding::as_header)
                && header_names.contains(bound)
            {
                return Err(OperationError::new(
                    "an operation must not declare the header its idempotency binding owns",
                ));
            }
        }

        let mut output_names = BTreeSet::new();
        for output in &self.outputs {
            if output.name.is_empty() {
                return Err(OperationError::new("an output name is required"));
            }
            if !output_names.insert(output.name.clone()) {
                return Err(OperationError::new(
                    "an output name is declared more than once",
                ));
            }
            validate_json_pointer(&output.pointer)?;
        }

        // The declared contracts. Both are derived from what is already above:
        // the input contract from the slots this declaration reads, and the
        // output contract from the pointers it extracts. What a module adds
        // here is only what the template cannot say — a type a query key does
        // not carry, a value the module consumes or composes, an output that
        // arrives as a header rather than as a body field.
        let mut inputs: BTreeMap<String, (ValueScalar, bool)> = BTreeMap::new();
        for (name, scalar) in &path_params {
            inputs.insert(name.clone(), (scalar.clone(), true));
        }
        for entry in &bound_headers {
            inputs.insert(entry.input.clone(), (entry.scalar.clone(), true));
        }
        // A query key and a body leaf carry no type of their own, so an
        // undeclared one admits any scalar until a module types it.
        for entry in &self.query {
            if let QueryEntry::Input { input, .. } = entry {
                inputs
                    .entry(input.clone())
                    .or_insert((ValueScalar::Json, true));
            }
        }
        if let BodySource::Json(template) = &body {
            for name in template.input_names() {
                inputs.entry(name).or_insert((ValueScalar::Json, true));
            }
        }
        for (name, scalar, required) in &self.declared_inputs {
            validate_input_name(name)?;
            if self.supplied_inputs.iter().any(|supplied| supplied == name) {
                return Err(OperationError::new(
                    "an input cannot be both declared and supplied by the connector",
                ));
            }
            inputs.insert(name.clone(), (scalar.clone(), required.is_required()));
        }
        for name in &self.supplied_inputs {
            validate_input_name(name)?;
            inputs.remove(name);
        }
        let contract_inputs = inputs
            .into_iter()
            .map(|(name, (scalar, required))| InputProjection {
                name,
                scalar,
                required,
            })
            .collect();

        let mut outputs: BTreeMap<String, OutputProjection> = BTreeMap::new();
        for output in &self.outputs {
            outputs.insert(
                output.name.clone(),
                OutputProjection {
                    name: output.name.clone(),
                    pointer: Some(output.pointer.clone()),
                    scalar: output.scalar.clone(),
                    required: output.required.is_required(),
                },
            );
        }
        for (name, scalar, required) in &self.declared_outputs {
            if name.is_empty() {
                return Err(OperationError::new("an output name is required"));
            }
            if outputs.contains_key(name) {
                return Err(OperationError::new(
                    "an output name is declared more than once",
                ));
            }
            outputs.insert(
                name.clone(),
                OutputProjection {
                    name: name.clone(),
                    pointer: None,
                    scalar: scalar.clone(),
                    required: required.is_required(),
                },
            );
        }
        let contract_outputs = outputs.into_values().collect();

        if self.deadline.is_zero() {
            return Err(OperationError::new(
                "a connector operation deadline must be positive",
            ));
        }

        Ok(Operation {
            id: self.id,
            version,
            method: self.method,
            path_template: self.path_template,
            path_params,
            odata_literals: self.odata_literals,
            query: self.query,
            headers,
            bound_headers,
            body,
            success_statuses: self.success_statuses,
            no_content_statuses: self.no_content_statuses,
            outputs: self.outputs,
            effect: self.effect,
            contract_inputs,
            contract_outputs,
            deadline: self.deadline,
        })
    }
}

/// A rendered request, before a credential is applied and before it becomes an
/// opaque [`PreparedHttpRequest`].
///
/// Only the SDK may mutate it: an auth plan adds its applied primitive and a
/// pagination plan advances it. A provider module receives one and can read it,
/// which is what keeps a hand-written processor unable to construct a URL,
/// header name, or credential of its own.
#[derive(Clone)]
pub struct RequestPlan {
    method: Method,
    url: Url,
    headers: HeaderMap,
    body: Vec<u8>,
    /// Whether an auth plan wrote a credential into the URL itself.
    ///
    /// Two of the SDK's plans do — `ApiKeyQuery` spends the secret as a query
    /// value, and `ApiKeyPathSegment` spends it as a path segment — and for
    /// those two the rendered URL is as sensitive as an `Authorization` header.
    /// A header is redacted by `HeaderValue::set_sensitive`; a URL has no such
    /// flag, so this is it.
    url_carries_credential: bool,
}

/// A rendered request prints its URL only when the URL carries no credential.
///
/// This is the only redaction a URL can have: there is no per-component
/// sensitivity flag on a [`Url`], so a plan that spends a secret in the path or
/// the query makes the whole URL unprintable and the origin is what is left.
impl std::fmt::Debug for RequestPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestPlan")
            .field("method", &self.method)
            .field("url", &self.redacted_url())
            .field("headers", &self.headers)
            .field("body", &self.body)
            .finish()
    }
}

impl RequestPlan {
    pub const fn method(&self) -> &Method {
        &self.method
    }

    pub const fn url(&self) -> &Url {
        &self.url
    }

    /// The URL a log line, a metric label, a diagnostic, or a fingerprint may
    /// carry.
    ///
    /// For every plan but the two that spend a secret inside the URL this is
    /// the URL itself. For those two it is the origin followed by
    /// `/<redacted>`, which names the destination — the thing an operator
    /// actually needs — without the credential that reached it.
    pub fn redacted_url(&self) -> String {
        if !self.url_carries_credential {
            return self.url.as_str().to_owned();
        }
        let mut origin = self.url.clone();
        origin.set_query(None);
        origin.set_fragment(None);
        origin.set_path("/");
        format!("{origin}<redacted>")
    }

    /// Whether an auth plan spent this request's credential inside the URL.
    pub const fn url_carries_credential(&self) -> bool {
        self.url_carries_credential
    }

    pub const fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The assembled header block size: every name and value, plus the `: ` and
    /// CRLF each header line spends on the wire.
    pub fn header_bytes(&self) -> usize {
        self.headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len() + 4)
            .sum()
    }

    pub(in crate::sdk) fn new(method: Method, url: Url, headers: HeaderMap, body: Vec<u8>) -> Self {
        Self {
            method,
            url,
            headers,
            body,
            url_carries_credential: false,
        }
    }

    /// Record that an auth plan wrote a credential into the URL. Only
    /// [`crate::sdk::auth`] calls this, and it is one-way.
    pub(in crate::sdk) fn mark_url_credential(&mut self) {
        self.url_carries_credential = true;
    }

    pub(in crate::sdk) fn set_header(
        &mut self,
        name: HeaderName,
        mut value: HeaderValue,
        sensitive: bool,
    ) {
        value.set_sensitive(sensitive);
        self.headers.insert(name, value);
    }

    pub(in crate::sdk) fn set_url(&mut self, url: Url) {
        self.url = url;
    }

    pub(in crate::sdk) fn url_mut(&mut self) -> &mut Url {
        &mut self.url
    }

    /// Apply the request ceilings and hand the request to the transport.
    pub fn into_prepared(self) -> Result<PreparedHttpRequest, ConnectorFailure> {
        if self.body.len() > MAX_HTTP_BODY_BYTES {
            return Err(ConnectorFailure::invariant(
                "connector request exceeds the declared body ceiling",
            ));
        }
        if self
            .headers
            .values()
            .any(|value| value.len() > MAX_HEADER_VALUE_BYTES)
        {
            return Err(ConnectorFailure::invariant(
                "connector request header exceeds the declared ceiling",
            ));
        }
        if self.header_bytes() > MAX_REQUEST_HEADER_BYTES {
            return Err(ConnectorFailure::invariant(
                "connector request headers exceed the declared ceiling",
            ));
        }
        Ok(PreparedHttpRequest::new(
            self.method,
            self.url,
            self.headers,
            self.body,
        ))
    }
}

pub(crate) fn validate_semver_core(version: &str) -> Result<(), OperationError> {
    let parts = version.split('.').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(OperationError::new(
            "connector operation version must be a SemVer core",
        ));
    }
    Ok(())
}

fn validate_input_name(name: &str) -> Result<(), OperationError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
    {
        return Err(OperationError::new("connector input binding is invalid"));
    }
    Ok(())
}

/// A static query key.
///
/// `$` is admitted because OData names every system query option with it, and
/// Microsoft Graph documents no alternative a connector may rely on: "On the
/// *beta* endpoint, the `$` prefix is optional. … On the *v1.0* endpoint, the
/// `$` prefix is optional for only a subset of APIs. For simplicity, always
/// include `$` across all versions." It is one more sub-delimiter RFC 3986
/// already allows in a query, and it can no more end a key or start a second
/// parameter than `.` or `-` can — the set stays closed, and a key still cannot
/// carry `=`, `&`, `%`, or a binding
/// ([[declarative-saas/decisions/047-the-sdk-widens-where-a-provider-forced-it-and-nowhere-else]]).
pub(in crate::sdk) fn validate_query_key(key: &str) -> Result<(), OperationError> {
    if key.is_empty()
        || !key.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '_' | '-' | '.' | '[' | ']' | '$')
        })
    {
        return Err(OperationError::new("query keys must be static and valid"));
    }
    Ok(())
}

/// A static, absolute JSON pointer into a provider body.
pub(in crate::sdk) fn validate_json_pointer(pointer: &str) -> Result<(), OperationError> {
    if !pointer.starts_with('/') || pointer.ends_with('/') || pointer.contains(['{', '}']) {
        return Err(OperationError::new(
            "a JSON pointer must be static and absolute",
        ));
    }
    Ok(())
}

/// Validate the path template and return the names it binds.
fn validate_path_template(template: &str) -> Result<BTreeSet<String>, OperationError> {
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
        return Err(OperationError::new(
            "path must be a static absolute path without authority, userinfo, query, fragment, or dot segments",
        ));
    }
    let mut bound = BTreeSet::new();
    let mut remaining = template;
    while let Some(index) = remaining.find('{') {
        let after = &remaining[index + 1..];
        let Some(end) = after.find('}') else {
            return Err(OperationError::new("path binding is not closed"));
        };
        let name = &after[..end];
        validate_input_name(name)?;
        if !bound.insert(name.to_owned()) {
            return Err(OperationError::new("a path binding appears more than once"));
        }
        remaining = &after[end + 1..];
    }
    if remaining.contains('}') {
        return Err(OperationError::new(
            "path template contains an unsupported binding",
        ));
    }
    Ok(bound)
}

/// Whether a JSON value satisfies a declared scalar.
fn scalar_admits(scalar: &ValueScalar, value: &JsonValue) -> bool {
    match scalar {
        ValueScalar::Json => true,
        ValueScalar::Boolean => value.is_boolean(),
        ValueScalar::Int32 | ValueScalar::Int64 => value.is_i64(),
        ValueScalar::UInt64 => value.is_u64(),
        ValueScalar::String
        | ValueScalar::Decimal
        | ValueScalar::Uuid
        | ValueScalar::Date
        | ValueScalar::Timestamp
        | ValueScalar::TimestampTz
        | ValueScalar::Custom { .. } => value.is_string(),
    }
}

/// Read one declared scalar input slot and render it as a wire string.
fn scalar_input(
    input: &JsonValue,
    name: &str,
    scalar: ValueScalar,
) -> Result<String, ConnectorFailure> {
    let Some(value) = input.get(name) else {
        return Err(ConnectorFailure::invariant(
            "a declared connector input value is missing",
        ));
    };
    if !scalar_admits(&scalar, value) {
        return Err(ConnectorFailure::invariant(
            "a declared connector input value does not match its declared type",
        ));
    }
    match value {
        JsonValue::String(value) => Ok(value.clone()),
        JsonValue::Number(value) => Ok(value.to_string()),
        JsonValue::Bool(value) => Ok(value.to_string()),
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => Err(
            ConnectorFailure::invariant("a declared connector input value must be scalar"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use donat_value_contract::ValueScalar;
    use reqwest::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::sdk::errors::ConnectorErrorClass;

    fn origin() -> Origin {
        Origin::parse("https://provider.example.test").expect("static test origin is valid")
    }

    /// `sdk_declares_static_keys`: a dynamic header name, query key, or method
    /// fails to build.
    #[test]
    fn sdk_declares_static_keys() {
        let dynamic_header = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .static_header("X-Tenant-{tenant}", "fixed")
            .success_statuses([StatusCode::OK])
            .build();
        assert!(dynamic_header.is_err());

        let dynamic_query = Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .query_input("{sort}", "sort")
            .success_statuses([StatusCode::OK])
            .build();
        assert!(dynamic_query.is_err());

        assert!(HttpMethod::parse("{method}").is_err());
        assert!(HttpMethod::parse("TRACE").is_err());
        assert_eq!(HttpMethod::parse("GET"), Ok(HttpMethod::Get));
    }

    /// `sdk_effect_gate_is_closed`: an operation with a mutation and no
    /// admitted idempotency evidence cannot be marked executable.
    #[test]
    fn sdk_effect_gate_is_closed() {
        use std::time::Duration;

        use crate::sdk::effect::{Effect, EffectClass, ExplicitKeyEvidence, IdempotencyBinding};

        fn create(effect: Effect) -> Result<Operation, OperationError> {
            Operation::post("item.create", "/v1/items")
                .version("1.0.0")
                .success_statuses([StatusCode::CREATED])
                .effect(effect)
                .build()
        }

        // A mutation cannot be described as a read.  This is the whole gate in
        // one line: there is no spelling of `POST` plus `ReadOnly`.
        assert!(
            create(Effect::read_only()).is_err(),
            "a POST declared ReadOnly does not build"
        );
        for builder in [
            Operation::put("item.replace", "/v1/items/{id}"),
            Operation::patch("item.update", "/v1/items/{id}"),
            Operation::delete("item.delete", "/v1/items/{id}"),
        ] {
            assert!(
                builder
                    .version("1.0.0")
                    .path_param("id", ValueScalar::String)
                    .success_statuses([StatusCode::OK])
                    .effect(Effect::read_only())
                    .build()
                    .is_err(),
                "no mutating method may be declared ReadOnly"
            );
        }

        // An unclassified mutation builds — a fixture may render a request —
        // but it is not executable, so nothing a Process can reach admits it.
        let unclassified = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::CREATED])
            .build()
            .expect("an unclassified operation still renders");
        assert_eq!(unclassified.effect_class(), None);
        assert!(
            !unclassified.is_executable(),
            "an operation with no declared class is never executable"
        );

        // Incomplete explicit-key evidence never becomes an executable class:
        // the evidence itself is refused before an operation can carry it.
        let day = Duration::from_secs(24 * 60 * 60);
        let binding = || {
            IdempotencyBinding::header("Idempotency-Key").expect("a static header name is valid")
        };
        assert!(
            ExplicitKeyEvidence::documented(binding(), "account", day, day, "cited").is_err(),
            "a clock margin that is not strictly smaller than the retention is not evidence"
        );
        assert!(ExplicitKeyEvidence::documented(binding(), "", day, day / 2, "cited").is_err());
        assert!(ExplicitKeyEvidence::documented(binding(), "account", day, day / 2, "").is_err());

        // Complete evidence is admitted, and it is the only way a POST becomes
        // executable.
        let executable = create(Effect::provider_idempotent_explicit_key(
            ExplicitKeyEvidence::documented(
                binding(),
                "account",
                day,
                Duration::from_secs(300),
                "the provider documents Idempotency-Key with a 24 hour retention",
            )
            .expect("complete evidence is admitted"),
        ))
        .expect("a documented explicit key builds");
        assert_eq!(
            executable.effect_class(),
            Some(EffectClass::ProviderIdempotentExplicitKey)
        );
        assert!(executable.is_executable());
        assert_eq!(
            executable
                .idempotency_binding()
                .and_then(IdempotencyBinding::as_header)
                .map(reqwest::header::HeaderName::as_str),
            Some("idempotency-key")
        );
        // ...and the operation may not also declare that header itself.
        assert!(
            Operation::post("item.create", "/v1/items")
                .version("1.0.0")
                .static_header("Idempotency-Key", "fixed")
                .success_statuses([StatusCode::CREATED])
                .effect(Effect::provider_idempotent_explicit_key(
                    ExplicitKeyEvidence::documented(
                        binding(),
                        "account",
                        day,
                        Duration::from_secs(300),
                        "cited",
                    )
                    .expect("complete evidence is admitted"),
                ))
                .build()
                .is_err()
        );

        // NaturalMethod is admitted only for the two methods HTTP defines
        // repeat-safety for, and only with the provider's own statement.
        let cited = "the provider documents PUT on a record id as repeat-safe";
        for builder in [
            Operation::put("item.replace", "/v1/items/{id}"),
            Operation::delete("item.delete", "/v1/items/{id}"),
        ] {
            let operation = builder
                .version("1.0.0")
                .path_param("id", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .effect(
                    Effect::provider_idempotent_natural_method(cited)
                        .expect("a cited statement is evidence"),
                )
                .build()
                .expect("a naturally idempotent PUT or DELETE builds");
            assert_eq!(
                operation.effect_class(),
                Some(EffectClass::ProviderIdempotentNaturalMethod)
            );
            assert!(operation.is_executable());
        }
        for builder in [
            Operation::post("item.create", "/v1/items"),
            Operation::patch("item.update", "/v1/items"),
            Operation::get("item.get", "/v1/items"),
        ] {
            assert!(
                builder
                    .version("1.0.0")
                    .success_statuses([StatusCode::OK])
                    .effect(
                        Effect::provider_idempotent_natural_method(cited)
                            .expect("a cited statement is evidence"),
                    )
                    .build()
                    .is_err(),
                "only PUT and DELETE are naturally idempotent"
            );
        }

        // The remaining mutation is declared, typed, and never executable.
        let inventory = create(
            Effect::inventory_only("the provider publishes no idempotency key for this create")
                .expect("a recorded reason is required"),
        )
        .expect("an inventory-only operation is a valid declaration");
        assert_eq!(inventory.effect_class(), Some(EffectClass::InventoryOnly));
        assert!(!inventory.is_executable());

        // A read is executable without any idempotency evidence at all, and an
        // explicit key on a read is refused rather than quietly accepted.
        let read = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .effect(Effect::read_only())
            .build()
            .expect("a GET declared ReadOnly builds");
        assert!(read.is_executable());
        assert!(read.idempotency_binding().is_none());
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .success_statuses([StatusCode::OK])
                .effect(Effect::provider_idempotent_explicit_key(
                    ExplicitKeyEvidence::documented(
                        binding(),
                        "account",
                        day,
                        Duration::from_secs(300),
                        "cited",
                    )
                    .expect("complete evidence is admitted"),
                ))
                .build()
                .is_err(),
            "a read does not become a mutation by carrying a key"
        );

        // The one way a mutation-shaped method is executable without
        // idempotency evidence: the provider's own statement that it changes
        // nothing. It is a citation a reviewer checks, never silence.
        let search = Operation::post("item.search", "/v1/items/search")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .effect(
                Effect::read_only_documented(
                    "the provider documents this search as creating and changing nothing",
                )
                .expect("a cited statement is an assertion"),
            )
            .build()
            .expect("a documented mutation-shaped read builds");
        assert_eq!(search.effect_class(), Some(EffectClass::ReadOnly));
        assert!(search.is_executable());
        assert!(
            Effect::read_only_documented("").is_err(),
            "silence is not an assertion"
        );
    }

    /// `sdk_path_values_are_encoded`: a value containing `/`, `..`, `%`, `?`,
    /// `#`, or NUL stays inside its segment.
    #[test]
    fn sdk_path_values_are_encoded() {
        let operation = Operation::get("item.get", "/v1/items/{id}/detail")
            .version("1.0.0")
            .path_param("id", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");

        let plan = operation
            .plan_request(&origin(), &json!({ "id": "../a/b%20?x#y\u{0}z" }))
            .expect("a hostile path value renders");

        assert_eq!(
            plan.url().path(),
            "/v1/items/%2E%2E%2Fa%2Fb%2520%3Fx%23y%00z/detail"
        );
        assert_eq!(plan.url().query(), None);
        assert_eq!(plan.url().fragment(), None);
        assert_eq!(plan.url().host_str(), Some("provider.example.test"));
    }

    /// `sdk_bounds_are_exact`: request, response, header, and aggregate
    /// ceilings accept the exact boundary and reject one over.
    #[test]
    fn sdk_bounds_are_exact() {
        use crate::sdk::pagination::PaginationBudget;
        use crate::sdk::transport::MAX_HTTP_BODY_BYTES;

        fn request_of(payload_bytes: usize) -> Result<PreparedHttpRequest, ConnectorFailure> {
            // `{"payload":"<value>"}` is 14 bytes of framing.
            let value = "x".repeat(payload_bytes - 14);
            let operation = Operation::post("item.create", "/v1/items")
                .version("1.0.0")
                .body(JsonTemplate::literal(json!({ "payload": value })))
                .success_statuses([StatusCode::OK])
                .build()
                .expect("static declaration is valid");
            operation
                .plan_request(
                    &Origin::parse("https://provider.example.test").expect("origin is valid"),
                    &json!({}),
                )
                .expect("a literal body renders")
                .into_prepared()
        }

        assert!(request_of(MAX_HTTP_BODY_BYTES).is_ok());
        assert_eq!(
            request_of(MAX_HTTP_BODY_BYTES + 1)
                .err()
                .expect("one byte over the request ceiling is refused")
                .class(),
            ConnectorErrorClass::Invariant
        );

        let operation = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");
        assert!(
            operation
                .decode_response(200, &vec![b'x'; MAX_HTTP_BODY_BYTES])
                .is_err_and(|failure| failure.class() == ConnectorErrorClass::Validation
                    && failure.code() == "connector_validation"),
            "the exact response ceiling is admitted and then fails only on JSON"
        );
        assert_eq!(
            operation
                .decode_response(200, &vec![b'x'; MAX_HTTP_BODY_BYTES + 1])
                .expect_err("one byte over the response ceiling is refused")
                .code(),
            "connector_response_too_large"
        );

        let header_value = "v".repeat(MAX_HEADER_VALUE_BYTES);
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .static_header("X-Exact", &header_value)
                .success_statuses([StatusCode::OK])
                .build()
                .is_ok()
        );
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .static_header("X-Over", &format!("{header_value}v"))
                .success_statuses([StatusCode::OK])
                .build()
                .is_err()
        );

        let budget = PaginationBudget::new(4, 4, 8, 1_000, std::time::Duration::from_secs(30));
        assert!(budget.admit_totals(1_000, 8).is_ok());
        assert!(budget.admit_totals(1_001, 8).is_err());
        assert!(budget.admit_totals(1_000, 9).is_err());
    }

    #[test]
    fn total_request_header_bytes_are_bounded() {
        let mut builder = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK]);
        // 8 headers of 4100 value bytes plus their names pass the ceiling.
        let value = "v".repeat(4_100);
        for index in 0..8 {
            builder = builder.static_header(&format!("X-Pad-{index}"), &value);
        }
        let operation = builder.build().expect("padded declaration is valid");
        let plan = operation
            .plan_request(&origin(), &json!({}))
            .expect("padded request renders");
        assert!(plan.header_bytes() > MAX_REQUEST_HEADER_BYTES);
        assert_eq!(
            plan.into_prepared()
                .err()
                .expect("the assembled header block is bounded")
                .class(),
            ConnectorErrorClass::Invariant
        );
    }

    #[test]
    fn an_operation_may_not_declare_a_header_the_sdk_owns() {
        for name in ["Authorization", "content-length", "Host"] {
            assert!(
                Operation::get("item.get", "/v1/items")
                    .version("1.0.0")
                    .static_header(name, "value")
                    .success_statuses([StatusCode::OK])
                    .build()
                    .is_err(),
                "{name} is applied by the SDK, never declared by an operation"
            );
        }
    }

    #[test]
    fn a_path_template_may_not_carry_an_authority_or_a_dot_segment() {
        for path in [
            "v1/items",
            "//provider.example.test/v1/items",
            "https://provider.example.test/v1/items",
            "/v1/../items",
            "/v1/./items",
            "/v1/items?state=open",
            "/v1/items#fragment",
            "/v1/items/{id",
            "/v1/items/{}",
        ] {
            assert!(
                Operation::get("item.get", path)
                    .version("1.0.0")
                    .path_param("id", ValueScalar::String)
                    .success_statuses([StatusCode::OK])
                    .build()
                    .is_err(),
                "path {path} must not build"
            );
        }
    }

    #[test]
    fn every_path_binding_is_declared_with_a_type() {
        assert!(
            Operation::get("item.get", "/v1/items/{id}")
                .version("1.0.0")
                .success_statuses([StatusCode::OK])
                .build()
                .is_err(),
            "an undeclared path binding does not build"
        );
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .path_param("id", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .build()
                .is_err(),
            "a declared path parameter the path never uses does not build"
        );
    }

    #[test]
    fn a_declared_path_parameter_type_is_enforced_at_render() {
        let operation = Operation::get("issue.get", "/repos/{owner}/issues/{number}")
            .version("1.0.0")
            .path_param("owner", ValueScalar::String)
            .path_param("number", ValueScalar::Int64)
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");

        let plan = operation
            .plan_request(&origin(), &json!({ "owner": "donat", "number": 42 }))
            .expect("typed values render");
        assert_eq!(plan.url().path(), "/repos/donat/issues/42");

        let failure = operation
            .plan_request(&origin(), &json!({ "owner": "donat", "number": "42" }))
            .expect_err("a string is not an Int64 path value");
        assert_eq!(failure.class(), ConnectorErrorClass::Invariant);

        let missing = operation
            .plan_request(&origin(), &json!({ "owner": "donat" }))
            .expect_err("a missing declared path value is an invariant failure");
        assert_eq!(missing.class(), ConnectorErrorClass::Invariant);
    }

    /// The deploy-time declarative connector's slot: the declaration fixes the
    /// path, not the scalar spelling of the value that fills it.
    #[test]
    fn an_untyped_path_slot_admits_every_scalar_and_refuses_every_structure() {
        let operation = Operation::get("item.get", "/v1/items/{id}")
            .version("1.0.0")
            .untyped_path_param("id")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("an untyped slot is a valid declaration");

        for (value, expected) in [
            (json!({ "id": "order-42" }), "/v1/items/order%2D42"),
            (json!({ "id": 42 }), "/v1/items/42"),
            (json!({ "id": true }), "/v1/items/true"),
        ] {
            assert_eq!(
                operation
                    .plan_request(&origin(), &value)
                    .expect("every scalar spelling renders")
                    .url()
                    .path(),
                expected
            );
        }
        for value in [
            json!({ "id": null }),
            json!({ "id": ["a"] }),
            json!({ "id": { "a": 1 } }),
            json!({}),
        ] {
            assert_eq!(
                operation
                    .plan_request(&origin(), &value)
                    .expect_err("a structure is not a path value")
                    .class(),
                ConnectorErrorClass::Invariant,
                "{value} must not reach a URL"
            );
        }

        // A hand-written declaration still cannot ask for `Json` by name.
        assert!(
            Operation::get("item.get", "/v1/items/{id}")
                .version("1.0.0")
                .path_param("id", ValueScalar::Json)
                .success_statuses([StatusCode::OK])
                .build()
                .is_err()
        );
    }

    #[test]
    fn a_processor_assembled_body_carries_the_provider_media_type() {
        let operation = Operation::post("session.create", "/v1/checkout/sessions")
            .version("1.0.0")
            .processor_body("application/x-www-form-urlencoded")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a processor body is a valid declaration");

        let plan = operation
            .plan_processor_request(
                &origin(),
                &json!({}),
                &HeaderMap::new(),
                b"mode=payment".to_vec(),
            )
            .expect("the processor body renders");
        assert_eq!(plan.body(), b"mode=payment");
        assert_eq!(
            plan.headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/x-www-form-urlencoded")
        );

        // The two body sources are exclusive in both directions.
        assert_eq!(
            operation
                .plan_request(&origin(), &json!({}))
                .expect_err("a processor body is not optional")
                .class(),
            ConnectorErrorClass::Invariant
        );
        let json_body = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .body(JsonTemplate::literal(json!({ "name": "widget" })))
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");
        assert_eq!(
            json_body
                .plan_processor_request(&origin(), &json!({}), &HeaderMap::new(), b"raw".to_vec())
                .expect_err("a declared template is not replaceable by processor bytes")
                .class(),
            ConnectorErrorClass::Invariant
        );
        assert!(
            Operation::post("item.create", "/v1/items")
                .version("1.0.0")
                .processor_body("application/\u{7f}")
                .success_statuses([StatusCode::OK])
                .build()
                .is_err(),
            "a media type is static and valid or the operation does not build"
        );
    }

    /// A deployment's own headers reach the request, and a declared operation
    /// header can never take one of their names.
    #[test]
    fn a_configured_deployment_header_cannot_be_replaced_by_a_declaration() {
        let operation = Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .static_header("X-Operation", "fixed")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");

        let mut configured = HeaderMap::new();
        configured.insert("x-tenant", HeaderValue::from_static("acme"));
        let plan = operation
            .plan_configured_request(&origin(), &json!({}), &configured)
            .expect("configured headers apply");
        assert_eq!(
            plan.headers()
                .get("x-tenant")
                .and_then(|value| value.to_str().ok()),
            Some("acme")
        );
        assert_eq!(
            plan.headers()
                .get("x-operation")
                .and_then(|value| value.to_str().ok()),
            Some("fixed")
        );

        let mut colliding = HeaderMap::new();
        colliding.insert("x-operation", HeaderValue::from_static("deployment"));
        assert_eq!(
            operation
                .plan_configured_request(&origin(), &json!({}), &colliding)
                .expect_err("a declaration must not overwrite deployment material")
                .class(),
            ConnectorErrorClass::Invariant
        );
    }

    #[test]
    fn success_statuses_are_declared_and_must_be_successful() {
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .build()
                .is_err(),
            "an operation with no declared success status does not build"
        );
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .success_statuses([StatusCode::MOVED_PERMANENTLY])
                .build()
                .is_err(),
            "a redirect is not a success status"
        );

        let operation = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK, StatusCode::CREATED])
            .build()
            .expect("static declaration is valid");
        assert!(operation.is_success(200));
        assert!(operation.is_success(201));
        assert!(!operation.is_success(204));
    }

    #[test]
    fn a_stable_id_and_a_semver_core_version_are_required() {
        assert!(
            Operation::get("", "/v1/items")
                .version("1.0.0")
                .success_statuses([StatusCode::OK])
                .build()
                .is_err()
        );
        for version in ["", "v1", "1.0", "1.0.0-rc.1"] {
            assert!(
                Operation::get("item.get", "/v1/items")
                    .version(version)
                    .success_statuses([StatusCode::OK])
                    .build()
                    .is_err(),
                "version {version} does not build"
            );
        }
        let operation = Operation::get("item.get", "/v1/items")
            .version("1.2.3")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a semver core version builds");
        assert_eq!(operation.id(), "item.get");
        assert_eq!(operation.version(), "1.2.3");
    }

    #[test]
    fn declared_output_pointers_keep_the_declaration_as_the_output_schema() {
        let operation = Operation::get("issue.get", "/v1/issues")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .output_pointer("title", "/title", ValueScalar::String, Required::No)
            .build()
            .expect("static declaration is valid");

        assert_eq!(
            operation
                .decode_response(200, br#"{"id":"issue_1"}"#)
                .expect("an optional pointer may be absent"),
            json!({ "id": "issue_1", "title": null }),
            "an absent optional field is published as an explicit null"
        );

        for body in [
            br#"{"title":"a"}"#.as_slice(),
            br#"{"id":null,"title":"a"}"#.as_slice(),
        ] {
            assert_eq!(
                operation
                    .decode_response(200, body)
                    .expect_err("a required pointer is not satisfied")
                    .class(),
                ConnectorErrorClass::Validation
            );
        }

        assert_eq!(
            operation
                .decode_response(200, br#"{"id":7}"#)
                .expect_err("a declared type is part of the contract")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    #[test]
    fn an_output_pointer_must_be_a_valid_static_json_pointer() {
        for pointer in ["id", "", "/id/"] {
            assert!(
                Operation::get("item.get", "/v1/items")
                    .version("1.0.0")
                    .success_statuses([StatusCode::OK])
                    .output_pointer("id", pointer, ValueScalar::String, Required::Yes)
                    .build()
                    .is_err(),
                "pointer {pointer} does not build"
            );
        }
    }

    #[test]
    fn query_keys_and_values_are_static_or_bound_and_always_encoded() {
        let operation = Operation::get("item.list", "/v1/items")
            .version("1.0.0")
            .query_static("api-version", "2026-01-01")
            .query_input("state", "state")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("static declaration is valid");

        let plan = operation
            .plan_request(&origin(), &json!({ "state": "one/two & three" }))
            .expect("query values render");
        assert_eq!(
            plan.url().query(),
            Some("api-version=2026%2D01%2D01&state=one%2Ftwo%20%26%20three")
        );
    }

    /// A slot inside an OData quoted string literal is doubled before it is
    /// encoded, because a receiver decodes `%27` back to `'` before it parses
    /// the expression (spec 015; Microsoft publishes `search(q='<text>')` and
    /// `range(address='<address>')` and no other spelling of either).
    #[test]
    fn an_odata_literal_path_value_is_doubled_before_it_is_encoded() {
        let operation = Operation::get("item.search", "/v1.0/me/drive/root/search(q='{query}')")
            .version("1.0.0")
            .odata_literal_path_param("query", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .build()
            .expect("an OData literal slot is a valid declaration");

        for (value, expected) in [
            ("O'Brien", "/v1.0/me/drive/root/search(q='O%27%27Brien')"),
            (
                "')/../drive/items/other",
                "/v1.0/me/drive/root/search(q='%27%27%29%2F%2E%2E%2Fdrive%2Fitems%2Fother')",
            ),
            ("plain", "/v1.0/me/drive/root/search(q='plain')"),
        ] {
            assert_eq!(
                operation
                    .plan_request(&origin(), &json!({ "query": value }))
                    .expect("the path renders")
                    .url()
                    .path(),
                expected,
                "value {value}"
            );
        }

        // An ordinary path slot is unchanged: the doubling belongs to the
        // declared kind and to nothing else.
        assert_eq!(
            Operation::get("item.get", "/v1/items/{id}")
                .version("1.0.0")
                .path_param("id", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .build()
                .expect("static declaration is valid")
                .plan_request(&origin(), &json!({ "id": "O'Brien" }))
                .expect("the path renders")
                .url()
                .path(),
            "/v1/items/O%27Brien"
        );

        // The kind is still a path parameter: it has to appear in the path.
        assert!(
            Operation::get("item.search", "/v1/items")
                .version("1.0.0")
                .odata_literal_path_param("query", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .build()
                .is_err()
        );
    }

    /// An OData system query option is a query key, and the widening that
    /// admits it does not admit a key that could end itself or start a second
    /// parameter (spec 015; Microsoft: "always include `$` across all
    /// versions").
    #[test]
    fn an_odata_system_query_option_is_a_static_query_key() {
        let operation = Operation::get("message.list", "/v1.0/me/messages")
            .version("1.0.0")
            .query_static("$select", "id,subject")
            .query_static("$top", "50")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("an OData query option is a valid static key");
        assert_eq!(
            operation
                .plan_request(&origin(), &json!({}))
                .expect("query values render")
                .url()
                .query(),
            Some("$select=id%2Csubject&$top=50")
        );

        for key in [
            "$select=x",
            "$top&$skip",
            "$se%20lect",
            "{$select}",
            "$select ",
        ] {
            assert!(
                Operation::get("message.list", "/v1.0/me/messages")
                    .version("1.0.0")
                    .query_static(key, "x")
                    .success_statuses([StatusCode::OK])
                    .build()
                    .is_err(),
                "query key `{key}` must not build"
            );
        }
    }

    #[test]
    fn a_body_template_binds_values_only_at_static_keys() {
        let operation = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .body(JsonTemplate::object([
                ("name", JsonTemplate::input("name")),
                ("source", JsonTemplate::literal(json!("donat"))),
            ]))
            .success_statuses([StatusCode::CREATED])
            .build()
            .expect("static declaration is valid");

        let plan = operation
            .plan_request(&origin(), &json!({ "name": "widget" }))
            .expect("body renders");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(plan.body()).expect("body is JSON"),
            json!({ "name": "widget", "source": "donat" })
        );
        assert_eq!(
            plan.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
    }

    #[test]
    fn a_declared_content_type_survives_the_json_default() {
        let operation = Operation::post("item.create", "/v1/items")
            .version("1.0.0")
            .static_header("Content-Type", "application/vnd.provider.v2+json")
            .body(JsonTemplate::literal(json!({ "name": "widget" })))
            .success_statuses([StatusCode::CREATED])
            .build()
            .expect("static declaration is valid");

        let plan = operation
            .plan_request(&origin(), &json!({}))
            .expect("body renders");
        assert_eq!(
            plan.headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/vnd.provider.v2+json")
        );
    }

    /// A declared header name whose *value* binds from a declared, typed input
    /// slot.
    ///
    /// Amazon's `CopyObject` needs `x-amz-copy-source`, whose value names the
    /// object being copied. The name stays in the declaration — there is still
    /// no API here that takes a header name from input — and only the value
    /// binds, exactly as a query value or a body leaf already does.
    #[test]
    fn a_declared_header_name_may_bind_its_value_from_a_typed_input_slot() {
        let operation = Operation::put("object.copy", "/{key}")
            .version("1.0.0")
            .path_param("key", ValueScalar::String)
            .header_input("x-amz-copy-source", "copy_source", ValueScalar::String)
            .static_header("x-amz-metadata-directive", "COPY")
            .success_statuses([StatusCode::OK])
            .build()
            .expect("a declared header with a bound value is a valid declaration");

        let plan = operation
            .plan_request(
                &origin(),
                &json!({ "key": "b.json", "copy_source": "/bucket/a.json" }),
            )
            .expect("the bound header value renders");
        assert_eq!(
            plan.headers()
                .get("x-amz-copy-source")
                .and_then(|value| value.to_str().ok()),
            Some("/bucket/a.json")
        );
        assert_eq!(
            plan.headers()
                .get("x-amz-metadata-directive")
                .and_then(|value| value.to_str().ok()),
            Some("COPY")
        );

        // The value is a declared, typed slot: the wrong type, a structure, and
        // an absent value are all refused rather than rendered.
        for input in [
            json!({ "key": "b.json", "copy_source": 7 }),
            json!({ "key": "b.json", "copy_source": null }),
            json!({ "key": "b.json", "copy_source": ["/bucket/a.json"] }),
            json!({ "key": "b.json" }),
        ] {
            assert_eq!(
                operation
                    .plan_request(&origin(), &input)
                    .expect_err("a declared slot is typed")
                    .class(),
                ConnectorErrorClass::Invariant,
                "{input}"
            );
        }

        // A value cannot smuggle a second header, an oversized value, or a
        // control character onto the request.
        for hostile in [
            "one\r\nx-amz-acl: public-read",
            "one\nx-amz-acl: public-read",
            "\u{0}",
        ] {
            assert_eq!(
                operation
                    .plan_request(
                        &origin(),
                        &json!({ "key": "b.json", "copy_source": hostile }),
                    )
                    .expect_err("a header value is one header value")
                    .class(),
                ConnectorErrorClass::Invariant,
                "{hostile:?}"
            );
        }
        assert!(
            operation
                .plan_request(
                    &origin(),
                    &json!({ "key": "b", "copy_source": "v".repeat(MAX_HEADER_VALUE_BYTES) }),
                )
                .is_ok(),
            "the exact header ceiling is admitted"
        );
        assert_eq!(
            operation
                .plan_request(
                    &origin(),
                    &json!({ "key": "b", "copy_source": "v".repeat(MAX_HEADER_VALUE_BYTES + 1) }),
                )
                .expect_err("one byte over the header ceiling is refused")
                .class(),
            ConnectorErrorClass::Invariant
        );

        // The name is still a declaration, and every rule a static header name
        // obeys it obeys too.
        for name in ["Authorization", "content-length", "Host", "X-Tenant-{t}"] {
            assert!(
                Operation::put("object.copy", "/v1/items")
                    .version("1.0.0")
                    .header_input(name, "value", ValueScalar::String)
                    .success_statuses([StatusCode::OK])
                    .build()
                    .is_err(),
                "{name} is not a name a declaration may bind"
            );
        }
        assert!(
            Operation::put("object.copy", "/v1/items")
                .version("1.0.0")
                .static_header("x-amz-copy-source", "/bucket/a.json")
                .header_input("x-amz-copy-source", "copy_source", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .build()
                .is_err(),
            "one header, one source"
        );
        assert!(
            Operation::put("object.copy", "/v1/items")
                .version("1.0.0")
                .header_input("x-amz-copy-source", "{copy_source}", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .build()
                .is_err(),
            "the input slot is a declared name"
        );
        assert!(
            Operation::put("object.copy", "/v1/items")
                .version("1.0.0")
                .header_input("x-amz-copy-source", "copy_source", ValueScalar::Json)
                .success_statuses([StatusCode::OK])
                .build()
                .is_err(),
            "a header value is a scalar, not a JSON document"
        );
        // ...and it is no more able to take the header an idempotency binding
        // owns than a static declaration is.
        assert!(
            Operation::post("item.create", "/v1/items")
                .version("1.0.0")
                .header_input("Idempotency-Key", "key", ValueScalar::String)
                .success_statuses([StatusCode::CREATED])
                .effect(
                    crate::sdk::effect::Effect::provider_idempotent_explicit_key(
                        crate::sdk::effect::ExplicitKeyEvidence::documented(
                            crate::sdk::effect::IdempotencyBinding::header("Idempotency-Key")
                                .expect("a static header name is valid"),
                            "account",
                            std::time::Duration::from_secs(86_400),
                            std::time::Duration::from_secs(300),
                            "cited",
                        )
                        .expect("complete evidence is admitted"),
                    )
                )
                .build()
                .is_err(),
            "a durable activity's key is not a caller's"
        );

        // ...and a deployment's own configured header is never replaced by one.
        let mut configured = HeaderMap::new();
        configured.insert("x-amz-copy-source", HeaderValue::from_static("/other/key"));
        assert_eq!(
            operation
                .plan_configured_request(
                    &origin(),
                    &json!({ "key": "b.json", "copy_source": "/bucket/a.json" }),
                    &configured,
                )
                .expect_err("a declaration must not overwrite deployment material")
                .class(),
            ConnectorErrorClass::Invariant
        );
    }

    /// `HEAD` is a read, and the effect gate treats it as one.
    ///
    /// Amazon's `HeadObject` — "The `HEAD` operation retrieves metadata from an
    /// object without returning the object itself" — is the case that needed
    /// it. A method that retrieves and returns nothing is read-only by its
    /// method exactly as a `GET` is, so it needs no provider assertion and
    /// cannot carry idempotency evidence.
    #[test]
    fn a_head_is_a_read_and_the_effect_gate_treats_it_as_one() {
        use crate::sdk::effect::{Effect, EffectClass, ExplicitKeyEvidence, IdempotencyBinding};
        use std::time::Duration;

        assert_eq!(HttpMethod::parse("HEAD"), Ok(HttpMethod::Head));
        assert!(!HttpMethod::Head.mutates(), "a HEAD retrieves and returns");
        assert_eq!(HttpMethod::Head.as_reqwest(), Method::HEAD);

        let head = Operation::head("object.head", "/{bucket}/{key}")
            .version("1.0.0")
            .untyped_path_param("bucket")
            .path_param("key", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .effect(Effect::read_only())
            .build()
            .expect("a HEAD declared ReadOnly builds");
        assert_eq!(head.method(), HttpMethod::Head);
        assert_eq!(head.effect_class(), Some(EffectClass::ReadOnly));
        assert!(head.is_executable());
        let plan = head
            .plan_request(&origin(), &json!({ "bucket": "b", "key": "report.json" }))
            .expect("a HEAD renders like every other read");
        assert_eq!(plan.method(), Method::HEAD);
        assert_eq!(plan.url().path(), "/b/report%2Ejson");
        assert!(plan.body().is_empty());

        // The gate answers a HEAD exactly as it answers a GET: no assertion is
        // wanted, and no idempotency class is admitted.
        assert!(
            Operation::head("object.head", "/{key}")
                .version("1.0.0")
                .path_param("key", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .effect(
                    Effect::read_only_documented("the provider documents this as changing nothing")
                        .expect("a cited statement is an assertion")
                )
                .build()
                .is_err(),
            "a HEAD is read-only by its method; it needs no assertion"
        );
        assert!(
            Operation::head("object.head", "/{key}")
                .version("1.0.0")
                .path_param("key", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .effect(Effect::provider_idempotent_explicit_key(
                    ExplicitKeyEvidence::documented(
                        IdempotencyBinding::header("Idempotency-Key")
                            .expect("a static header name is valid"),
                        "account",
                        Duration::from_secs(86_400),
                        Duration::from_secs(300),
                        "cited",
                    )
                    .expect("complete evidence is admitted"),
                ))
                .build()
                .is_err(),
            "a read does not become a mutation by carrying a key"
        );
        assert!(
            Operation::head("object.head", "/{key}")
                .version("1.0.0")
                .path_param("key", ValueScalar::String)
                .success_statuses([StatusCode::OK])
                .effect(
                    Effect::provider_idempotent_natural_method("cited")
                        .expect("a cited statement is evidence")
                )
                .build()
                .is_err(),
            "NaturalMethod idempotency is admitted only for PUT and DELETE"
        );
    }

    /// A documented empty-bodied success is a success.
    ///
    /// Providers publish them constantly — SendGrid answers `202` to a mail
    /// send and `204` to a list delete with no body at all, Typeform answers
    /// `200` to a response delete the same way — and before a declaration could
    /// say so, every one of them was decoded as malformed JSON and reported as
    /// a `validation` failure.
    #[test]
    fn a_declared_no_content_success_is_a_success_with_an_empty_output() {
        let accepted = Operation::post("mail.send", "/v3/mail/send")
            .version("1.0.0")
            .success_statuses([StatusCode::ACCEPTED])
            .no_content_statuses([StatusCode::ACCEPTED])
            .build()
            .expect("a declared no-content success is a valid declaration");
        assert!(accepted.is_no_content_success(202));
        assert_eq!(
            accepted
                .decode_response(202, b"")
                .expect("a documented empty body is the documented success"),
            json!({})
        );
        // A provider that keeps the connection warm with whitespace has still
        // sent no content.
        assert_eq!(
            accepted
                .decode_response(202, b"\r\n  ")
                .expect("whitespace is not content"),
            json!({})
        );
        // ...and a provider that did send a body is still decoded from it.
        assert_eq!(
            accepted
                .decode_response(202, br#"{"job_id":"job_1"}"#)
                .expect("a body that arrived is still read"),
            json!({ "job_id": "job_1" })
        );

        // One status of several: SendGrid's list delete answers `200` with a
        // job and `204` with nothing, and the declaration says which is which.
        let delete = Operation::delete("list.delete", "/v3/marketing/lists/{list_id}")
            .version("1.0.0")
            .path_param("list_id", ValueScalar::String)
            .success_statuses([StatusCode::OK, StatusCode::NO_CONTENT])
            .no_content_statuses([StatusCode::NO_CONTENT])
            .output_pointer("job_id", "/job_id", ValueScalar::String, Required::No)
            .build()
            .expect("a per-status no-content success is a valid declaration");
        assert!(!delete.is_no_content_success(200));
        assert_eq!(
            delete
                .decode_response(204, b"")
                .expect("the documented 204 carries no body"),
            json!({ "job_id": null }),
            "an empty body decodes exactly as an empty object would"
        );
        assert_eq!(
            delete
                .decode_response(200, br#"{"job_id":"job_3"}"#)
                .expect("the documented 200 carries a job"),
            json!({ "job_id": "job_3" })
        );
        assert_eq!(
            delete
                .decode_response(200, b"")
                .expect_err("a status that declared no such thing still needs its body")
                .class(),
            ConnectorErrorClass::Validation
        );

        // An operation that declares output pointers still fails on a missing
        // body: a required pointer cannot be satisfied by silence, so the two
        // declarations do not compose at all.
        let required = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .build()
            .expect("static declaration is valid");
        assert_eq!(
            required
                .decode_response(200, b"")
                .expect_err("a required pointer is not satisfied by an absent body")
                .class(),
            ConnectorErrorClass::Validation
        );
        assert!(
            Operation::get("item.get", "/v1/items")
                .version("1.0.0")
                .success_statuses([StatusCode::OK])
                .no_content_statuses([StatusCode::OK])
                .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
                .build()
                .is_err(),
            "a no-content success cannot silently drop a required output pointer"
        );

        // A no-content status is one of the operation's own success statuses,
        // or the declaration is describing a response it never admits.
        assert!(
            Operation::post("mail.send", "/v3/mail/send")
                .version("1.0.0")
                .success_statuses([StatusCode::ACCEPTED])
                .no_content_statuses([StatusCode::NO_CONTENT])
                .build()
                .is_err(),
            "a no-content status that is not a success status does not build"
        );
        // ...and it never turns an undeclared status into a success.
        assert_eq!(
            accepted
                .decode_response(204, b"")
                .expect_err("an undeclared status is not rescued by an empty body")
                .class(),
            ConnectorErrorClass::Permanent
        );
    }

    #[test]
    fn an_undeclared_status_is_never_a_silent_success() {
        let operation = Operation::get("item.get", "/v1/items")
            .version("1.0.0")
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .build()
            .expect("static declaration is valid");

        let failure = operation
            .decode_response(299, br#"{"id":"issue_1"}"#)
            .expect_err("an undeclared 2xx is a failure, not a success");
        assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    }

    /// The SDK owns the idempotency header, so it is also the only thing that
    /// can fill it: a declaration that names it does not build, and an
    /// operation whose class binds one is not sendable without it.
    ///
    /// Spec 026 §2 is why this exists as a method rather than as a line in one
    /// provider module: three of the batch's providers publish a header binding,
    /// and a module that forgot to write it would send a payment with no key at
    /// all and no failure to show for it.
    #[test]
    fn a_declared_explicit_key_is_written_by_the_sdk_and_nowhere_else() {
        use crate::sdk::effect::{Effect, ExplicitKeyEvidence, IdempotencyBinding};

        let origin = Origin::parse("https://api.example.test").expect("a fixed origin is valid");
        let keyed = Operation::post("thing.create", "/things")
            .version("1.0.0")
            .body(JsonTemplate::object([(
                "name",
                JsonTemplate::input("name"),
            )]))
            .declared_input("name", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .effect(Effect::provider_idempotent_explicit_key(
                ExplicitKeyEvidence::documented(
                    IdempotencyBinding::header("Idempotency-Key").expect("static"),
                    "account",
                    Duration::from_secs(600),
                    Duration::from_secs(60),
                    "the provider documents the header, the scope, and a ten minute retention",
                )
                .expect("complete evidence is admitted"),
            ))
            .build()
            .expect("the declaration is valid");

        let mut request = keyed
            .plan_request(&origin, &serde_json::json!({ "name": "x" }))
            .expect("the declared request renders");
        assert!(
            request.headers().get("idempotency-key").is_none(),
            "a rendered request carries no key until the activity's own is written"
        );
        keyed
            .apply_idempotency_key(&mut request, "activity-42")
            .expect("the declared binding takes the activity key");
        assert_eq!(
            request
                .headers()
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok()),
            Some("activity-42")
        );
        // A second call replaces rather than appends, so a retry cannot send two.
        keyed
            .apply_idempotency_key(&mut request, "activity-43")
            .expect("the binding is written, not accumulated");
        assert_eq!(
            request.headers().get_all("idempotency-key").iter().count(),
            1
        );
        // A key that could forge a header field is refused rather than escaped.
        assert!(
            keyed
                .apply_idempotency_key(&mut request, "activity\r\nx-injected: 1")
                .is_err()
        );

        // Every other class binds nothing, so writing a key is a no-op rather
        // than a header a provider never asked for.
        let read = Operation::get("thing.get", "/things/{id}")
            .version("1.0.0")
            .path_param("id", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .effect(Effect::read_only())
            .build()
            .expect("the declaration is valid");
        let mut request = read
            .plan_request(&origin, &serde_json::json!({ "id": "1" }))
            .expect("the declared request renders");
        read.apply_idempotency_key(&mut request, "activity-42")
            .expect("an operation with no binding writes nothing");
        assert!(request.headers().get("idempotency-key").is_none());

        // A body binding belongs to the module that assembles the body, and
        // asking this method for one is refused rather than silently skipped.
        let body_bound = Operation::post("thing.send", "/things")
            .version("1.0.0")
            .body(JsonTemplate::object([("Id", JsonTemplate::input("id"))]))
            .declared_input("id", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK])
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .effect(Effect::provider_idempotent_explicit_key(
                ExplicitKeyEvidence::documented(
                    IdempotencyBinding::body_pointer("/DeduplicationId").expect("static"),
                    "queue",
                    Duration::from_secs(300),
                    Duration::from_secs(60),
                    "the provider documents a body field and a five minute interval",
                )
                .expect("complete evidence is admitted"),
            ))
            .build()
            .expect("the declaration is valid");
        let mut request = body_bound
            .plan_request(&origin, &serde_json::json!({ "id": "1" }))
            .expect("the declared request renders");
        assert!(
            body_bound
                .apply_idempotency_key(&mut request, "activity-42")
                .is_err()
        );
    }
}
