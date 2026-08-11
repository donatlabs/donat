//! Mercado Pago — the payment read, its refunds, and the customer record behind
//! them.
//!
//! Ground truth is Mercado Pago's own published documentation, read on
//! 2026-08-10. Every reference page is published as Markdown (each page carries
//! a "Show as Markdown" link and the portal publishes an index at
//! `/developers/en/reference/llms.txt`), and the pages this module is written
//! from are:
//!
//! * *Get payment* — "**GET** `/v1/payments/{id}`".
//! * *Create refund* — "**POST** `/v1/payments/{id}/refunds`", with its
//!   `X-Idempotency-Key` header.
//! * *Get refunds list* — "**GET** `/v1/payments/{id}/refunds`".
//! * *Create customer*, *Get customer*, *Search customers*, *Update customer* —
//!   `/v1/customers`, `/v1/customers/{id}`, and `/v1/customers/search`.
//! * *Idempotency* — "To avoid duplication, it is mandatory to send a unique key
//!   in the header `X-Idempotency-Key` that identifies the creation of a single
//!   payment… If the payment has already been created, your information is
//!   returned without creating a new payment."
//! * "Server-side: use the **Access Token** (private credential) in
//!   `Authorization: Bearer <ACCESS_TOKEN>`."
//!
//! # The near-miss: a documented key with no documented retention
//!
//! Mercado Pago is the batch's clearest **near-miss**, and it is recorded here
//! rather than stretched into a class it does not reach.
//!
//! What Mercado Pago publishes is the *binding*, and it publishes it as
//! mandatory: the *Create refund* reference lists `X-Idempotency-Key` under
//! **Header** as `(string, required)` — "This feature allows you to safely retry
//! requests without the risk of accidentally performing the same action more
//! than once. This is useful for avoiding errors, such as creating two identical
//! refunds, for example." Its idempotency guide adds the replay behaviour: "If
//! the payment has already been created, your information is returned without
//! creating a new payment."
//!
//! What it never publishes is **how long the key is remembered**, or the
//! namespace it is remembered in. Spec 026 §2 requires all three — binding,
//! uniqueness scope, retention — and this programme has already recorded two
//! near-misses rather than stretching one (Twilio's `Idempotency-Token` on a
//! different resource, OpenAI's `Idempotency-Key` in a specification for an API
//! it calls rather than the one it serves). Microsoft's `transactionId`, which
//! is published "to avoid redundant POST operations" with no window, is the
//! precedent that matches exactly: the operation is inventory-only for that
//! reason and nothing else.
//!
//! So `refund.create` is **`InventoryOnly`**, and it is not a candidate for the
//! at-most-once opt-in either. ADR 063 admits a class on evidence of an
//! *absence*, and there is no absence here: Mercado Pago publishes a mechanism,
//! this connector binds it on every send, and stepping past a real provider
//! mechanism with a Process-level opt-in is the failure ADR 042 exists to
//! prevent. Spec 026 §3 says the same thing about refunds from the other
//! direction, and this is the operation it is about. What would make it
//! executable is one sentence from Mercado Pago naming a window.
//!
//! One further published fact is worth recording beside the near-miss, because a
//! reviewer will find it: the *Get payment* and *Search payments* references
//! carry the error `400 | 2001 | Already posted the same request in the last
//! minute.` That is a duplicate *rejection* inside one minute, not a replay of
//! the first response, and it says nothing about a key. A send repeated inside
//! the minute fails; the same send a minute later succeeds and moves money
//! again. It is the opposite of an idempotency contract, and it is why the
//! window question matters here more than anywhere else in the batch.
//!
//! # The customer surface is a different question, and it has an absence
//!
//! Mercado Pago's own instruction is "Always include required headers
//! (`Authorization`, `X-Idempotency-Key` when applicable)", and it publishes
//! where it applies: "the idempotency header (`X-Idempotency-Key`) [is
//! mandatory] in requests to the **Payments and Refunds API**". The customer
//! references publish their complete request contract — parameters, no header
//! section, no client-supplied request identifier anywhere — so `customer.create`
//! is `AtMostOnce` (ADR 063) on that absence, and `customer.update` is a `PUT`
//! Mercado Pago documents as "Renew the data of a customer… send the parameters
//! with the information you want to update", which is a partial update rather
//! than a write to a fixed identity, so it stays `InventoryOnly`.
//!
//! # `payment.search` is not declared, and the reason is the response
//!
//! Mercado Pago's *Search payments* reference documents its query parameters and
//! then, under "Response parameters", states: "This endpoint has no response
//! body." A connector cannot declare an output contract from that, and inventing
//! one — even one that matches what the customer search publishes — would be a
//! second description of a provider that could disagree with the first. The
//! payment reads this module declares are the ones whose responses Mercado Pago
//! publishes.
//!
//! # Money arrives as a number *and* as a string
//!
//! *Get payment* types `transaction_amount` as `(number, optional)` — "Product's
//! cost" — and its own response example prints `"transaction_amount": "24.50"`,
//! a string. Both forms are the provider's, so every money field here is
//! declared [`ValueScalar::Json`]: it is the one scalar that carries either
//! without coercing it, and `mercado_pago_amounts_survive` pins that a number
//! stays a number, a string stays a string, and neither is rounded, truncated,
//! or re-formatted on the way into a Process.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "mercado_pago";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The origin every reference page renders its `curl` examples against.
const ORIGIN: &str = "https://api.mercadopago.com";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Mercado Pago's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Mercado Pago declarations are valid"))
            .build()
            .expect("the Mercado Pago declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Mercado Pago publishes a per-endpoint table of `(status, error, description)`
/// whose `error` column is a numeric code — `1` Params Error, `2001` "Already
/// posted the same request in the last minute", `3005` "Not valid action, the
/// resource is in a state that does not allow this operation" — and every one of
/// them arrives as a `400`. The codes are per endpoint and open, so the map is
/// keyed on the status alone and reads no body: a map keyed on a table that
/// differs per endpoint would have holes in it.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "Must provide your access_token to proceed" is documented as a
            // `400`; a rejected token is the `401`/`403` pair.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            .on_statuses([404, 405, 409], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Mercado Pago error map is a valid declaration")
    });
    &MAP
}

/// Mercado Pago declares no continuation plan here.
///
/// The one collection whose response envelope it publishes — *Search customers*
/// — documents `paging.limit`, `paging.offset`, and `paging.total` in the
/// *response* and exactly one query parameter, `email`, in the request. There is
/// no documented request binding for an offset or a limit, so no plan in the
/// closed set can advance a page, and one attempt is one request
/// ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]).
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION).success_statuses([StatusCode::OK])
}

/// The payment fields a billing process reads. Money is [`ValueScalar::Json`];
/// see the module header.
fn payment_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Json, Required::Yes)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer(
            "status_detail",
            "/status_detail",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "transaction_amount",
            "/transaction_amount",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "transaction_amount_refunded",
            "/transaction_amount_refunded",
            ValueScalar::Json,
            Required::No,
        )
        .output_pointer(
            "currency_id",
            "/currency_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "external_reference",
            "/external_reference",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
}

/// The refund fields the *Create refund* and *Get refunds list* references
/// publish.
fn refund_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Json, Required::No)
        .output_pointer("payment_id", "/payment_id", ValueScalar::Json, Required::No)
        .output_pointer("amount", "/amount", ValueScalar::Json, Required::No)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "unique_sequence_number",
            "/unique_sequence_number",
            ValueScalar::String,
            Required::No,
        )
}

/// The customer fields the *Get customer* reference publishes.
fn customer_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::String, Required::Yes)
        .output_pointer("email", "/email", ValueScalar::String, Required::No)
        .output_pointer(
            "first_name",
            "/first_name",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("last_name", "/last_name", ValueScalar::String, Required::No)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
}

/// Why the refund is declared and not executable. The whole of it is quotation
/// plus the one thing that is missing.
const REFUND_NEAR_MISS: &str = "Mercado Pago documents the binding and makes it mandatory — the Create refund reference lists \
     `X-Idempotency-Key` under Header as `(string, required)`, \"This feature allows you to safely \
     retry requests without the risk of accidentally performing the same action more than once… \
     such as creating two identical refunds\" — and documents the replay — \"If the payment has \
     already been created, your information is returned without creating a new payment\" — but \
     publishes **no retention** for the key and no namespace it is unique within. A key the \
     provider may already have forgotten is not an idempotency key, and spec 026 §2 requires all \
     three; this is a near-miss recorded rather than stretched, exactly as Microsoft's \
     `transactionId` is. The at-most-once class is not available either: it is admitted on \
     evidence of an absence, and there is no absence here";

/// The absence the customer surface really does have.
const CUSTOMER_NO_KEY: &str = "the Create customer and Update customer references publish their complete request contract — \
     `email`, `first_name`, `last_name`, `phone`, `identification`, `default_address`, `address` — \
     with no header section and no client-supplied request identifier, and Mercado Pago publishes \
     where its one key does apply: \"it is mandatory to use the idempotency header \
     (X-Idempotency-Key) in requests to the Payments and Refunds API\", which the customer API is \
     not";

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let payment_get = payment_output(
        common(Operation::get("payment.get", "/v1/payments/{payment_id}"))
            .path_param("payment_id", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    let refund_list = common(Operation::get(
        "refund.list",
        "/v1/payments/{payment_id}/refunds",
    ))
    .path_param("payment_id", ValueScalar::String)
    // "Get all Refunds for a specific payment" — the documented response is a
    // bare JSON array, so the whole document is the output and the declaration
    // says so rather than pointing into it.
    .declared_output("refunds", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // The refund. Declared, typed, tested, and not executable: see the module
    // header and `REFUND_NEAR_MISS`.
    let refund_create = refund_output(
        common(Operation::post(
            "refund.create",
            "/v1/payments/{payment_id}/refunds",
        ))
        .path_param("payment_id", ValueScalar::String)
        // "If the amount field is filled, it will create a partial refund, if
        // not, it will create a full refund."
        .body(JsonTemplate::object([(
            "amount",
            JsonTemplate::input("amount"),
        )]))
        .declared_input("amount", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(Effect::inventory_only(REFUND_NEAR_MISS)?)
    .build()?;

    let customer_get = customer_output(
        common(Operation::get(
            "customer.get",
            "/v1/customers/{customer_id}",
        ))
        .path_param("customer_id", ValueScalar::String),
    )
    .effect(Effect::read_only())
    .build()?;

    let customer_search = common(Operation::get("customer.search", "/v1/customers/search"))
        // "email (string, required) — Customer's email."
        .query_input("email", "email")
        .output_pointer("results", "/results", ValueScalar::Json, Required::Yes)
        .output_pointer("total", "/paging/total", ValueScalar::Json, Required::No)
        .output_pointer("limit", "/paging/limit", ValueScalar::Json, Required::No)
        .output_pointer("offset", "/paging/offset", ValueScalar::Json, Required::No)
        .effect(Effect::read_only())
        .build()?;

    let customer_create = customer_output(
        common(Operation::post("customer.create", "/v1/customers"))
            .body(JsonTemplate::object([
                ("email", JsonTemplate::input("email")),
                ("first_name", JsonTemplate::input("first_name")),
                ("last_name", JsonTemplate::input("last_name")),
                ("identification", JsonTemplate::input("identification")),
            ]))
            .declared_input("email", ValueScalar::Json, Required::Yes)
            .declared_input("first_name", ValueScalar::Json, Required::Yes)
            .declared_input("last_name", ValueScalar::Json, Required::Yes)
            .declared_input("identification", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::OK, StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        CUSTOMER_NO_KEY,
        "a second customer record with a new id; Mercado Pago publishes no deduplication on the \
         email address for this endpoint, and a Process that meant to reuse the first one would \
         have to find it with `customer.search`",
    )?))
    .build()?;

    let customer_update = customer_output(
        common(Operation::put(
            "customer.update",
            "/v1/customers/{customer_id}",
        ))
        .path_param("customer_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("email", JsonTemplate::input("email")),
            ("first_name", JsonTemplate::input("first_name")),
            ("last_name", JsonTemplate::input("last_name")),
        ]))
        .declared_input("email", ValueScalar::Json, Required::Yes)
        .declared_input("first_name", ValueScalar::Json, Required::Yes)
        .declared_input("last_name", ValueScalar::Json, Required::Yes),
    )
    // A `PUT`, and still not `NaturalMethod`: Mercado Pago documents it as
    // "Renew the data of a customer. Indicate the customer ID and send the
    // parameters with the information you want to update", which is a partial
    // update rather than a write of a whole resource, and it publishes no
    // statement about repeating one.
    .effect(Effect::inventory_only(
        "Mercado Pago documents PUT /v1/customers/{id} as \"send the parameters with the \
         information you want to update\", which is a partial update rather than a write to a \
         fixed resource identity, so spec 010 §7's `NaturalMethod` evidence is not there to cite; \
         its one published key is scoped to \"the Payments and Refunds API\", which this is not",
    )?)
    .build()?;

    Ok(vec![
        payment_get,
        refund_list,
        refund_create,
        customer_get,
        customer_search,
        customer_create,
        customer_update,
    ])
}
