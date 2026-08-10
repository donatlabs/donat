//! PayPal's REST APIs — orders, captures and refunds, invoices, subscriptions,
//! and the transaction report.
//!
//! Ground truth is PayPal's own published documentation and its own published
//! OpenAPI descriptions, read on 2026-08-10:
//!
//! * <https://github.com/paypal/paypal-rest-api-specifications> — PayPal's own
//!   OpenAPI descriptions: `checkout_orders_v2.json`, `payments_payment_v2.json`,
//!   `invoicing_v2.json`, `billing_subscriptions_v1.json`, and
//!   `reporting_transactions_v1.json`. Every path, status, parameter, and money
//!   type below is read from those files.
//! * <https://developer.paypal.com/api/rest/> — the access token: "POST
//!   /v1/oauth2/token", Basic authentication over `CLIENT_ID:CLIENT_SECRET`, and
//!   the body "grant_type=client_credentials"; then "replace `ACCESS-TOKEN` with
//!   your access token in the authorization header: `-H Authorization: Bearer
//!   ACCESS-TOKEN`".
//! * <https://developer.paypal.com/api/rest/reference/idempotency/> — the
//!   uniqueness scope, quoted below.
//! * <https://developer.paypal.com/api/rest/requests/> — the general header
//!   guide, including the 45-day refund example this module deliberately does
//!   *not* build a class on.
//! * <https://developer.paypal.com/api/rest/responses/> — the status table this
//!   module's error map is built from, including "`429 Too Many Requests` – Too
//!   many requests. Blocked due to rate limiting."
//!
//! # The credential is a client-credentials token, and that is why this is here
//!
//! PayPal authorizes every REST call with an OAuth2 **client-credentials**
//! access token minted from a client id and secret. It is the first connector in
//! the programme to declare [`AuthPlan::oauth2_client_credentials`], which is
//! why the plan's serving half had to be built first: the plan and its
//! `token_request` existed in the SDK and nothing in the executor ever called
//! them, so a connector declaring it would have sent unauthenticated requests
//! ([[034-a-declaration-the-runtime-ignores-is-a-defect]], and
//! [[072-a-minted-credential-is-spent-inside-one-attempt]] for the fix).
//!
//! The token is fetched once per logical attempt, spends the operation's own
//! deadline, and is dropped when the attempt ends. It is never stored: spec 011
//! §8's `oauth_client_credentials_is_not_persisted` is the rule, and this
//! connector configures no `oauth2` block at all — its credential is two
//! ordinary `SecretRef`s.
//!
//! # The idempotency evidence, and why it is read per API
//!
//! PayPal publishes the **binding** and the **uniqueness scope** once, for the
//! whole REST surface:
//!
//! * **Binding.** "`PayPal-Request-Id` contains a unique user-generated ID that
//!   the server stores for a period of time. Use this header to enforce
//!   idempotency on REST API `POST` calls."
//! * **Uniqueness scope.** "The `PayPal-Request-Id` header value must be unique
//!   for both each request and an API call type. For example, authorize payment
//!   and capture authorized payment." The namespace is therefore the API
//!   caller's own account — the client credentials this instance is configured
//!   with — further separated by the call type, which is exactly the namespace a
//!   durable activity's key already lives in.
//!
//! The **retention** it publishes per API, and the numbers differ, which is why
//! this module carries more than one:
//!
//! * Orders v2 — "The server stores keys for 6 hours. The API callers can
//!   request the times to up to 72 hours by speaking to their Account Manager."
//!   ([`ORDERS_KEY_RETENTION`])
//! * Billing Subscriptions v1 — "The server stores keys for 72 hours."
//!   ([`SUBSCRIPTIONS_KEY_RETENTION`])
//! * Payments v2 — the header is published ("A unique ID identifying the request
//!   header for idempotency purposes") **with no window at all**.
//! * Invoicing v2 — the header does not appear in the description at all.
//!
//! The declared retention is the one the operation's *own* reference publishes,
//! and the default upgrade PayPal offers ("up to 72 hours by speaking to their
//! Account Manager") is deliberately not taken: a deployment that arranged it
//! may narrow its horizon but this connector may not widen the class on a
//! conversation it cannot see. Where an operation's own reference publishes no
//! window, the class is refused — see `refund.create` below, and
//! [[073-a-retention-is-read-from-the-reference-that-owns-the-operation]].
//!
//! # A replay answers `200` where the first send answered `201`
//!
//! This is a property of the declaration, not a note: PayPal documents, for
//! *Capture payment for order*, "A successful response to a non-idempotent
//! request returns the HTTP `201 Created` status code with a JSON response body
//! that shows captured payment details. If a duplicate response is retried,
//! returns the HTTP `200 OK` status code", and for *Create order*, "A successful
//! response to an idempotent request returns the HTTP `200 OK` status code with
//! a JSON response body that shows order details." Every keyed operation here
//! therefore declares **both** `200` and `201` as success. A declaration that
//! admitted only `201` would read a successful deduplication — the exact thing
//! the key exists to produce — as a failure.
//!
//! # Money is a string beside its currency
//!
//! PayPal's `Money` object is `{ "currency_code": …, "value": … }` and it types
//! `value` as `"type": "string"` with the pattern
//! `^((-?[0-9]+)|(-?([0-9]+)?[.][0-9]+))$`, describing it as "An integer for
//! currencies like `JPY` that are not typically fractional… A decimal fraction
//! for currencies like `TND` that are subdivided into thousandths." Every money
//! field here is therefore [`ValueScalar::String`] and is carried verbatim in
//! both directions, per
//! [[071-money-is-declared-in-the-shape-the-provider-publishes-it]]. Nothing
//! here parses an amount into a number or renders one as a number: `"10.00"` is
//! not `10.0`, and a currency PayPal writes as `"1000"` is not `1000`.
//!
//! # What is not here
//!
//! PayPal has no customer API in this surface, so spec 026 §3's "customer read,
//! list, create and update" has no counterpart and none is invented. Webhooks,
//! disputes, payouts, plans and products, vaulting, and partner referrals are
//! out of scope (spec 026 §6), and the webhook surface is the one that matters
//! most for a payments provider — it is flagged in this batch's report.

use std::sync::LazyLock;
use std::time::Duration;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{
    AbsenceSearch, Effect, ExplicitKeyEvidence, IdempotencyBinding, NoIdempotencyEvidence,
};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{
    JsonTemplate, Operation, OperationBuilder, OperationError, Origin, Required,
};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "paypal";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// `servers: [{ url: "https://api-m.paypal.com" }]`, in PayPal's own OpenAPI for
/// every API this connector calls.
const ORIGIN: &str = "https://api-m.paypal.com";

/// "POST /v1/oauth2/token" — the same origin, which is what PayPal publishes and
/// is why this connector declares no second host.
const TOKEN_PATH: &str = "/v1/oauth2/token";

/// `PayPal-Request-Id`, the header the whole REST surface documents.
pub const IDEMPOTENCY_HEADER: &str = "PayPal-Request-Id";

/// "The `PayPal-Request-Id` header value must be unique for both each request
/// and an API call type."
const KEY_SCOPE: &str =
    "the PayPal account whose client credentials made the request, separated by API call type";

/// Orders v2: "The server stores keys for 6 hours."
pub const ORDERS_KEY_RETENTION: Duration = Duration::from_secs(6 * 60 * 60);

/// Billing Subscriptions v1: "The server stores keys for 72 hours."
pub const SUBSCRIPTIONS_KEY_RETENTION: Duration = Duration::from_secs(72 * 60 * 60);

/// Donat's own allowance for clock disagreement between this engine and PayPal.
/// It is policy rather than provider evidence, and the effect gate refuses a
/// margin that is not strictly smaller than the documented retention.
pub const CLOCK_SAFETY_MARGIN: Duration = Duration::from_secs(60);

/// The longest a durable activity may keep resending one request under the same
/// idempotency key and still be deduplicated.
///
/// One instance holds operations from two APIs with two published windows, so
/// the deployment-wide horizon is the **shortest** of them less the clock safety
/// margin: a horizon that fitted Billing Subscriptions' 72 hours would be four
/// hundred times past Orders v2's window, and past that point PayPal is not
/// deduplicating anything — it is taking a new payment.
pub const SEND_HORIZON: Duration =
    Duration::from_secs(shortest_retention_secs() - CLOCK_SAFETY_MARGIN.as_secs());

const fn shortest_retention_secs() -> u64 {
    if ORDERS_KEY_RETENTION.as_secs() < SUBSCRIPTIONS_KEY_RETENTION.as_secs() {
        ORDERS_KEY_RETENTION.as_secs()
    } else {
        SUBSCRIPTIONS_KEY_RETENTION.as_secs()
    }
}

/// Invoicing v2: "`page_size` — The maximum number of templates to return in the
/// response", `maximum: 100`.
const INVOICE_PAGE_SIZE: u32 = 100;

/// Transaction search: "`page_size` — The number of items to return in the
/// response."
const TRANSACTION_PAGE_SIZE: u32 = 100;

/// One deployment's PayPal configuration.
///
/// The only thing a deployment decides here is how long a durable activity may
/// keep resending one idempotent request; the account is the credential's, and
/// the origin is PayPal's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaypalConfiguration {
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

impl Default for PaypalConfiguration {
    fn default() -> Self {
        Self::new()
    }
}

impl PaypalConfiguration {
    pub const fn new() -> Self {
        Self {
            send_horizon: SEND_HORIZON,
        }
    }

    /// The window a durable activity may keep resending one request in.
    ///
    /// It must fit inside the shortest documented retention less the clock
    /// safety margin. Equality is admitted; one millisecond more is refused,
    /// because past that point PayPal has forgotten the key and the same send is
    /// a second order or a second subscription.
    pub fn with_send_horizon(mut self, horizon: Duration) -> Result<Self, ConfigurationError> {
        if horizon.is_zero() || horizon > SEND_HORIZON {
            return Err(ConfigurationError::new(
                "send_horizon",
                "the send horizon must fit inside PayPal's shortest documented key retention (six \
                 hours, Orders v2) less the clock safety margin",
            ));
        }
        self.send_horizon = horizon;
        Ok(self)
    }

    pub const fn send_horizon(&self) -> Duration {
        self.send_horizon
    }
}

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("PayPal's published origin is valid"))
            // "Call the /v1/oauth2/token endpoint with your client ID and client
            // secret… grant_type=client_credentials". No scope is requested:
            // PayPal grants the scopes the app was configured with and publishes
            // no scope parameter for this grant.
            .credential(CredentialSpec::for_plan(
                AuthPlan::oauth2_client_credentials(
                    Origin::parse(ORIGIN).expect("PayPal's published token origin is valid"),
                    TOKEN_PATH,
                    &[],
                )
                .expect("PayPal's published token endpoint is valid"),
            ))
            .operations(operations().expect("the PayPal declarations are valid"))
            .build()
            .expect("the PayPal declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map, built from PayPal's own status table.
///
/// It reads no body: PayPal's error `name` is a per-API vocabulary
/// (`INVALID_REQUEST`, `NOT_AUTHORIZED`, `UNPROCESSABLE_ENTITY`, and a long tail
/// of per-endpoint issues) rather than one stable code list a map could key on,
/// and the `message` is prose that must not cross this boundary.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request – Request is not well-formed, syntactically
            // incorrect, or violates schema" and "422 Unprocessable Entity – The
            // API cannot complete the requested action", which is also the
            // status of a failed business validation.
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "401 Unauthorized" and "403 Forbidden – Authorization failed due
            // to insufficient permissions."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found", "405 Method Not Allowed", "415 Unsupported Media
            // Type", and "409 Conflict – The request could not be completed due
            // to a conflict with the current state of the resource" all need a
            // different request rather than the same one again.
            .on_statuses([404, 405, 409, 415], ConnectorErrorClass::Permanent)
            // "429 Too Many Requests – Too many requests. Blocked due to rate
            // limiting."
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error" and "503 Service Unavailable – The
            // server cannot handle the request for a service due to temporary
            // maintenance."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            // "debug_id" is PayPal's own correlation identifier and the one
            // thing its support asks for; it is a header on every answer.
            .correlation_header("paypal_debug_id", "paypal-debug-id")
            .build()
            .expect("the PayPal error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each paged collection.
///
/// Two of PayPal's APIs publish a page-number walk this SDK can spend, and they
/// are the only two collections here: Invoicing v2's *List invoices* ("a
/// combination of `page=1` and `page_size=20` returns the first 20") and
/// Transaction search's *List transactions* ("the combination of `page=1` and
/// `page_size=20` returns the first 20 items"). Every other operation is a
/// single resource and declares no plan
/// ([[058-a-declared-walk-is-the-executors-walk]]).
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static INVOICES: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number("/items", "page", "page_size", INVOICE_PAGE_SIZE)
            .expect("the PayPal invoice page plan is valid")
    });
    static TRANSACTIONS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::page_number(
            "/transaction_details",
            "page",
            "page_size",
            TRANSACTION_PAGE_SIZE,
        )
        .expect("the PayPal transaction page plan is valid")
    });
    match operation_id {
        "invoice.list" => Some(&INVOICES),
        "transaction.list" => Some(&TRANSACTIONS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// A keyed mutation's success statuses.
///
/// Both, always, and the reason is the class itself: PayPal answers `201` to the
/// first send and `200` to the deduplicated replay, so an operation that
/// admitted only one of them would read the key doing its job as a failure.
fn keyed_statuses(builder: OperationBuilder) -> OperationBuilder {
    builder.success_statuses([StatusCode::OK, StatusCode::CREATED])
}

/// The evidence a keyed operation is admitted on, for one API's own published
/// retention.
fn explicit_key(retention: Duration, retention_quotation: &str) -> Result<Effect, OperationError> {
    Ok(Effect::provider_idempotent_explicit_key(
        ExplicitKeyEvidence::documented(
            IdempotencyBinding::header(IDEMPOTENCY_HEADER)?,
            KEY_SCOPE,
            retention,
            CLOCK_SAFETY_MARGIN,
            &format!(
                "PayPal documents the binding — \"PayPal-Request-Id contains a unique \
                 user-generated ID that the server stores for a period of time. Use this header \
                 to enforce idempotency on REST API POST calls\" — the scope — \"The \
                 PayPal-Request-Id header value must be unique for both each request and an API \
                 call type\" — and, in this operation's own reference, the retention — \
                 \"{retention_quotation}\""
            ),
        )?,
    ))
}

/// The money pair PayPal publishes, read at one pointer prefix.
///
/// `value` is a **string** in PayPal's own schema, and it is declared as one
/// here so that neither direction turns it into a float.
fn money(builder: OperationBuilder, name: &str, pointer: &str) -> OperationBuilder {
    builder
        .output_pointer(
            &format!("{name}_currency_code"),
            &format!("{pointer}/currency_code"),
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            &format!("{name}_value"),
            &format!("{pointer}/value"),
            ValueScalar::String,
            Required::No,
        )
}

/// The order fields a billing process reads.
fn order_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("intent", "/intent", ValueScalar::String, Required::No)
        .output_pointer(
            "purchase_units",
            "/purchase_units",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "create_time",
            "/create_time",
            ValueScalar::String,
            Required::No,
        )
}

/// The capture and refund fields a billing process reads. Both resources
/// publish the same three: an id, a status, and the `Money` that moved.
fn captured_output(builder: OperationBuilder) -> OperationBuilder {
    money(
        builder
            .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
            .output_pointer("status", "/status", ValueScalar::String, Required::No),
        "amount",
        "/amount",
    )
    .output_pointer(
        "create_time",
        "/create_time",
        ValueScalar::String,
        Required::No,
    )
}

/// The invoice fields a billing process reads.
fn invoice_output(builder: OperationBuilder) -> OperationBuilder {
    money(
        money(
            builder
                .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
                .output_pointer("status", "/status", ValueScalar::String, Required::No)
                .output_pointer(
                    "invoice_number",
                    "/detail/invoice_number",
                    ValueScalar::String,
                    Required::No,
                )
                .output_pointer(
                    "invoice_date",
                    "/detail/invoice_date",
                    ValueScalar::String,
                    Required::No,
                ),
            "amount",
            "/amount",
        ),
        "due_amount",
        "/due_amount",
    )
}

/// The subscription fields a billing process reads.
fn subscription_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("plan_id", "/plan_id", ValueScalar::String, Required::No)
        .output_pointer(
            "start_time",
            "/start_time",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("quantity", "/quantity", ValueScalar::String, Required::No)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // ---- Orders v2 -------------------------------------------------------
    //
    // "The server stores keys for 6 hours", so both writes here are
    // `ExplicitKey` on a six-hour window.
    let orders_retention = "The server stores keys for 6 hours. The API callers can request the \
                            times to up to 72 hours by speaking to their Account Manager.";

    let order_create = order_output(keyed_statuses(common(Operation::post(
        "order.create",
        "/v2/checkout/orders",
    ))))
    .body(JsonTemplate::object([
        ("intent", JsonTemplate::input("intent")),
        ("purchase_units", JsonTemplate::input("purchase_units")),
    ]))
    // "intent — The intent to either capture payment immediately or authorize
    // a payment for an order after order creation", required.
    .declared_input("intent", ValueScalar::String, Required::Yes)
    // "purchase_units — An array of purchase units… Each purchase unit
    // establishes a contract between a payer and the payee."
    .declared_input("purchase_units", ValueScalar::Json, Required::Yes)
    .effect(explicit_key(ORDERS_KEY_RETENTION, orders_retention)?)
    .build()?;

    let order_get = order_output(
        common(Operation::get("order.get", "/v2/checkout/orders/{id}"))
            .path_param("id", ValueScalar::String)
            .success_statuses([StatusCode::OK]),
    )
    // Orders v2 *Show order details*: "Shows details for an order, by ID."
    .effect(Effect::read_only())
    .build()?;

    // "Captures payment for an order. To successfully capture payment for an
    // order, the buyer must first approve the order." The body is optional and
    // PayPal documents an empty one, which is what a process that captures the
    // whole approved order sends.
    let order_capture = order_output(keyed_statuses(common(Operation::post(
        "order.capture",
        "/v2/checkout/orders/{id}/capture",
    ))))
    .path_param("id", ValueScalar::String)
    .effect(explicit_key(ORDERS_KEY_RETENTION, orders_retention)?)
    .build()?;

    // ---- Payments v2 -----------------------------------------------------

    let capture_get = captured_output(
        common(Operation::get("capture.get", "/v2/payments/captures/{id}"))
            .path_param("id", ValueScalar::String)
            .success_statuses([StatusCode::OK]),
    )
    // Payments v2 *Show captured payment details*: "Shows details for a
    // captured payment, by ID."
    .effect(Effect::read_only())
    .build()?;

    // The refund. It is declared, typed, and tested, and it is **not**
    // executable — see the module header and
    // [[073-a-retention-is-read-from-the-reference-that-owns-the-operation]].
    let refund_create = captured_output(keyed_statuses(common(Operation::post(
        "refund.create",
        "/v2/payments/captures/{id}/refund",
    ))))
    .path_param("id", ValueScalar::String)
    .body(JsonTemplate::object([
        ("amount", JsonTemplate::input("amount")),
        ("note_to_payer", JsonTemplate::input("note_to_payer")),
        ("invoice_id", JsonTemplate::input("invoice_id")),
    ]))
    // PayPal's `Money`: `{ "currency_code": …, "value": … }`, carried through as
    // the caller supplied it.
    .declared_input("amount", ValueScalar::Json, Required::No)
    .declared_input("note_to_payer", ValueScalar::String, Required::No)
    .declared_input("invoice_id", ValueScalar::String, Required::No)
    .effect(Effect::inventory_only(
        "PayPal publishes the binding and the replay for a refund — Payments v2 types \
         `PayPal-Request-Id` as \"A unique ID identifying the request header for idempotency \
         purposes\" and its own example \"Demonstrates an idempotent refund request where the \
         same PayPal-Request-Id is used, resulting in a 200 OK response with the existing refund \
         details\" — but publishes no retention for it in that reference, and its idempotency \
         guide says \"for information about how long the server stores the ID, see the reference \
         for your API\". A key the provider may already have forgotten is not a key (spec 026 \
         §2), and a second refund is not an outcome to accept an at-most-once trade on (spec 026 \
         §3). One sentence from PayPal naming a window in the Payments v2 reference would make \
         this executable.",
    )?)
    .build()?;

    // ---- Invoicing v2 ----------------------------------------------------

    let invoice_list = common(Operation::get("invoice.list", "/v2/invoicing/invoices"))
        // "total_required — Indicates whether the to show total_pages and
        // total_items in the response."
        .query_static("total_required", "true")
        .success_statuses([StatusCode::OK])
        .output_pointer("items", "/items", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "total_items",
            "/total_items",
            ValueScalar::Int64,
            Required::No,
        )
        // Invoicing v2 *List invoices*: "Lists invoices. To filter the
        // invoices that appear in the response, you can specify one or more
        // optional query parameters."
        .effect(Effect::read_only())
        .build()?;

    let invoice_get = invoice_output(
        common(Operation::get(
            "invoice.get",
            "/v2/invoicing/invoices/{invoice_id}",
        ))
        .path_param("invoice_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    // Invoicing v2 *Show invoice details*: "Shows details for an invoice, by
    // ID."
    .effect(Effect::read_only())
    .build()?;

    // Invoicing v2 does not publish `PayPal-Request-Id` at all: the term does
    // not occur anywhere in PayPal's own `invoicing_v2.json`, which enumerates
    // every parameter of every one of its endpoints.
    let invoice_create = invoice_output(
        common(Operation::post("invoice.create", "/v2/invoicing/invoices"))
            .body(JsonTemplate::object([
                ("detail", JsonTemplate::input("detail")),
                ("invoicer", JsonTemplate::input("invoicer")),
                (
                    "primary_recipients",
                    JsonTemplate::input("primary_recipients"),
                ),
                ("items", JsonTemplate::input("items")),
            ]))
            .declared_input("detail", ValueScalar::Json, Required::Yes)
            .declared_input("invoicer", ValueScalar::Json, Required::No)
            .declared_input("primary_recipients", ValueScalar::Json, Required::No)
            .declared_input("items", ValueScalar::Json, Required::No)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        "PayPal's own OpenAPI description of Invoicing v2 (`invoicing_v2.json` in \
         paypal/paypal-rest-api-specifications), which enumerates every parameter of every \
         endpoint: the string `PayPal-Request-Id` does not occur in it, and the only \
         `POST /v2/invoicing/invoices` parameters are the body's own fields",
        "a second draft invoice with a new `INV2-` id against the same recipient, which a \
         `send` would then deliver as a duplicate bill",
    )?))
    .build()?;

    // ---- Billing Subscriptions v1 ----------------------------------------

    let subscription_create = subscription_output(keyed_statuses(common(Operation::post(
        "subscription.create",
        "/v1/billing/subscriptions",
    ))))
    .body(JsonTemplate::object([
        ("plan_id", JsonTemplate::input("plan_id")),
        ("quantity", JsonTemplate::input("quantity")),
        ("subscriber", JsonTemplate::input("subscriber")),
        (
            "application_context",
            JsonTemplate::input("application_context"),
        ),
    ]))
    // "plan_id — The ID of the plan", required.
    .declared_input("plan_id", ValueScalar::String, Required::Yes)
    // "quantity — The quantity of the product in the subscription", typed by
    // PayPal as a string.
    .declared_input("quantity", ValueScalar::String, Required::No)
    .declared_input("subscriber", ValueScalar::Json, Required::No)
    .declared_input("application_context", ValueScalar::Json, Required::No)
    .effect(explicit_key(
        SUBSCRIPTIONS_KEY_RETENTION,
        "The server stores keys for 72 hours.",
    )?)
    .build()?;

    let subscription_get = subscription_output(
        common(Operation::get(
            "subscription.get",
            "/v1/billing/subscriptions/{id}",
        ))
        .path_param("id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    // Billing Subscriptions v1 *Show subscription details*: "Shows details for
    // a subscription, by ID."
    .effect(Effect::read_only())
    .build()?;

    // ---- Transaction search ----------------------------------------------
    //
    // "start_date" and "end_date" are `required: true` in PayPal's own
    // description, so they are declared required here rather than defaulted.
    let transaction_list = common(Operation::get(
        "transaction.list",
        "/v1/reporting/transactions",
    ))
    .query_input("start_date", "start_date")
    .query_input("end_date", "end_date")
    .query_input("transaction_status", "transaction_status")
    .query_input("fields", "fields")
    .success_statuses([StatusCode::OK])
    .output_pointer(
        "transaction_details",
        "/transaction_details",
        ValueScalar::Json,
        Required::Yes,
    )
    .output_pointer(
        "total_items",
        "/total_items",
        ValueScalar::Int64,
        Required::No,
    )
    // Transaction search *List transactions*: "Lists transactions."
    .effect(Effect::read_only())
    .build()?;

    Ok(vec![
        order_create,
        order_get,
        order_capture,
        capture_get,
        refund_create,
        invoice_list,
        invoice_get,
        invoice_create,
        subscription_create,
        subscription_get,
        transaction_list,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `paypal_send_horizon_fits_the_window` (spec 026 §4 proof 2): the compiled
    /// maximum send horizon fits inside the **shortest** documented retention
    /// less the clock margin. Equality passes; one millisecond over rejects.
    #[test]
    fn paypal_send_horizon_fits_the_window() {
        assert_eq!(ORDERS_KEY_RETENTION, Duration::from_secs(21_600));
        assert_eq!(SUBSCRIPTIONS_KEY_RETENTION, Duration::from_secs(259_200));
        assert!(CLOCK_SAFETY_MARGIN < ORDERS_KEY_RETENTION);
        assert_eq!(SEND_HORIZON, ORDERS_KEY_RETENTION - CLOCK_SAFETY_MARGIN);

        let configured = PaypalConfiguration::new();
        assert_eq!(configured.send_horizon(), SEND_HORIZON);
        assert!(
            configured.clone().with_send_horizon(SEND_HORIZON).is_ok(),
            "the exact horizon is admitted"
        );
        assert_eq!(
            configured
                .clone()
                .with_send_horizon(SEND_HORIZON + Duration::from_millis(1))
                .expect_err("one millisecond over the horizon is refused")
                .setting(),
            "send_horizon"
        );
        assert!(configured.with_send_horizon(Duration::ZERO).is_err());

        // The horizon is measured against a retention the classes publish, and
        // it is the smallest of them: a horizon derived from the 72-hour window
        // would be four hundred times past the six-hour one.
        let mut retentions = Vec::new();
        for operation in connector().operations() {
            if let Some(evidence) = operation.effect().and_then(Effect::explicit_key_evidence) {
                assert_eq!(
                    evidence.retention().clock_safety_margin(),
                    CLOCK_SAFETY_MARGIN,
                    "{}",
                    operation.id()
                );
                retentions.push(evidence.retention().minimum());
            }
        }
        let shortest = retentions
            .iter()
            .copied()
            .min()
            .expect("this connector publishes at least one keyed operation");
        assert_eq!(shortest, ORDERS_KEY_RETENTION);
        assert_eq!(shortest - CLOCK_SAFETY_MARGIN, SEND_HORIZON);
    }
}
