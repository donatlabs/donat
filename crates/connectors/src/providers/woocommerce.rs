//! WooCommerce's REST API v3, served from one merchant's own WordPress site.
//!
//! Ground truth is WooCommerce's own published reference, read on 2026-08-10.
//! The whole v3 API is published as one document,
//! <https://woocommerce.github.io/woocommerce-rest-api-docs/>, with fragment
//! anchors; every quotation below is from it and the anchor is named where it
//! matters.
//!
//! * `#introduction` — "The current WP REST API integration version is `v3`
//!   which takes a first-order position in endpoints", and the requirement
//!   "Pretty permalinks in `Settings > Permalinks` so that the custom endpoints
//!   are supported. __Default permalinks will not work.__"
//! * `#authentication` — "You may use HTTP Basic Auth by providing the REST API
//!   Consumer Key as the username and the REST API Consumer Secret as the
//!   password", and "You may access the API over either HTTP or HTTPS, but
//!   *HTTPS is recommended where possible*", beside the HTTP alternative: "You
//!   must use OAuth 1.0a "one-legged" authentication to ensure REST API
//!   credentials cannot be intercepted by an attacker."
//! * `#requestresponse-format` — "Successful requests will return a `200 OK`
//!   HTTP status", "Resource IDs are returned as integers", "Any decimal
//!   monetary amount, such as prices or totals, will be returned as strings with
//!   two decimal places", and "Dates are returned in ISO8601 format".
//! * `#pagination` — the `page`, `per_page`, and `offset` parameters, the
//!   `X-WP-Total` and `X-WP-TotalPages` headers, and "Pagination info is
//!   included in the Link Header ... It's recommended that you follow these
//!   values instead of building your own URLs where possible."
//! * `#errors` — the four-row status table and the `code`/`message`/`data.status`
//!   body this module's error map is built from.
//!
//! # The origin is the whole store URL, and it is deploy-time configuration
//!
//! Every other templated connector in this workspace fills one host label under
//! a constant vendor suffix. WooCommerce has no vendor suffix at all: the store
//! *is* the provider, its host is whatever domain the merchant owns, and the
//! only constant is the `/wp-json/wc/v3` path prefix. `OriginSpec::TemplatedHost`
//! cannot describe that, so this connector declares `OriginSpec::DeploymentOrigin`
//! — the same variant the deploy-time declarative connector uses, for the same
//! reason: the deployment names the provider rather than this workspace. It is
//! not an escape from fixed origins. The value is read once from
//! `config.settings.store_origin`, validated by [`validate_store_origin`] before
//! a listener opens, and becomes the same immutable origin every other connector
//! renders against; nothing in an input, a response, or a continuation can move
//! it afterwards.
//!
//! Two things are refused there rather than accepted quietly.
//!
//! **A non-`https` store.** WooCommerce publishes Basic authentication only for
//! HTTPS and publishes OAuth 1.0a signing as the *alternative* for plain HTTP,
//! and the SDK has no OAuth 1.0a plan. Sending this connector's declared Basic
//! credential to an `http://` store would put the consumer secret on the wire in
//! clear, so the configuration is refused instead.
//!
//! **A store in a subdirectory.** `Origin` is a scheme, a host, and a port, and
//! WooCommerce publishes no statement about subdirectory installations at all —
//! the word does not occur in its reference. A deployment whose WordPress lives
//! under a path is refused with its configuration key named rather than served a
//! URL this module composed by guessing.
//!
//! # A collection is a bare array
//!
//! Every list endpoint answers a JSON array at the document root, so those
//! operations publish the whole document and their plan collects the root.
//!
//! # Page size
//!
//! WooCommerce publishes the default — "Requests that return multiple items will
//! be paginated to 10 items by default" — and publishes no maximum. The
//! WordPress REST API handbook, which WooCommerce links to as "the official
//! WordPress REST API documentation", publishes the ceiling this declaration
//! stays inside: "`?per_page=`: specify the number of records to return in one
//! request, specified as an integer from 1 to 100."
//!
//! # Effect classification
//!
//! **Complete published contract, no key in it.** The string `idempot` does not
//! occur in the 4.4 MB v3 reference, in the whole published documentation
//! repository covering every API version, or in the developer-portal REST API
//! page. The nearest published mechanisms are not idempotency keys and are
//! recorded as near-misses: `cart_hash` is "MD5 hash of cart items to ensure
//! orders are not modified. <read-only>" — server-computed, not client-supplied
//! — and the OAuth `oauth_nonce` protects a *signature* against replay on the
//! HTTP path, not an application-level create.
//!
//! The three creates are therefore `AtMostOnce` (ADR 063). The two updates stay
//! `InventoryOnly`: WooCommerce publishes nothing about `PUT` beyond "This API
//! lets you make changes to an order", which is neither spec 010 §7's
//! `NaturalMethod` evidence nor a consequence ADR 063 can bound.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde_json::Value as JsonValue;

use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, FieldClassification, OriginSpec};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure, ErrorMap};
use crate::sdk::operation::{
    JsonTemplate, Operation, OperationBuilder, OperationError, Origin, Required,
};
use crate::sdk::pagination::Pagination;

/// The connector name a deployment selects.
pub const NAME: &str = "woocommerce";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key carrying the store's whole origin.
pub const STORE_ORIGIN: &str = "store_origin";

/// The deploy-time configuration key carrying the REST API consumer key.
///
/// It is the HTTP Basic *username*, and WooCommerce pairs it with the consumer
/// secret, which is the credential this connector resolves from a `SecretRef`.
pub const CONSUMER_KEY: &str = "consumer_key";

/// The route prefix every endpoint here is served from.
const PREFIX: &str = "/wp-json/wc/v3";

/// Inside WordPress's own published ceiling of 100; WooCommerce publishes only
/// the default of 10.
const PAGE_SIZE: &str = "100";

/// One deployment's declaration.
///
/// `consumer_key` is the Basic username, which `AuthPlan::basic` takes where the
/// plan is built — so this declaration is completed by one deployment, exactly
/// as Twilio's and Jira's are.
pub fn connector(consumer_key: &str) -> Result<Connector, OperationError> {
    validate_consumer_key(consumer_key)?;
    Connector::declare(NAME, VERSION)
        .origin(OriginSpec::deployment_origin(STORE_ORIGIN)?)
        .credential(
            CredentialSpec::for_plan(AuthPlan::basic(consumer_key)?)
                .with_field(CONSUMER_KEY, FieldClassification::NonSecret),
        )
        .operations(operations()?)
        .build()
}

/// The Basic username's grammar.
///
/// WooCommerce publishes no format for a consumer key beyond the placeholder
/// `consumer_key`, so what is checked here is what would make the *request*
/// wrong rather than a shape the provider published: a value that is empty, that
/// carries a colon (which would forge the Basic separator), or that is not
/// printable ASCII. `AuthPlan::basic` refuses all three; this restates the
/// refusal at the configuration key so a deployment is told which setting is
/// wrong.
fn validate_consumer_key(consumer_key: &str) -> Result<(), OperationError> {
    if consumer_key.trim().is_empty() || consumer_key.chars().any(char::is_whitespace) {
        return Err(OperationError::new(
            "the WooCommerce consumer key must be a non-empty value without whitespace",
        ));
    }
    AuthPlan::basic(consumer_key).map(|_| ())
}

/// The declaration a reviewer and the registry read, with a placeholder consumer
/// key no deployment uses.
pub fn declaration_shape() -> Result<Connector, OperationError> {
    connector("ck_deployment_configured")
}

/// Whether a configured store origin is one this connector may send its declared
/// credential to.
///
/// See the module documentation: `https` is required because WooCommerce
/// publishes Basic authentication for HTTPS only, and a path is refused because
/// an origin is a scheme, a host, and a port.
pub fn validate_store_origin(value: &str) -> Result<(), OperationError> {
    let origin = Origin::parse(value)?;
    if origin.as_url().scheme() != "https" {
        return Err(OperationError::new(
            "a WooCommerce store origin must be https: WooCommerce publishes Basic authentication \
             over HTTPS only, and publishes OAuth 1.0a signing as the alternative for plain HTTP",
        ));
    }
    Ok(())
}

/// This connector's error map.
///
/// WooCommerce publishes a four-row status table — `400`, `401`, `404`, `500` —
/// and a `code` string beside `message` and `data.status`. It publishes no
/// enumeration of `code` values: four appear in the whole reference, two of them
/// on one endpoint. The map is therefore keyed on statuses, and the statuses
/// beyond WooCommerce's four rows are marked below as this declaration's own
/// reading rather than as its statements — a store is an ordinary web server
/// behind an ordinary host, and answering a `502` or a `429` `permanent` would
/// end a Process for a condition that clears by itself.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "400 Bad Request — Invalid request, e.g. using an unsupported HTTP
            // method."
            .on_status(400, ConnectorErrorClass::Validation)
            // "401 Unauthorized — Authentication or permission error, e.g.
            // incorrect API keys." WooCommerce publishes no `403` at all; one
            // arriving from WordPress itself means the same thing.
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "404 Not Found — Requests to resources that don't exist or are
            // missing." `409` and `410` are WordPress's, not WooCommerce's.
            .on_statuses([404, 405, 409, 410, 415], ConnectorErrorClass::Permanent)
            // Not published by WooCommerce for `wc/v3`: its own rate limiting is
            // published for the Store API, a different surface, where the hint
            // is `RateLimit-Retry-After` rather than `Retry-After`. A store is
            // still served by a host that may rate-limit, and treating that as
            // permanent would be a worse answer than retrying.
            .on_status(429, ConnectorErrorClass::Http429)
            // "500 Internal Server Error — Server error"; the gateway statuses
            // beside it are the host's rather than WooCommerce's.
            .on_statuses([500, 502, 503, 504], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the WooCommerce error map is a valid declaration")
    });
    &MAP
}

/// Decode one WooCommerce response: the declared success statuses, then the declared
/// contract.
///
/// It is the declaration-driven answer written out per module rather than
/// inherited, so that the serving runtime asks every connector in this batch the
/// same question and each one answers with its own error map.
pub fn decode(
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

/// The continuation plan of each collection.
///
/// "Pagination info is included in the Link Header", with `rel="next"`, and a
/// response that has no further page carries no `next` link — an absence, which
/// is what the plan ends on. Every walked collection is a bare JSON array at the
/// document root, so the items pointer is the empty pointer.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static COLLECTION: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("", "next").expect("the WooCommerce link plan is valid")
    });
    match operation_id {
        "order.list" | "customer.list" | "product.list" | "order_note.list" => Some(&COLLECTION),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// The reason every write in this module carries.
const NO_KEY: &str = "the string `idempot` does not occur in WooCommerce's published v3 REST API \
                      reference — one 4.4 MB document covering authentication, pagination, errors, \
                      and every order, customer, product, and order-note endpoint — nor anywhere \
                      in its published documentation repository for any API version, nor on its \
                      developer-portal REST API page: no request header, no body property, and no \
                      response field carries a client-supplied request identifier or a \
                      deduplication behaviour. The two nearest published values are read-only: \
                      `cart_hash` is \"MD5 hash of cart items to ensure orders are not modified. \
                      <read-only>\", and the OAuth `oauth_nonce` protects a signature against \
                      replay rather than a create";

/// One write whose repeat would leave a second thing behind (ADR 063).
fn at_most_once(repeat_produces: &str) -> Result<Effect, OperationError> {
    Ok(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        NO_KEY,
        repeat_produces,
    )?))
}

/// The reason both updates carry.
const PARTIAL_UPDATE: &str = "WooCommerce publishes nothing about what a `PUT` does beyond \"This \
                              API lets you make changes to an order\" and \"This API lets you make \
                              changes to a customer\": no statement that the request replaces the \
                              resource, which is spec 010 §7's NaturalMethod evidence, and no \
                              statement of what a second identical send produces, which is what \
                              ADR 063 admits a class on. Its own update examples send only the \
                              fields being changed";

/// The published order properties a process reads.
///
/// "Resource IDs are returned as integers" and "Any decimal monetary amount,
/// such as prices or totals, will be returned as strings with two decimal
/// places", which is why `total` is a string and `id` is not.
fn order_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("number", "/number", ValueScalar::String, Required::No)
        .output_pointer("status", "/status", ValueScalar::String, Required::No)
        .output_pointer("currency", "/currency", ValueScalar::String, Required::No)
        .output_pointer("total", "/total", ValueScalar::String, Required::No)
        .output_pointer(
            "customer_id",
            "/customer_id",
            ValueScalar::Int64,
            Required::No,
        )
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer(
            "date_modified",
            "/date_modified",
            ValueScalar::String,
            Required::No,
        )
}

/// The published customer properties a process reads.
fn customer_output(builder: OperationBuilder) -> OperationBuilder {
    builder
        .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
        .output_pointer("email", "/email", ValueScalar::String, Required::No)
        .output_pointer(
            "first_name",
            "/first_name",
            ValueScalar::String,
            Required::No,
        )
        .output_pointer("last_name", "/last_name", ValueScalar::String, Required::No)
        .output_pointer("role", "/role", ValueScalar::String, Required::No)
        .output_pointer(
            "date_created",
            "/date_created",
            ValueScalar::String,
            Required::No,
        )
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let order_get = order_output(
        common(Operation::get(
            "order.get",
            &format!("{PREFIX}/orders/{{order_id}}"),
        ))
        .path_param("order_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let order_list = common(Operation::get("order.list", &format!("{PREFIX}/orders")))
        // "Limit result set to orders assigned a specific status. Options:
        // `any`, `pending`, `processing`, ...", so a caller that wants every
        // order has a published value to send rather than an omission.
        .query_input("status", "status")
        .query_static("per_page", PAGE_SIZE)
        .success_statuses([StatusCode::OK])
        // The collection is a bare JSON array, so the whole document is the
        // output.
        .declared_output("items", ValueScalar::Json, Required::Yes)
        .effect(Effect::read_only())
        .build()?;

    let order_create = order_output(
        common(Operation::post("order.create", &format!("{PREFIX}/orders")))
            .body(JsonTemplate::object([
                ("payment_method", JsonTemplate::input("payment_method")),
                ("customer_id", JsonTemplate::input("customer_id")),
                ("billing", JsonTemplate::input("billing")),
                ("shipping", JsonTemplate::input("shipping")),
                ("line_items", JsonTemplate::input("line_items")),
                ("set_paid", JsonTemplate::input("set_paid")),
            ]))
            .declared_input("billing", ValueScalar::Json, Required::Yes)
            .declared_input("shipping", ValueScalar::Json, Required::Yes)
            .declared_input("line_items", ValueScalar::Json, Required::Yes)
            .declared_input("set_paid", ValueScalar::Boolean, Required::Yes)
            .success_statuses([StatusCode::OK]),
    )
    .effect(at_most_once(
        "a second order with a new `id` and a new `number` — and, where the create carried \
         `set_paid`, a second order the store treats as paid",
    )?)
    .build()?;

    let order_update = order_output(
        common(Operation::put(
            "order.update",
            &format!("{PREFIX}/orders/{{order_id}}"),
        ))
        .path_param("order_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("status", JsonTemplate::input("status")),
            ("customer_note", JsonTemplate::input("customer_note")),
        ]))
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    let customer_get = customer_output(
        common(Operation::get(
            "customer.get",
            &format!("{PREFIX}/customers/{{customer_id}}"),
        ))
        .path_param("customer_id", ValueScalar::Int64)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::read_only())
    .build()?;

    let customer_list = common(Operation::get(
        "customer.list",
        &format!("{PREFIX}/customers"),
    ))
    // "Limit result set to resources with a specific role. Options: `all`,
    // `administrator`, ..., `customer`. Default is `customer`."
    .query_input("role", "role")
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "`email` — The email address for the customer. <mandatory>"
    let customer_create = customer_output(
        common(Operation::post(
            "customer.create",
            &format!("{PREFIX}/customers"),
        ))
        .body(JsonTemplate::object([
            ("email", JsonTemplate::input("email")),
            ("first_name", JsonTemplate::input("first_name")),
            ("last_name", JsonTemplate::input("last_name")),
            ("username", JsonTemplate::input("username")),
            ("billing", JsonTemplate::input("billing")),
        ]))
        .declared_input("email", ValueScalar::String, Required::Yes)
        .declared_input("billing", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(at_most_once(
        "a second customer with a new id, unless WordPress refuses the duplicate registration — \
         WooCommerce publishes no `code` for that case, so which of the two happens is not \
         something this connector can promise either way",
    )?)
    .build()?;

    let customer_update = customer_output(
        common(Operation::put(
            "customer.update",
            &format!("{PREFIX}/customers/{{customer_id}}"),
        ))
        .path_param("customer_id", ValueScalar::Int64)
        .body(JsonTemplate::object([
            ("first_name", JsonTemplate::input("first_name")),
            ("last_name", JsonTemplate::input("last_name")),
            ("billing", JsonTemplate::input("billing")),
        ]))
        .declared_input("billing", ValueScalar::Json, Required::Yes)
        .success_statuses([StatusCode::OK]),
    )
    .effect(Effect::inventory_only(PARTIAL_UPDATE)?)
    .build()?;

    let product_get = common(Operation::get(
        "product.get",
        &format!("{PREFIX}/products/{{product_id}}"),
    ))
    .path_param("product_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/name", ValueScalar::String, Required::No)
    .output_pointer("sku", "/sku", ValueScalar::String, Required::No)
    .output_pointer("status", "/status", ValueScalar::String, Required::No)
    .output_pointer("price", "/price", ValueScalar::String, Required::No)
    .effect(Effect::read_only())
    .build()?;

    let product_list = common(Operation::get(
        "product.list",
        &format!("{PREFIX}/products"),
    ))
    // "Limit result set to products assigned a specific status. Options:
    // `any`, `draft`, `pending`, `private` and `publish`. Default is `any`."
    .query_input("status", "status")
    .query_static("per_page", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let order_note_list = common(Operation::get(
        "order_note.list",
        &format!("{PREFIX}/orders/{{order_id}}/notes"),
    ))
    .path_param("order_id", ValueScalar::Int64)
    // "Limit result to customers or internal notes. Options: `any`, `customer`
    // and `internal`. Default is `any`."
    .query_input("type", "type")
    .success_statuses([StatusCode::OK])
    .declared_output("items", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    // "`note` — Order note content. <mandatory>", "`customer_note` — If true,
    // the note will be shown to customers and they will be notified."
    let order_note_create = common(Operation::post(
        "order_note.create",
        &format!("{PREFIX}/orders/{{order_id}}/notes"),
    ))
    .path_param("order_id", ValueScalar::Int64)
    .body(JsonTemplate::object([
        ("note", JsonTemplate::input("note")),
        ("customer_note", JsonTemplate::input("customer_note")),
    ]))
    .declared_input("note", ValueScalar::String, Required::Yes)
    .declared_input("customer_note", ValueScalar::Boolean, Required::Yes)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("author", "/author", ValueScalar::String, Required::No)
    .output_pointer("note", "/note", ValueScalar::String, Required::No)
    .output_pointer(
        "date_created",
        "/date_created",
        ValueScalar::String,
        Required::No,
    )
    .effect(at_most_once(
        "a second note on the same order — and, where `customer_note` is true, a second \
         notification to the customer",
    )?)
    .build()?;

    Ok(vec![
        order_get,
        order_list,
        order_create,
        order_update,
        customer_get,
        customer_list,
        customer_create,
        customer_update,
        product_get,
        product_list,
        order_note_list,
        order_note_create,
    ])
}
