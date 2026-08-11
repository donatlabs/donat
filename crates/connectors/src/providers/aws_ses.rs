//! Amazon SES (API v2) — transactional send from one configured identity.
//!
//! Written against Amazon's own published API reference for the Amazon SES API
//! v2, whose requests are ordinary REST-JSON on the Region's endpoint:
//! `POST /v2/email/outbound-emails`, `GET /v2/email/templates`,
//! `GET /v2/email/templates/{TemplateName}`, `GET /v2/email/identities`.
//!
//! # Why every send here is `AtMostOnce`
//!
//! `SendEmail` publishes no idempotency token, client token, request token, or
//! deduplication field of any kind. Its complete documented request body is
//! `ConfigurationSetName`, `Content`, `Destination`, `EmailTags`, `EndpointId`,
//! `FeedbackForwardingEmailAddress`, `FeedbackForwardingEmailAddressIdentityArn`,
//! `FromEmailAddress`, `FromEmailAddressIdentityArn`, `ListManagementOptions`,
//! `ReplyToAddresses`, and `TenantName` — none of which the reference describes
//! as deduplicating a repeat.
//!
//! The nearest thing the page offers is the response's `MessageId`, and AWS
//! describes it as the opposite of an idempotency key: "A unique identifier for
//! the message that is generated when the message is accepted." A second
//! identical send is accepted and produces a second identifier, which means a
//! second email. That is the search and the consequence ADR 063 admits an
//! `AtMostOnce` class on: SES cannot absorb the duplicate, so the only thing
//! Donat can do is refuse to make it, and a Process reaches either send only by
//! declaring `at_most_once` and a route for an outcome nobody can know.
//!
//! So both sends are declared, typed, tested — and not executable from a
//! Process. That is spec 010 §7 working, not a gap in this module: a durable
//! activity may be retried after an ambiguous worker loss, and an email that
//! arrives twice cannot be withdrawn.
//!
//! # Error classification
//!
//! SES v2's per-operation error tables publish a distinct HTTP status for each
//! documented failure — `TooManyRequestsException` at 429, `NotFoundException`
//! at 404, `BadRequestException` and `MessageRejected` at 400 — and the API
//! reference does not document a machine-readable error code *field* in the
//! response body. The map below therefore classifies on status, which reaches
//! exactly one class for every documented error name. The one consequence worth
//! stating: the common `ThrottlingException`, which SES publishes at 400, is
//! indistinguishable from a validation failure without a code field this
//! connector has no documented pointer for. The throttling these operations do
//! publish, `TooManyRequestsException`, arrives at 429 and reaches `http_429`.

use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use crate::providers::aws::{self, ConfigurationError};
use crate::sdk::connector::OperationRejection;
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{JsonTemplate, OperationError};
use crate::sdk::{
    AbsenceSearch, AuthPlan, Connector, ConnectorConfiguration, CredentialSpec, Effect,
    NoIdempotencyEvidence, Operation, Origin, OriginSpec, Pagination, PaginationBudget,
    RequestPlan, Required,
};

pub const CONNECTOR_NAME: &str = "aws_ses";
pub const CONNECTOR_VERSION: &str = "1.0.0";
pub const SERVICE: &str = "ses";
const REQUEST_SHAPE_VERSION: &str = "1.0.0";
const HOST_TEMPLATE: &str = "email.{region}.amazonaws.com";
pub const REGION_CONFIGURATION_KEY: &str = "region";

/// AWS: `ListEmailTemplates`' page size "has to be at least 1, and can be no
/// more than 100".
const MAX_TEMPLATE_PAGE: i64 = 100;
/// AWS: `ListEmailIdentities`' page size "has to be at least 0, and can be no
/// more than 1000".
const MAX_IDENTITY_PAGE: i64 = 1_000;
/// The largest message body this connector will send. It is a Donat bound.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// One deployment's SES configuration: the Region and the verified sending
/// identity. Both are deploy-time; an operation input can name neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SesConfiguration {
    region: String,
    from_email_address: String,
    max_message_bytes: usize,
}

impl SesConfiguration {
    pub fn new(region: &str, from_email_address: &str) -> Result<Self, ConfigurationError> {
        aws::validate_region(region)?;
        validate_email_address(from_email_address).map_err(|_| {
            ConfigurationError::new(
                "from_email_address",
                "the sending identity must be one verified email address",
            )
        })?;
        Ok(Self {
            region: region.to_owned(),
            from_email_address: from_email_address.to_owned(),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        })
    }

    pub fn with_max_message_bytes(mut self, bytes: usize) -> Result<Self, ConfigurationError> {
        if bytes == 0 || bytes > DEFAULT_MAX_MESSAGE_BYTES {
            return Err(ConfigurationError::new(
                "max_message_bytes",
                "max_message_bytes is positive and at most the compiled message ceiling",
            ));
        }
        self.max_message_bytes = bytes;
        Ok(self)
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    pub fn from_email_address(&self) -> &str {
        &self.from_email_address
    }

    pub const fn max_message_bytes(&self) -> usize {
        self.max_message_bytes
    }

    pub fn connector_configuration(&self) -> ConnectorConfiguration {
        ConnectorConfiguration::from_deployment([(REGION_CONFIGURATION_KEY, self.region.as_str())])
    }
}

/// One address, printable ASCII with exactly one `@` and no header-injection
/// characters. A sending identity reaches a message header, so a value with a
/// newline in it is refused rather than escaped.
fn validate_email_address(address: &str) -> Result<(), ()> {
    let valid = (3..=254).contains(&address.len())
        && address.matches('@').count() == 1
        && !address.starts_with('@')
        && !address.ends_with('@')
        && address.chars().all(|character| {
            character.is_ascii_graphic() && !matches!(character, ',' | ';' | '<' | '>')
        });
    if valid { Ok(()) } else { Err(()) }
}

/// This module's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        Connector::declare(CONNECTOR_NAME, CONNECTOR_VERSION)
            .origin(
                OriginSpec::templated_host("https", HOST_TEMPLATE, None)
                    .expect("the SES regional endpoint template is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::aws_sigv4(SERVICE).expect("ses is a static service code"),
            ))
            .operations(operations(None).expect("the SES declaration is valid"))
            .build()
            .expect("the SES declaration is valid")
    });
    &CONNECTOR
}

/// The sending identity leaf of a send body: a literal for a compiled instance,
/// and the declaration's own placeholder slot before any deployment exists.
/// The declaration's own placeholder for the configured sending identity. An
/// operation input may name recipients; the sender is deploy-time material.
pub(crate) const FROM_SLOT: &str = "from_email_address_from_configuration";

fn from_template(from_email_address: Option<&str>) -> JsonTemplate {
    match from_email_address {
        Some(address) => JsonTemplate::literal(json!(address)),
        None => JsonTemplate::input(FROM_SLOT),
    }
}

/// The reason both sends carry, recorded on the operation itself.
const NO_IDEMPOTENCY_KEY: &str = "the Amazon SES API v2 SendEmail reference publishes no idempotency token, client token, or \
     deduplication field; its MessageId is \"A unique identifier for the message that is \
     generated when the message is accepted\", which is server-issued and therefore not a key a \
     retry could carry";

/// One send whose repeat would deliver a second message (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_IDEMPOTENCY_KEY,
        repeat_produces,
    )?))
}

fn operations(from_email_address: Option<&str>) -> Result<Vec<Operation>, OperationError> {
    let from = || from_template(from_email_address);
    Ok(vec![
        // POST /v2/email/outbound-emails with a Simple message.
        Operation::post("email.send", "/v2/email/outbound-emails")
            .version(REQUEST_SHAPE_VERSION)
            .supplied_input(FROM_SLOT)
            .body(JsonTemplate::object([
                ("FromEmailAddress", from()),
                (
                    "Destination",
                    JsonTemplate::object([("ToAddresses", JsonTemplate::input("to"))]),
                ),
                (
                    "Content",
                    JsonTemplate::object([(
                        "Simple",
                        JsonTemplate::object([
                            (
                                "Subject",
                                JsonTemplate::object([("Data", JsonTemplate::input("subject"))]),
                            ),
                            (
                                "Body",
                                JsonTemplate::object([(
                                    "Text",
                                    JsonTemplate::object([(
                                        "Data",
                                        JsonTemplate::input("text_body"),
                                    )]),
                                )]),
                            ),
                        ]),
                    )]),
                ),
            ]))
            .success_statuses([StatusCode::OK])
            .output_pointer(
                "message_id",
                "/MessageId",
                ValueScalar::String,
                Required::Yes,
            )
            .effect(at_most_once(
                "a second delivered email with a new MessageId: SES accepts the repeat and \
                 sends it",
            )?)
            .build()?,
        // The same endpoint with a Templated message.
        Operation::post("email.send_template", "/v2/email/outbound-emails")
            .version(REQUEST_SHAPE_VERSION)
            .supplied_input(FROM_SLOT)
            .body(JsonTemplate::object([
                ("FromEmailAddress", from()),
                (
                    "Destination",
                    JsonTemplate::object([("ToAddresses", JsonTemplate::input("to"))]),
                ),
                (
                    "Content",
                    JsonTemplate::object([(
                        "Template",
                        JsonTemplate::object([
                            ("TemplateName", JsonTemplate::input("template_name")),
                            ("TemplateData", JsonTemplate::input("template_data")),
                        ]),
                    )]),
                ),
            ]))
            .success_statuses([StatusCode::OK])
            .output_pointer(
                "message_id",
                "/MessageId",
                ValueScalar::String,
                Required::Yes,
            )
            .effect(at_most_once(
                "a second delivered email with a new MessageId: SES accepts the repeat and \
                 sends it",
            )?)
            .build()?,
        // GET /v2/email/templates/{TemplateName}
        Operation::get("template.get", "/v2/email/templates/{template_name}")
            .version(REQUEST_SHAPE_VERSION)
            .path_param("template_name", ValueScalar::String)
            .success_statuses([StatusCode::OK])
            .output_pointer(
                "template_name",
                "/TemplateName",
                ValueScalar::String,
                Required::Yes,
            )
            .output_pointer(
                "template_content",
                "/TemplateContent",
                ValueScalar::Json,
                Required::Yes,
            )
            .effect(Effect::read_only())
            .build()?,
        // GET /v2/email/templates?PageSize=
        Operation::get("template.list", "/v2/email/templates")
            .version(REQUEST_SHAPE_VERSION)
            .query_input("PageSize", "page_size")
            // Defaulted by `SesInstance::plan` to AWS's own documented page, so
            // a Process may omit it.
            .declared_input("page_size", ValueScalar::Int64, Required::No)
            .success_statuses([StatusCode::OK])
            .output_pointer(
                "templates",
                "/TemplatesMetadata",
                ValueScalar::Json,
                Required::Yes,
            )
            .output_pointer(
                "next_token",
                "/NextToken",
                ValueScalar::String,
                Required::No,
            )
            .effect(Effect::read_only())
            .build()?,
        // GET /v2/email/identities?PageSize=
        Operation::get("identity.list", "/v2/email/identities")
            .version(REQUEST_SHAPE_VERSION)
            .query_input("PageSize", "page_size")
            // Defaulted by `SesInstance::plan` to AWS's own documented page, so
            // a Process may omit it.
            .declared_input("page_size", ValueScalar::Int64, Required::No)
            .success_statuses([StatusCode::OK])
            .output_pointer(
                "identities",
                "/EmailIdentities",
                ValueScalar::Json,
                Required::Yes,
            )
            .output_pointer(
                "next_token",
                "/NextToken",
                ValueScalar::String,
                Required::No,
            )
            .effect(Effect::read_only())
            .build()?,
    ])
}

/// SES v2's documented failure statuses, each reaching exactly one closed class.
pub fn error_map() -> ErrorMap {
    ErrorMap::builder(ConnectorErrorClass::Permanent)
        // TooManyRequestsException: "Too many requests have been made to the
        // operation." HTTP Status Code: 429.
        .on_status(429, ConnectorErrorClass::Http429)
        // AccessDeniedException, ExpiredTokenException, IncompleteSignature,
        // OptInRequired, UnrecognizedClientException (403) and NotAuthorized
        // (401). A signature outside AWS's clock tolerance arrives here too.
        .on_statuses([401, 403], ConnectorErrorClass::Authentication)
        // BadRequestException, MessageRejected, MailFromDomainNotVerifiedException,
        // AccountSuspendedException, SendingPausedException, LimitExceededException,
        // ValidationError, RequestAbortedException.
        .on_status(400, ConnectorErrorClass::Validation)
        // RequestEntityTooLargeException: "The request entity is too large."
        .on_status(413, ConnectorErrorClass::Validation)
        // RequestTimeoutException: "The request timed out."
        .on_status(408, ConnectorErrorClass::Timeout)
        // NotFoundException and UnknownOperationException.
        .on_status(404, ConnectorErrorClass::Permanent)
        // InternalFailure (500) and ServiceUnavailable (503).
        .on_statuses(500..=599, ConnectorErrorClass::Http5xx)
        .correlation_header("request_id", "x-amzn-requestid")
        .build()
        .expect("the SES error map is a valid declaration")
}

/// One compiled SES connector instance.
#[derive(Debug, Clone)]
pub struct SesInstance {
    configuration: SesConfiguration,
    origin: Origin,
    operations: Vec<Operation>,
}

impl SesInstance {
    pub fn compile(configuration: &SesConfiguration) -> Result<Self, ConfigurationError> {
        let origin = connector()
            .resolve_origin(&configuration.connector_configuration())
            .map_err(|_| {
                ConfigurationError::new("region", "the configured region is not an SES endpoint")
            })?;
        Self::compile_against(configuration, origin)
    }

    /// The same compilation against an explicit origin, for a crate-local test
    /// against the SDK's provider stub.
    #[cfg(any(test, feature = "testing"))]
    pub fn compile_for_stub(
        configuration: &SesConfiguration,
        origin: Origin,
    ) -> Result<Self, ConfigurationError> {
        Self::compile_against(configuration, origin)
    }

    fn compile_against(
        configuration: &SesConfiguration,
        origin: Origin,
    ) -> Result<Self, ConfigurationError> {
        let operations = operations(Some(configuration.from_email_address())).map_err(|_| {
            ConfigurationError::new(
                "from_email_address",
                "the configured sending identity is not a valid address",
            )
        })?;
        Ok(Self {
            configuration: configuration.clone(),
            origin,
            operations,
        })
    }

    pub const fn configuration(&self) -> &SesConfiguration {
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

    pub fn admit_operation(&self, id: &str) -> Result<&Operation, OperationRejection> {
        let operation = self.operation(id).ok_or(OperationRejection::Undeclared)?;
        if !operation.is_executable() {
            return Err(OperationRejection::InventoryOnly);
        }
        Ok(operation)
    }

    /// The continuation plan of a listing operation.
    ///
    /// AWS: "A token indicating that there are additional email templates
    /// available to be listed. Pass this token to a subsequent
    /// `ListEmailTemplates` call".
    pub fn pagination(&self, id: &str) -> Option<Pagination> {
        let (items, _) = match id {
            "template.list" => ("/TemplatesMetadata", MAX_TEMPLATE_PAGE),
            "identity.list" => ("/EmailIdentities", MAX_IDENTITY_PAGE),
            _ => return None,
        };
        Some(
            Pagination::token_in_body(items, "/NextToken", "NextToken")
                .expect("the SES continuation plan is a valid declaration"),
        )
    }

    /// The budget one listing attempt spends across every page it fetches.
    pub fn list_budget(&self, id: &str, time_to_live: Duration) -> PaginationBudget {
        let max_items = match id {
            "identity.list" => MAX_IDENTITY_PAGE,
            _ => MAX_TEMPLATE_PAGE,
        } as usize;
        PaginationBudget::new(
            8,
            8,
            max_items,
            self.configuration.max_message_bytes,
            time_to_live,
        )
    }

    /// Render one operation.
    pub fn plan(
        &self,
        operation: &Operation,
        input: &JsonValue,
    ) -> Result<RequestPlan, ConnectorFailure> {
        let mut rendered = aws::with_deploy_time_values(input, [])?;
        match operation.id() {
            "email.send" | "email.send_template" => {
                admit_recipients(&rendered)?;
                if operation.id() == "email.send" {
                    let subject = required_string(&rendered, "subject")?;
                    let body = required_string(&rendered, "text_body")?;
                    if subject.len() + body.len() > self.configuration.max_message_bytes {
                        // Before any request is made.
                        return Err(ConnectorFailure::validation(
                            "connector message exceeds the configured ceiling",
                        ));
                    }
                } else {
                    required_string(&rendered, "template_name")?;
                    // AWS types `TemplateData` as a String, so a JSON object
                    // here would be a different field than the one documented.
                    let data = required_string(&rendered, "template_data")?;
                    if data.len() > self.configuration.max_message_bytes {
                        return Err(ConnectorFailure::validation(
                            "connector message exceeds the configured ceiling",
                        ));
                    }
                }
                operation.plan_request(&self.origin, &rendered)
            }
            "template.list" => {
                aws::defaulted(&mut rendered, "page_size", json!(50));
                admit_page_size(&rendered, MAX_TEMPLATE_PAGE)?;
                operation.plan_request(&self.origin, &rendered)
            }
            "identity.list" => {
                aws::defaulted(&mut rendered, "page_size", json!(100));
                admit_page_size(&rendered, MAX_IDENTITY_PAGE)?;
                operation.plan_request(&self.origin, &rendered)
            }
            _ => operation.plan_request(&self.origin, &rendered),
        }
    }

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
        operation.decode_response(status, body)
    }
}

fn required_string(input: &JsonValue, name: &str) -> Result<String, ConnectorFailure> {
    match input.get(name) {
        Some(JsonValue::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(ConnectorFailure::validation(
            "a declared connector input value is missing or not a string",
        )),
    }
}

/// The recipients of one send: a bounded list of addresses, each validated
/// before it reaches a message header.
fn admit_recipients(input: &JsonValue) -> Result<(), ConnectorFailure> {
    let Some(JsonValue::Array(recipients)) = input.get("to") else {
        return Err(ConnectorFailure::validation(
            "connector recipients must be a list of addresses",
        ));
    };
    if recipients.is_empty() || recipients.len() > 50 {
        return Err(ConnectorFailure::validation(
            "connector recipient list is outside the declared bounds",
        ));
    }
    for recipient in recipients {
        let valid = recipient
            .as_str()
            .is_some_and(|address| validate_email_address(address).is_ok());
        if !valid {
            return Err(ConnectorFailure::validation(
                "connector recipient is not one address",
            ));
        }
    }
    Ok(())
}

fn admit_page_size(input: &JsonValue, maximum: i64) -> Result<(), ConnectorFailure> {
    match input.get("page_size").and_then(JsonValue::as_i64) {
        Some(value) if (1..=maximum).contains(&value) => Ok(()),
        _ => Err(ConnectorFailure::validation(
            "connector list page size is outside the declared bounds",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sdk::EffectClass;
    use crate::sdk::pagination::undeclared_status_gate;
    use crate::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
    use crate::sdk::transport::RawHttpResponse;

    const ACCESS_KEY_ID: &str = "AKIDONATEXAMPLE";
    const FROM: &str = "billing@donat.example";

    fn configuration() -> SesConfiguration {
        SesConfiguration::new("eu-west-1", FROM).expect("a static configuration is valid")
    }

    fn credential() -> crate::sdk::Credential {
        aws::credential(ACCESS_KEY_ID, SECRET_SENTINEL, "eu-west-1", None)
    }

    fn instance(stub: &ProviderStub) -> SesInstance {
        SesInstance::compile_for_stub(&configuration(), stub.origin())
            .expect("a static configuration compiles")
    }

    fn signed(instance: &SesInstance, id: &str, input: JsonValue) -> RequestPlan {
        let operation = instance.operation(id).expect("the operation is declared");
        let mut request = instance
            .plan(operation, &input)
            .expect("the request renders");
        AuthPlan::aws_sigv4(SERVICE)
            .expect("ses is a static service code")
            .apply(&credential(), &mut request, None)
            .expect("the request signs");
        request
    }

    /// `aws_ses_request_shape`, `aws_ses_auth_is_applied`: the documented
    /// REST-JSON request, signed, with the sending identity from configuration.
    #[tokio::test]
    async fn aws_ses_request_shape_and_auth_are_applied() {
        let stub = ProviderStub::start([
            Expectation::new("POST", "/v2/email/outbound-emails")
                .header("content-type", "application/json")
                .json_body(json!({
                    "FromEmailAddress": FROM,
                    "Destination": { "ToAddresses": ["customer@example.test"] },
                    "Content": {
                        "Simple": {
                            "Subject": { "Data": "Your invoice" },
                            "Body": { "Text": { "Data": "Attached." } },
                        }
                    },
                }))
                .respond_json(200, json!({ "MessageId": "0100-abc" })),
            Expectation::new("POST", "/v2/email/outbound-emails")
                .json_body(json!({
                    "FromEmailAddress": FROM,
                    "Destination": { "ToAddresses": ["customer@example.test"] },
                    "Content": {
                        "Template": {
                            "TemplateName": "invoice",
                            "TemplateData": "{\"name\":\"Ada\"}",
                        }
                    },
                }))
                .respond_json(200, json!({ "MessageId": "0100-def" })),
            Expectation::new("GET", "/v2/email/templates/invoice%2Dv2")
                .query("")
                .respond_json(
                    200,
                    json!({ "TemplateName": "invoice-v2", "TemplateContent": { "Subject": "s" } }),
                ),
            Expectation::new("GET", "/v2/email/identities")
                .query("PageSize=100")
                .respond_json(200, json!({ "EmailIdentities": [] })),
        ])
        .await;
        let instance = instance(&stub);

        for (id, input) in [
            (
                "email.send",
                json!({
                    "to": ["customer@example.test"],
                    "subject": "Your invoice",
                    "text_body": "Attached.",
                }),
            ),
            (
                "email.send_template",
                json!({
                    "to": ["customer@example.test"],
                    "template_name": "invoice",
                    "template_data": "{\"name\":\"Ada\"}",
                }),
            ),
            ("template.get", json!({ "template_name": "invoice-v2" })),
            ("identity.list", json!({})),
        ] {
            let request = signed(&instance, id, input);
            let authorization = request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .expect("every request is signed")
                .to_owned();
            assert!(
                authorization.contains("/eu-west-1/ses/aws4_request,"),
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

    /// `aws_ses_region_and_target_are_deploy_time`: input cannot change the
    /// region, endpoint, or sending identity.
    #[test]
    fn aws_ses_region_and_target_are_deploy_time() {
        let instance = SesInstance::compile(&configuration()).expect("a configuration compiles");
        let send = instance.operation("email.send").expect("declared");
        let good = json!({
            "to": ["customer@example.test"],
            "subject": "s",
            "text_body": "b",
        });

        for name in ["region", "endpoint", "host", "url"] {
            let mut hostile = good.clone();
            hostile[name] = json!("attacker.invalid");
            assert_eq!(
                instance
                    .plan(send, &hostile)
                    .expect_err("deploy-time material is not input")
                    .code(),
                "connector_input_names_deploy_time_configuration"
            );
        }

        let request = instance.plan(send, &good).expect("the request renders");
        assert_eq!(
            request.url().as_str(),
            "https://email.eu-west-1.amazonaws.com/v2/email/outbound-emails"
        );
        let body: JsonValue = serde_json::from_slice(request.body()).expect("the body is JSON");
        assert_eq!(
            body["FromEmailAddress"],
            json!(FROM),
            "the sending identity comes from configuration alone"
        );

        // A `from` in input is simply not a slot the declaration has, so it
        // never reaches the body.
        let mut with_from = good.clone();
        with_from["FromEmailAddress"] = json!("attacker@example.invalid");
        with_from["from"] = json!("attacker@example.invalid");
        let rendered: JsonValue = serde_json::from_slice(
            instance
                .plan(send, &with_from)
                .expect("an unknown input name is ignored by the declaration")
                .body(),
        )
        .expect("the body is JSON");
        assert_eq!(rendered["FromEmailAddress"], json!(FROM));

        // A hostile template name stays one encoded path segment.
        let get = instance.operation("template.get").expect("declared");
        assert_eq!(
            instance
                .plan(get, &json!({ "template_name": "../../identities" }))
                .expect("a hostile name renders")
                .url()
                .path(),
            "/v2/email/templates/%2E%2E%2F%2E%2E%2Fidentities"
        );

        for address in ["", "no-at-sign", "a@b@c", "a@b\nBcc: c@d", "a@b,c@d"] {
            assert!(
                SesConfiguration::new("eu-west-1", address).is_err(),
                "sending identity {address} is refused at startup"
            );
        }
        assert!(SesConfiguration::new("EU-WEST-1", FROM).is_err());
    }

    /// `aws_ses_effects_are_classified`: every operation carries a class, and
    /// both sends are at-most-once (ADR 063) — SES publishes no token to bind,
    /// so a Process reaches them only by accepting that a send it asked for may
    /// never be made.
    #[test]
    fn aws_ses_effects_are_classified() {
        let instance = SesInstance::compile(&configuration()).expect("a configuration compiles");
        assert_eq!(
            [
                "email.send",
                "email.send_template",
                "template.get",
                "template.list",
                "identity.list"
            ]
            .map(|id| instance.operation(id).and_then(Operation::effect_class)),
            [
                Some(EffectClass::AtMostOnce),
                Some(EffectClass::AtMostOnce),
                Some(EffectClass::ReadOnly),
                Some(EffectClass::ReadOnly),
                Some(EffectClass::ReadOnly),
            ]
        );
        for id in ["email.send", "email.send_template"] {
            assert!(
                instance.admit_operation(id).is_ok(),
                "{id} is admitted here and gated again by the activity's own opt-in"
            );
            let effect = instance
                .operation(id)
                .and_then(Operation::effect)
                .expect("classified");
            let evidence = effect
                .no_idempotency_evidence()
                .expect("an at-most-once class carries the search that found no token");
            assert!(
                evidence
                    .searched_documentation()
                    .contains("publishes no idempotency token"),
                "{id} records what was searched"
            );
            assert!(
                evidence
                    .repeat_produces()
                    .contains("a second delivered email"),
                "{id} records what an operator is accepting"
            );
        }
        assert!(instance.admit_operation("template.list").is_ok());
        assert_eq!(
            instance.admit_operation("email.send_raw"),
            Err(OperationRejection::Undeclared)
        );
    }

    /// `aws_ses_error_map`: every documented status reaches exactly one class,
    /// throttling reaches `http_429`, and provider text never crosses.
    #[test]
    fn aws_ses_error_map() {
        let map = error_map();
        let body = serde_json::to_vec(&json!({
            "message": format!("shard db-7.internal rejected key {SECRET_SENTINEL}"),
        }))
        .expect("a fixture serializes");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-amzn-requestid",
            "req_01H".parse().expect("a test header"),
        );

        for (status, expected) in [
            // TooManyRequestsException
            (429, ConnectorErrorClass::Http429),
            // AccessDeniedException, UnrecognizedClientException, and the
            // clock-skew rejection of a signature outside AWS's tolerance.
            (403, ConnectorErrorClass::Authentication),
            // NotAuthorized
            (401, ConnectorErrorClass::Authentication),
            // BadRequestException, MessageRejected, SendingPausedException
            (400, ConnectorErrorClass::Validation),
            // RequestEntityTooLargeException
            (413, ConnectorErrorClass::Validation),
            // RequestTimeoutException
            (408, ConnectorErrorClass::Timeout),
            // NotFoundException
            (404, ConnectorErrorClass::Permanent),
            (500, ConnectorErrorClass::Http5xx),
            (503, ConnectorErrorClass::Http5xx),
            // Unmapped: the declared fallback answers.
            (418, ConnectorErrorClass::Permanent),
        ] {
            let failure = map.classify(status, &headers, &body);
            assert_eq!(failure.class(), expected, "status {status}");
            assert_eq!(failure.provider_status(), Some(status));
            let surface = format!(
                "{} {} {} {failure:?}",
                failure.code(),
                failure.safe_message(),
                failure.diagnostic()
            );
            for leaked in [SECRET_SENTINEL, "db-7.internal", "shard"] {
                assert!(!surface.contains(leaked), "status {status} leaked {leaked}");
            }
            assert_eq!(
                failure
                    .correlation_ids()
                    .get("request_id")
                    .map(String::as_str),
                Some("req_01H")
            );
        }
    }

    /// `aws_ses_output_contract`: the declared pointers are complete and typed,
    /// and a missing required pointer is a `validation` failure, not a null.
    #[tokio::test]
    async fn aws_ses_output_contract() {
        let stub = ProviderStub::start([]).await;
        let instance = instance(&stub);
        let send = instance.operation("email.send").expect("declared");

        assert_eq!(
            instance
                .decode(send, 200, &HeaderMap::new(), br#"{"MessageId":"0100-abc"}"#)
                .expect("the declared output is satisfied"),
            json!({ "message_id": "0100-abc" })
        );
        for body in [br#"{}"#.as_slice(), br#"{"MessageId":null}"#.as_slice()] {
            assert_eq!(
                instance
                    .decode(send, 200, &HeaderMap::new(), body)
                    .expect_err("a missing required pointer is not a null")
                    .class(),
                ConnectorErrorClass::Validation
            );
        }
        assert_eq!(
            instance
                .decode(send, 200, &HeaderMap::new(), br#"{"MessageId":7}"#)
                .expect_err("a declared type is part of the contract")
                .class(),
            ConnectorErrorClass::Validation
        );

        let list = instance.operation("template.list").expect("declared");
        assert_eq!(
            instance
                .decode(
                    list,
                    200,
                    &HeaderMap::new(),
                    br#"{"TemplatesMetadata":[{"TemplateName":"a"}]}"#,
                )
                .expect("an absent optional pointer is an explicit null"),
            json!({ "templates": [{ "TemplateName": "a" }], "next_token": null })
        );
        // An undeclared status is never a silent success.
        assert_eq!(
            instance
                .decode(list, 404, &HeaderMap::new(), br#"{"message":"nope"}"#)
                .expect_err("404 is not a declared success")
                .class(),
            ConnectorErrorClass::Permanent
        );
    }

    /// `aws_ses_pagination_is_bounded`: the declared plan terminates, spends one
    /// budget, and never leaves the compiled origin.
    #[tokio::test]
    async fn aws_ses_pagination_is_bounded() {
        let stub = ProviderStub::start([
            Expectation::new("GET", "/v2/email/templates")
                .query("PageSize=50")
                .respond_json(
                    200,
                    json!({ "TemplatesMetadata": [{ "TemplateName": "a" }], "NextToken": "t2" }),
                ),
            Expectation::new("GET", "/v2/email/templates")
                .query("PageSize=50&NextToken=t2")
                .respond_json(
                    200,
                    json!({ "TemplatesMetadata": [{ "TemplateName": "b" }] }),
                ),
        ])
        .await;
        let instance = instance(&stub);
        let list = instance.operation("template.list").expect("declared");
        let pagination = instance
            .pagination("template.list")
            .expect("a listing declares a continuation plan");

        let items = pagination
            .collect(
                instance
                    .plan(list, &json!({}))
                    .expect("the request renders"),
                instance.origin(),
                &instance.list_budget("template.list", Duration::from_secs(5)),
                undeclared_status_gate,
                |request| async { stub.send(request).await },
            )
            .await
            .expect("the walk terminates");
        assert_eq!(
            items,
            vec![
                json!({ "TemplateName": "a" }),
                json!({ "TemplateName": "b" })
            ]
        );
        stub.assert_satisfied();

        // The budget is shared with the attempt: a plan that never stops
        // offering a continuation fails rather than looping or truncating.
        let endless = ProviderStub::start((0..8).map(|_| {
            Expectation::new("GET", "/v2/email/templates").respond_json(
                200,
                json!({ "TemplatesMetadata": [{ "TemplateName": "a" }], "NextToken": "t" }),
            )
        }))
        .await;
        let endless_instance = SesInstance::compile_for_stub(&configuration(), endless.origin())
            .expect("a static configuration compiles");
        let failure = pagination
            .collect(
                endless_instance
                    .plan(list, &json!({}))
                    .expect("the request renders"),
                endless_instance.origin(),
                &endless_instance.list_budget("template.list", Duration::from_secs(5)),
                undeclared_status_gate,
                |request| async { endless.send(request).await },
            )
            .await
            .expect_err("an endless continuation is bounded, not truncated");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "connector_pagination_budget");

        // A continuation token that spells another origin becomes a query
        // value on the compiled origin, never a destination.
        let offsite = ProviderStub::start([]).await;
        let offsite_instance = SesInstance::compile_for_stub(&configuration(), offsite.origin())
            .expect("a static configuration compiles");
        let offsite_origin = offsite_instance.origin().clone();
        let seen = std::sync::atomic::AtomicUsize::new(0);
        let items = pagination
            .collect(
                offsite_instance
                    .plan(list, &json!({}))
                    .expect("the request renders"),
                offsite_instance.origin(),
                &offsite_instance.list_budget("template.list", Duration::from_secs(5)),
                undeclared_status_gate,
                |request| {
                    let offsite_origin = offsite_origin.clone();
                    let page = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async move {
                        assert_eq!(
                            request.url().host_str(),
                            offsite_origin.as_url().host_str(),
                            "a continuation never leaves the compiled origin"
                        );
                        if page > 0 {
                            let query = request
                                .url()
                                .query()
                                .expect("the continuation is a query value");
                            assert!(
                                query.contains("NextToken=https%3A%2F%2Fattacker")
                                    && !query.contains("://"),
                                "a hostile token is percent-encoded into the query, not followed: \
                                 {query}"
                            );
                        }
                        Ok(RawHttpResponse::json(
                            StatusCode::OK,
                            if page == 0 {
                                json!({
                                    "TemplatesMetadata": [],
                                    "NextToken": "https://attacker.invalid/x",
                                })
                            } else {
                                json!({ "TemplatesMetadata": [] })
                            },
                        ))
                    }
                },
            )
            .await
            .expect("the walk terminates on the compiled origin");
        assert!(items.is_empty());
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    /// `aws_ses_bounds`: a message over the configured ceiling is refused before
    /// any request is made, and the recipient list is bounded and validated.
    #[tokio::test]
    async fn aws_ses_bounds() {
        let stub = ProviderStub::start([]).await;
        let configuration = configuration()
            .with_max_message_bytes(64)
            .expect("a lowered ceiling is valid");
        let instance = SesInstance::compile_for_stub(&configuration, stub.origin())
            .expect("a static configuration compiles");
        let send = instance.operation("email.send").expect("declared");
        let message = |body: String| json!({ "to": ["customer@example.test"], "subject": "s", "text_body": body });

        // The exact ceiling renders; one byte over is refused before a request.
        assert!(instance.plan(send, &message("x".repeat(63))).is_ok());
        assert_eq!(
            instance
                .plan(send, &message("x".repeat(64)))
                .expect_err("one byte over the configured ceiling is refused")
                .class(),
            ConnectorErrorClass::Validation
        );

        for hostile in [
            json!({ "to": [], "subject": "s", "text_body": "b" }),
            json!({ "to": "customer@example.test", "subject": "s", "text_body": "b" }),
            json!({ "to": ["a@b\nBcc: c@d"], "subject": "s", "text_body": "b" }),
            json!({ "to": ["ok@example.test"], "subject": "", "text_body": "b" }),
            json!({ "to": (0..51).map(|i| format!("a{i}@example.test")).collect::<Vec<_>>(),
                    "subject": "s", "text_body": "b" }),
        ] {
            assert_eq!(
                instance
                    .plan(send, &hostile)
                    .expect_err("a message outside the declared bounds never leaves")
                    .class(),
                ConnectorErrorClass::Validation
            );
        }

        let templates = instance.operation("template.list").expect("declared");
        assert!(
            instance
                .plan(templates, &json!({ "page_size": 100 }))
                .is_ok()
        );
        assert!(
            instance
                .plan(templates, &json!({ "page_size": 101 }))
                .is_err()
        );
        assert!(
            instance
                .plan(templates, &json!({ "page_size": 0 }))
                .is_err()
        );
        let identities = instance.operation("identity.list").expect("declared");
        assert!(
            instance
                .plan(identities, &json!({ "page_size": 1000 }))
                .is_ok()
        );
        assert!(
            instance
                .plan(identities, &json!({ "page_size": 1001 }))
                .is_err()
        );
        stub.assert_satisfied();
    }
}
