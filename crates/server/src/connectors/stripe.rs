//! Narrow Stripe Checkout connector: the reference processor-backed connector.
//!
//! Only `checkout.create_session` and the signed
//! `checkout.session.completed` inbound shape live here.  This is not a
//! generic Stripe API client: callers cannot select a URL, method, header, or
//! request schema.  The production transport always uses Stripe's fixed API
//! origin; the loopback origin below is compiled only for crate-local tests.
//!
//! The request is an SDK declaration and travels the shared SDK transport, and
//! the credential is applied by the SDK's `Bearer` plan rather than by a header
//! this module formats.  What stays hand-written is the part a declaration
//! cannot express: Stripe's Checkout API takes form-encoded pairs with indexed
//! repeated keys (`line_items[0][price]`), so the body is assembled by a
//! processor here and handed to the declaration as bytes.  The processor
//! chooses nothing else — not the method, origin, path, query, or any header
//! name.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use donat_connector_abi::{BoundedString, Hash256, VerifiedInboundEvent};
use donat_connectors::sdk::{
    AuthPlan, Connector, Credential, CredentialSpec, Effect, ExplicitKeyEvidence, HostResolver,
    HttpTransport, IdempotencyBinding, Operation as SdkOperation, Origin, OriginSpec,
    RawHttpResponse, ReqwestTransport, Secret, SignatureEncoding, SystemResolver,
    TransportErrorKind, Trigger, WebhookVerifier,
};
use donat_ir::TypedValue;
use donat_metadata::{ConnectorConfig, ConnectorOperation, ConnectorOperationProfile};
// HMAC now lives behind the SDK's verifier; the alias survives for the
// crate-local tests that sign a fixture body.
#[cfg(test)]
use hmac::Hmac;
use reqwest::{
    StatusCode, Url,
    header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use donat_connector_abi::TriggerId;
use donat_connector_catalog::TriggerSpec;
use futures_util::future::BoxFuture;
use serde::Serialize;

use super::{
    CompiledWebhookTrigger, ConnectorDefinition, ConnectorErrorClass, ConnectorFailure,
    ConnectorModule, ConnectorRegistryError, ConnectorSuccess, ModuleContext, RegisteredConnector,
    STRIPE_DEFINITION, WebhookInstance, WebhookRejection, canonical_json_sha256, catalog,
    http::MAX_HTTP_BODY_BYTES,
};
use crate::state::ConnectorConfigError;

pub const CREATE_CHECKOUT_SESSION_OPERATION: &str = "checkout.create_session";
pub const COMPLETED_WEBHOOK_OPERATION: &str = "checkout.completed_webhook";
pub const STRIPE_OPERATION_VERSION: &str = "v1";
pub const COMPLETED_WEBHOOK_TRIGGER: &str = "checkout.session.completed";
pub const STRIPE_TRIGGER_VERSION: &str = "1.0.0";

const STRIPE_API_ORIGIN: &str = "https://api.stripe.com";
const WEBHOOK_TIMESTAMP_TOLERANCE: i64 = 300;
/// The SemVer core of the declared request shape, which changes when this
/// module's request does.  `STRIPE_OPERATION_VERSION` remains the deployment
/// identity that enters the configuration fingerprint.
const CHECKOUT_REQUEST_SHAPE_VERSION: &str = "1.0.0";
/// Stripe documents this header as the idempotency key binding for every
/// Checkout Session create.
const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

#[cfg(test)]
type HmacSha256 = Hmac<Sha256>;

/// This module's static declaration (spec 010 §4): the reference
/// processor-backed connector, on Stripe's one fixed API origin.
pub(crate) fn connector() -> &'static Connector {
    static CONNECTOR: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        Connector::declare(
            STRIPE_DEFINITION.module_name,
            STRIPE_DEFINITION.semantic_version,
        )
        .origin(OriginSpec::fixed(STRIPE_API_ORIGIN).expect("Stripe's fixed API origin is valid"))
        .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
        .operation(checkout_session_declaration(None).expect("the Checkout declaration is valid"))
        .trigger(
            Trigger::webhook(
                COMPLETED_WEBHOOK_TRIGGER,
                STRIPE_TRIGGER_VERSION,
                completed_webhook_verification(),
            )
            .expect("the Checkout completion trigger is valid")
            .with_raw_body_max_bytes(MAX_HTTP_BODY_BYTES)
            .expect("the shared body ceiling is a valid trigger ceiling"),
        )
        .build()
        .expect("the Stripe declaration is valid")
    });
    &CONNECTOR
}

/// Stripe's inbound signature scheme: one `Stripe-Signature` header carrying
/// `t=<unix seconds>` and one or more `v1=<hex>` digests of
/// `<timestamp>.<raw body>`, accepted inside a five-minute window.
fn completed_webhook_verification() -> WebhookVerifier {
    WebhookVerifier::hmac_timestamped("Stripe-Signature")
        .expect("a static header name is valid")
        .signature_element("v1")
        .timestamp_element("t")
        .separator(".")
        .encoding(SignatureEncoding::Hex)
        .tolerance(Duration::from_secs(WEBHOOK_TIMESTAMP_TOLERANCE as u64))
        .build()
        .expect("the Stripe signature scheme is a valid declaration")
}

/// The documented Checkout Session create: one POST, on Stripe's fixed origin,
/// with a form-encoded body this module's processor assembles.
///
/// `api_version` is deployment material — the account's pinned API version —
/// so the static declaration is built without it and one instance's compiled
/// operation is built with it.
fn checkout_session_declaration(
    api_version: Option<&str>,
) -> Result<SdkOperation, donat_connectors::sdk::operation::OperationError> {
    let mut builder =
        SdkOperation::post(CREATE_CHECKOUT_SESSION_OPERATION, "/v1/checkout/sessions")
            .version(CHECKOUT_REQUEST_SHAPE_VERSION)
            .processor_body("application/x-www-form-urlencoded")
            .success_statuses([StatusCode::OK])
            // Stripe documents `Idempotency-Key` on every POST, keys retained
            // for 24 hours and unique per account, so a Checkout create is
            // `ProviderIdempotent::ExplicitKey` and a durable activity may send
            // it again after an ambiguous loss. The clock safety margin is
            // Donat policy rather than provider evidence, and is strictly
            // smaller than the documented retention.
            .effect(Effect::provider_idempotent_explicit_key(
                ExplicitKeyEvidence::documented(
                    IdempotencyBinding::header(IDEMPOTENCY_KEY.as_str())?,
                    "stripe account",
                    Duration::from_secs(24 * 60 * 60),
                    Duration::from_secs(300),
                    "Stripe documents the Idempotency-Key header on POST requests and retains saved keys for 24 hours",
                )?,
            ));
    if let Some(api_version) = api_version {
        builder = builder.static_header("Stripe-Version", api_version);
    }
    builder.build()
}

/// This module's own deploy-time metadata rules.
pub(crate) fn validate_instance_metadata(
    instance: &donat_metadata::ConnectorInstance,
    path: &str,
    errors: &mut Vec<ConnectorConfigError>,
) {
    let config = &instance.config;
    if config.secret_key.is_none() {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.secret_key"),
            "secret_key is required for the stripe connector",
        ));
    }
    if config.webhook_secret.is_none() {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.webhook_secret"),
            "webhook_secret is required for the stripe connector",
        ));
    }
    // Stripe authenticates with a secret key, so this module has no request to
    // put an OAuth2 access token on. A declaration nothing reads is a defect
    // ([[034-a-declaration-the-runtime-ignores-is-a-defect]]), so it is refused
    // here rather than accepted and ignored.
    if config.oauth2.is_some() {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.oauth2"),
            "the stripe connector authenticates with `secret_key` and cannot apply an OAuth2 \
             credential; remove `config.oauth2`",
        ));
    }
    if config.api_version.as_deref().is_none_or(str::is_empty) {
        errors.push(ConnectorConfigError::new(
            format!("{path}.config.api_version"),
            "api_version is required for the stripe connector",
        ));
    }
    if let Err(error) = validate_stripe_instance_metadata(config, &instance.operations) {
        errors.push(ConnectorConfigError::new(
            format!("{path}.operations"),
            error.to_string(),
        ));
    }
}

/// The small, typed checkout input accepted by the compiled operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutSessionInput {
    pub mode: CheckoutMode,
    pub success_url: String,
    pub cancel_url: String,
    pub client_reference_id: Uuid,
    pub line_items: Vec<CheckoutLineItem>,
}

/// Stripe Checkout modes supported by this narrow operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutMode {
    Payment,
    Subscription,
    Setup,
}

impl CheckoutMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Payment => "payment",
            Self::Subscription => "subscription",
            Self::Setup => "setup",
        }
    }

    fn parse(value: &str) -> Result<Self, ConnectorFailure> {
        match value {
            "payment" => Ok(Self::Payment),
            "subscription" => Ok(Self::Subscription),
            "setup" => Ok(Self::Setup),
            _ => Err(validation_failure("Stripe Checkout mode is unsupported")),
        }
    }
}

/// One price-backed line item. Inline price data and arbitrary product fields
/// are deliberately outside the Phase-1 contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutLineItem {
    pub price: String,
    pub quantity: u64,
}

/// The subset of a created Checkout Session that can safely cross the
/// connector boundary into a process result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckoutSession {
    pub id: String,
    pub url: String,
    pub status: String,
    pub expires_at: i64,
}

impl CheckoutSessionInput {
    pub fn from_json(input: JsonValue) -> Result<Self, ConnectorFailure> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            mode: String,
            success_url: String,
            cancel_url: String,
            client_reference_id: String,
            line_items: Vec<LineItem>,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LineItem {
            price: String,
            quantity: u64,
        }

        let input: Input = serde_json::from_value(input).map_err(|_| {
            validation_failure("Stripe Checkout input does not match the declared contract")
        })?;
        validate_redirect_url(&input.success_url)?;
        validate_redirect_url(&input.cancel_url)?;
        if input.line_items.is_empty() {
            return Err(validation_failure(
                "Stripe Checkout requires at least one line item",
            ));
        }
        let line_items = input
            .line_items
            .into_iter()
            .map(|item| {
                if item.price.is_empty() || item.quantity == 0 {
                    return Err(validation_failure(
                        "Stripe Checkout line items require a price and positive quantity",
                    ));
                }
                Ok(CheckoutLineItem {
                    price: item.price,
                    quantity: item.quantity,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let client_reference_id = Uuid::parse_str(&input.client_reference_id).map_err(|_| {
            validation_failure("Stripe Checkout client_reference_id must be a UUID")
        })?;
        Ok(Self {
            mode: CheckoutMode::parse(&input.mode)?,
            success_url: input.success_url,
            cancel_url: input.cancel_url,
            client_reference_id,
            line_items,
        })
    }
}

/// Static configuration failures are startup/validate failures, never
/// activity failures.  Their messages contain no resolved environment value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StripeConfigError {
    message: &'static str,
}

impl StripeConfigError {
    fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl fmt::Display for StripeConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for StripeConfigError {}

/// Validate the metadata-only surface of a Stripe instance.  The connector
/// has a fixed provider origin, so accepting an endpoint/header/network field
/// here would turn it into the generic HTTP module it intentionally is not.
pub(crate) fn validate_stripe_instance_metadata(
    config: &ConnectorConfig,
    operations: &[ConnectorOperation],
) -> Result<(), StripeConfigError> {
    if config.base_url.is_some() {
        return Err(StripeConfigError::new(
            "stripe connector does not accept base_url",
        ));
    }
    if config.network_policy.is_some() {
        return Err(StripeConfigError::new(
            "stripe connector does not accept network_policy",
        ));
    }
    if !config.headers.is_empty() {
        return Err(StripeConfigError::new(
            "stripe connector does not accept configured headers",
        ));
    }
    let mut declared = HashSet::new();
    for operation in operations {
        if !declared.insert(operation.name.as_str()) {
            return Err(StripeConfigError::new(
                "stripe connector operation is declared more than once",
            ));
        }
        if operation.name != CREATE_CHECKOUT_SESSION_OPERATION {
            return Err(StripeConfigError::new(
                "stripe connector operation is not compiled into this binary",
            ));
        }
        if !matches!(&operation.profile, ConnectorOperationProfile::Undeclared(_)) {
            return Err(StripeConfigError::new(
                "stripe checkout operation has no configurable HTTP profile",
            ));
        }
        let Some(capacity) = operation.capacity() else {
            return Err(StripeConfigError::new(
                "capacity is required for every connector operation",
            ));
        };
        if capacity.max_in_flight == 0
            || capacity.rate_limit.permits == 0
            || capacity.rate_limit.burst == 0
            || !valid_rate_period(&capacity.rate_limit.per)
        {
            return Err(StripeConfigError::new(
                "stripe connector operation capacity is invalid",
            ));
        }
    }
    Ok(())
}

/// The compiled Stripe Checkout module.  Its secrets remain private and are
/// resolved only from the named environment variables in deployment metadata.
pub struct StripeConnector {
    /// The API key, held as an SDK credential: this module never formats an
    /// `Authorization` header itself, it declares the plan that applies one.
    credential: Credential,
    /// The inbound signing secret, held so it can authenticate raw bytes and
    /// not so it can be read: the SDK's verifier takes a [`Secret`] and gives
    /// nothing back.
    webhook_secret: Secret,
    origin: Origin,
    /// The one declared request this connector can make.
    checkout_session: SdkOperation,
    transport: ReqwestTransport,
}

impl StripeConnector {
    pub(crate) fn from_metadata_config(
        config: &ConnectorConfig,
    ) -> Result<Self, StripeConfigError> {
        validate_stripe_instance_metadata(config, &[])?;
        let secret_key = config
            .secret_key
            .as_ref()
            .ok_or_else(|| StripeConfigError::new("stripe secret_key is required"))
            .and_then(|reference| {
                std::env::var(&reference.value_from_env).map_err(|_| {
                    StripeConfigError::new("stripe secret_key environment value is unavailable")
                })
            })?;
        let webhook_secret = config
            .webhook_secret
            .as_ref()
            .ok_or_else(|| StripeConfigError::new("stripe webhook_secret is required"))
            .and_then(|reference| {
                std::env::var(&reference.value_from_env).map_err(|_| {
                    StripeConfigError::new("stripe webhook_secret environment value is unavailable")
                })
            })?;
        let api_version = config
            .api_version
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| StripeConfigError::new("stripe api_version is required"))?;
        Self::new(secret_key, webhook_secret, api_version, STRIPE_API_ORIGIN)
    }

    #[cfg(test)]
    fn for_test(
        secret_key: &str,
        webhook_secret: &str,
        api_version: &str,
    ) -> Result<Self, StripeConfigError> {
        Self::new(
            secret_key.to_owned(),
            webhook_secret.to_owned(),
            api_version,
            STRIPE_API_ORIGIN,
        )
    }

    #[cfg(test)]
    fn with_test_endpoint(
        secret_key: &str,
        webhook_secret: &str,
        api_version: &str,
        base_url: &str,
    ) -> Result<Self, StripeConfigError> {
        Self::new(
            secret_key.to_owned(),
            webhook_secret.to_owned(),
            api_version,
            base_url,
        )
    }

    fn new(
        secret_key: impl Into<String>,
        webhook_secret: impl Into<String>,
        api_version: &str,
        base_url: &str,
    ) -> Result<Self, StripeConfigError> {
        let secret_key = secret_key.into();
        let webhook_secret = webhook_secret.into();
        if secret_key.is_empty() {
            return Err(StripeConfigError::new("stripe secret_key is required"));
        }
        if webhook_secret.is_empty() {
            return Err(StripeConfigError::new("stripe webhook_secret is required"));
        }
        let api_version = HeaderValue::from_str(api_version)
            .map_err(|_| StripeConfigError::new("stripe api_version is invalid"))?
            .to_str()
            .expect("validated header value is visible ASCII")
            .to_owned();
        let base_url = Url::parse(base_url)
            .map_err(|_| StripeConfigError::new("stripe fixed API origin is invalid"))?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(StripeConfigError::new("stripe fixed API origin is invalid"));
        }
        let origin = {
            let mut origin = base_url.clone();
            origin.set_path("/");
            Origin::parse(origin.as_str())
                .map_err(|_| StripeConfigError::new("stripe fixed API origin is invalid"))?
        };
        // The same declaration the static connector publishes, plus this
        // deployment's pinned API version.
        let checkout_session = checkout_session_declaration(Some(&api_version))
            .map_err(|_| StripeConfigError::new("stripe api_version is invalid"))?;
        Ok(Self {
            credential: Credential::secret(secret_key),
            webhook_secret: Secret::new(webhook_secret),
            origin,
            checkout_session,
            transport: ReqwestTransport::new(),
        })
    }

    pub async fn create_checkout_session(
        &self,
        input: CheckoutSessionInput,
        idempotency_key: &str,
        deadline: tokio::time::Instant,
    ) -> Result<CheckoutSession, ConnectorFailure> {
        if deadline <= tokio::time::Instant::now() {
            return Err(timeout_failure());
        }
        // A first lookup occurs before we construct the outbound body, then a
        // second one immediately before connecting pins the result the
        // transport uses. The fixed host is not caller input, but the same
        // rebinding defense still applies to production traffic.
        self.resolve_under_deadline(deadline).await?;
        // Stripe's Checkout API is form-encoded with indexed repeated keys, so
        // the body is assembled here rather than by a JSON template. Every key
        // is a literal written from Stripe's published contract.
        let body = {
            let mut form = url::form_urlencoded::Serializer::new(String::new());
            form.append_pair("mode", input.mode.as_str());
            form.append_pair("success_url", &input.success_url);
            form.append_pair("cancel_url", &input.cancel_url);
            form.append_pair(
                "client_reference_id",
                &input.client_reference_id.to_string(),
            );
            for (index, item) in input.line_items.iter().enumerate() {
                form.append_pair(&format!("line_items[{index}][price]"), &item.price);
                form.append_pair(
                    &format!("line_items[{index}][quantity]"),
                    &item.quantity.to_string(),
                );
            }
            form.finish().into_bytes()
        };
        if body.len() > MAX_HTTP_BODY_BYTES {
            return Err(invariant_failure(
                "Stripe Checkout request exceeds the 1 MiB limit",
            ));
        }
        let mut deployment = HeaderMap::new();
        deployment.insert(
            IDEMPOTENCY_KEY,
            HeaderValue::from_str(idempotency_key)
                .map_err(|_| invariant_failure("connector activity idempotency key is invalid"))?,
        );
        let mut request = self.checkout_session.plan_processor_request(
            &self.origin,
            &JsonValue::Null,
            &deployment,
            body,
        )?;
        AuthPlan::bearer().apply(&self.credential, &mut request, None)?;

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
            TransportErrorKind::ResponseTooLarge => {
                validation_failure("Stripe Checkout response exceeds the 1 MiB limit")
            }
        })?;
        self.validate_peer(&destination, response.peer())?;
        decode_checkout_response(response)
    }

    async fn resolve_under_deadline(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Vec<IpAddr>, ConnectorFailure> {
        let url = self.origin.as_url();
        let host = url
            .host_str()
            .expect("validated Stripe API origin has host");
        let port = url
            .port_or_known_default()
            .expect("validated Stripe API origin has port");
        tokio::time::timeout_at(deadline, SystemResolver.resolve(host, port))
            .await
            .map_err(|_| timeout_failure())?
            .map_err(|_| transport_failure())
    }

    /// The connection must land on one of the addresses this request already
    /// resolved, so a name cannot resolve to one address for validation and
    /// another for transport. Egress reachability itself is a network-layer
    /// concern.
    fn validate_peer(
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

    pub fn verify_completed_webhook(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
    ) -> Result<VerifiedInboundEvent, WebhookRejection> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| WebhookRejection::TimestampOutOfTolerance)?
            .as_secs()
            .try_into()
            .map_err(|_| WebhookRejection::TimestampOutOfTolerance)?;
        self.verify_completed_webhook_at_inner(headers, raw_body, now)
    }

    #[cfg(test)]
    fn verify_completed_webhook_at(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
        now: i64,
    ) -> Result<VerifiedInboundEvent, WebhookRejection> {
        self.verify_completed_webhook_at_inner(headers, raw_body, now)
    }

    fn verify_completed_webhook_at_inner(
        &self,
        headers: &HeaderMap,
        raw_body: &[u8],
        now: i64,
    ) -> Result<VerifiedInboundEvent, WebhookRejection> {
        // The declared trigger owns the raw-body ceiling and the signature
        // scheme; this module supplies the secret and the receiving clock. The
        // scheme it declares is the one Stripe publishes: HMAC-SHA256 over
        // `<timestamp>.<raw body>`, hex, inside a five-minute window.
        connector()
            .trigger(COMPLETED_WEBHOOK_TRIGGER)
            .expect("the Stripe declaration publishes its completion trigger")
            .verify(headers, raw_body, &self.webhook_secret, now)?;

        // Signature verification above intentionally precedes every JSON
        // operation. A malformed or hostile unverified payload is never
        // transformed into a process-visible object.
        #[derive(Deserialize)]
        struct Event {
            id: String,
            #[serde(rename = "type")]
            event_type: String,
            data: EventData,
        }
        #[derive(Deserialize)]
        struct EventData {
            object: SessionObject,
        }
        #[derive(Deserialize)]
        struct SessionObject {
            object: String,
            id: String,
            client_reference_id: String,
            payment_status: String,
        }

        let event: Event =
            serde_json::from_slice(raw_body).map_err(|_| WebhookRejection::MalformedPayload)?;
        if event.event_type != COMPLETED_WEBHOOK_TRIGGER
            || event.data.object.object != "checkout.session"
            || event.id.is_empty()
            || event.data.object.id.is_empty()
            || event.data.object.payment_status.is_empty()
        {
            return Err(WebhookRejection::UnsupportedEvent);
        }
        let client_reference_id = Uuid::parse_str(&event.data.object.client_reference_id)
            .map_err(|_| WebhookRejection::UnsupportedEvent)?;
        let provider_event_id = BoundedString::try_new(&event.id, 256)
            .map_err(|_| WebhookRejection::UnsupportedEvent)?;
        let event_type = BoundedString::try_new(&event.event_type, 256)
            .map_err(|_| WebhookRejection::UnsupportedEvent)?;
        let output = TypedValue::Object(BTreeMap::from([
            ("provider_event_id".to_owned(), TypedValue::String(event.id)),
            (
                "event_type".to_owned(),
                TypedValue::String(event.event_type),
            ),
            (
                "checkout_session_id".to_owned(),
                TypedValue::String(event.data.object.id),
            ),
            (
                "client_reference_id".to_owned(),
                TypedValue::String(client_reference_id.to_string()),
            ),
            (
                "payment_status".to_owned(),
                TypedValue::String(event.data.object.payment_status),
            ),
        ]));
        let redacted_metadata = TypedValue::Object(BTreeMap::from([(
            "normalized_event".to_owned(),
            TypedValue::String(COMPLETED_WEBHOOK_TRIGGER.to_owned()),
        )]));
        VerifiedInboundEvent::try_new(
            provider_event_id,
            event_type,
            output,
            Hash256::new(Sha256::digest(raw_body).into()),
            redacted_metadata,
        )
        .map_err(|_| WebhookRejection::UnsupportedEvent)
    }
}

// ---------------------------------------------------------------------------
// The deployment-selected instance this module publishes to the registry.
// ---------------------------------------------------------------------------

/// A selected Stripe Checkout operation. The operation name is still checked
/// at dispatch even after startup validation so a future job cannot reach an
/// unenabled provider capability.
struct CompiledStripeOperation {
    configuration_fingerprint: String,
    serialization_key_input: Option<String>,
}

/// One deployment-selected instance of the `stripe` module.
pub(crate) struct StripeInstance {
    connector: StripeConnector,
    operations: BTreeMap<String, CompiledStripeOperation>,
    webhook: CompiledWebhookTrigger,
}

/// Compile one instance of this module from validated deployment metadata.
pub(crate) fn build_registered_instance(
    context: &mut ModuleContext<'_>,
) -> Result<Box<dyn RegisteredConnector>, ConnectorRegistryError> {
    let instance = context.instance;
    let invalid = |message: String| ConnectorRegistryError::InvalidConfiguration {
        instance: instance.name.clone(),
        message,
    };
    validate_stripe_instance_metadata(&instance.config, &instance.operations)
        .map_err(|error| invalid(error.to_string()))?;
    let connector = StripeConnector::from_metadata_config(&instance.config)
        .map_err(|error| invalid(error.to_string()))?;
    let webhook_spec = catalog::compile_stripe_checkout_completed_trigger_spec(
        context.metadata,
        context.definition,
    )
    .map_err(invalid)?;
    let webhook = CompiledWebhookTrigger {
        source_name: context.source_name.to_owned(),
        spec: Arc::new(webhook_spec),
        configuration_fingerprint: stripe_webhook_configuration_fingerprint(
            context.definition,
            &instance.config,
        ),
    };
    let mut operations = BTreeMap::new();
    for operation in &instance.operations {
        // The declaration is the gate, at build as well as at validation: an
        // operation this connector does not declare, or declares as
        // inventory-only, never becomes a registry instance.
        context
            .connector
            .admit_operation(&operation.name)
            .map_err(|rejection| invalid(rejection.message().to_owned()))?;
        let compiled = CompiledStripeOperation {
            configuration_fingerprint: stripe_configuration_fingerprint(
                context.definition,
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
    Ok(Box::new(StripeInstance {
        connector,
        operations,
        webhook,
    }))
}

impl RegisteredConnector for StripeInstance {
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
            if operation != CREATE_CHECKOUT_SESSION_OPERATION {
                return Err(ConnectorFailure::new(
                    ConnectorErrorClass::Invariant,
                    "connector_invariant",
                    "connector operation is not compiled into this binary",
                ));
            }
            execute_checkout_from_json(&self.connector, input, idempotency_key, deadline).await
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
            .and_then(|operation| operation.serialization_key_input.as_deref())
    }

    fn webhook(&self) -> Option<WebhookInstance<'_>> {
        Some(WebhookInstance {
            source_name: &self.webhook.source_name,
            // Stripe is the one module whose Process-owned inbound transaction
            // has landed, so its verified deliveries are correlated and
            // acknowledged rather than answered with `503` (spec 013 §0).
            delivery: super::WebhookDelivery::Correlated {
                trigger: self.webhook.spec.as_ref(),
                connector: &self.connector,
            },
        })
    }

    fn trigger_spec(&self, source_name: &str, trigger: TriggerId) -> Option<Arc<TriggerSpec>> {
        (self.webhook.source_name == source_name && self.publishes(trigger))
            .then(|| self.webhook.spec.clone())
    }

    fn trigger_configuration_fingerprint(&self, trigger: TriggerId) -> Option<&str> {
        self.publishes(trigger)
            .then_some(self.webhook.configuration_fingerprint.as_str())
    }
}

impl StripeInstance {
    /// Whether this instance's compiled trigger is the one being asked for.
    fn publishes(&self, trigger: TriggerId) -> bool {
        matches!(
            self.webhook.spec.as_ref(),
            TriggerSpec::Webhook {
                trigger: candidate,
                ..
            } if *candidate == trigger
        )
    }
}

#[derive(Serialize)]
struct StripeConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    operation_name: &'a str,
    operation_version: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    api_version: &'a str,
    secret_key_environment: &'a str,
    webhook_secret_environment: &'a str,
    capacity: &'a donat_metadata::ConnectorCapacity,
}

#[derive(Serialize)]
struct StripeWebhookConfigurationFingerprint<'a> {
    module_name: &'a str,
    module_semantic_version: &'a str,
    runtime_abi: u32,
    trigger_name: &'a str,
    trigger_version: &'a str,
    endpoint_identity: &'a str,
    credential_identity: &'a str,
    api_version: &'a str,
    webhook_secret_environment: &'a str,
}

fn stripe_configuration_fingerprint(
    definition: ConnectorDefinition,
    config: &ConnectorConfig,
    operation: &ConnectorOperation,
) -> String {
    let secret_key_environment = &config
        .secret_key
        .as_ref()
        .expect("Stripe secret key was validated before fingerprinting")
        .value_from_env;
    let webhook_secret_environment = &config
        .webhook_secret
        .as_ref()
        .expect("Stripe webhook secret was validated before fingerprinting")
        .value_from_env;
    let api_version = config
        .api_version
        .as_deref()
        .expect("Stripe API version was validated before fingerprinting");
    let capacity = operation
        .capacity()
        .expect("Stripe operation capacity was validated before fingerprinting");
    let canonical = StripeConfigurationFingerprint {
        module_name: definition.module_name,
        module_semantic_version: definition.semantic_version,
        runtime_abi: definition.runtime_abi,
        operation_name: &operation.name,
        operation_version: STRIPE_OPERATION_VERSION,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        api_version,
        secret_key_environment,
        webhook_secret_environment,
        capacity,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated Stripe fingerprint fields always serialize to JSON");
    format!("{:x}", Sha256::digest(bytes))
}

fn stripe_webhook_configuration_fingerprint(
    definition: ConnectorDefinition,
    config: &ConnectorConfig,
) -> String {
    let webhook_secret_environment = &config
        .webhook_secret
        .as_ref()
        .expect("Stripe webhook secret was validated before fingerprinting")
        .value_from_env;
    let api_version = config
        .api_version
        .as_deref()
        .expect("Stripe API version was validated before fingerprinting");
    let canonical = StripeWebhookConfigurationFingerprint {
        module_name: definition.module_name,
        module_semantic_version: definition.semantic_version,
        runtime_abi: definition.runtime_abi,
        trigger_name: COMPLETED_WEBHOOK_TRIGGER,
        trigger_version: STRIPE_TRIGGER_VERSION,
        endpoint_identity: &config.endpoint_identity,
        credential_identity: &config.credential_identity,
        api_version,
        webhook_secret_environment,
    };
    let bytes = serde_json::to_vec(&canonical)
        .expect("validated Stripe webhook fingerprint fields serialize");
    format!("{:x}", Sha256::digest(bytes))
}

impl ConnectorModule for StripeConnector {
    fn definition(&self) -> ConnectorDefinition {
        ConnectorDefinition {
            module_name: "stripe",
            semantic_version: "0.1.0",
            runtime_abi: 1,
        }
    }
}

fn decode_checkout_response(
    response: RawHttpResponse,
) -> Result<CheckoutSession, ConnectorFailure> {
    if response.status != StatusCode::OK {
        return Err(match response.status.as_u16() {
            408 => timeout_failure(),
            429 => ConnectorFailure::new(
                ConnectorErrorClass::Http429,
                "connector_http_429",
                "Stripe rate limited the Checkout request",
            )
            .with_retry_after(retry_after(response.headers())),
            401 | 403 => ConnectorFailure::new(
                ConnectorErrorClass::Authentication,
                "connector_http_authentication",
                "Stripe rejected connector authentication",
            ),
            400..=499 => validation_failure("Stripe rejected the declared Checkout request"),
            500..=599 => ConnectorFailure::new(
                ConnectorErrorClass::Http5xx,
                "connector_http_5xx",
                "Stripe returned a server error",
            ),
            _ => ConnectorFailure::new(
                ConnectorErrorClass::Permanent,
                "connector_unsupported_http_status",
                "Stripe returned an unsupported HTTP status",
            ),
        });
    }
    #[derive(Deserialize)]
    struct Session {
        id: String,
        url: String,
        status: String,
        expires_at: i64,
    }
    let session: Session = serde_json::from_slice(response.body())
        .map_err(|_| validation_failure("Stripe returned malformed Checkout Session JSON"))?;
    if session.id.is_empty() || session.url.is_empty() || session.status.is_empty() {
        return Err(validation_failure(
            "Stripe Checkout response did not satisfy the declared contract",
        ));
    }
    Ok(CheckoutSession {
        id: session.id,
        url: session.url,
        status: session.status,
        expires_at: session.expires_at,
    })
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

fn validate_redirect_url(value: &str) -> Result<(), ConnectorFailure> {
    let url = Url::parse(value)
        .map_err(|_| validation_failure("Stripe Checkout redirect URL must be absolute HTTP(S)"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(validation_failure(
            "Stripe Checkout redirect URL must be absolute HTTP(S)",
        ));
    }
    Ok(())
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

pub(crate) async fn execute_checkout_from_json(
    connector: &StripeConnector,
    input: JsonValue,
    idempotency_key: &str,
    deadline: tokio::time::Instant,
) -> Result<ConnectorSuccess, ConnectorFailure> {
    let fingerprint = canonical_json_sha256(&input);
    let input = CheckoutSessionInput::from_json(input)?;
    let session = connector
        .create_checkout_session(input, idempotency_key, deadline)
        .await?;
    Ok(ConnectorSuccess {
        output: json!({
            "id": session.id,
            "url": session.url,
            "status": session.status,
            "expires_at": session.expires_at,
        }),
        request_fingerprint: fingerprint,
    })
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::State,
        http::{HeaderMap, Request, StatusCode},
        response::IntoResponse,
        routing::post,
    };
    use hmac::Mac;
    use serde_json::json;
    use tokio::sync::Mutex;
    use uuid::Uuid;

    use super::{
        super::{ConnectorErrorClass, canonical_json_sha256},
        CheckoutLineItem, CheckoutMode, CheckoutSessionInput, HmacSha256, StripeConnector,
        TypedValue,
    };

    const WEBHOOK_SECRET: &str = "whsec_independent_test_secret";
    const COMPLETED_BODY: &[u8] = br#"{"id":"evt_test_42","type":"checkout.session.completed","data":{"object":{"object":"checkout.session","id":"cs_test_42","client_reference_id":"00000000-0000-4000-8000-000000000042","payment_status":"paid"}}}"#;
    const COMPLETED_SIGNATURE: &str =
        "t=1700000000,v1=63daf41047a8f0c622caed4542cbb3d34b1b08296e3a0e0eab30d94533dc891d";

    struct LocalServer {
        base_url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl LocalServer {
        async fn start(app: Router) -> Self {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("local Stripe stub listener binds");
            let address = listener
                .local_addr()
                .expect("local Stripe stub listener exposes its address");
            let task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("local Stripe stub serves requests");
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

    fn checkout_input() -> CheckoutSessionInput {
        CheckoutSessionInput {
            mode: CheckoutMode::Payment,
            success_url: "https://merchant.example.test/checkout/success".to_owned(),
            cancel_url: "https://merchant.example.test/checkout/cancel".to_owned(),
            client_reference_id: Uuid::parse_str("00000000-0000-4000-8000-000000000042")
                .expect("fixed UUID literal is valid"),
            line_items: vec![CheckoutLineItem {
                price: "price_monthly_basic".to_owned(),
                quantity: 2,
            }],
        }
    }

    fn deadline() -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(2)
    }

    fn signature_headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Stripe-Signature",
            value.parse().expect("test signature syntax is valid"),
        );
        headers
    }

    fn signed_signature_headers(timestamp: i64, raw_body: &[u8]) -> HeaderMap {
        let mut mac = HmacSha256::new_from_slice(WEBHOOK_SECRET.as_bytes())
            .expect("test webhook secret is a valid HMAC key");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(raw_body);
        let signature = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        signature_headers(&format!("t={timestamp},v1={signature}"))
    }

    #[tokio::test]
    async fn stripe_checkout_posts_form_and_returns_typed_session() {
        // This fails if the narrow module uses JSON, changes a documented form
        // field, lets a caller choose a URL/method/header, or returns an
        // untyped provider object instead of its documented Session fields.
        #[derive(Default)]
        struct ObservedRequest {
            method: Option<String>,
            path: Option<String>,
            authorization: Option<String>,
            idempotency_key: Option<String>,
            content_type: Option<String>,
            api_version: Option<String>,
            form: BTreeMap<String, String>,
        }

        async fn create_session(
            State(observed): State<std::sync::Arc<Mutex<ObservedRequest>>>,
            request: Request<Body>,
        ) -> impl IntoResponse {
            let (parts, body) = request.into_parts();
            let form = url::form_urlencoded::parse(
                &to_bytes(body, 1024 * 1024)
                    .await
                    .expect("bounded test request body is readable"),
            )
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
            let mut observed = observed.lock().await;
            observed.method = Some(parts.method.to_string());
            observed.path = Some(parts.uri.path().to_owned());
            observed.authorization = parts
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            observed.idempotency_key = parts
                .headers
                .get("idempotency-key")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            observed.content_type = parts
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            observed.api_version = parts
                .headers
                .get("stripe-version")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            observed.form = form;
            (
                StatusCode::OK,
                axum::Json(json!({
                    "id": "cs_test_42",
                    "url": "https://checkout.stripe.test/c/pay/cs_test_42",
                    "status": "open",
                    "expires_at": 1_700_000_600
                })),
            )
        }

        let observed = std::sync::Arc::new(Mutex::new(ObservedRequest::default()));
        let server = LocalServer::start(
            Router::new()
                .route("/v1/checkout/sessions", post(create_session))
                .with_state(observed.clone()),
        )
        .await;
        let connector = StripeConnector::with_test_endpoint(
            "sk_test_local_contract_key",
            WEBHOOK_SECRET,
            "2026-07-27",
            &server.base_url,
        )
        .expect("test-only local Stripe endpoint is valid");

        let session = connector
            .create_checkout_session(checkout_input(), "activity-00042", deadline())
            .await
            .expect("local Stripe contract response is accepted");

        assert_eq!(session.id, "cs_test_42");
        assert_eq!(session.url, "https://checkout.stripe.test/c/pay/cs_test_42");
        assert_eq!(session.status, "open");
        assert_eq!(session.expires_at, 1_700_000_600);

        let observed = observed.lock().await;
        assert_eq!(observed.method.as_deref(), Some("POST"));
        assert_eq!(observed.path.as_deref(), Some("/v1/checkout/sessions"));
        assert_eq!(
            observed.authorization.as_deref(),
            Some("Bearer sk_test_local_contract_key")
        );
        assert_eq!(observed.idempotency_key.as_deref(), Some("activity-00042"));
        assert_eq!(
            observed.content_type.as_deref(),
            Some("application/x-www-form-urlencoded")
        );
        assert_eq!(observed.api_version.as_deref(), Some("2026-07-27"));
        assert_eq!(
            observed.form,
            BTreeMap::from([
                ("mode".to_owned(), "payment".to_owned()),
                (
                    "success_url".to_owned(),
                    "https://merchant.example.test/checkout/success".to_owned(),
                ),
                (
                    "cancel_url".to_owned(),
                    "https://merchant.example.test/checkout/cancel".to_owned(),
                ),
                (
                    "client_reference_id".to_owned(),
                    "00000000-0000-4000-8000-000000000042".to_owned(),
                ),
                (
                    "line_items[0][price]".to_owned(),
                    "price_monthly_basic".to_owned(),
                ),
                ("line_items[0][quantity]".to_owned(), "2".to_owned()),
            ])
        );
    }

    #[tokio::test]
    async fn stripe_checkout_classifies_provider_failures_and_retains_supplied_idempotency_key() {
        // This fails if Phase-1 turns provider failures into a generic error,
        // retries a provider request itself, or changes the durable activity
        // key supplied by the process worker between attempts.
        #[derive(Default)]
        struct ObservedFailures {
            attempts: AtomicUsize,
            idempotency_keys: Mutex<Vec<String>>,
        }

        async fn respond(
            State(observed): State<std::sync::Arc<ObservedFailures>>,
            request: Request<Body>,
        ) -> impl IntoResponse {
            let attempt = observed.attempts.fetch_add(1, Ordering::SeqCst);
            observed.idempotency_keys.lock().await.push(
                request
                    .headers()
                    .get("idempotency-key")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_owned(),
            );
            match attempt {
                0 => (StatusCode::BAD_REQUEST, HeaderMap::new(), "{}"),
                1 => (StatusCode::UNAUTHORIZED, HeaderMap::new(), "{}"),
                2 => {
                    let mut headers = HeaderMap::new();
                    headers.insert("Retry-After", "7".parse().expect("valid retry header"));
                    (StatusCode::TOO_MANY_REQUESTS, headers, "{}")
                }
                _ => (StatusCode::SERVICE_UNAVAILABLE, HeaderMap::new(), "{}"),
            }
        }

        let observed = std::sync::Arc::new(ObservedFailures::default());
        let server = LocalServer::start(
            Router::new()
                .route("/v1/checkout/sessions", post(respond))
                .with_state(observed.clone()),
        )
        .await;
        let connector = StripeConnector::with_test_endpoint(
            "sk_test_local_contract_key",
            WEBHOOK_SECRET,
            "2026-07-27",
            &server.base_url,
        )
        .expect("test-only local Stripe endpoint is valid");

        let failures = [
            ConnectorErrorClass::Validation,
            ConnectorErrorClass::Authentication,
            ConnectorErrorClass::Http429,
            ConnectorErrorClass::Http5xx,
        ];
        for expected in failures {
            let failure = connector
                .create_checkout_session(checkout_input(), "activity-stable-key", deadline())
                .await
                .expect_err("the local provider status is not a Checkout success");
            assert_eq!(failure.class(), expected);
            if expected == ConnectorErrorClass::Http429 {
                assert_eq!(failure.retry_after(), Some(Duration::from_secs(7)));
            }
        }
        assert_eq!(
            *observed.idempotency_keys.lock().await,
            vec![
                "activity-stable-key".to_owned(),
                "activity-stable-key".to_owned(),
                "activity-stable-key".to_owned(),
                "activity-stable-key".to_owned(),
            ],
            "the module does not generate a new idempotency key for repeated logical delivery"
        );
        assert_eq!(
            observed.attempts.load(Ordering::SeqCst),
            4,
            "the connector never retries a provider response behind the process worker's back"
        );
    }

    #[test]
    fn stripe_request_fingerprint_is_independent_of_json_object_key_order() {
        let first = canonical_json_sha256(&json!({
            "mode": "payment",
            "line_items": [{ "price": "price_basic", "quantity": 1 }],
            "redirects": { "success": "https://merchant.example.test/success", "cancel": "https://merchant.example.test/cancel" }
        }));
        let reordered = canonical_json_sha256(&json!({
            "redirects": { "cancel": "https://merchant.example.test/cancel", "success": "https://merchant.example.test/success" },
            "line_items": [{ "quantity": 1, "price": "price_basic" }],
            "mode": "payment"
        }));

        assert_eq!(first, reordered);
    }

    #[test]
    fn stripe_webhook_verifies_raw_bytes_before_json_and_exposes_duplicate_identity() {
        // This fails if JSON is parsed before HMAC validation, if verification
        // uses a re-serialized body, or if an ingress caller cannot use the
        // stable provider event ID as its durable deduplication identity.
        let connector = StripeConnector::for_test("sk_test_unused", WEBHOOK_SECRET, "2026-07-27")
            .expect("fixed test config is valid");

        let first = connector
            .verify_completed_webhook_at(
                &signature_headers(COMPLETED_SIGNATURE),
                COMPLETED_BODY,
                1_700_000_120,
            )
            .expect("a correctly signed checkout completion verifies");
        let duplicate = connector
            .verify_completed_webhook_at(
                &signature_headers(COMPLETED_SIGNATURE),
                COMPLETED_BODY,
                1_700_000_120,
            )
            .expect("the same verified event remains available for durable deduplication");
        assert_eq!(first.provider_event_id(), "evt_test_42");
        assert_eq!(duplicate.provider_event_id(), first.provider_event_id());
        let TypedValue::Object(output) = first.output() else {
            panic!("verified webhook output is an object");
        };
        assert_eq!(
            output.get("checkout_session_id"),
            Some(&TypedValue::String("cs_test_42".to_owned()))
        );
        assert_eq!(
            output.get("client_reference_id"),
            Some(&TypedValue::String(
                Uuid::parse_str("00000000-0000-4000-8000-000000000042")
                    .expect("fixed UUID literal is valid")
                    .to_string()
            ))
        );
        assert_eq!(
            output.get("payment_status"),
            Some(&TypedValue::String("paid".to_owned()))
        );

        let mut modified = COMPLETED_BODY.to_vec();
        modified.push(b' ');
        let modified_error = connector
            .verify_completed_webhook_at(
                &signature_headers(COMPLETED_SIGNATURE),
                &modified,
                1_700_000_120,
            )
            .expect_err("a signature over different raw bytes is rejected");
        assert_eq!(modified_error.code(), "webhook_signature_invalid");

        let malformed_error = connector
            .verify_completed_webhook_at(
                &signature_headers(
                    "t=1700000000,v1=0000000000000000000000000000000000000000000000000000000000000000",
                ),
                br#"{"not":"json"#,
                1_700_000_120,
            )
            .expect_err("a malformed body with an invalid signature never reaches JSON parsing");
        assert_eq!(malformed_error.code(), "webhook_signature_invalid");
    }

    #[test]
    fn stripe_webhook_rejects_stale_timestamp_and_unsupported_object_after_verification() {
        // This fails if timestamp tolerance is ignored or an arbitrary signed
        // Stripe event object is allowed into a command-safe process payload.
        let connector = StripeConnector::for_test("sk_test_unused", WEBHOOK_SECRET, "2026-07-27")
            .expect("fixed test config is valid");
        let stale = connector
            .verify_completed_webhook_at(
                &signature_headers(COMPLETED_SIGNATURE),
                COMPLETED_BODY,
                1_700_000_301,
            )
            .expect_err("timestamps older than the five-minute tolerance are rejected");
        assert_eq!(stale.code(), "webhook_signature_expired");

        let future = connector
            .verify_completed_webhook_at(
                &signed_signature_headers(1_700_000_301, COMPLETED_BODY),
                COMPLETED_BODY,
                1_700_000_000,
            )
            .expect_err("a valid signature from far in the future is rejected");
        assert_eq!(future.code(), "webhook_signature_expired");

        const UNSUPPORTED_BODY: &[u8] = br#"{"id":"evt_test_other","type":"payment_intent.succeeded","data":{"object":{"object":"payment_intent","id":"pi_test_42","client_reference_id":"00000000-0000-4000-8000-000000000042","payment_status":"paid"}}}"#;
        const UNSUPPORTED_SIGNATURE: &str =
            "t=1700000000,v1=cf7e2e1de305dc691b009ed54ca05278b7597e6de4a23988335402dc6c41e80b";
        let unsupported = connector
            .verify_completed_webhook_at(
                &signature_headers(UNSUPPORTED_SIGNATURE),
                UNSUPPORTED_BODY,
                1_700_000_120,
            )
            .expect_err("only checkout.session.completed is normalized");
        assert_eq!(unsupported.code(), "webhook_event_unsupported");
    }
}
