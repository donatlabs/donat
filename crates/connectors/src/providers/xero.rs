//! Xero's Accounting API — contacts, invoices, and the payments against them.
//!
//! Ground truth is Xero's own published documentation and its own published
//! OpenAPI description, read on 2026-08-10:
//!
//! * <https://github.com/XeroAPI/Xero-OpenAPI> `xero_accounting.yaml`, Xero's
//!   own description of this API. It declares
//!   `servers: [{ url: "https://api.xero.com/api.xro/2.0" }]`, the required
//!   header parameter `xero-tenant-id` ("Xero identifier for Tenant"), the
//!   `Idempotency-Key` header parameter on every mutating operation, the
//!   `unitdp` parameter, and the money fields this module's `amounts` proof
//!   pins (`"type": "number", "format": "double", "x-is-money": true`).
//! * <https://developer.xero.com/documentation/guides/idempotent-requests/idempotency/>
//!   — the three quotations the effect class stands on, below.
//! * <https://developer.xero.com/documentation/api/accounting/requests-and-responses>
//!   — "JSON formatted responses are also supported by setting the “Accept”
//!   value in the http header to “application/json” when making a request", the
//!   paging contract, and the `.NET` date format.
//! * <https://developer.xero.com/documentation/api/accounting/responsecodes> —
//!   the status table this module's error map is built from.
//! * <https://developer.xero.com/documentation/guides/oauth2/limits/> — "Minute
//!   Limit: 60 calls per minute", "Exceeding a rate limit will result in an HTTP
//!   429 (too many requests) response", and "you will also receive a Retry-After
//!   http header that tells you how many seconds to wait".
//!
//! # The effect class, and the three things it needed
//!
//! This is the batch's clearest `ProviderIdempotent::ExplicitKey`, because Xero
//! publishes all three parts of the evidence spec 026 §2 requires:
//!
//! * **Binding.** "To guarantee idempotency, you need to include the
//!   ‘Idempotency-Key’ header on your requests. Xero will then cache the
//!   response to these requests and if subsequent requests are made with the
//!   same idempotency key, they won't be processed and instead the cached
//!   response will be returned." It applies to exactly the methods this module
//!   binds it on: "Xero will only process idempotency for requests that mutate
//!   data, I.e. POST, PUT and PATCH."
//! * **Uniqueness scope.** "Key re-use is procesed per app. If a key has been
//!   re-used by the same app on a different request a 400 server error will be
//!   returned." The namespace is the app — this deployment's own OAuth2 client —
//!   which is exactly the namespace a durable activity's key lives in.
//! * **Retention.** "Idempotency keys are intended to help resolve transient
//!   issues only and so keys are stored for **6 minutes** from the time of the
//!   first call, after which they expire."
//!
//! Six minutes is short, and that is the whole reason [`SEND_HORIZON`] exists.
//! Xero also publishes what happens past it — "Repeating the same key after
//! expiry won't produce this error and will instead be processed as a new key,
//! this should be avoided" — so an activity that keeps retrying past the window
//! stops being idempotent while still holding an idempotency key, which is the
//! `aws_sqs` hazard with a window twenty-five times smaller. A deployment's send
//! horizon is checked at startup against the documented retention less
//! [`CLOCK_SAFETY_MARGIN`]; equality is admitted and one millisecond more is
//! refused.
//!
//! Two further sentences shaped this module rather than its class. "If an
//! idempotent request errors out internally, the error will be cached and
//! returned when the request is re-run even if the internal error is resolved" —
//! so a retry inside the window can return a cached `500`, which this module's
//! error map classifies exactly as it classifies a fresh one. And "Idempotency
//! is checked after rate limits have been applied and thus duplicate requests
//! will count towards rate limits", which is why a `429` here is not evidence
//! that the first send did not happen.
//!
//! # The tenant is deploy-time, and it is not a host
//!
//! Xero serves every organisation from one origin and selects the organisation
//! with the required `xero-tenant-id` header. That makes it the same kind of
//! value a templated host is — the thing that decides whose books are written —
//! so it is filled from deploy-time configuration and from nothing else. There
//! is no operation input, provider response, or continuation that can move it,
//! which `xero_tenant_comes_only_from_deploy_time_configuration` holds shut.
//!
//! # Money is a JSON number, and this module does not reformat it
//!
//! Xero types every money field as `"type": "number", "format": "double"` with
//! its own `x-is-money: true` marker, so the wire form is a JSON *number*. The
//! SDK's [`ValueScalar::Decimal`] means a decimal **string** — that is how this
//! workspace keeps a decimal out of a float — so declaring Xero's `Total` as
//! `Decimal` would refuse every real Xero response, and declaring it `Int64`
//! would refuse its fractional part. Every money field here is therefore
//! declared [`ValueScalar::Json`], which is the one scalar in the contract that
//! carries the provider's own number through unchanged, and
//! `xero_amounts_survive` pins all three halves of that: the digits Xero sent
//! survive the decode, a `Decimal` declaration really would refuse them, and
//! nothing here stringifies, rounds, or truncates an amount.
//!
//! The declaration also asks for four decimal places wherever Xero offers them —
//! "e.g. unitdp=4 – (Unit Decimal Places) You can opt in to use four decimal
//! places for unit amounts" — because the alternative is Xero rounding a unit
//! amount to two before this connector ever sees it.
//!
//! # What is not here
//!
//! Xero publishes no refund endpoint: money is returned by raising a credit note
//! (`ACCRECCREDIT`) and allocating it, which is two writes and a different
//! surface. Spec 026 §3 asks for "the refund create where the provider documents
//! one", and Xero documents none, so this module declares none rather than
//! inventing one out of two calls.

use std::sync::LazyLock;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{Effect, ExplicitKeyEvidence, IdempotencyBinding};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "xero";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ url: "https://api.xero.com/api.xro/2.0" }]`.
const ORIGIN: &str = "https://api.xero.com";

/// The deploy-time configuration key that names the organisation every request
/// is made against.
pub const TENANT_ID: &str = "tenant_id";

/// "xero-tenant-id — Xero identifier for Tenant", `required: true`.
pub const TENANT_HEADER: &str = "xero-tenant-id";

/// "keys are stored for 6 minutes from the time of the first call, after which
/// they expire."
pub const KEY_RETENTION: Duration = Duration::from_secs(6 * 60);

/// Donat's own allowance for clock disagreement between this engine and Xero.
/// It is policy rather than provider evidence, and the effect gate refuses a
/// margin that is not strictly smaller than the documented retention.
pub const CLOCK_SAFETY_MARGIN: Duration = Duration::from_secs(60);

/// The longest a durable activity may keep resending one request under the same
/// idempotency key and still be deduplicated: the documented retention less the
/// clock safety margin.
pub const SEND_HORIZON: Duration =
    Duration::from_secs(KEY_RETENTION.as_secs() - CLOCK_SAFETY_MARGIN.as_secs());

/// "The default page size is 100, with a maximum of 1000 and a minimum of 1."
const PAGE_SIZE: u32 = 100;

/// "e.g. unitdp=4 – (Unit Decimal Places) You can opt in to use four decimal
/// places for unit amounts."
const UNIT_DECIMAL_PLACES: &str = "4";

/// One deployment's Xero configuration.
///
/// It carries the two things a deployment decides and a request may not: which
/// organisation this instance writes to, and how long a durable activity may
/// keep resending one idempotent request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XeroConfiguration {
    tenant_id: String,
    send_horizon: Duration,
}

/// A deploy-time configuration failure, named by the setting that caused it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigurationError {
    setting: &'static str,
    message: &'static str,
}

impl ConfigurationError {
    const fn new(setting: &'static str, message: &'static str) -> Self {
        Self { setting, message }
    }

    pub const fn setting(&self) -> &'static str {
        self.setting
    }

    pub const fn message(&self) -> &'static str {
        self.message
    }
}

impl std::fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.setting, self.message)
    }
}

impl std::error::Error for ConfigurationError {}

impl XeroConfiguration {
    /// Validate one deployment's organisation at startup.
    ///
    /// Xero's tenant identifier is a GUID — "xero-tenant-id: YOUR_XERO_TENANT_ID"
    /// with the example `00000000-0000-0000-0000-000000000000` throughout its
    /// own description — and it becomes a *header value*, so the grammar is
    /// closed here rather than trusted: anything but a hyphenated hexadecimal
    /// identifier is refused before a listener opens.
    pub fn new(tenant_id: &str) -> Result<Self, ConfigurationError> {
        let shape = |value: &str| {
            value.len() == 36
                && value.chars().enumerate().all(|(index, character)| {
                    if matches!(index, 8 | 13 | 18 | 23) {
                        character == '-'
                    } else {
                        character.is_ascii_hexdigit()
                    }
                })
        };
        if !shape(tenant_id) {
            return Err(ConfigurationError::new(
                TENANT_ID,
                "tenant_id is the organisation's Xero tenant identifier, a hyphenated GUID",
            ));
        }
        Ok(Self {
            tenant_id: tenant_id.to_owned(),
            send_horizon: SEND_HORIZON,
        })
    }

    /// The window a durable activity may keep resending one request in.
    ///
    /// It must fit inside the documented retention less the clock safety
    /// margin. Equality is admitted; one millisecond more is refused, because
    /// past that point Xero documents that the same key "will instead be
    /// processed as a new key" — which is a second write, not a replay.
    pub fn with_send_horizon(mut self, horizon: Duration) -> Result<Self, ConfigurationError> {
        if horizon.is_zero() || horizon > SEND_HORIZON {
            return Err(ConfigurationError::new(
                "send_horizon",
                "the send horizon must fit inside the documented six-minute key retention less \
                 the clock safety margin",
            ));
        }
        self.send_horizon = horizon;
        Ok(self)
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub const fn send_horizon(&self) -> Duration {
        self.send_horizon
    }
}

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Xero's published origin is valid"))
            // Xero's Accounting API is authorized with the OAuth2 code flow, so
            // this connector configures no secret at all: the access token is
            // the source-local credential store's, and the seam applies it.
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_authorization_code(),
            ))
            .operations(operations().expect("the Xero declarations are valid"))
            .build()
            .expect("the Xero declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, built from Xero's own "Codes summary" table.
///
/// It reads no body: a `400` carries an `ApiException` whose `Type` is prose
/// ("ValidationException") rather than a stable machine-readable code, and Xero
/// publishes no code list to key a map on.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — A validation exception has occurred", which is
            // also the status a re-used idempotency key with a different
            // request earns: "Idempotency Key: KEY_VALUE is used with a
            // different request."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized — Invalid authorization credentials" and "403
            // Not Permitted — User doesn't have permission to access the
            // resource."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found", "412 Precondition Failed", and "501 Not
            // Implemented" all need a different request.
            .on_statuses([404, 412, 501], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Error — An unhandled error with the Xero API", and
            // "503 Not Available — API is currently unavailable – typically due
            // to a scheduled outage – try again soon", which is also the status
            // of "The Organisation is offline" with its own recommended retry.
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Xero error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each paged collection.
///
/// "To utilise paging you must append a page query parameter to the URL e.g.
/// `?page=1`", with "The default page size is 100", and "Paged results are
/// available on the Invoices, Contacts, CreditNotes, BankTransactions,
/// ManualJournals, Payments, PurchaseOrders, Prepayments and Overpayments
/// endpoints" — which is every collection this module declares.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static CONTACTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/Contacts", "page", "pageSize", PAGE_SIZE)
            .expect("the Xero contact page plan is valid")
    });
    static INVOICES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/Invoices", "page", "pageSize", PAGE_SIZE)
            .expect("the Xero invoice page plan is valid")
    });
    static PAYMENTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/Payments", "page", "pageSize", PAGE_SIZE)
            .expect("the Xero payment page plan is valid")
    });
    match operation_id {
        "contact.list" => Some(&CONTACTS),
        "invoice.list" => Some(&INVOICES),
        "payment.list" => Some(&PAYMENTS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        // "JSON formatted responses are also supported by setting the “Accept”
        // value in the http header to “application/json”" — and without it Xero
        // answers XML, which this connector cannot read.
        .static_header("Accept", "application/json")
        .success_statuses([StatusCode::OK])
}

/// The evidence every mutating operation here is admitted on.
///
/// One constructor call per operation, because [`ExplicitKeyEvidence`] is not
/// `Copy` and each operation carries its own — but the three quotations are the
/// same three, and they are the module header's.
fn explicit_key() -> Result<Effect, OperationError> {
    Ok(Effect::provider_idempotent_explicit_key(
        ExplicitKeyEvidence::documented(
            IdempotencyBinding::header("Idempotency-Key")?,
            // "Key re-use is procesed per app."
            "the Xero app whose OAuth2 client made the request",
            KEY_RETENTION,
            CLOCK_SAFETY_MARGIN,
            "Xero documents the binding — \"To guarantee idempotency, you need to include the \
             ‘Idempotency-Key’ header on your requests… if subsequent requests are made with the \
             same idempotency key, they won't be processed and instead the cached response will \
             be returned\" — the scope — \"Key re-use is procesed per app\" — and the retention — \
             \"keys are stored for 6 minutes from the time of the first call, after which they \
             expire\"",
        )?,
    ))
}

/// The contact fields a billing process reads.
fn contact_output(builder: OperationBuilder, index: &str) -> OperationBuilder {
    builder
        .output_pointer(
            "contact_id",
            &format!("/Contacts/{index}/ContactID"),
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "name",
            &format!("/Contacts/{index}/Name"),
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "email_address",
            &format!("/Contacts/{index}/EmailAddress"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "contact_status",
            &format!("/Contacts/{index}/ContactStatus"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "updated_date_utc",
            &format!("/Contacts/{index}/UpdatedDateUTC"),
            ValueScalar::String,
            Required::Yes,
        )
}

/// The invoice fields a billing process reads, money included.
fn invoice_output(builder: OperationBuilder, index: &str) -> OperationBuilder {
    builder
        .output_pointer(
            "invoice_id",
            &format!("/Invoices/{index}/InvoiceID"),
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "invoice_number",
            &format!("/Invoices/{index}/InvoiceNumber"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "type",
            &format!("/Invoices/{index}/Type"),
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "status",
            &format!("/Invoices/{index}/Status"),
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "currency_code",
            &format!("/Invoices/{index}/CurrencyCode"),
            ValueScalar::String,
            Required::No,
        )
        // "Total of Invoice tax inclusive (i.e. SubTotal + TotalTax)", typed by
        // Xero as a double. See the module header for why a money field here is
        // `Json` rather than `Decimal`.
        .output_pointer(
            "total",
            &format!("/Invoices/{index}/Total"),
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "amount_due",
            &format!("/Invoices/{index}/AmountDue"),
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "amount_paid",
            &format!("/Invoices/{index}/AmountPaid"),
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "updated_date_utc",
            &format!("/Invoices/{index}/UpdatedDateUTC"),
            ValueScalar::String,
            Required::Yes,
        )
}

/// The payment fields a billing process reads.
fn payment_output(builder: OperationBuilder, index: &str) -> OperationBuilder {
    builder
        .output_pointer(
            "payment_id",
            &format!("/Payments/{index}/PaymentID"),
            ValueScalar::String,
            Required::Yes,
        )
        // "The amount of the payment. Must be less than or equal to the
        // outstanding amount owing on the invoice e.g. 200.00."
        .output_pointer(
            "amount",
            &format!("/Payments/{index}/Amount"),
            ValueScalar::Json,
            Required::Yes,
        )
        .output_pointer(
            "date",
            &format!("/Payments/{index}/Date"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "status",
            &format!("/Payments/{index}/Status"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "reference",
            &format!("/Payments/{index}/Reference"),
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
///
/// A create is a `PUT` and an update is a `POST`, which is Xero's own split:
/// `PUT /Invoices` "Creates one or more sales invoices or purchase bills" while
/// `POST /Invoices` "Updates or creates" them. Both carry the same
/// `Idempotency-Key` evidence, and neither is `NaturalMethod`: a `PUT` here
/// creates a *new* resource with a Xero-generated identifier rather than writing
/// to a fixed one, so the method proves nothing and the key is what makes it
/// repeat-safe.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let contact_list = common(Operation::get("contact.list", "/api.xro/2.0/Contacts"))
        // "The where parameter allows you to filter on endpoints and elements
        // that don't have explicit parameters", percent-encoded as a value.
        .query_input("where", "where")
        .output_pointer("contacts", "/Contacts", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "item_count",
            "/pagination/itemCount",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let contact_get = contact_output(
        common(Operation::get(
            "contact.get",
            "/api.xro/2.0/Contacts/{contact_id}",
        ))
        .path_param("contact_id", ValueScalar::Uuid),
        "0",
    )
    .effect(Effect::read_only())
    .build()?;

    let contact_create = contact_output(
        common(Operation::put("contact.create", "/api.xro/2.0/Contacts"))
            .body(JsonTemplate::object([(
                "Contacts",
                JsonTemplate::input("contacts"),
            )]))
            .declared_input("contacts", ValueScalar::Json, Required::Yes),
        "0",
    )
    .effect(explicit_key()?)
    .build()?;

    let contact_update = contact_output(
        common(Operation::post(
            "contact.update",
            "/api.xro/2.0/Contacts/{contact_id}",
        ))
        .path_param("contact_id", ValueScalar::Uuid)
        .body(JsonTemplate::object([(
            "Contacts",
            JsonTemplate::input("contacts"),
        )]))
        .declared_input("contacts", ValueScalar::Json, Required::Yes),
        "0",
    )
    .effect(explicit_key()?)
    .build()?;

    let invoice_list = common(Operation::get("invoice.list", "/api.xro/2.0/Invoices"))
        .query_input("where", "where")
        .query_static("unitdp", UNIT_DECIMAL_PLACES)
        .output_pointer("invoices", "/Invoices", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "item_count",
            "/pagination/itemCount",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let invoice_get = invoice_output(
        common(Operation::get(
            "invoice.get",
            "/api.xro/2.0/Invoices/{invoice_id}",
        ))
        .path_param("invoice_id", ValueScalar::Uuid)
        .query_static("unitdp", UNIT_DECIMAL_PLACES),
        "0",
    )
    .effect(Effect::read_only())
    .build()?;

    let invoice_create = invoice_output(
        common(Operation::put("invoice.create", "/api.xro/2.0/Invoices"))
            .query_static("unitdp", UNIT_DECIMAL_PLACES)
            .body(JsonTemplate::object([(
                "Invoices",
                JsonTemplate::input("invoices"),
            )]))
            .declared_input("invoices", ValueScalar::Json, Required::Yes),
        "0",
    )
    .effect(explicit_key()?)
    .build()?;

    let payment_list = common(Operation::get("payment.list", "/api.xro/2.0/Payments"))
        .query_input("where", "where")
        .output_pointer("payments", "/Payments", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "item_count",
            "/pagination/itemCount",
            ValueScalar::Int64,
            Required::No,
        )
        .effect(Effect::read_only())
        .build()?;

    let payment_get = payment_output(
        common(Operation::get(
            "payment.get",
            "/api.xro/2.0/Payments/{payment_id}",
        ))
        .path_param("payment_id", ValueScalar::Uuid),
        "0",
    )
    .effect(Effect::read_only())
    .build()?;

    // "POST /Payments — Creates a single payment for invoice or credit notes."
    // This is the operation that moves money in this connector, and it is
    // executable because Xero publishes a key, a scope, and a retention for it.
    let payment_create = payment_output(
        common(Operation::post("payment.create", "/api.xro/2.0/Payments"))
            .body(JsonTemplate::object([
                ("Invoice", JsonTemplate::input("invoice")),
                ("Account", JsonTemplate::input("account")),
                ("Amount", JsonTemplate::input("amount")),
                ("Date", JsonTemplate::input("date")),
                ("Reference", JsonTemplate::input("reference")),
            ]))
            .declared_input("invoice", ValueScalar::Json, Required::Yes)
            .declared_input("account", ValueScalar::Json, Required::Yes)
            // The number Xero documents ("e.g. 200.00"), carried as the JSON
            // value the caller supplied rather than retyped on the way past.
            .declared_input("amount", ValueScalar::Json, Required::Yes)
            .declared_input("date", ValueScalar::Json, Required::Yes)
            .declared_input("reference", ValueScalar::Json, Required::Yes),
        "0",
    )
    .effect(explicit_key()?)
    .build()?;

    Ok(vec![
        contact_list,
        contact_get,
        contact_create,
        contact_update,
        invoice_list,
        invoice_get,
        invoice_create,
        payment_list,
        payment_get,
        payment_create,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `xero_send_horizon_fits_the_window` (spec 026 §4 proof 2): the compiled
    /// maximum send horizon fits inside the documented retention less the clock
    /// margin. Equality passes; one millisecond over rejects.
    #[test]
    fn xero_send_horizon_fits_the_window() {
        assert_eq!(KEY_RETENTION, Duration::from_secs(360));
        assert!(
            CLOCK_SAFETY_MARGIN < KEY_RETENTION,
            "the margin is strictly smaller than the documented retention"
        );
        assert_eq!(SEND_HORIZON, KEY_RETENTION - CLOCK_SAFETY_MARGIN);

        let configured = || {
            XeroConfiguration::new("00000000-0000-0000-0000-000000000042")
                .expect("a GUID tenant identifier is valid")
        };
        assert_eq!(configured().send_horizon(), SEND_HORIZON);
        assert!(
            configured().with_send_horizon(SEND_HORIZON).is_ok(),
            "the exact horizon is admitted"
        );
        assert_eq!(
            configured()
                .with_send_horizon(SEND_HORIZON + Duration::from_millis(1))
                .expect_err("one millisecond over the horizon is refused")
                .setting(),
            "send_horizon"
        );
        assert!(configured().with_send_horizon(Duration::ZERO).is_err());

        // The horizon is measured against the retention the class publishes, so
        // the two cannot drift apart.
        let evidence = connector()
            .operation("payment.create")
            .and_then(Operation::effect)
            .and_then(Effect::explicit_key_evidence)
            .expect("the payment create carries explicit key evidence")
            .clone();
        assert_eq!(
            evidence.retention().minimum() - evidence.retention().clock_safety_margin(),
            SEND_HORIZON
        );
    }

    /// A tenant identifier is a GUID, and it is checked at startup because it
    /// becomes a header value on every request this instance makes.
    #[test]
    fn xero_tenant_identifier_grammar_is_closed() {
        assert!(XeroConfiguration::new("00000000-0000-0000-0000-000000000042").is_ok());
        for rejected in [
            "",
            "not-a-guid",
            "00000000-0000-0000-0000-00000000004",
            "00000000-0000-0000-0000-0000000000429",
            "00000000-0000-0000-0000-00000000004g",
            "00000000_0000_0000_0000_000000000042",
            "00000000-0000-0000-0000-000000000042\r\nx-injected: 1",
        ] {
            assert_eq!(
                XeroConfiguration::new(rejected)
                    .expect_err("a value that is not a GUID is refused")
                    .setting(),
                TENANT_ID,
                "{rejected}"
            );
        }
    }
}
