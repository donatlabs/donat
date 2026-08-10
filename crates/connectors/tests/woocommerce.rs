//! WooCommerce connector proofs (spec 023 §4), against the SDK's local provider
//! stub.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use donat_connectors::providers::woocommerce;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    Operation, OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const CONSUMER_KEY: &str = "ck_conformance_consumer_key";
const ORDER_ID: i64 = 727;
const CUSTOMER_ID: i64 = 25;

fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        woocommerce::connector(CONSUMER_KEY).expect("a valid consumer key declares")
    });
    &CONNECTOR
}

fn operation(id: &str) -> &'static Operation {
    connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the woocommerce declaration publishes {id}"))
}

/// "You may use HTTP Basic Auth by providing the REST API Consumer Key as the
/// username and the REST API Consumer Secret as the password."
fn expected_authorization() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{CONSUMER_KEY}:{SECRET_SENTINEL}"))
    )
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::basic(CONSUMER_KEY)
        .expect("the published username form is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn order() -> JsonValue {
    json!({
        "id": ORDER_ID,
        "number": "727",
        "status": "processing",
        "currency": "USD",
        "total": "29.99",
        "customer_id": CUSTOMER_ID,
        "date_created": "2026-08-01T09:00:00",
        "date_modified": "2026-08-02T09:00:00",
    })
}

fn customer() -> JsonValue {
    json!({
        "id": CUSTOMER_ID,
        "email": "joe@example.com",
        "first_name": "Joe",
        "last_name": "Doe",
        "role": "customer",
        "date_created": "2026-08-01T09:00:00",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("order.get", json!({ "order_id": ORDER_ID })),
        ("order.list", json!({ "status": "any" })),
        (
            "order.create",
            json!({
                "payment_method": "bacs", "customer_id": CUSTOMER_ID,
                "billing": {}, "shipping": {}, "line_items": [], "set_paid": false,
            }),
        ),
        (
            "order.update",
            json!({ "order_id": ORDER_ID, "status": "completed", "customer_note": null }),
        ),
        ("customer.get", json!({ "customer_id": CUSTOMER_ID })),
        ("customer.list", json!({ "role": "all" })),
        (
            "customer.create",
            json!({ "email": "joe@example.com", "first_name": "Joe", "last_name": "Doe",
                    "username": null, "billing": {} }),
        ),
        (
            "customer.update",
            json!({ "customer_id": CUSTOMER_ID, "first_name": "Joe",
                    "last_name": "Doe", "billing": {} }),
        ),
        ("product.get", json!({ "product_id": 99 })),
        ("product.list", json!({ "status": "any" })),
        (
            "order_note.list",
            json!({ "order_id": ORDER_ID, "type": "any" }),
        ),
        (
            "order_note.create",
            json!({ "order_id": ORDER_ID, "note": "Order ok!!!", "customer_note": false }),
        ),
    ]
}

/// `woocommerce_request_shape`: exact method, path, query, headers, and body for
/// every operation, all under the one published route prefix.
#[tokio::test]
async fn woocommerce_request_shape() {
    let note = json!({ "id": 281, "author": "system", "note": "Order ok!!!",
                       "date_created": "2026-08-02T09:00:00" });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/wp-json/wc/v3/orders/{ORDER_ID}"))
            .query("")
            .header("authorization", &expected_authorization())
            .no_body()
            .respond_json(200, order()),
        Expectation::new("GET", "/wp-json/wc/v3/orders")
            .query("status=any&per_page=100")
            .respond_json(200, json!([order()])),
        Expectation::new("POST", "/wp-json/wc/v3/orders")
            .json_body(json!({
                "payment_method": "bacs", "customer_id": CUSTOMER_ID,
                "billing": {}, "shipping": {}, "line_items": [], "set_paid": false,
            }))
            .respond_json(200, order()),
        Expectation::new("PUT", &format!("/wp-json/wc/v3/orders/{ORDER_ID}"))
            .json_body(json!({ "status": "completed", "customer_note": null }))
            .respond_json(200, order()),
        Expectation::new("GET", &format!("/wp-json/wc/v3/customers/{CUSTOMER_ID}"))
            .query("")
            .respond_json(200, customer()),
        Expectation::new("GET", "/wp-json/wc/v3/customers")
            .query("role=all&per_page=100")
            .respond_json(200, json!([customer()])),
        Expectation::new("POST", "/wp-json/wc/v3/customers")
            .json_body(json!({ "email": "joe@example.com", "first_name": "Joe",
                               "last_name": "Doe", "username": null, "billing": {} }))
            .respond_json(200, customer()),
        Expectation::new("PUT", &format!("/wp-json/wc/v3/customers/{CUSTOMER_ID}"))
            .json_body(json!({ "first_name": "Joe", "last_name": "Doe", "billing": {} }))
            .respond_json(200, customer()),
        Expectation::new("GET", "/wp-json/wc/v3/products/99")
            .query("")
            .respond_json(
                200,
                json!({ "id": 99, "name": "Cap", "sku": "cap",
                                       "status": "publish", "price": "9.00" }),
            ),
        Expectation::new("GET", "/wp-json/wc/v3/products")
            .query("status=any&per_page=100")
            .respond_json(200, json!([])),
        Expectation::new("GET", &format!("/wp-json/wc/v3/orders/{ORDER_ID}/notes"))
            .query("type=any")
            .respond_json(200, json!([note.clone()])),
        Expectation::new("POST", &format!("/wp-json/wc/v3/orders/{ORDER_ID}/notes"))
            .json_body(json!({ "note": "Order ok!!!", "customer_note": false }))
            .respond_json(200, note),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/wp-json/wc/v3/"),
            "{id} renders the published route prefix: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `woocommerce_auth_is_applied`: the consumer secret reaches the wire as the
/// Basic password under the configured consumer key, never as a query value.
#[tokio::test]
async fn woocommerce_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/wp-json/wc/v3/orders/{ORDER_ID}"),
    )
    .header("authorization", &expected_authorization())
    .respond_json(200, order())])
    .await;

    let request = render(&stub, "order.get", json!({ "order_id": ORDER_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("a base64 header is ASCII"),
        expected_authorization()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    // WooCommerce publishes a query-string fallback for servers that strip the
    // header; this connector declares the header form and only that one.
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        connector().credential(),
        woocommerce::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();

    for hostile in ["", "  ", "ck with space", "ck:secret"] {
        assert!(woocommerce::connector(hostile).is_err(), "`{hostile}`");
    }
    assert!(woocommerce::declaration_shape().is_ok());
}

/// `woocommerce_host_comes_only_from_deploy_time_configuration` (spec 023 §4
/// proof 1): the store's whole origin is one configuration value, and input, a
/// provider body, and a continuation each fail to move it.
#[tokio::test]
async fn woocommerce_host_comes_only_from_deploy_time_configuration() {
    assert_eq!(
        connector().origin().host_variable(),
        Some(woocommerce::STORE_ORIGIN)
    );

    let origin = connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            woocommerce::STORE_ORIGIN,
            "https://shop.example.test",
        )]))
        .expect("a configured store origin resolves");
    assert_eq!(origin.as_url().as_str(), "https://shop.example.test/");

    // 1. Operation input. A value that spells another authority stays inside its
    //    own query or path segment on the configured host.
    let request = operation("order.list")
        .plan_request(&origin, &json!({ "status": "https://attacker.invalid/" }))
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("shop.example.test"));
    assert_eq!(request.url().path(), "/wp-json/wc/v3/orders");

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("order.get")
        .extract_output(&json!({ "id": ORDER_ID, "number": "https://attacker.invalid" }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("number"),
        Some(&json!("https://attacker.invalid"))
    );

    // 3. A `Link` continuation to another origin is refused rather than
    //    followed, on a deployment origin exactly as on a fixed one.
    let stub = ProviderStub::start([Expectation::new("GET", "/wp-json/wc/v3/orders")
        .respond_header(
            "link",
            "<https://attacker.invalid/wp-json/wc/v3/orders?page=2>; rel=\"next\"",
        )
        .respond_json(200, json!([]))])
    .await;
    let failure = woocommerce::pagination("order.list")
        .expect("order.list declares a plan")
        .collect(
            render(&stub, "order.list", json!({ "status": "any" })),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    stub.assert_satisfied();

    // And the configured value is checked at deploy time: WooCommerce publishes
    // Basic authentication over HTTPS only, and an origin is a scheme, a host,
    // and a port.
    woocommerce::validate_store_origin("https://shop.example.test")
        .expect("a plain https store origin is admitted");
    woocommerce::validate_store_origin("https://shop.example.test:8443")
        .expect("a port is part of an origin");
    for refused in [
        "http://shop.example.test",
        "https://shop.example.test/store",
        "https://user:pass@shop.example.test",
        "https://shop.example.test/?rest_route=/wc/v3",
        "shop.example.test",
        "ftp://shop.example.test",
        "",
    ] {
        assert!(
            woocommerce::validate_store_origin(refused).is_err(),
            "`{refused}` is not a store origin this connector may send Basic credentials to"
        );
    }
}

/// `woocommerce_error_map`: every documented status reaches exactly one closed
/// class, and none of WooCommerce's prose crosses the boundary.
#[tokio::test]
async fn woocommerce_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (415, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/wp-json/wc/v3/orders/{ORDER_ID}"),
        )
        .respond_json(
            status,
            json!({
                "code": "woocommerce_rest_shop_order_invalid_id",
                "message": format!("shop.example.test rejected {SECRET_SENTINEL}"),
                "data": { "status": status },
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "order.get", json!({ "order_id": ORDER_ID })))
            .await
            .expect("the stub answers");

        let failure = woocommerce::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [
            SECRET_SENTINEL,
            "shop.example.test",
            "woocommerce_rest_shop_order",
        ] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `woocommerce_rate_limit_is_classified`: WooCommerce publishes no rate limit
/// for `wc/v3` at all, so a `429` from the store's own host is retryable and its
/// hint is whatever the response carried, clamped.
#[tokio::test]
async fn woocommerce_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/wp-json/wc/v3/orders/{ORDER_ID}")).respond_json(
            429,
            json!({ "code": "too_many_requests", "message": "slow down" }),
        ),
        Expectation::new("GET", &format!("/wp-json/wc/v3/orders/{ORDER_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(
                429,
                json!({ "code": "too_many_requests", "message": "slow down" }),
            ),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "order.get", json!({ "order_id": ORDER_ID })))
            .await
            .expect("the stub answers");
        failures.push(woocommerce::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "WooCommerce publishes no Retry-After for wc/v3, so the connector invents none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `woocommerce_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is WooCommerce's own `Link` header, it is followed as a
/// destination on this origin, and the walk makes exactly the number of requests
/// the plan declares.
#[tokio::test]
async fn woocommerce_cursor_is_opaque_and_bounded() {
    let plan = woocommerce::pagination("order.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/wp-json/wc/v3/orders")
            .query("status=any&per_page=100")
            .respond_header("x-wp-total", "2")
            .respond_header(
                "link",
                "</wp-json/wc/v3/orders?page=2&per_page=100>; rel=\"next\", \
                 </wp-json/wc/v3/orders?page=2&per_page=100>; rel=\"last\"",
            )
            .respond_json(200, json!([{ "id": 1 }])),
        Expectation::new("GET", "/wp-json/wc/v3/orders")
            .query("page=2&per_page=100")
            .respond_json(200, json!([{ "id": 2 }])),
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
        .expect("the walk follows one link and stops where the header stops");
    assert_eq!(orders, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `woocommerce_pagination_is_bounded`: the declared plan terminates and
/// respects the call, page, item, and byte budgets.
#[tokio::test]
async fn woocommerce_pagination_is_bounded() {
    let plan = woocommerce::pagination("order.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/wp-json/wc/v3/orders")
                .respond_header("link", "</wp-json/wc/v3/orders?page=9>; rel=\"next\"")
                .respond_json(200, json!([{ "id": 1 }, { "id": 2 }]))
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "order.list", json!({ "status": "any" })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    for id in [
        "order.list",
        "customer.list",
        "product.list",
        "order_note.list",
    ] {
        assert_eq!(
            woocommerce::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            "",
            "{id} collects the bare array WooCommerce answers with"
        );
    }
    for id in [
        "order.get",
        "order.create",
        "order.update",
        "customer.get",
        "customer.create",
        "customer.update",
        "product.get",
        "order_note.create",
    ] {
        assert!(
            woocommerce::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `woocommerce_effects_are_classified`: every operation carries a class, and
/// the two updates are unreachable from a Process.
#[test]
fn woocommerce_effects_are_classified() {
    let expected = [
        ("order.get", EffectClass::ReadOnly),
        ("order.list", EffectClass::ReadOnly),
        ("order.create", EffectClass::AtMostOnce),
        ("order.update", EffectClass::InventoryOnly),
        ("customer.get", EffectClass::ReadOnly),
        ("customer.list", EffectClass::ReadOnly),
        ("customer.create", EffectClass::AtMostOnce),
        ("customer.update", EffectClass::InventoryOnly),
        ("product.get", EffectClass::ReadOnly),
        ("product.list", EffectClass::ReadOnly),
        ("order_note.list", EffectClass::ReadOnly),
        ("order_note.create", EffectClass::AtMostOnce),
    ];
    assert_eq!(connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector().admit_operation("order.update"),
        Err(OperationRejection::InventoryOnly)
    );

    let evidence = operation("order.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("cart_hash"));
    assert!(evidence.repeat_produces().contains("second order"));
}

/// `woocommerce_output_contract`: the declared pointers read WooCommerce's own
/// objects, with its own typing — "Resource IDs are returned as integers" and
/// "Any decimal monetary amount ... will be returned as strings".
#[test]
fn woocommerce_output_contract() {
    let get = operation("order.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&order()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": ORDER_ID, "number": "727", "status": "processing", "currency": "USD",
            "total": "29.99", "customer_id": CUSTOMER_ID,
            "date_created": "2026-08-01T09:00:00", "date_modified": "2026-08-02T09:00:00",
        })
    );
    // A total that arrived as a number is a contract violation, not a coercion.
    assert_eq!(
        get.decode_response(200, br#"{"id":727,"total":29.99}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"727"}"#)
            .expect_err("an id that is not an integer is not an order")
            .class(),
        ConnectorErrorClass::Validation
    );
    // "Blank fields are generally included as `null` or emtpy string instead of
    // being omitted", so only the identity is demanded.
    assert_eq!(
        get.decode_response(200, br#"{"id":727}"#)
            .expect("only the identity is required")
            .get("total"),
        Some(&json!(null))
    );
}
