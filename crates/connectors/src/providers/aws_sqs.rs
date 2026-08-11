//! Amazon SQS — send and receive on one configured queue.
//!
//! Written against Amazon's own published API reference. The requests use the
//! AWS JSON protocol AWS documents as the default: `POST /` on the Region's
//! endpoint, `Content-Type: application/x-amz-json-1.0`, and
//! `X-Amz-Target: AmazonSQS.<Action>`, with the queue URL as a body field.
//!
//! # `message.send` and the effect gate
//!
//! This is the first operation outside Stripe to reach
//! [`crate::sdk::EffectClass::ProviderIdempotentExplicitKey`] on documented
//! evidence, and it reaches it **only on a FIFO queue**.
//!
//! AWS documents the binding, its behaviour, and its retention:
//!
//! > "The token used for deduplication of sent messages. If a message with a
//! > particular `MessageDeduplicationId` is sent successfully, any messages sent
//! > with the same `MessageDeduplicationId` are accepted successfully but aren't
//! > delivered during the 5-minute deduplication interval."
//! > — *Amazon SQS API Reference*, `SendMessage`, `MessageDeduplicationId`
//!
//! > "Unlike standard queues, FIFO queues don't introduce duplicate messages.
//! > FIFO queues help you avoid sending duplicates to a queue. If you retry the
//! > `SendMessage` action within the 5-minute deduplication interval, Amazon SQS
//! > doesn't introduce any duplicates into the queue."
//! > — *Amazon SQS Developer Guide*, *Exactly-once processing*
//!
//! and AWS documents the edge the safety margin exists for:
//!
//! > "If a message is sent successfully but the acknowledgement is lost and the
//! > message is resent with the same `MessageDeduplicationId` after the
//! > deduplication interval, Amazon SQS can't detect duplicate messages."
//!
//! That last sentence is why the class needs a horizon and not just a key: the
//! retention is finite, so a durable activity that keeps retrying past it stops
//! being deduplicated. [`DEDUPLICATION_INTERVAL`] is AWS's five minutes,
//! [`CLOCK_SAFETY_MARGIN`] is Donat's own allowance for clock disagreement and
//! is strictly smaller, and a deployment's send horizon has to fit inside the
//! difference — checked at startup, not at send time.
//!
//! On a **standard** queue the same operation is `AtMostOnce` (ADR 063), because
//! AWS documents the opposite semantics:
//!
//! > "Standard queues ensure at-least-once message delivery, but due to the
//! > highly distributed architecture, more than one copy of a message might be
//! > delivered, and messages may occasionally arrive out of order."
//! > — *Amazon SQS Developer Guide*, *Amazon SQS standard queues*
//!
//! and `MessageDeduplicationId` "applies only to FIFO (first-in-first-out)
//! queues". A standard queue publishes no deduplication of any kind, so a
//! standard-queue send is admitted only where the Process activity referencing
//! it declared `at_most_once` and a route for an outcome nobody can know: Donat
//! sends it once or not at all, because the queue cannot absorb a second copy.
//! The queue type is deploy-time configuration and is validated at startup.
//!
//! # `message.delete`
//!
//! AWS documents `DeleteMessage` as safe to repeat — "If you use an old
//! `ReceiptHandle`, the request will succeed, but the message might not be
//! deleted" — but the JSON protocol expresses it as a `POST`, and the SDK
//! admits `ProviderIdempotent::NaturalMethod` only for `PUT` and `DELETE`
//! (ADR declarative-saas/042). There is no admitted class for "a `POST` the
//! provider documents as repeat-safe", so the operation is declared, typed,
//! tested, and `InventoryOnly`. ADR 063 does not admit it either: at-most-once
//! trades the retry away, and an operation AWS documents as safe to repeat needs
//! a class that keeps it. Admitting it needs evidence of documented
//! repeat-safety on a method HTTP does not define it for — a different decision.

use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};

use crate::providers::aws::{self, ConfigurationError};
use crate::sdk::connector::OperationRejection;
use crate::sdk::effect::{
    AbsenceSearch, ExplicitKeyEvidence, IdempotencyBinding, NoIdempotencyEvidence,
};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, OperationError};
use crate::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, CredentialSpec, Effect, Operation, Origin,
    OriginSpec, RequestPlan, Required,
};

pub const CONNECTOR_NAME: &str = "aws_sqs";
pub const CONNECTOR_VERSION: &str = "1.0.0";
pub const SERVICE: &str = "sqs";
const REQUEST_SHAPE_VERSION: &str = "1.0.0";
const HOST_TEMPLATE: &str = "sqs.{region}.amazonaws.com";
pub const REGION_CONFIGURATION_KEY: &str = "region";
/// AWS: `Content-Type: application/x-amz-json-1.0`.
const JSON_PROTOCOL_CONTENT_TYPE: &str = "application/x-amz-json-1.0";

/// The documented FIFO deduplication interval: "aren't delivered during the
/// 5-minute deduplication interval".
pub const DEDUPLICATION_INTERVAL: Duration = Duration::from_secs(300);

/// Donat's own allowance for clock disagreement between this engine and Amazon
/// SQS, strictly smaller than [`DEDUPLICATION_INTERVAL`]. It is policy rather
/// than provider evidence, and the effect gate refuses a margin that is not
/// strictly smaller than the documented retention.
pub const CLOCK_SAFETY_MARGIN: Duration = Duration::from_secs(60);

/// The longest a durable activity may keep resending one message under the same
/// deduplication id and still be deduplicated: the documented interval less the
/// clock safety margin.
pub const MAX_SEND_HORIZON: Duration =
    Duration::from_secs(DEDUPLICATION_INTERVAL.as_secs() - CLOCK_SAFETY_MARGIN.as_secs());

/// AWS: "The maximum size is 1 MiB or 1,048,576 bytes".
pub const PROVIDER_MAX_MESSAGE_BYTES: usize = 1_048_576;
/// Donat's default ceiling for one message body.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// AWS: "The maximum length of `MessageDeduplicationId` is 128 characters."
const MAX_DEDUPLICATION_ID_CHARS: usize = 128;
/// AWS: "Valid values: 1 to 10." for `MaxNumberOfMessages`.
const MAX_MESSAGES_PER_RECEIVE: i64 = 10;

/// Which delivery contract the configured queue publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueType {
    /// A FIFO queue: deduplicated within the documented interval.
    Fifo,
    /// A standard queue: at-least-once, no deduplication.
    Standard,
}

impl QueueType {
    pub fn parse(value: &str) -> Result<Self, ConfigurationError> {
        match value {
            "fifo" => Ok(Self::Fifo),
            "standard" => Ok(Self::Standard),
            _ => Err(ConfigurationError::new(
                "queue_type",
                "queue_type is `fifo` or `standard`",
            )),
        }
    }

    pub const fn is_fifo(self) -> bool {
        matches!(self, Self::Fifo)
    }
}

/// One deployment's SQS configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqsConfiguration {
    region: String,
    account_id: String,
    queue_name: String,
    queue_type: QueueType,
    send_horizon: Duration,
    max_message_bytes: usize,
}

impl SqsConfiguration {
    /// Validate one deployment's queue at startup.
    ///
    /// The queue type and the queue name have to agree: AWS documents that "To
    /// determine whether a queue is FIFO, you can check whether `QueueName` ends
    /// with the `.fifo` suffix", so a deployment that calls a queue FIFO without
    /// the suffix — or the other way round — is refused rather than trusted.
    pub fn new(
        region: &str,
        account_id: &str,
        queue_name: &str,
        queue_type: QueueType,
    ) -> Result<Self, ConfigurationError> {
        aws::validate_region(region)?;
        aws::validate_account_id(account_id)?;
        validate_queue_name(queue_name)?;
        if queue_name.ends_with(".fifo") != queue_type.is_fifo() {
            return Err(ConfigurationError::new(
                "queue_type",
                "a FIFO queue name ends with .fifo and a standard queue name does not",
            ));
        }
        Ok(Self {
            region: region.to_owned(),
            account_id: account_id.to_owned(),
            queue_name: queue_name.to_owned(),
            queue_type,
            send_horizon: MAX_SEND_HORIZON,
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        })
    }

    /// The window a durable activity may keep resending one message in.
    ///
    /// It must fit inside the documented deduplication interval less the clock
    /// safety margin. Equality is admitted; one millisecond more is refused,
    /// because past that point AWS documents that it "can't detect duplicate
    /// messages" and the send stops being idempotent.
    pub fn with_send_horizon(mut self, horizon: Duration) -> Result<Self, ConfigurationError> {
        if horizon.is_zero() || horizon > MAX_SEND_HORIZON {
            return Err(ConfigurationError::new(
                "send_horizon",
                "the send horizon must fit inside the deduplication interval less the clock \
                 safety margin",
            ));
        }
        self.send_horizon = horizon;
        Ok(self)
    }

    pub fn with_max_message_bytes(mut self, bytes: usize) -> Result<Self, ConfigurationError> {
        if bytes == 0 || bytes > PROVIDER_MAX_MESSAGE_BYTES {
            return Err(ConfigurationError::new(
                "max_message_bytes",
                "max_message_bytes is positive and at most the documented 1 MiB message size",
            ));
        }
        self.max_message_bytes = bytes;
        Ok(self)
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    pub const fn queue_type(&self) -> QueueType {
        self.queue_type
    }

    pub const fn send_horizon(&self) -> Duration {
        self.send_horizon
    }

    pub const fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    /// The documented queue URL, composed from deploy-time values only.
    pub fn queue_url(&self) -> String {
        format!(
            "https://sqs.{}.amazonaws.com/{}/{}",
            self.region, self.account_id, self.queue_name
        )
    }

    pub fn connector_configuration(&self) -> ConnectorConfiguration {
        ConnectorConfiguration::from_deployment([(REGION_CONFIGURATION_KEY, self.region.as_str())])
    }
}

/// A queue name, as AWS's naming rules spell it, optionally with the documented
/// `.fifo` suffix.
fn validate_queue_name(queue_name: &str) -> Result<(), ConfigurationError> {
    let stem = queue_name.strip_suffix(".fifo").unwrap_or(queue_name);
    let valid = (1..=80).contains(&stem.len())
        && stem.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        });
    if valid {
        Ok(())
    } else {
        Err(ConfigurationError::new(
            "queue_name",
            "a queue name is up to 80 alphanumerics, hyphens, and underscores, optionally with \
             the .fifo suffix",
        ))
    }
}

/// The documented explicit-key evidence for a FIFO send.
fn fifo_send_evidence() -> Result<ExplicitKeyEvidence, OperationError> {
    ExplicitKeyEvidence::documented(
        IdempotencyBinding::body_pointer("/MessageDeduplicationId")?,
        // AWS deduplicates a `MessageDeduplicationId` within one FIFO queue.
        "FIFO queue",
        DEDUPLICATION_INTERVAL,
        CLOCK_SAFETY_MARGIN,
        "Amazon SQS documents MessageDeduplicationId on SendMessage: \"If a message with a \
         particular MessageDeduplicationId is sent successfully, any messages sent with the same \
         MessageDeduplicationId are accepted successfully but aren't delivered during the 5-minute \
         deduplication interval\", and \"If you retry the SendMessage action within the 5-minute \
         deduplication interval, Amazon SQS doesn't introduce any duplicates into the queue\"",
    )
}

/// This module's static declaration (spec 010 §4).
///
/// `message.send` carries the FIFO evidence here, because that is the class the
/// operation can reach. Which class a *deployment* gets is decided when its
/// instance compiles: a standard queue downgrades it to `InventoryOnly` on
/// AWS's own at-least-once statement.
pub fn connector() -> &'static Connector {
    static CONNECTOR: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        Connector::declare(CONNECTOR_NAME, CONNECTOR_VERSION)
            .origin(
                OriginSpec::templated_host("https", HOST_TEMPLATE, None)
                    .expect("the SQS regional endpoint template is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::aws_sigv4(SERVICE).expect("sqs is a static service code"),
            ))
            .operations(operations(None, QueueType::Fifo).expect("the SQS declaration is valid"))
            .build()
            .expect("the SQS declaration is valid")
    });
    &CONNECTOR
}

/// The queue URL leaf of every request body: a literal for a compiled instance,
/// and the declaration's own placeholder slot before any deployment exists.
fn queue_url_template(queue_url: Option<&str>) -> JsonTemplate {
    match queue_url {
        Some(queue_url) => JsonTemplate::literal(json!(queue_url)),
        None => JsonTemplate::input(QUEUE_URL_SLOT),
    }
}

/// The message group of a FIFO send, which a standard queue's send does not
/// carry: AWS documents `MessageGroupId` as applying "only to FIFO
/// (first-in-first-out) queues", so a standard-queue declaration must not
/// publish a field its request never renders.
fn send_group(
    builder: crate::sdk::OperationBuilder,
    queue_type: QueueType,
) -> crate::sdk::OperationBuilder {
    if queue_type.is_fifo() {
        builder.declared_input("group_id", ValueScalar::String, Required::Yes)
    } else {
        builder
    }
}

/// The declaration's own placeholder for the queue every request targets.
///
/// It exists only before a deployment configured one, and it is filled from
/// that configuration when one does. `queue_url` is a reserved input name, so
/// no caller can reach it either way.
const QUEUE_URL_SLOT: &str = "queue_url_from_configuration";

fn action(id: &str, target: &str) -> crate::sdk::OperationBuilder {
    Operation::post(id, "/")
        .version(REQUEST_SHAPE_VERSION)
        .static_header("X-Amz-Target", &format!("AmazonSQS.{target}"))
        .static_header("Content-Type", JSON_PROTOCOL_CONTENT_TYPE)
        .supplied_input(QUEUE_URL_SLOT)
        .success_statuses([StatusCode::OK])
}

fn operations(
    queue_url: Option<&str>,
    queue_type: QueueType,
) -> Result<Vec<Operation>, OperationError> {
    let queue = || queue_url_template(queue_url);

    // The send body differs by queue type because the provider's contract does:
    // `MessageDeduplicationId` and `MessageGroupId` are FIFO-only fields.
    let send_body = if queue_type.is_fifo() {
        JsonTemplate::object([
            ("QueueUrl", queue()),
            ("MessageBody", JsonTemplate::input("body")),
            ("MessageGroupId", JsonTemplate::input("group_id")),
            (
                "MessageDeduplicationId",
                JsonTemplate::input("deduplication_id"),
            ),
        ])
    } else {
        JsonTemplate::object([
            ("QueueUrl", queue()),
            ("MessageBody", JsonTemplate::input("body")),
        ])
    };
    let send_effect = if queue_type.is_fifo() {
        Effect::provider_idempotent_explicit_key(fifo_send_evidence()?)
    } else {
        // ADR 063: the standard-queue send is the deployment-conditional half
        // of [[046-an-effect-class-can-depend-on-deploy-time-configuration]],
        // and Amazon's own exclusion is the search that establishes the
        // absence. It is executable only where the Process accepted a send
        // that may never be made.
        Effect::at_most_once(NoIdempotencyEvidence::searched(
            AbsenceSearch::PublishedContract,
            "Amazon documents MessageDeduplicationId as applying \"only to FIFO \
             (first-in-first-out) queues\", and documents standard queues as the opposite — \
             \"Standard queues ensure at-least-once message delivery, but due to the highly \
             distributed architecture, more than one copy of a message might be delivered\" — so \
             this queue publishes no key to bind and no window to keep a send inside",
            "a second message on the queue, delivered to consumers a second time",
        )?)
    };

    Ok(vec![
        send_group(
            action("message.send", "SendMessage").body(send_body),
            queue_type,
        )
        .declared_input("body", ValueScalar::String, Required::Yes)
        // The deduplication id is the durable activity's own stable key,
        // which no caller may choose; on a standard queue the slot does not
        // exist at all, because AWS documents the field as FIFO-only.
        .supplied_input("deduplication_id")
        .output_pointer(
            "message_id",
            "/MessageId",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "md5_of_body",
            "/MD5OfMessageBody",
            ValueScalar::String,
            Required::Yes,
        )
        // "This parameter applies only to FIFO (first-in-first-out) queues."
        .output_pointer(
            "sequence_number",
            "/SequenceNumber",
            ValueScalar::String,
            Required::No,
        )
        .effect(send_effect)
        .build()?,
        // ReceiveMessage retrieves messages; it removes none. AWS documents that
        // removal is a separate call — "Deletes the specified message from the
        // specified queue" — so a receive creates nothing and deletes nothing.
        // The visibility timeout it starts is a lease on delivery, not a change
        // to the queue's contents, and AWS documents that a lost response may be
        // retried: "it is possible to retry the same action with an identical
        // ReceiveRequestAttemptId to retrieve the same set of messages".
        action("message.receive", "ReceiveMessage")
            .body(JsonTemplate::object([
                ("QueueUrl", queue()),
                (
                    "MaxNumberOfMessages",
                    JsonTemplate::input("max_number_of_messages"),
                ),
                ("WaitTimeSeconds", JsonTemplate::input("wait_time_seconds")),
            ]))
            // Both are defaulted by `SqsInstance::plan` to AWS's own documented
            // single non-waiting receive, so a Process may omit them.
            .declared_input("max_number_of_messages", ValueScalar::Int64, Required::No)
            .declared_input("wait_time_seconds", ValueScalar::Int64, Required::No)
            .output_pointer("messages", "/Messages", ValueScalar::Json, Required::No)
            .effect(Effect::read_only_documented(
                "Amazon SQS documents ReceiveMessage as retrieving messages and DeleteMessage as \
                 the call that removes one — \"Deletes the specified message from the specified \
                 queue\" — so a receive adds no message and removes none; its visibility timeout \
                 is a lease on delivery rather than a change to the queue's contents",
            )?)
            .build()?,
        action("message.delete", "DeleteMessage")
            .body(JsonTemplate::object([
                ("QueueUrl", queue()),
                ("ReceiptHandle", JsonTemplate::input("receipt_handle")),
            ]))
            .declared_input("receipt_handle", ValueScalar::String, Required::Yes)
            // AWS documents "an HTTP 200 response with an empty HTTP body", so
            // the module composes the one field this operation publishes.
            .declared_output("deleted", ValueScalar::Boolean, Required::Yes)
            .effect(Effect::inventory_only(
                "Amazon SQS documents DeleteMessage as repeat-safe — \"If you use an old \
                 ReceiptHandle, the request will succeed, but the message might not be deleted\" \
                 — but the AWS JSON protocol expresses it as a POST, and \
                 ProviderIdempotent::NaturalMethod is admitted only for PUT and DELETE. ADR 063 \
                 does not admit it either: a documented repeat-safe delete needs a class that \
                 permits the retry, not the at-most-once class that forbids it",
            )?)
            .build()?,
        action("queue.attributes", "GetQueueAttributes")
            .body(JsonTemplate::object([
                ("QueueUrl", queue()),
                ("AttributeNames", JsonTemplate::literal(json!(["All"]))),
            ]))
            .output_pointer(
                "attributes",
                "/Attributes",
                ValueScalar::Json,
                Required::Yes,
            )
            .effect(Effect::read_only_documented(
                "Amazon SQS documents GetQueueAttributes as \"Gets attributes for the specified \
                 queue\", which creates and changes nothing",
            )?)
            .build()?,
    ])
}

/// SQS's documented failure codes, each reaching exactly one closed class.
///
/// The code is read from the field AWS's own error example publishes it in:
/// `"__type": "com.amazonaws.sqs#QueueDoesNotExist"`. Both the qualified and the
/// bare spelling are declared, because AWS documents the qualified form by
/// example and names the errors themselves in their bare form.
///
/// `RequestThrottled` — "The request was denied due to request throttling" — is
/// published at HTTP 400, and `ThrottlingException` at 403. Both reach
/// `http_429`, which is the class a Process routes a rate limit through; a
/// status rule alone could not tell either of them from a validation failure or
/// an authentication failure.
pub fn error_map() -> ErrorMap {
    let mut builder = ErrorMap::builder(ConnectorErrorClass::Permanent).code_pointer("/__type");
    for (code, class) in [
        ("RequestThrottled", ConnectorErrorClass::Http429),
        ("ThrottlingException", ConnectorErrorClass::Http429),
        ("KmsThrottled", ConnectorErrorClass::Http429),
        ("OverLimit", ConnectorErrorClass::Http429),
        ("AccessDeniedException", ConnectorErrorClass::Authentication),
        ("IncompleteSignature", ConnectorErrorClass::Authentication),
        ("InvalidClientTokenId", ConnectorErrorClass::Authentication),
        (
            "MissingAuthenticationToken",
            ConnectorErrorClass::Authentication,
        ),
        ("NotAuthorized", ConnectorErrorClass::Authentication),
        ("InvalidSecurity", ConnectorErrorClass::Authentication),
        // "The request reached the service more than 15 minutes after the date
        // stamp on the request... or the date stamp on the request is more than
        // 15 minutes in the future": the documented clock-skew rejection.
        ("RequestExpired", ConnectorErrorClass::Authentication),
        ("InvalidMessageContents", ConnectorErrorClass::Validation),
        ("InvalidParameterValue", ConnectorErrorClass::Validation),
        ("InvalidAttributeName", ConnectorErrorClass::Validation),
        ("MissingParameter", ConnectorErrorClass::Validation),
        ("ValidationError", ConnectorErrorClass::Validation),
        ("ReceiptHandleIsInvalid", ConnectorErrorClass::Validation),
        ("QueueDoesNotExist", ConnectorErrorClass::Permanent),
        ("UnsupportedOperation", ConnectorErrorClass::Permanent),
        ("InternalFailure", ConnectorErrorClass::Http5xx),
        ("ServiceUnavailable", ConnectorErrorClass::Http5xx),
    ] {
        builder = builder
            .on_code(&format!("com.amazonaws.sqs#{code}"), class)
            .on_code(code, class);
    }
    builder
        .on_status(429, ConnectorErrorClass::Http429)
        .on_statuses([401, 403], ConnectorErrorClass::Authentication)
        .on_status(400, ConnectorErrorClass::Validation)
        .on_status(408, ConnectorErrorClass::Timeout)
        .on_statuses(500..=599, ConnectorErrorClass::Http5xx)
        .correlation_header("request_id", "x-amzn-requestid")
        .build()
        .expect("the SQS error map is a valid declaration")
}

/// The `MessageDeduplicationId` one durable step sends under.
///
/// It is derived from the activity's stable step key and from nothing else, so
/// a retry of the same step deduplicates and two different steps do not. AWS
/// bounds the field — "The maximum length of `MessageDeduplicationId` is 128
/// characters. `MessageDeduplicationId` can contain alphanumeric characters
/// (`a-z`, `A-Z`, `0-9`) and punctuation" — so a step key that does not fit is
/// hashed rather than truncated: truncation would collide two distinct steps.
pub fn deduplication_id(stable_step_key: &str) -> String {
    let fits = !stable_step_key.is_empty()
        && stable_step_key.chars().count() <= MAX_DEDUPLICATION_ID_CHARS
        && stable_step_key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character.is_ascii_punctuation());
    if fits {
        stable_step_key.to_owned()
    } else {
        Sha256::digest(stable_step_key.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}

/// One compiled SQS connector instance.
#[derive(Debug, Clone)]
pub struct SqsInstance {
    configuration: SqsConfiguration,
    origin: Origin,
    operations: Vec<Operation>,
}

impl SqsInstance {
    pub fn compile(configuration: &SqsConfiguration) -> Result<Self, ConfigurationError> {
        let origin = connector()
            .resolve_origin(&configuration.connector_configuration())
            .map_err(|_| {
                ConfigurationError::new("region", "the configured region is not an SQS endpoint")
            })?;
        Self::compile_against(configuration, origin)
    }

    /// The same compilation against an explicit origin, for a crate-local test
    /// against the SDK's provider stub.
    #[cfg(any(test, feature = "testing"))]
    pub fn compile_for_stub(
        configuration: &SqsConfiguration,
        origin: Origin,
    ) -> Result<Self, ConfigurationError> {
        Self::compile_against(configuration, origin)
    }

    fn compile_against(
        configuration: &SqsConfiguration,
        origin: Origin,
    ) -> Result<Self, ConfigurationError> {
        // The horizon check is a startup check: a deployment whose send horizon
        // reaches past the documented deduplication interval would be sending
        // under an idempotency key the provider has already forgotten.
        if configuration.send_horizon > MAX_SEND_HORIZON {
            return Err(ConfigurationError::new(
                "send_horizon",
                "the send horizon must fit inside the deduplication interval less the clock \
                 safety margin",
            ));
        }
        let operations = operations(Some(&configuration.queue_url()), configuration.queue_type)
            .map_err(|_| {
                ConfigurationError::new("queue_name", "the configured queue is not a valid target")
            })?;
        Ok(Self {
            configuration: configuration.clone(),
            origin,
            operations,
        })
    }

    pub const fn configuration(&self) -> &SqsConfiguration {
        &self.configuration
    }

    pub const fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Every operation this compiled instance carries, in declaration order.
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    pub fn operation(&self, id: &str) -> Option<&Operation> {
        self.operations
            .iter()
            .find(|operation| operation.id() == id)
    }

    /// The gate a deployment meets. A standard-queue `message.send` is refused
    /// here, because its compiled class is `InventoryOnly`.
    pub fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        let operation = self.operation(id).ok_or(OperationRejection::Undeclared)?;
        if !operation.is_executable() {
            return Err(OperationRejection::InventoryOnly);
        }
        Ok(operation)
    }

    /// Render one operation.
    ///
    /// `stable_step_key` is the durable activity's own key. A FIFO send binds it
    /// to `MessageDeduplicationId`; every other operation ignores it, and no
    /// operation reads a deduplication id from input — the reserved-name check
    /// refuses one outright.
    pub fn plan(
        &self,
        operation: &Operation,
        input: &JsonValue,
        stable_step_key: Option<&str>,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let mut rendered = aws::with_deploy_time_values(input, [])?;
        match operation.id() {
            "message.send" => {
                let body = match rendered.get("body") {
                    Some(JsonValue::String(body)) => body.clone(),
                    _ => {
                        return Err(ConnectorFailure::validation(
                            "connector message body must be a string",
                        ));
                    }
                };
                if body.is_empty() || body.len() > self.configuration.max_message_bytes {
                    return Err(ConnectorFailure::validation(
                        "connector message body is outside the configured bounds",
                    ));
                }
                if self.configuration.queue_type.is_fifo() {
                    let key = stable_step_key.ok_or_else(|| {
                        // Without a stable key there is nothing to deduplicate
                        // on, so the send is not the operation that was
                        // classified. It fails rather than sending unkeyed.
                        ConnectorFailure::invariant(
                            "connector operation requires the activity's stable idempotency key",
                        )
                    })?;
                    aws::defaulted(
                        &mut rendered,
                        "deduplication_id",
                        json!(deduplication_id(key)),
                    );
                    // AWS: "If you do not provide a MessageGroupId when sending
                    // a message to a FIFO queue, the action fails."
                    if !rendered
                        .get("group_id")
                        .is_some_and(|value| value.as_str().is_some_and(|value| !value.is_empty()))
                    {
                        return Err(ConnectorFailure::validation(
                            "connector FIFO send requires a message group",
                        ));
                    }
                }
                operation.plan_request(&self.origin, &rendered)
            }
            "message.receive" => {
                aws::defaulted(&mut rendered, "max_number_of_messages", json!(1));
                aws::defaulted(&mut rendered, "wait_time_seconds", json!(0));
                admit_receive_bounds(&rendered)?;
                operation.plan_request(&self.origin, &rendered)
            }
            _ => operation.plan_request(&self.origin, &rendered),
        }
    }

    /// The declared output of one operation.
    ///
    /// `message.delete` is the one case the SDK's JSON decoder cannot answer:
    /// AWS documents it as returning "an HTTP 200 response with an empty HTTP
    /// body", and an empty body is not JSON.
    pub fn decode(
        &self,
        operation: &Operation,
        status: u16,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<JsonValue, ConnectorFailure> {
        if !operation.is_success(status) {
            return Err(error_map().classify(status, headers, body));
        }
        if operation.id() == "message.delete" {
            return if body.is_empty() {
                Ok(json!({ "deleted": true }))
            } else {
                Err(ConnectorFailure::validation(
                    "connector provider response did not satisfy the declared contract",
                ))
            };
        }
        operation.decode_response(status, body)
    }
}

fn admit_receive_bounds(input: &JsonValue) -> Result<(), ConnectorFailure> {
    let count = input
        .get("max_number_of_messages")
        .and_then(JsonValue::as_i64);
    let wait = input.get("wait_time_seconds").and_then(JsonValue::as_i64);
    match (count, wait) {
        // AWS: "Valid values: 1 to 10." and a wait of at most 20 seconds; this
        // connector caps the wait at 20 and does not offer long polling as a
        // durable trigger.
        (Some(count), Some(wait))
            if (1..=MAX_MESSAGES_PER_RECEIVE).contains(&count) && (0..=20).contains(&wait) =>
        {
            Ok(())
        }
        _ => Err(ConnectorFailure::validation(
            "connector receive bounds are outside the declared range",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::EffectClass;
    use crate::sdk::effect::KeyRetention;
    use crate::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};

    const ACCESS_KEY_ID: &str = "AKIDONATEXAMPLE";
    const ACCOUNT_ID: &str = "111122223333";
    const STEP_KEY: &str = "process-7:step-3:attempt";

    fn fifo() -> SqsConfiguration {
        SqsConfiguration::new("eu-west-1", ACCOUNT_ID, "orders.fifo", QueueType::Fifo)
            .expect("a static configuration is valid")
    }

    fn standard() -> SqsConfiguration {
        SqsConfiguration::new("eu-west-1", ACCOUNT_ID, "orders", QueueType::Standard)
            .expect("a static configuration is valid")
    }

    fn credential() -> crate::sdk::Credential {
        aws::credential(ACCESS_KEY_ID, SECRET_SENTINEL, "eu-west-1", None)
    }

    fn instance(stub: &ProviderStub, configuration: &SqsConfiguration) -> SqsInstance {
        SqsInstance::compile_for_stub(configuration, stub.origin())
            .expect("a static configuration compiles")
    }

    fn signed(instance: &SqsInstance, id: &str, input: JsonValue) -> RequestPlan {
        let operation = instance.operation(id).expect("the operation is declared");
        let mut request = instance
            .plan(operation, &input, Some(STEP_KEY))
            .expect("the request renders");
        AuthPlan::aws_sigv4(SERVICE)
            .expect("sqs is a static service code")
            .apply(&credential(), &mut request, None)
            .expect("the request signs");
        request
    }

    /// `aws_sqs_request_shape`, `aws_sqs_auth_is_applied`: the AWS JSON
    /// protocol request AWS documents, signed, with the queue named only by
    /// configuration.
    #[tokio::test]
    async fn aws_sqs_request_shape_and_auth_are_applied() {
        let queue_url = "https://sqs.eu-west-1.amazonaws.com/111122223333/orders.fifo";
        let stub = ProviderStub::start([
            Expectation::new("POST", "/")
                .header("x-amz-target", "AmazonSQS.SendMessage")
                .header("content-type", "application/x-amz-json-1.0")
                .json_body(json!({
                    "QueueUrl": queue_url,
                    "MessageBody": "order-42",
                    "MessageGroupId": "orders",
                    "MessageDeduplicationId": STEP_KEY,
                }))
                .respond_json(
                    200,
                    json!({ "MessageId": "msg-1", "MD5OfMessageBody": "abc", "SequenceNumber": "18" }),
                ),
            Expectation::new("POST", "/")
                .header("x-amz-target", "AmazonSQS.ReceiveMessage")
                .json_body(json!({
                    "QueueUrl": queue_url,
                    "MaxNumberOfMessages": 1,
                    "WaitTimeSeconds": 0,
                }))
                .respond_json(200, json!({ "Messages": [{ "MessageId": "msg-1" }] })),
            Expectation::new("POST", "/")
                .header("x-amz-target", "AmazonSQS.GetQueueAttributes")
                .json_body(json!({ "QueueUrl": queue_url, "AttributeNames": ["All"] }))
                .respond_json(200, json!({ "Attributes": { "FifoQueue": "true" } })),
        ])
        .await;
        let instance = instance(&stub, &fifo());

        for (id, input) in [
            (
                "message.send",
                json!({ "body": "order-42", "group_id": "orders" }),
            ),
            ("message.receive", json!({})),
            ("queue.attributes", json!({})),
        ] {
            let request = signed(&instance, id, input);
            let authorization = request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .expect("every request is signed")
                .to_owned();
            assert!(
                authorization.contains("/eu-west-1/sqs/aws4_request,"),
                "{id} is scoped to the configured region and service"
            );
            assert!(
                !format!("{:?} {authorization}", request.headers()).contains(SECRET_SENTINEL),
                "{id} must not carry the secret access key"
            );
            let response = stub.send(request).await.expect("the stub answers");
            let operation = instance.operation(id).expect("declared");
            instance
                .decode(
                    operation,
                    response.status.as_u16(),
                    response.headers(),
                    response.body(),
                )
                .expect("the declared output is satisfied");
        }
        stub.assert_satisfied();
    }

    /// `aws_sqs_standard_queue_is_not_executable`: one operation id, two
    /// deployment-conditional classes (ADR 046). A FIFO send is provider
    /// idempotent on Amazon's documented deduplication; a standard-queue send
    /// is at-most-once on Amazon's documented *exclusion* of it (ADR 063), so
    /// it is reachable only by a Process that declared the opt-in.
    #[test]
    fn aws_sqs_standard_queue_is_not_executable() {
        let standard = SqsInstance::compile(&standard()).expect("a configuration compiles");
        let send = standard.operation("message.send").expect("declared");
        assert_eq!(send.effect_class(), Some(EffectClass::AtMostOnce));
        assert!(
            send.effect()
                .and_then(crate::sdk::Effect::no_idempotency_evidence)
                .is_some_and(|evidence| evidence.searched_documentation().contains("only to FIFO")),
            "the class carries Amazon's own exclusion as its search"
        );
        assert!(
            standard.admit_operation("message.receive").is_ok(),
            "a standard queue can still be read"
        );

        let fifo = SqsInstance::compile(&fifo()).expect("a configuration compiles");
        let send = fifo
            .admit_operation("message.send")
            .expect("a FIFO send is executable on documented evidence");
        assert_eq!(
            send.effect_class(),
            Some(EffectClass::ProviderIdempotentExplicitKey)
        );
        assert_eq!(
            send.idempotency_binding(),
            Some(&IdempotencyBinding::body_pointer("/MessageDeduplicationId").expect("static")),
            "the key is bound where AWS documents it"
        );

        // The identifier is a function of the stable step key alone.
        let rendered = |input: JsonValue, key: &str| {
            let request = fifo
                .plan(send, &input, Some(key))
                .expect("the request renders");
            serde_json::from_slice::<JsonValue>(request.body()).expect("the body is JSON")
        };
        let body = rendered(json!({ "body": "a", "group_id": "g" }), STEP_KEY);
        assert_eq!(body["MessageDeduplicationId"], json!(STEP_KEY));
        assert_eq!(
            rendered(
                json!({ "body": "different", "group_id": "other" }),
                STEP_KEY
            )["MessageDeduplicationId"],
            json!(STEP_KEY),
            "the identifier does not move when the input does"
        );
        assert_ne!(
            rendered(json!({ "body": "a", "group_id": "g" }), "other-step")["MessageDeduplicationId"],
            json!(STEP_KEY),
            "and it does move when the step does"
        );

        // Input cannot supply one, and a send without a stable key never goes.
        for hostile in [
            json!({ "body": "a", "group_id": "g", "message_deduplication_id": "chosen" }),
            json!({ "body": "a", "group_id": "g", "idempotency_key": "chosen" }),
        ] {
            assert_eq!(
                fifo.plan(send, &hostile, Some(STEP_KEY))
                    .expect_err("input may not choose the deduplication identifier")
                    .code(),
                "connector_input_names_deploy_time_configuration"
            );
        }
        assert_eq!(
            fifo.plan(send, &json!({ "body": "a", "group_id": "g" }), None)
                .expect_err("a keyless FIFO send is not the operation that was classified")
                .class(),
            ConnectorErrorClass::Invariant
        );
        assert_eq!(
            fifo.plan(send, &json!({ "body": "a" }), Some(STEP_KEY))
                .expect_err("AWS documents that a FIFO send without a message group fails")
                .class(),
            ConnectorErrorClass::Validation
        );

        // A step key outside the documented grammar is hashed, never truncated.
        let long = "x".repeat(200);
        let hashed = deduplication_id(&long);
        assert_eq!(hashed.len(), 64);
        assert!(hashed.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(hashed, deduplication_id(&"x".repeat(201)));
        assert_eq!(deduplication_id("a-simple.key"), "a-simple.key");
        assert_ne!(deduplication_id("with space"), "with space");

        // And a deployment cannot lie about the queue type.
        assert!(
            SqsConfiguration::new("eu-west-1", ACCOUNT_ID, "orders", QueueType::Fifo).is_err(),
            "a FIFO queue name ends with .fifo"
        );
        assert!(
            SqsConfiguration::new("eu-west-1", ACCOUNT_ID, "orders.fifo", QueueType::Standard)
                .is_err()
        );
        assert!(QueueType::parse("neither").is_err());
    }

    /// `aws_sqs_dedup_window_is_bounded`: the compiled send horizon fits inside
    /// the documented deduplication interval less the clock safety margin.
    /// Equality passes; one millisecond over rejects.
    #[test]
    fn aws_sqs_dedup_window_is_bounded() {
        assert_eq!(DEDUPLICATION_INTERVAL, Duration::from_secs(300));
        assert!(
            CLOCK_SAFETY_MARGIN < DEDUPLICATION_INTERVAL,
            "the margin is strictly smaller than the documented retention"
        );
        assert_eq!(
            MAX_SEND_HORIZON,
            DEDUPLICATION_INTERVAL - CLOCK_SAFETY_MARGIN
        );

        assert!(
            fifo().with_send_horizon(MAX_SEND_HORIZON).is_ok(),
            "the exact horizon is admitted"
        );
        assert_eq!(
            fifo()
                .with_send_horizon(MAX_SEND_HORIZON + Duration::from_millis(1))
                .expect_err("one millisecond over the horizon is refused")
                .setting(),
            "send_horizon"
        );
        assert!(fifo().with_send_horizon(Duration::ZERO).is_err());

        // The compiled class carries the same numbers, and the gate itself
        // refuses a margin that is not strictly smaller than the retention.
        let fifo = SqsInstance::compile(&fifo()).expect("a configuration compiles");
        let evidence = fifo
            .operation("message.send")
            .and_then(Operation::effect)
            .and_then(crate::sdk::Effect::explicit_key_evidence)
            .expect("a FIFO send carries explicit key evidence");
        assert_eq!(evidence.retention().minimum(), DEDUPLICATION_INTERVAL);
        assert_eq!(
            evidence.retention().clock_safety_margin(),
            CLOCK_SAFETY_MARGIN
        );
        assert_eq!(evidence.retention().scope(), "FIFO queue");
        assert!(
            evidence
                .citation()
                .contains("5-minute deduplication interval")
        );
        assert!(
            ExplicitKeyEvidence::documented(
                IdempotencyBinding::body_pointer("/MessageDeduplicationId").expect("static"),
                "FIFO queue",
                DEDUPLICATION_INTERVAL,
                DEDUPLICATION_INTERVAL,
                "cited",
            )
            .is_err(),
            "a margin equal to the retention is not evidence"
        );
        // The retention the class publishes is what a horizon is measured
        // against, so the two cannot drift apart.
        let retention: &KeyRetention = evidence.retention();
        assert_eq!(
            retention.minimum() - retention.clock_safety_margin(),
            MAX_SEND_HORIZON
        );
    }

    /// `aws_sqs_region_and_target_are_deploy_time`: input cannot change the
    /// region, endpoint, or queue.
    #[test]
    fn aws_sqs_region_and_target_are_deploy_time() {
        let fifo = SqsInstance::compile(&fifo()).expect("a configuration compiles");
        let send = fifo.operation("message.send").expect("declared");
        for hostile in [
            json!({ "body": "a", "group_id": "g", "queue_url": "https://attacker.invalid/q" }),
            json!({ "body": "a", "group_id": "g", "region": "us-east-1" }),
            json!({ "body": "a", "group_id": "g", "account_id": "999999999999" }),
            json!({ "body": "a", "group_id": "g", "endpoint": "https://attacker.invalid" }),
        ] {
            assert_eq!(
                fifo.plan(send, &hostile, Some(STEP_KEY))
                    .expect_err("deploy-time material is not input")
                    .code(),
                "connector_input_names_deploy_time_configuration"
            );
        }

        let request = fifo
            .plan(
                send,
                &json!({ "body": "a", "group_id": "g" }),
                Some(STEP_KEY),
            )
            .expect("the request renders");
        assert_eq!(
            request.url().as_str(),
            "https://sqs.eu-west-1.amazonaws.com/"
        );
        assert_eq!(
            serde_json::from_slice::<JsonValue>(request.body()).expect("the body is JSON")["QueueUrl"],
            json!("https://sqs.eu-west-1.amazonaws.com/111122223333/orders.fifo"),
            "the queue is named by configuration alone"
        );
        for account in ["", "12345", "11112222333a"] {
            assert!(
                SqsConfiguration::new("eu-west-1", account, "orders", QueueType::Standard).is_err(),
                "account {account} is refused at startup"
            );
        }
        assert!(
            SqsConfiguration::new(
                "eu-west-1",
                ACCOUNT_ID,
                "orders/../other",
                QueueType::Standard
            )
            .is_err()
        );
    }

    /// `aws_sqs_effects_are_classified`: every operation carries a class.
    #[test]
    fn aws_sqs_effects_are_classified() {
        let fifo = SqsInstance::compile(&fifo()).expect("a configuration compiles");
        assert_eq!(
            [
                "message.send",
                "message.receive",
                "message.delete",
                "queue.attributes"
            ]
            .map(|id| fifo.operation(id).and_then(Operation::effect_class)),
            [
                Some(EffectClass::ProviderIdempotentExplicitKey),
                Some(EffectClass::ReadOnly),
                Some(EffectClass::InventoryOnly),
                Some(EffectClass::ReadOnly),
            ]
        );
        assert_eq!(
            fifo.admit_operation("message.delete"),
            Err(OperationRejection::InventoryOnly)
        );
        assert!(
            fifo.operation("message.delete")
                .and_then(Operation::effect)
                .and_then(crate::sdk::Effect::inventory_reason)
                .is_some_and(|reason| reason.contains("NaturalMethod is admitted only for PUT")),
            "an inventory-only operation records why it is not executable"
        );
        assert_eq!(
            fifo.admit_operation("message.purge"),
            Err(OperationRejection::Undeclared)
        );
    }

    /// `aws_sqs_error_map`: the documented codes each reach one class,
    /// throttling reaches `http_429` from both statuses AWS publishes it at,
    /// and no provider text crosses.
    #[test]
    fn aws_sqs_error_map() {
        let map = error_map();
        let body = |code: &str| {
            serde_json::to_vec(&json!({
                "__type": format!("com.amazonaws.sqs#{code}"),
                "message": format!("shard db-7.internal rejected key {SECRET_SENTINEL}"),
            }))
            .expect("a fixture serializes")
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-amzn-requestid",
            "req_01H".parse().expect("a test header"),
        );

        for (status, code, expected) in [
            (400, "RequestThrottled", ConnectorErrorClass::Http429),
            (403, "ThrottlingException", ConnectorErrorClass::Http429),
            (400, "OverLimit", ConnectorErrorClass::Http429),
            (400, "QueueDoesNotExist", ConnectorErrorClass::Permanent),
            (
                400,
                "InvalidMessageContents",
                ConnectorErrorClass::Validation,
            ),
            (400, "InvalidSecurity", ConnectorErrorClass::Authentication),
            (400, "RequestExpired", ConnectorErrorClass::Authentication),
            (
                403,
                "InvalidClientTokenId",
                ConnectorErrorClass::Authentication,
            ),
            (500, "InternalFailure", ConnectorErrorClass::Http5xx),
            (503, "ServiceUnavailable", ConnectorErrorClass::Http5xx),
            (418, "Teapot", ConnectorErrorClass::Permanent),
        ] {
            let failure = map.classify(status, &headers, &body(code));
            assert_eq!(failure.class(), expected, "{code}");
            assert_eq!(failure.provider_status(), Some(status));
            let surface = format!(
                "{} {} {} {failure:?}",
                failure.code(),
                failure.safe_message(),
                failure.diagnostic()
            );
            for leaked in [SECRET_SENTINEL, "db-7.internal", "shard", code] {
                assert!(!surface.contains(leaked), "{code} leaked {leaked}");
            }
            assert_eq!(
                failure
                    .correlation_ids()
                    .get("request_id")
                    .map(String::as_str),
                Some("req_01H")
            );
        }

        // A body that is not the documented envelope still reaches one class,
        // by status.
        assert_eq!(
            map.classify(429, &headers, b"<html>").class(),
            ConnectorErrorClass::Http429
        );
        assert_eq!(
            map.classify(400, &headers, b"").class(),
            ConnectorErrorClass::Validation
        );
    }

    /// `aws_sqs_output_contract`, `aws_sqs_bounds`: the declared outputs are
    /// complete and typed, and the message and receive bounds are exact.
    #[tokio::test]
    async fn aws_sqs_output_contract_and_bounds() {
        let stub = ProviderStub::start([]).await;
        let configuration = fifo()
            .with_max_message_bytes(32)
            .expect("a lowered ceiling is valid");
        let instance = instance(&stub, &configuration);
        let send = instance.operation("message.send").expect("declared");

        assert_eq!(
            instance
                .decode(
                    send,
                    200,
                    &HeaderMap::new(),
                    br#"{"MessageId":"m1","MD5OfMessageBody":"abc","SequenceNumber":"18"}"#,
                )
                .expect("the declared output is satisfied"),
            json!({ "message_id": "m1", "md5_of_body": "abc", "sequence_number": "18" })
        );
        assert_eq!(
            instance
                .decode(
                    send,
                    200,
                    &HeaderMap::new(),
                    br#"{"MD5OfMessageBody":"abc"}"#
                )
                .expect_err("a missing required pointer is not a null")
                .class(),
            ConnectorErrorClass::Validation
        );

        let delete = instance.operation("message.delete").expect("declared");
        assert_eq!(
            instance
                .decode(delete, 200, &HeaderMap::new(), b"")
                .expect("an empty body is the documented success"),
            json!({ "deleted": true })
        );

        // The exact message ceiling renders; one byte over is refused before
        // any request is made, and so is an empty body.
        let fits = json!({ "body": "x".repeat(32), "group_id": "g" });
        assert!(instance.plan(send, &fits, Some(STEP_KEY)).is_ok());
        for outside in [
            json!({ "body": "x".repeat(33), "group_id": "g" }),
            json!({ "body": "", "group_id": "g" }),
            json!({ "body": 7, "group_id": "g" }),
        ] {
            assert_eq!(
                instance
                    .plan(send, &outside, Some(STEP_KEY))
                    .expect_err("a body outside the configured bounds never leaves")
                    .class(),
                ConnectorErrorClass::Validation
            );
        }
        assert!(
            fifo()
                .with_max_message_bytes(PROVIDER_MAX_MESSAGE_BYTES)
                .is_ok()
        );
        assert!(
            fifo()
                .with_max_message_bytes(PROVIDER_MAX_MESSAGE_BYTES + 1)
                .is_err(),
            "one byte over the documented 1 MiB message size is refused"
        );

        let receive = instance.operation("message.receive").expect("declared");
        for outside in [
            json!({ "max_number_of_messages": 11 }),
            json!({ "wait_time_seconds": 21 }),
        ] {
            assert!(instance.plan(receive, &outside, None).is_err());
        }
        assert!(
            instance
                .plan(
                    receive,
                    &json!({ "max_number_of_messages": 10, "wait_time_seconds": 20 }),
                    None
                )
                .is_ok(),
            "the exact documented bounds are admitted"
        );
        stub.assert_satisfied();
    }
}
