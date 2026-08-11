//! Shopify's Admin REST API and its signed webhook deliveries.
//!
//! Ground truth is Shopify's own published documentation, read on 2026-08-10:
//!
//! * <https://shopify.dev/docs/api/admin-rest> — "All Admin REST API endpoints
//!   follow this pattern: `https://{store_name}.myshopify.com/admin/api/2026-07/{resource}.json`",
//!   "All REST Admin API queries require a valid Shopify access token", and
//!   "Include your token as a `X-Shopify-Access-Token` header on all API
//!   queries."
//! * <https://shopify.dev/docs/apps/build/authentication-authorization/access-tokens/authorization-code-grant>
//!   — the shop identifier's published grammar: "To match for the hostname form
//!   `{shop}.myshopify.com`, you can use the following regular expression:
//!   `/^[a-zA-Z0-9][a-zA-Z0-9\-]*\.myshopify\.com/`".
//! * <https://shopify.dev/docs/api/admin-rest/usage/versioning> — "Shopify API
//!   versions are explicitly declared in the URLs that your app makes requests
//!   to the REST Admin API (`/admin/api/{api_version}/{endpoint}.json`)", and
//!   "Each stable version is supported for a minimum of 12 months."
//! * <https://shopify.dev/docs/api/admin-rest/usage/rate-limits> — the leaky
//!   bucket, "If the bucket size is exceeded, then an HTTP `429 Too Many
//!   Requests` error is returned", and the `Retry-After` header.
//! * <https://shopify.dev/docs/api/usage/response-codes> — the status table
//!   this module's error map is built from.
//! * <https://shopify.dev/docs/api/admin-rest/usage/pagination> — "REST
//!   endpoints support cursor-based pagination", the `link` header with
//!   `rel={next}`, and "The maximum `limit` value is `250`."
//! * <https://shopify.dev/docs/api/admin-rest/usage/idempotent-requests>,
//!   <https://shopify.dev/docs/api/admin-rest/latest/resources/order>, and
//!   <https://shopify.dev/docs/api/admin-rest/latest/resources/product>.
//! * <https://shopify.dev/docs/apps/build/webhooks/verify-deliveries> and
//!   <https://shopify.dev/docs/apps/build/webhooks/delivery-structure> for the
//!   inbound half.
//!
//! # The origin is the shop
//!
//! This is the batch's one `OriginSpec::TemplatedHost`: the host *is* the
//! merchant's store domain, so it cannot be a compile-time constant, and it is
//! filled from deploy-time connector configuration and from nowhere else. The
//! SDK admits a single lowercase host label there, which is a strict subset of
//! Shopify's own published `[a-zA-Z0-9][a-zA-Z0-9\-]*`.
//!
//! # Legacy status
//!
//! Shopify prints on every REST page: "The REST Admin API is a legacy API as of
//! October 1, 2024. Starting April 1, 2025, all new public apps must be built
//! exclusively with the GraphQL Admin API." The Product resource carries a
//! second, sharper notice — "Listing, creating, updating, and deleting products
//! is deprecated as of REST API 2024-04" — and every endpoint here is still
//! served at `2026-07`, which is the version this declaration pins. That is
//! recorded rather than hidden: a deployment choosing this connector is
//! choosing a legacy surface with a published end date.
//!
//! # Effect classification
//!
//! Shopify *does* publish an idempotency mechanism for the REST Admin API, and
//! it publishes exactly which requests it covers: "POST requests that process
//! credit card payments, create billing attempts for subscriptions, or capture
//! revenue details accept idempotency keys." Order creation and product
//! mutation are none of those three, and neither resource's reference page
//! mentions idempotency at all. `order.create` is therefore `AtMostOnce`
//! (ADR 063) on a documented *exclusion* rather than on an absence — the
//! strongest form of this evidence in the batch — and a repeat leaves a second
//! order with a new `order_number`.
//!
//! `product.update` is a `PUT`, and it is still `InventoryOnly`: Shopify
//! publishes no statement that the endpoint replaces the resource, and its own
//! examples send only the fields being changed ("Update a product's SEO title
//! and description") while an array member is replaced by presence ("Update a
//! product by clearing product images" sends `images: []`). A `PUT` whose body
//! is partial is not a write to a fixed resource identity, so spec 010 §7's
//! `NaturalMethod` evidence is not there to cite — and ADR 063 does not reach it
//! either, because Shopify publishes nothing about what a second identical `PUT`
//! produces.
//!
//! `product.delete` is `ProviderIdempotent::NaturalMethod`: it is a `DELETE`
//! against one fixed product id, Shopify documents it as "Deletes a product."
//! and documents its response as `200 OK` with the body `{}`, so two identical
//! sends leave the same one absent product. What Shopify does *not* publish is
//! the status of the second send, so this module does not admit `404` as a
//! success — a repeat that answers `404` is classified `Permanent` rather than
//! quietly reported as a delete that happened.

use std::sync::LazyLock;

use donat_value_contract::ValueScalar;
use reqwest::StatusCode;

use crate::providers::inbound::{EventIdentifier, TriggerEvent};
use crate::sdk::auth::AuthPlan;
use crate::sdk::connector::{Connector, CredentialSpec, OriginSpec, Trigger};
use crate::sdk::effect::{AbsenceSearch, Effect, NoIdempotencyEvidence};
use crate::sdk::errors::{ConnectorErrorClass, ErrorMap};
use crate::sdk::operation::{JsonTemplate, Operation, OperationBuilder, OperationError, Required};
use crate::sdk::pagination::Pagination;
use crate::sdk::webhook::{SignatureEncoding, WebhookVerifier};

/// The connector name a deployment selects.
pub const NAME: &str = "shopify";

/// The SemVer core of this connector's contract.
pub const VERSION: &str = "1.0.0";

/// The deploy-time configuration key that fills the templated host: the store's
/// own `myshopify.com` label.
pub const SHOP: &str = "shop";

/// The pinned Admin API version. Shopify releases one per quarter and supports
/// each "for a minimum of 12 months"; `2026-07` is the stable version published
/// today, accessible until 2027-07-16.
pub const API_VERSION: &str = "2026-07";

/// "The maximum `limit` value is `250`."
const PAGE_SIZE: &str = "250";

/// This connector's static declaration (spec 010 §4).
pub fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        let mut builder = Connector::declare(NAME, VERSION)
            .origin(
                OriginSpec::templated_host("https", "{shop}.myshopify.com", None)
                    .expect("Shopify's published host form is valid"),
            )
            .credential(CredentialSpec::for_plan(
                AuthPlan::api_key_header("X-Shopify-Access-Token")
                    .expect("Shopify's published credential header is valid"),
            ))
            .operations(operations().expect("the Shopify declarations are valid"));
        for event in events() {
            builder = builder.trigger(
                Trigger::webhook(event.provider_event(), VERSION, verification())
                    .expect("a Shopify trigger declaration is valid"),
            );
        }
        builder.build().expect("the Shopify declaration is valid")
    });
    &CONNECTOR
}

/// Shopify's inbound signature scheme.
///
/// "Each HTTPS delivery includes a **base64-encoded** HMAC signature in the
/// `X-Shopify-Hmac-SHA256` header, generated using your app's client secret and
/// the raw request body." The header value is the digest and nothing else —
/// there is no prefix and no timestamp in the scheme — so a replayed authentic
/// delivery is indistinguishable from the original and replay protection has to
/// come from the delivery identifier rather than from a window.
pub fn verification() -> WebhookVerifier {
    WebhookVerifier::hmac_body("X-Shopify-Hmac-Sha256", SignatureEncoding::Base64)
        .expect("the Shopify signature scheme is a valid declaration")
}

/// "`X-Shopify-Webhook-Id` — A unique composite key per delivery. Use to
/// identify and deduplicate individual deliveries."
pub const DELIVERY_HEADER: &str = "X-Shopify-Webhook-Id";

/// "`X-Shopify-Topic` — The topic name (for example, `products/update`)."
pub const TOPIC_HEADER: &str = "X-Shopify-Topic";

/// The inbound events this connector declares (spec 013 §3).
///
/// "Every delivery includes a JSON body containing the full REST resource for
/// the subscribed topic" — the resource is at the document root, with no
/// envelope, which is why every pointer here starts at `/id` rather than at
/// `/order/id`.
pub fn events() -> &'static [TriggerEvent] {
    static EVENTS: LazyLock<Vec<TriggerEvent>> = LazyLock::new(|| {
        let order = |topic: &'static str| {
            TriggerEvent::declare(
                topic,
                EventIdentifier::Header(DELIVERY_HEADER),
                [
                    ("id", "/id", ValueScalar::Int64, Required::Yes),
                    (
                        "admin_graphql_api_id",
                        "/admin_graphql_api_id",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "order_number",
                        "/order_number",
                        ValueScalar::Int64,
                        Required::Yes,
                    ),
                    (
                        "financial_status",
                        "/financial_status",
                        ValueScalar::String,
                        Required::No,
                    ),
                    // Money is a JSON *string* in every Shopify payload
                    // (`"total_price": "404.95"`), so it is typed as one.
                    (
                        "total_price",
                        "/total_price",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    (
                        "updated_at",
                        "/updated_at",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("a Shopify order event declaration is valid")
        };
        vec![
            order("orders/create"),
            order("orders/updated"),
            order("orders/paid"),
            TriggerEvent::declare(
                "products/update",
                EventIdentifier::Header(DELIVERY_HEADER),
                [
                    ("id", "/id", ValueScalar::Int64, Required::Yes),
                    (
                        "admin_graphql_api_id",
                        "/admin_graphql_api_id",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                    ("title", "/title", ValueScalar::String, Required::Yes),
                    ("handle", "/handle", ValueScalar::String, Required::Yes),
                    ("status", "/status", ValueScalar::String, Required::Yes),
                    (
                        "updated_at",
                        "/updated_at",
                        ValueScalar::String,
                        Required::Yes,
                    ),
                ],
            )
            .expect("the Shopify products/update event declaration is valid"),
        ]
    });
    &EVENTS
}

/// The ordered error map.
///
/// Shopify's `errors` value is polymorphic — a string, an array of strings, or
/// an object of field to messages — and none of those shapes carries a stable
/// machine-readable code, so the map is keyed on the documented status table
/// only and the body is never read.
pub fn error_map() -> &'static ErrorMap {
    static MAP: LazyLock<ErrorMap> = LazyLock::new(|| {
        ErrorMap::builder(ConnectorErrorClass::Permanent)
            // "generally due to bad syntax", and "422 Unprocessable Entity —
            // The request body contains semantic errors."
            .on_statuses([400, 422], ConnectorErrorClass::Validation)
            // "401 Unauthorized — The client doesn't have correct
            // authentication credentials", and "403 Forbidden — ... typically
            // caused by incorrect access scopes."
            .on_statuses([401, 403], ConnectorErrorClass::Authentication)
            // "402 Payment Required — The shop is frozen", "404 Not Found",
            // "406 Not Acceptable", "409 Resource Conflict", "415 Unsupported
            // Media Type", "423 Locked", "430 Shopify Security Rejection",
            // "501 Not Implemented", "540 Temporarily Disabled". Every one of
            // them needs a change on this deployment's side or the merchant's.
            .on_statuses(
                [402, 404, 406, 409, 414, 415, 423, 430, 501, 540],
                ConnectorErrorClass::Permanent,
            )
            .on_status(429, ConnectorErrorClass::Http429)
            // "5xx Errors — An internal error occurred in Shopify", plus the
            // documented `530 Origin DNS Error` and `504 Gateway Timeout`.
            .on_statuses([500, 502, 503, 504, 530], ConnectorErrorClass::Http5xx)
            .build()
            .expect("the Shopify error map is a valid declaration")
    });
    &MAP
}

/// The continuation plan of each collection.
///
/// "REST endpoints support cursor-based pagination", the continuation arrives
/// as a `link` header with `rel={next}`, and "If your initial request doesn't
/// return enough records to generate an additional page of results, then the
/// response won't have a link header" — so the absence of the header is the
/// documented end of the walk. The `page_info` cursor is never rebuilt here:
/// "The `page_info` parameter can't be modified and must be used exactly as it
/// appears in the link header URL", which is exactly what following the
/// continuation as a destination does.
pub fn pagination(operation_id: &str) -> Option<&'static Pagination> {
    static ORDERS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("/orders", "next").expect("the Shopify order link plan is valid")
    });
    static PRODUCTS: LazyLock<Pagination> = LazyLock::new(|| {
        Pagination::link_header("/products", "next")
            .expect("the Shopify product link plan is valid")
    });
    match operation_id {
        "order.list" => Some(&ORDERS),
        "product.list" => Some(&PRODUCTS),
        _ => None,
    }
}

fn common(builder: OperationBuilder) -> OperationBuilder {
    builder.version(VERSION)
}

/// Every operation this connector publishes.
fn operations() -> Result<Vec<Operation>, OperationError> {
    let order_get = common(Operation::get(
        "order.get",
        "/admin/api/2026-07/orders/{order_id}.json",
    ))
    .path_param("order_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/order/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/order/name", ValueScalar::String, Required::Yes)
    .output_pointer(
        "financial_status",
        "/order/financial_status",
        ValueScalar::String,
        Required::No,
    )
    .output_pointer(
        "total_price",
        "/order/total_price",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "created_at",
        "/order/created_at",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let order_list = common(Operation::get(
        "order.list",
        "/admin/api/2026-07/orders.json",
    ))
    // "The GET orders endpoint returns open orders by default" — the status a
    // caller wants is a declared input rather than an inherited default.
    .query_input("status", "status")
    .query_static("limit", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .output_pointer("orders", "/orders", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let product_get = common(Operation::get(
        "product.get",
        "/admin/api/2026-07/products/{product_id}.json",
    ))
    .path_param("product_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/product/id", ValueScalar::Int64, Required::Yes)
    .output_pointer(
        "title",
        "/product/title",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "handle",
        "/product/handle",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "status",
        "/product/status",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "updated_at",
        "/product/updated_at",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::read_only())
    .build()?;

    let product_list = common(Operation::get(
        "product.list",
        "/admin/api/2026-07/products.json",
    ))
    .query_static("limit", PAGE_SIZE)
    .success_statuses([StatusCode::OK])
    .output_pointer("products", "/products", ValueScalar::Json, Required::Yes)
    .effect(Effect::read_only())
    .build()?;

    let product_update = common(Operation::put(
        "product.update",
        "/admin/api/2026-07/products/{product_id}.json",
    ))
    .path_param("product_id", ValueScalar::Int64)
    .body(JsonTemplate::object([(
        "product",
        JsonTemplate::object([
            ("title", JsonTemplate::input("title")),
            ("status", JsonTemplate::input("status")),
        ]),
    )]))
    .success_statuses([StatusCode::OK])
    .output_pointer("id", "/product/id", ValueScalar::Int64, Required::Yes)
    .output_pointer(
        "title",
        "/product/title",
        ValueScalar::String,
        Required::Yes,
    )
    .output_pointer(
        "updated_at",
        "/product/updated_at",
        ValueScalar::String,
        Required::Yes,
    )
    .effect(Effect::inventory_only(
        "Shopify publishes no statement that PUT /products/{id}.json replaces the resource, and \
         its own examples send only the fields being changed while an array member is replaced by \
         presence, so this is a partial update rather than a write to a fixed resource identity; \
         Shopify's idempotency mechanism is scoped to \"POST requests that process credit card \
         payments, create billing attempts for subscriptions, or capture revenue details\", which \
         this is not",
    )?)
    .build()?;

    // "Deletes a product." The documented response is `HTTP/1.1 200 OK` with
    // the body `{}` — no resource, no envelope — so the declaration carries no
    // output pointer and admits the empty body as the documented success.
    let product_delete = common(Operation::delete(
        "product.delete",
        "/admin/api/2026-07/products/{product_id}.json",
    ))
    .path_param("product_id", ValueScalar::Int64)
    .success_statuses([StatusCode::OK])
    .no_content_statuses([StatusCode::OK])
    .effect(Effect::provider_idempotent_natural_method(
        "Shopify documents DELETE /admin/api/{version}/products/{product_id}.json as \"Deletes a \
         product.\" against one fixed product id, answering `200 OK` with the body `{}`, so two \
         identical sends leave the same one absent product",
    )?)
    .build()?;

    let order_create = common(Operation::post(
        "order.create",
        "/admin/api/2026-07/orders.json",
    ))
    .body(JsonTemplate::object([(
        "order",
        JsonTemplate::object([
            ("line_items", JsonTemplate::input("line_items")),
            ("email", JsonTemplate::input("email")),
        ]),
    )]))
    .success_statuses([StatusCode::CREATED])
    .output_pointer("id", "/order/id", ValueScalar::Int64, Required::Yes)
    .output_pointer("name", "/order/name", ValueScalar::String, Required::Yes)
    .output_pointer(
        "order_number",
        "/order/order_number",
        ValueScalar::Int64,
        Required::Yes,
    )
    .effect(Effect::at_most_once(NoIdempotencyEvidence::searched(
        AbsenceSearch::PublishedContract,
        "Shopify publishes an idempotency key for the REST Admin API and publishes exactly which \
             requests it covers — \"POST requests that process credit card payments, create \
             billing attempts for subscriptions, or capture revenue details accept idempotency \
             keys\" — and order creation is none of the three; the Order reference documents no \
             `unique_token` and no client-supplied request identifier. A documented exclusion is \
             stronger than an absence",
        "a second order with a new `id` and a new `order_number` — and, on a trial or Partner \
             development store, one more of the five new orders a minute Shopify allows",
    )?))
    .build()?;

    Ok(vec![
        order_get,
        order_list,
        product_get,
        product_list,
        product_update,
        product_delete,
        order_create,
    ])
}
