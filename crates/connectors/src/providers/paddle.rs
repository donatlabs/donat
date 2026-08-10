//! Paddle Billing — customers, subscriptions, transactions, and the adjustment
//! that refunds one.
//!
//! Ground truth is Paddle's own published documentation, read on 2026-08-10.
//! Paddle publishes every reference page as Markdown ("Every page has a
//! Markdown sibling — append `.md` to any URL") and a documentation index at
//! <https://developer.paddle.com/llms.txt>, which is what makes the absence
//! recorded below a *machine-checkable* one:
//!
//! * <https://developer.paddle.com/api-reference/about/authentication> — "All
//!   requests to the Paddle API require authentication unless explicitly
//!   stated. The API uses Bearer authentication", with the header
//!   `Authorization: Bearer pdl_live_apikey_…`.
//! * <https://developer.paddle.com/api-reference/about/versioning> — "We update
//!   the Paddle API version when making breaking changes. Specify the version
//!   using the `Paddle-Version` header when making requests", and "The current
//!   version of the Paddle API is **version 1**."
//! * <https://developer.paddle.com/api-reference/about/data-types> — "When
//!   making requests, specify `application/json` as your `Content-Type`", and
//!   the money rule this module's `amounts` proof pins.
//! * <https://developer.paddle.com/api-reference/about/pagination> — the
//!   cursor, `meta.pagination`, and the sentence that decides this module has no
//!   pagination plan.
//! * <https://developer.paddle.com/api-reference/about/rate-limiting> — "An IP
//!   address can make up to **240 requests per minute**", the
//!   `too_many_requests` error, and "it includes a `Retry-After` response header
//!   to let you know how long to wait before retrying your request".
//! * <https://developer.paddle.com/api-reference/about/errors> — the error
//!   object, `error.type`, `error.code`, and the status families.
//! * The endpoint references for customers, subscriptions, transactions, and
//!   adjustments.
//!
//! # Money is a string in the lowest denomination
//!
//! "Monetary values are returned as strings in the lowest denomination for a
//! currency. For example, the API returns values in cents for `USD` and pence
//! for `GBP`", and Paddle publishes the table that makes the point: `USD`
//! $24.99 is `"2499"` and `JPY` ¥1000 is `"1000"`. Every amount this module
//! declares is therefore [`ValueScalar::String`], in both directions: an
//! adjustment's `amount` is rendered as the JSON string a caller supplied, and a
//! total is published as the string Paddle sent. A connector that read one of
//! these into a float would turn ¥1000 into 1000.0 and, on the way back, an
//! amount Paddle never wrote.
//!
//! # There is no pagination plan, and the reason is `next`
//!
//! Paddle's continuation is a URL: "`next` (string): URL containing the query
//! parameters of the original request, along with the `after` parameter that
//! marks the starting point of the next page. **Always returned, even if
//! `has_more` is `false`**", and exhaustion is published separately —
//! "Check `has_more` to see if there's another page. When `has_more` is
//! `false`, you've reached the last page."
//!
//! No plan in the SDK's closed set reads a second field to decide whether the
//! continuation it just read is real, so a `next_uri_in_body` walk here would
//! follow a URL Paddle publishes for a page that does not exist, on every
//! collection, until the budget failed the attempt and threw the pages away.
//! This is Sentry's shape exactly
//! ([[055-a-cursor-in-a-body-is-not-a-pagination-plan]]): the connector declares
//! **no** plan and one attempt is one page.
//!
//! It also declares no `after` binding, for the reason Sentry declares no
//! `cursor` one: the continuation Paddle publishes is a *URL*, a Process may not
//! choose a URL, and the `after` value inside it is not published as a field of
//! its own — "the Paddle ID of the last entity in the page acts as the cursor
//! for the next page". What every collection does publish is `has_more`, so a
//! Process can see that its page is partial rather than believing a truncated
//! list is the whole one.
//!
//! # Effect classification
//!
//! **Machine-checkable absence.** The string `idempot` does not occur anywhere
//! in Paddle's published documentation index (`llms.txt`), in the request
//! contract of any endpoint used here, or in the shared "about" pages that
//! enumerate every header the API reads — `Authorization`, `Content-Type`,
//! `Paddle-Version`, and `Skip-Count`. Paddle publishes no idempotency key, no
//! client-supplied request identifier, and no deduplication behaviour;
//! `meta.request_id` is a *Paddle*-generated identifier returned in every
//! response for support to look a request up by, not one a caller may send.
//!
//! `customer.create` and `transaction.create` are therefore `AtMostOnce`
//! (ADR 063). `customer.update` is a `PATCH` whose repeat sets the same fields
//! to the same values, which is not the consequence that class exists to bound,
//! so it stays `InventoryOnly` — the same line this programme drew for HubSpot's
//! and SendGrid's updates.
//!
//! **The refund is deliberately not executable.** `adjustment.create` with
//! `action: refund` is the operation that moves money back to a customer, and
//! Paddle publishes no idempotency mechanism for it. Spec 026 §3 is explicit
//! that at-most-once is the wrong trade here, and this module agrees for a
//! reason of Paddle's own: "Most refunds for live accounts are created with the
//! status of `pending_approval` until reviewed by Paddle", so a second send that
//! Donat cannot rule out becomes a *second pending refund* on the same
//! transaction, approved by a human who has no way to know the first one exists.
//! An operator may reasonably accept "this email might never be sent"; nobody
//! should casually accept "this customer might be refunded twice, or not at
//! all". It is declared, typed, tested, and `InventoryOnly`, and the way to make
//! it executable is a Paddle idempotency key, not a Process opt-in.

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
pub const NAME: &str = "paddle";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// Paddle's one published API origin.
const ORIGIN: &str = "https://api.paddle.com";

/// "Specify the version using the `Paddle-Version` header when making
/// requests", and "The current version of the Paddle API is **version 1**."
const API_VERSION_HEADER: &str = "Paddle-Version";
const API_VERSION: &str = "1";

/// "Most list endpoints return 50 results by default and a maximum of 200."
const PAGE_SIZE: &str = "50";

/// "[Listing transactions] returns 30 (the default and maximum)."
const TRANSACTION_PAGE_SIZE: &str = "30";

/// "[Listing adjustments] returns 10 by default and a maximum of 50."
const ADJUSTMENT_PAGE_SIZE: &str = "50";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        Connector::declare(NAME, VERSION)
            .origin(OriginSpec::fixed(ORIGIN).expect("Paddle's published origin is valid"))
            .credential(CredentialSpec::for_plan(AuthPlan::bearer()))
            .operations(operations().expect("the Paddle declarations are valid"))
            .build()
            .expect("the Paddle declaration is valid")
    });
    &CONNECTOR
}

/// The ordered error map.
///
/// Paddle publishes a stable machine-readable `error.code`, and this map
/// deliberately reads none of them: the list is per entity and open — Paddle
/// documents a page per code — so a map keyed on it would have holes, while the
/// status families it publishes are complete. The one code worth naming here is
/// `concurrent_modification`, a `409` Paddle suggests retrying; the closed class
/// set has no "conflict, try again" member, and answering it `http_5xx` would
/// tell a Process the provider failed when it did not. It is `permanent` with
/// its status attached, and a Process that wants to try again does so as a
/// Process.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "invalid_field — Request does not pass validation", and a
            // malformed body or an unparseable cursor arrive the same way.
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "invalid_token — API key is invalid" and "forbidden — permission
            // denied for this API key".
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "not_found — Entity … not found" needs a different request.
            .on_statuses([404, 405, 409, 415], ConnectorErrorClass::Permanent)
            .on_status(429, ConnectorErrorClass::Http429)
            // "Though rare, you may get a `5xx` response code. This means
            // there's a problem with the Paddle API rather than your request.
            // Retry with exponential backoff."
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Paddle error map is a valid declaration")
    });
    &MAP
}

/// Paddle declares no continuation plan: see the module header.
///
/// It is a function rather than a constant so that the registry wires the same
/// lookup shape for every module ([[058-a-declared-walk-is-the-executors-walk]]),
/// and so that a future Paddle plan has one place to appear.
pub const fn pagination(_operation_id: &str) -> Option<&'static Pagination> {
    None
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder
        .version(VERSION)
        .static_header(API_VERSION_HEADER, API_VERSION)
}

/// What every list response publishes.
///
/// `meta.pagination.next` is deliberately absent: it is a URL, this connector
/// will not follow one, and a Process cannot send one either — publishing it
/// would publish a destination nothing can reach. `has_more` is the field that
/// carries information a Process can act on: "Check `has_more` to see if
/// there's another page."
fn collection_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("data", "/data", ValueScalar::Json, Required::Yes)
        .output_pointer(
            "has_more",
            "/meta/pagination/has_more",
            ValueScalar::Boolean,
            Required::Yes,
        )
        .output_pointer(
            "estimated_total",
            "/meta/pagination/estimated_total",
            ValueScalar::Int64,
            Required::No,
        )
}

/// One list endpoint: the declared page size, and nothing a caller can widen.
fn collection(id: &'static str, path: &str, page_size: &str) -> OperationBuilder {
    collection_output(common(Operation::get(id, path)).query_static("per_page", page_size))
}

/// The customer fields Paddle documents as required on every customer entity.
fn customer_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/data/id", ValueScalar::String, Required::Yes)
        .output_pointer("email", "/data/email", ValueScalar::String, Required::Yes)
        .output_pointer("name", "/data/name", ValueScalar::String, Required::No)
        .output_pointer("status", "/data/status", ValueScalar::String, Required::Yes)
        .output_pointer(
            "created_at",
            "/data/created_at",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "updated_at",
            "/data/updated_at",
            ValueScalar::String,
            Required::Yes,
        )
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur anywhere in Paddle's published documentation index \
     (developer.paddle.com/llms.txt), in this endpoint's own reference page, or in the shared \
     \"about\" pages that enumerate the headers the API reads — `Authorization`, `Content-Type`, \
     `Paddle-Version`, and `Skip-Count`; the only request identifier Paddle publishes is the \
     `meta.request_id` it generates itself and returns in every response";

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    // Every declared query binding is one a billing process always has a value
    // for: the SDK's query slots are mandatory, so a filter a caller might not
    // have is a filter this declaration does not offer.
    let customer_list = collection("customer.list", "/customers", PAGE_SIZE)
        // "Return entities that match the specified status", whose documented
        // values are `active` and `archived`.
        .query_input("status", "status")
        .success_statuses([StatusCode::OK])
        .effect(Effect::read_only())
        .build()?;

    let customer_get = customer_output(
        common(Operation::get("customer.get", "/customers/{customer_id}"))
            .path_param("customer_id", ValueScalar::String)
            .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let customer_create = customer_output(
        common(Operation::post("customer.create", "/customers"))
            .body(JsonTemplate::object([
                ("email", JsonTemplate::input("email")),
                ("name", JsonTemplate::input("name")),
                ("locale", JsonTemplate::input("locale")),
                ("custom_data", JsonTemplate::input("custom_data")),
            ]))
            // "email: string (email) (required)"; the rest are nullable.
            .declared_input("email", ValueScalar::String, Required::Yes)
            .declared_input("name", ValueScalar::Json, Required::Yes)
            .declared_input("locale", ValueScalar::Json, Required::Yes)
            .declared_input("custom_data", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        "a second customer with a new `ctm_` id; Paddle does not deduplicate on the email address \
         — a create with an address that already exists answers `409 customer_already_exists` \
         rather than returning the existing customer, so the two outcomes are not the same either \
         way",
    )?))
    .build()?;

    let customer_update = customer_output(
        common(Operation::patch(
            "customer.update",
            "/customers/{customer_id}",
        ))
        .path_param("customer_id", ValueScalar::String)
        .body(JsonTemplate::object([
            ("name", JsonTemplate::input("name")),
            ("email", JsonTemplate::input("email")),
            ("status", JsonTemplate::input("status")),
        ]))
        .declared_input("name", ValueScalar::Json, Required::Yes)
        .declared_input("email", ValueScalar::Json, Required::Yes)
        .declared_input("status", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(NO_KEY)?)
    .build()?;

    let subscription_list = collection("subscription.list", "/subscriptions", PAGE_SIZE)
        // "Return entities related to the specified customer."
        .query_input("customer_id", "customer_id")
        .success_statuses([StatusCode::OK])
        .effect(Effect::read_only())
        .build()?;

    let subscription_get = common(Operation::get(
        "subscription.get",
        "/subscriptions/{subscription_id}",
    ))
    .path_param("subscription_id", ValueScalar::String)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/data/id", ValueScalar::String, Required::Yes)
    .output_pointer("status", "/data/status", ValueScalar::String, Required::Yes)
    .output_pointer(
        "customer_id",
        "/data/customer_id",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "currency_code",
        "/data/currency_code",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "next_billed_at",
        "/data/next_billed_at",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "updated_at",
        "/data/updated_at",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let transaction_list = collection("transaction.list", "/transactions", TRANSACTION_PAGE_SIZE)
        .query_input("customer_id", "customer_id")
        .success_statuses([StatusCode::OK])
        .effect(Effect::read_only())
        .build()?;

    let transaction_get = transaction_output(
        common(Operation::get(
            "transaction.get",
            "/transactions/{transaction_id}",
        ))
        .path_param("transaction_id", ValueScalar::String)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let transaction_create = transaction_output(
        common(Operation::post("transaction.create", "/transactions"))
            .body(JsonTemplate::object([
                ("items", JsonTemplate::input("items")),
                ("customer_id", JsonTemplate::input("customer_id")),
                ("currency_code", JsonTemplate::input("currency_code")),
                ("collection_mode", JsonTemplate::input("collection_mode")),
                ("custom_data", JsonTemplate::input("custom_data")),
            ]))
            .declared_input("items", ValueScalar::Json, Required::Yes)
            .declared_input("customer_id", ValueScalar::Json, Required::Yes)
            .declared_input("currency_code", ValueScalar::Json, Required::Yes)
            .declared_input("collection_mode", ValueScalar::Json, Required::Yes)
            .declared_input("custom_data", ValueScalar::Json, Required::Yes)
            .success_statuses([StatusCode::CREATED]),
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::MachineReadableDescription,
        NO_KEY,
        "a second transaction with a new `txn_` id for the same items — and, for an \
         automatically-collected transaction that Paddle completes, a second charge against the \
         customer's payment method",
    )?))
    .build()?;

    let adjustment_list = collection("adjustment.list", "/adjustments", ADJUSTMENT_PAGE_SIZE)
        .query_input("transaction_id", "transaction_id")
        .success_statuses([StatusCode::OK])
        .effect(Effect::read_only())
        .build()?;

    // The refund. See the module header for why this one is not executable and
    // is not a candidate for the at-most-once opt-in either.
    let adjustment_create = common(Operation::post("adjustment.create", "/adjustments"))
        .body(JsonTemplate::object([
            ("action", JsonTemplate::input("action")),
            ("type", JsonTemplate::input("type")),
            ("transaction_id", JsonTemplate::input("transaction_id")),
            ("reason", JsonTemplate::input("reason")),
            ("items", JsonTemplate::input("items")),
        ]))
        .declared_input("action", ValueScalar::String, Required::Yes)
        .declared_input("type", ValueScalar::Json, Required::Yes)
        .declared_input("transaction_id", ValueScalar::String, Required::Yes)
        .declared_input("reason", ValueScalar::String, Required::Yes)
        .declared_input("items", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::CREATED])
        .output_pointer("id", "/data/id", ValueScalar::String, Required::Yes)
        .output_pointer("action", "/data/action", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/data/status", ValueScalar::String, Required::Yes)
        .output_pointer(
            "transaction_id",
            "/data/transaction_id",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "currency_code",
            "/data/currency_code",
            ValueScalar::String,
            Required::Yes,
        )
        // "Monetary values are returned as strings in the lowest denomination
        // for a currency."
        .output_pointer(
            "total",
            "/data/totals/total",
            ValueScalar::String,
            Required::No,
        )
        .effect(Effect::inventory_only(
            "Paddle publishes no idempotency key for `POST /adjustments`, and a refund is not an \
             operation an at-most-once opt-in may cover: \"Most refunds for live accounts are \
             created with the status of `pending_approval` until reviewed by Paddle\", so a second \
             send leaves a second pending refund against the same transaction for a human to \
             approve, and refusing the second send instead leaves a customer's refund in an \
             outcome nobody can read",
        )?)
        .build()?;

    Ok(vec![
        customer_list,
        customer_get,
        customer_create,
        customer_update,
        subscription_list,
        subscription_get,
        transaction_list,
        transaction_get,
        transaction_create,
        adjustment_list,
        adjustment_create,
    ])
}

/// The transaction fields Paddle documents as required, plus the two totals a
/// billing process reads.
///
/// Every one of them is a string, because that is what Paddle publishes: "The
/// API returns values in cents for `USD` and pence for `GBP`."
fn transaction_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/data/id", ValueScalar::String, Required::Yes)
        .output_pointer("status", "/data/status", ValueScalar::String, Required::Yes)
        .output_pointer(
            "customer_id",
            "/data/customer_id",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "currency_code",
            "/data/currency_code",
            ValueScalar::String,
            Required::Yes,
        )
        .output_pointer(
            "invoice_number",
            "/data/invoice_number",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "grand_total",
            "/data/details/totals/grand_total",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "created_at",
            "/data/created_at",
            ValueScalar::String,
            Required::Yes,
        )
}
