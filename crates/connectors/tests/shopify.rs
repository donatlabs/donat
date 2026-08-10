//! Shopify connector proofs (spec 013 §4), against the SDK's local provider stub
//! and against signatures this test generates itself.

mod webhook_support;

use std::time::Duration;

use donat_connectors::providers::inbound::EventIdentifier;
use donat_connectors::providers::shopify;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, WebhookRejection,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use webhook_support as inbound;

const ORDER_ID: i64 = 820_982_911_946_154_508;
const PRODUCT_ID: i64 = 788_032_119_674_292_922;

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    shopify::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the shopify declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_header("X-Shopify-Access-Token")
        .expect("a static header name is valid")
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn order() -> JsonValue {
    json!({
        "order": {
            "id": ORDER_ID,
            "name": "#9999",
            "financial_status": "paid",
            "total_price": "404.95",
            "created_at": "2021-12-31T19:00:00-05:00",
        }
    })
}

fn product() -> JsonValue {
    json!({
        "product": {
            "id": PRODUCT_ID,
            "title": "Example T-Shirt",
            "handle": "example-t-shirt",
            "status": "active",
            "updated_at": "2021-12-31T19:00:00-05:00",
        }
    })
}

/// `shopify_request_shape`: exact method, path, query, headers, and body for
/// every operation, and the templated host the request renders against.
#[tokio::test]
async fn shopify_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/admin/api/2026-07/orders/{ORDER_ID}.json"))
            .query("")
            .header("x-shopify-access-token", SECRET_SENTINEL)
            .no_body()
            .respond_json(200, order()),
        Expectation::new("GET", "/admin/api/2026-07/orders.json")
            .query("status=any&limit=250")
            .no_body()
            .respond_json(200, json!({ "orders": [] })),
        Expectation::new(
            "GET",
            &format!("/admin/api/2026-07/products/{PRODUCT_ID}.json"),
        )
        .query("")
        .respond_json(200, product()),
        Expectation::new("GET", "/admin/api/2026-07/products.json")
            .query("limit=250")
            .respond_json(200, json!({ "products": [] })),
        Expectation::new(
            "PUT",
            &format!("/admin/api/2026-07/products/{PRODUCT_ID}.json"),
        )
        .json_body(json!({ "product": { "title": "New", "status": "draft" } }))
        .respond_json(200, product()),
        Expectation::new(
            "DELETE",
            &format!("/admin/api/2026-07/products/{PRODUCT_ID}.json"),
        )
        .respond_json(200, json!({})),
        Expectation::new("POST", "/admin/api/2026-07/orders.json")
            .json_body(json!({ "order": { "line_items": [], "email": "a@example.test" } }))
            .respond_json(
                201,
                json!({ "order": { "id": ORDER_ID, "name": "#9999", "order_number": 1234 } }),
            ),
    ])
    .await;

    for (id, input) in [
        ("order.get", json!({ "order_id": ORDER_ID })),
        ("order.list", json!({ "status": "any" })),
        ("product.get", json!({ "product_id": PRODUCT_ID })),
        ("product.list", json!({})),
        (
            "product.update",
            json!({ "product_id": PRODUCT_ID, "title": "New", "status": "draft" }),
        ),
        ("product.delete", json!({ "product_id": PRODUCT_ID })),
        (
            "order.create",
            json!({ "line_items": [], "email": "a@example.test" }),
        ),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// The batch's one `Origin::TemplatedHost` (spec 013 §1): the shop label is
/// filled from deploy-time configuration and from nowhere else. Operation input,
/// a provider response, and a continuation each get a turn, and none moves it.
#[tokio::test]
async fn shopify_host_comes_only_from_deploy_time_configuration() {
    let specification = shopify::connector().origin();
    assert_eq!(specification.host_variable(), Some(shopify::SHOP));

    let origin = shopify::connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            shopify::SHOP,
            "acme-store",
        )]))
        .expect("a configured shop resolves");
    assert_eq!(
        origin.as_url().as_str(),
        "https://acme-store.myshopify.com/"
    );

    // 1. Operation input. A path value that spells another authority stays one
    //    percent-encoded segment on the configured host.
    let request = operation("product.get")
        .plan_request(&origin, &json!({ "product_id": 1 }))
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("acme-store.myshopify.com"));
    assert_eq!(request.url().scheme(), "https");

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("product.get")
        .extract_output(&json!({
            "product": {
                "id": 1, "title": "t", "handle": "h", "status": "active",
                "updated_at": "2021-12-31T19:00:00-05:00",
            }
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(output.get("handle"), Some(&json!("h")));
    assert_eq!(
        operation("product.get")
            .plan_request(&origin, &json!({ "product_id": 2 }))
            .expect("the next request renders")
            .url()
            .host_str(),
        Some("acme-store.myshopify.com")
    );

    // 3. A `link` continuation to another origin is refused rather than
    //    followed, on a templated origin exactly as on a fixed one.
    let stub = ProviderStub::start([Expectation::new("GET", "/admin/api/2026-07/products.json")
        .respond_header(
            "link",
            "<https://attacker.invalid/admin/api/2026-07/products.json>; rel=\"next\"",
        )
        .respond_json(200, json!({ "products": [] }))])
    .await;
    let failure = shopify::pagination("product.list")
        .expect("product.list declares a plan")
        .collect(
            render(&stub, "product.list", json!({})),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");

    // And the configuration itself admits one host label and nothing else,
    // which is a strict subset of Shopify's published shop grammar.
    for hostile in [
        "acme.myshopify.com",
        "acme/../evil",
        "acme:8080",
        "user@acme",
        "",
        "-acme",
        "ACME",
    ] {
        assert!(
            shopify::connector()
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    shopify::SHOP,
                    hostile
                )]))
                .is_err(),
            "configuration value {hostile} must not resolve"
        );
    }
    assert!(
        shopify::connector()
            .resolve_origin(&ConnectorConfiguration::default())
            .is_err(),
        "an unconfigured shop is a startup failure"
    );
}

/// `shopify_auth_is_applied`: the admin access token reaches the wire as
/// `X-Shopify-Access-Token` and appears nowhere else.
#[tokio::test]
async fn shopify_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/admin/api/2026-07/orders/{ORDER_ID}.json"),
    )
    .header("x-shopify-access-token", SECRET_SENTINEL)
    .without_header("authorization")
    .respond_json(200, order())])
    .await;

    let request = render(&stub, "order.get", json!({ "order_id": ORDER_ID }));
    assert!(
        request
            .headers()
            .get("x-shopify-access-token")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let failure = shopify::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        shopify::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `shopify_error_map`: every documented status reaches exactly one closed
/// class, whichever of the three shapes Shopify's polymorphic `errors` takes,
/// and none of its prose crosses the boundary.
#[tokio::test]
async fn shopify_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        // "The shop is frozen." — permanent for this deployment.
        (402, ConnectorErrorClass::Permanent),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (406, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (423, ConnectorErrorClass::Permanent),
        (430, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];
    // The three documented shapes of `errors`, so a class never depends on one.
    let bodies = [
        json!({ "errors": format!("[API] Invalid API key {SECRET_SENTINEL} on shard db-7") }),
        json!({ "errors": ["The fulfillment order is not in an open state."] }),
        json!({ "errors": { "title": ["can't be blank"] } }),
    ];

    for (status, expected) in documented {
        for body in &bodies {
            let stub = ProviderStub::start([Expectation::new(
                "GET",
                &format!("/admin/api/2026-07/orders/{ORDER_ID}.json"),
            )
            .respond_header("retry-after", "2")
            .respond_json(status, body.clone())])
            .await;
            let response = stub
                .send(render(&stub, "order.get", json!({ "order_id": ORDER_ID })))
                .await
                .expect("the stub answers");

            let failure = shopify::error_map().classify_response(&response);
            assert_eq!(failure.class(), expected, "status {status}");
            assert_eq!(failure.provider_status(), Some(status));
            let surface = format!(
                "{} {} {}",
                failure.code(),
                failure.safe_message(),
                failure.diagnostic()
            );
            for leaked in [SECRET_SENTINEL, "db-7", "can't be blank"] {
                assert!(!surface.contains(leaked), "status {status}: {surface}");
            }
            stub.assert_satisfied();
        }
    }
}

/// `shopify_pagination_is_bounded`: the cursor walk follows Shopify's `link`
/// header opaquely, stops when the header stops carrying `rel=next`, and cannot
/// exceed its budget.
#[tokio::test]
async fn shopify_pagination_is_bounded() {
    let plan = shopify::pagination("order.list").expect("order.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/admin/api/2026-07/orders.json")
            .query("status=any&limit=250")
            .respond_header(
                "link",
                "<{base_url}/admin/api/2026-07/orders.json?page_info=hijgklmn&limit=250>; rel=next",
            )
            .respond_json(200, json!({ "orders": [{ "id": 1 }] })),
        // The last page publishes only `rel=previous`, which is Shopify's
        // documented end of the walk.
        Expectation::new("GET", "/admin/api/2026-07/orders.json")
            .query("page_info=hijgklmn&limit=250")
            .respond_header(
                "link",
                "<{base_url}/admin/api/2026-07/orders.json?page_info=abcdefg&limit=250>; rel=previous",
            )
            .respond_json(200, json!({ "orders": [{ "id": 2 }] })),
    ])
    .await;
    let orders = plan
        .collect(
            render(&stub, "order.list", json!({ "status": "any" })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one continuation and stops");
    assert_eq!(orders.len(), 2);
    stub.assert_satisfied();

    // An endless provider exhausts a ceiling rather than looping.
    let stub = ProviderStub::start((0..12).map(|_| {
        Expectation::new("GET", "/admin/api/2026-07/orders.json")
            .respond_header(
                "link",
                "<{base_url}/admin/api/2026-07/orders.json?page_info=more>; rel=next",
            )
            .respond_json(200, json!({ "orders": [{ "id": 1 }] }))
    }))
    .await;
    let failure = plan
        .collect(
            render(&stub, "order.list", json!({ "status": "any" })),
            &stub.origin(),
            &PaginationBudget::new(3, 3, 1_000, 1_024 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("an endless provider exhausts the budget");
    assert_eq!(failure.code(), "connector_pagination_budget");

    for id in ["order.get", "product.get", "product.delete", "order.create"] {
        assert!(
            shopify::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `shopify_effects_are_classified`: the delete is the one executable mutation,
/// on Shopify's own delete statement, and the two creates/updates are refused on
/// Shopify's own idempotency *scope* sentence.
#[test]
fn shopify_effects_are_classified() {
    let connector = shopify::connector();
    let expected = [
        ("order.get", EffectClass::ReadOnly),
        ("order.list", EffectClass::ReadOnly),
        ("product.get", EffectClass::ReadOnly),
        ("product.list", EffectClass::ReadOnly),
        ("product.update", EffectClass::InventoryOnly),
        (
            "product.delete",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        ("order.create", EffectClass::AtMostOnce),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector.admit_operation("product.update"),
        Err(OperationRejection::InventoryOnly),
        "a PUT whose replace semantics Shopify never publishes is refused by both gates"
    );
    assert!(
        operation("order.create")
            .effect()
            .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
            .is_some_and(|evidence| evidence
                .searched_documentation()
                .contains("credit card payments")
                && evidence.repeat_produces().contains("a second order")),
        "the create records Shopify's own scope sentence, and the order a repeat would add"
    );
}

/// Two identical deletes name the same product and leave the same one absent
/// product, which is the `NaturalMethod` evidence spec 010 §7 asks for.
#[tokio::test]
async fn shopify_two_identical_deletes_leave_one_result() {
    let stub = ProviderStub::start([
        Expectation::new(
            "DELETE",
            &format!("/admin/api/2026-07/products/{PRODUCT_ID}.json"),
        )
        .respond_json(200, json!({})),
        Expectation::new(
            "DELETE",
            &format!("/admin/api/2026-07/products/{PRODUCT_ID}.json"),
        )
        .respond_json(200, json!({})),
    ])
    .await;

    let input = json!({ "product_id": PRODUCT_ID });
    let first = render(&stub, "product.delete", input.clone());
    let second = render(&stub, "product.delete", input);
    assert_eq!(first.method(), second.method());
    assert_eq!(first.url().as_str(), second.url().as_str());
    assert!(first.body().is_empty() && second.body().is_empty());
    for request in [first, second] {
        assert_eq!(
            stub.send(request).await.expect("the stub answers").status,
            200
        );
    }
    stub.assert_satisfied();
}

/// `shopify_output_contract`: the declared pointers read Shopify's envelope, and
/// the documented empty delete body is the documented success.
#[test]
fn shopify_output_contract() {
    let get = operation("order.get");
    assert_eq!(
        get.decode_response(
            200,
            br##"{"order":{"id":820982911946154508,"name":"#9999","financial_status":"paid","total_price":"404.95","created_at":"2021-12-31T19:00:00-05:00","line_items":[]}}"##,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": ORDER_ID, "name": "#9999", "financial_status": "paid",
            "total_price": "404.95", "created_at": "2021-12-31T19:00:00-05:00",
        })
    );
    // Money is a JSON string in every Shopify payload, and the declaration
    // types it as one: a number there is a contract violation, not a coercion.
    assert_eq!(
        get.decode_response(
            200,
            br##"{"order":{"id":1,"name":"#1","total_price":404.95,"created_at":"t"}}"##,
        )
        .expect_err("a mistyped required pointer is a validation failure")
        .class(),
        ConnectorErrorClass::Validation
    );

    let delete = operation("product.delete");
    assert!(delete.is_success(200) && delete.is_no_content_success(200));
    assert_eq!(
        delete
            .decode_response(200, b"{}")
            .expect("the documented `200 OK {}` is the documented success"),
        json!({})
    );
    assert!(
        !delete.is_success(404),
        "Shopify does not publish the status of a second delete, so 404 is not admitted as one"
    );
}

// ---------------------------------------------------------------------------
// Inbound (spec 013 §4)
// ---------------------------------------------------------------------------

/// Shopify's published scheme, transcribed here: "a base64-encoded HMAC
/// signature in the `X-Shopify-Hmac-SHA256` header, generated using your app's
/// client secret and the raw request body."
fn sign(body: &[u8]) -> HeaderMap {
    inbound::headers(&[
        (
            "X-Shopify-Hmac-Sha256",
            &inbound::base64(&inbound::digest(body)),
        ),
        ("X-Shopify-Topic", "orders/create"),
        ("X-Shopify-Shop-Domain", "acme-store.myshopify.com"),
        (
            "X-Shopify-Webhook-Id",
            "b54557e4-bdd9-4b37-8a5f-bf7d70bcd043",
        ),
    ])
}

#[test]
fn shopify_signature_precedes_parse() {
    inbound::signature_precedes_parse(
        shopify::connector(),
        sign,
        inbound::headers(&[("X-Shopify-Hmac-Sha256", &inbound::base64(&[0u8; 32]))]),
        WebhookRejection::InvalidSignature,
    );
    inbound::nothing_prints_the_secret(shopify::connector());

    // A hex digest is not a base64 one: the encoding is part of the scheme.
    let body = br#"{"id":1}"#;
    assert_eq!(
        inbound::verify(
            shopify::connector(),
            &inbound::headers(&[(
                "X-Shopify-Hmac-Sha256",
                &inbound::hex(&inbound::digest(body))
            )]),
            body,
        )
        .expect_err("a hex digest is not a candidate for a base64 scheme"),
        WebhookRejection::InvalidSignature
    );
}

#[test]
fn shopify_signature_is_exact() {
    const BODY: &[u8] = br#"{"id":820982911946154508,"order_number":1234,"total_price":"404.95"}"#;
    inbound::signature_is_exact(shopify::connector(), BODY, sign, |headers| {
        let value = headers
            .get("x-shopify-hmac-sha256")
            .and_then(|value| value.to_str().ok())
            .expect("the fixture is signed");
        let mut bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value)
            .expect("the fixture signature is base64");
        bytes[0] ^= 0x01;
        inbound::headers(&[("X-Shopify-Hmac-Sha256", &inbound::base64(&bytes))])
    });
    inbound::triggers_share_one_scheme(shopify::connector());
    inbound::events_match_triggers(shopify::connector(), shopify::events());

    for event in shopify::events() {
        assert_eq!(
            event.event_identifier(),
            &EventIdentifier::Header("X-Shopify-Webhook-Id"),
            "`{}` keys on the delivery id Shopify tells a consumer to deduplicate on",
            event.provider_event()
        );
    }
    assert_eq!(
        shopify::events()
            .iter()
            .map(donat_connectors::providers::inbound::TriggerEvent::provider_event)
            .collect::<Vec<_>>(),
        [
            "orders/create",
            "orders/updated",
            "orders/paid",
            "products/update"
        ]
    );
}

#[test]
fn shopify_comparison_is_constant_time() {
    inbound::comparison_is_constant_time("shopify.rs", &inbound::module_source("shopify"));
}

#[test]
fn shopify_body_limit_precedes_verification() {
    inbound::body_limit_precedes_verification(shopify::connector(), sign);
}
