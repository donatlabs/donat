//! Paddle connector proofs (spec 026 §4), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::paddle;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const CUSTOMER: &str = "ctm_01hv6y1jedq4p1n0yqn5ba3ky4";
const TRANSACTION: &str = "txn_01hvcc93znj3mpqt1tenkjb04y";
const SUBSCRIPTION: &str = "sub_01hvccbx32q2gb40sqx7n42430";

/// The SDK percent-encodes every path value per segment, and a Paddle ID's
/// underscore is not in its unreserved set — so the path a request really sends
/// is what the stub is told to expect.
fn encoded(id: &str) -> String {
    id.replace('_', "%5F")
}

fn operation(id: &str) -> &'static Operation {
    paddle::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the paddle declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

/// One customer entity, with the fields Paddle documents as required.
fn customer() -> JsonValue {
    json!({
        "data": {
            "id": CUSTOMER,
            "status": "active",
            "custom_data": null,
            "name": "Jo Brown",
            "email": "jo@example.com",
            "marketing_consent": false,
            "locale": "en",
            "created_at": "2024-04-11T15:57:24.813Z",
            "updated_at": "2024-04-11T15:57:24.813Z",
            "import_meta": null,
        },
        "meta": { "request_id": "9bcdcc29-e180-4055-ad3d-d37e5dc5e56d" },
    })
}

/// A page of results, with the `meta.pagination` object Paddle publishes on
/// every list response.
fn page(has_more: bool) -> JsonValue {
    json!({
        "data": [],
        "meta": {
            "request_id": "9346b365-4cad-43a6-b7c1-48ff6a1c7836",
            "pagination": {
                "per_page": 50,
                "next": "https://api.paddle.com/customers?after=ctm_01h8&per_page=50",
                "has_more": has_more,
                "estimated_total": 1,
            },
        },
    })
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("customer.list", json!({ "status": "active" })),
        ("customer.get", json!({ "customer_id": CUSTOMER })),
        (
            "customer.create",
            json!({ "email": "jo@example.com", "name": "Jo Brown", "locale": null,
                    "custom_data": null }),
        ),
        (
            "customer.update",
            json!({ "customer_id": CUSTOMER, "name": "Jo Brown-Anderson", "email": null,
                    "status": null }),
        ),
        ("subscription.list", json!({ "customer_id": CUSTOMER })),
        (
            "subscription.get",
            json!({ "subscription_id": SUBSCRIPTION }),
        ),
        ("transaction.list", json!({ "customer_id": CUSTOMER })),
        ("transaction.get", json!({ "transaction_id": TRANSACTION })),
        (
            "transaction.create",
            json!({ "items": [{ "price_id": "pri_01", "quantity": 1 }],
                    "customer_id": CUSTOMER, "currency_code": "USD",
                    "collection_mode": "automatic", "custom_data": null }),
        ),
        ("adjustment.list", json!({ "transaction_id": TRANSACTION })),
        (
            "adjustment.create",
            json!({ "action": "refund", "type": "partial", "transaction_id": TRANSACTION,
                    "reason": "error",
                    "items": [{ "item_id": "txnitm_01", "type": "partial", "amount": "2499" }] }),
        ),
    ]
}

/// `paddle_request_shape`: exact method, path, query, headers, and body for
/// every operation.
#[tokio::test]
async fn paddle_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/customers")
            .query("per_page=50&status=active")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("paddle-version", "1")
            .no_body()
            .respond_json(200, page(false)),
        Expectation::new("GET", &format!("/customers/{}", encoded(CUSTOMER)))
            .respond_json(200, customer()),
        Expectation::new("POST", "/customers")
            .header("content-type", "application/json")
            .json_body(json!({
                "email": "jo@example.com", "name": "Jo Brown",
                "locale": null, "custom_data": null,
            }))
            .respond_json(201, customer()),
        Expectation::new("PATCH", &format!("/customers/{}", encoded(CUSTOMER)))
            .json_body(json!({ "name": "Jo Brown-Anderson", "email": null, "status": null }))
            .respond_json(200, customer()),
        Expectation::new("GET", "/subscriptions")
            .query(&format!("per_page=50&customer_id={}", encoded(CUSTOMER)))
            .respond_json(200, page(false)),
        Expectation::new("GET", &format!("/subscriptions/{}", encoded(SUBSCRIPTION)))
            .respond_json(200, subscription()),
        Expectation::new("GET", "/transactions")
            .query(&format!("per_page=30&customer_id={}", encoded(CUSTOMER)))
            .respond_json(200, page(false)),
        Expectation::new("GET", &format!("/transactions/{}", encoded(TRANSACTION)))
            .respond_json(200, transaction()),
        Expectation::new("POST", "/transactions")
            .json_body(json!({
                "items": [{ "price_id": "pri_01", "quantity": 1 }],
                "customer_id": CUSTOMER, "currency_code": "USD",
                "collection_mode": "automatic", "custom_data": null,
            }))
            .respond_json(201, transaction()),
        Expectation::new("GET", "/adjustments")
            .query(&format!(
                "per_page=50&transaction_id={}",
                encoded(TRANSACTION)
            ))
            .respond_json(200, page(false)),
        Expectation::new("POST", "/adjustments")
            .json_body(json!({
                "action": "refund", "type": "partial", "transaction_id": TRANSACTION,
                "reason": "error",
                "items": [{ "item_id": "txnitm_01", "type": "partial", "amount": "2499" }],
            }))
            .respond_json(201, adjustment()),
    ])
    .await;

    for (id, input) in inputs() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

fn subscription() -> JsonValue {
    json!({
        "data": {
            "id": SUBSCRIPTION,
            "status": "active",
            "customer_id": CUSTOMER,
            "currency_code": "USD",
            "next_billed_at": "2024-05-11T15:57:24.813Z",
            "created_at": "2024-04-11T15:57:24.813Z",
            "updated_at": "2024-04-11T15:57:24.813Z",
        },
        "meta": { "request_id": "9346b365-4cad-43a6-b7c1-48ff6a1c7836" },
    })
}

/// A transaction whose money is exactly what Paddle documents: strings in the
/// lowest denomination.
fn transaction() -> JsonValue {
    json!({
        "data": {
            "id": TRANSACTION,
            "status": "completed",
            "customer_id": CUSTOMER,
            "currency_code": "JPY",
            "invoice_number": "127-10001",
            "details": { "totals": { "subtotal": "1000", "tax": "0", "grand_total": "1000" } },
            "created_at": "2024-04-11T15:57:24.813Z",
            "updated_at": "2024-04-11T15:57:24.813Z",
        },
        "meta": { "request_id": "9346b365-4cad-43a6-b7c1-48ff6a1c7836" },
    })
}

fn adjustment() -> JsonValue {
    json!({
        "data": {
            "id": "adj_01hvgf2s84dr6reszzg29zbvcm",
            "action": "refund",
            "type": "partial",
            "status": "pending_approval",
            "transaction_id": TRANSACTION,
            "customer_id": CUSTOMER,
            "reason": "error",
            "currency_code": "USD",
            "totals": { "subtotal": "2499", "tax": "0", "total": "2499" },
            "created_at": "2024-04-11T15:57:24.813Z",
        },
        "meta": { "request_id": "9346b365-4cad-43a6-b7c1-48ff6a1c7836" },
    })
}

/// `paddle_auth_is_applied`: "The API uses Bearer authentication", the key
/// reaches the wire as `Authorization: Bearer …`, and it appears nowhere else.
#[tokio::test]
async fn paddle_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/customers/{}", encoded(CUSTOMER)),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, customer())])
    .await;

    let request = render(&stub, "customer.get", json!({ "customer_id": CUSTOMER }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        paddle::connector().credential(),
        paddle::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `paddle_error_map`: every documented status reaches exactly one closed
/// class, the one machine-readable code this map reads is honoured, and none of
/// Paddle's prose or its `request_id` crosses the boundary.
#[tokio::test]
async fn paddle_error_map() {
    let documented = [
        (400, "invalid_field", ConnectorErrorClass::Validation),
        (422, "invalid_field", ConnectorErrorClass::Validation),
        (401, "invalid_token", ConnectorErrorClass::Authentication),
        (403, "forbidden", ConnectorErrorClass::Authentication),
        (404, "not_found", ConnectorErrorClass::Permanent),
        (
            409,
            "customer_already_exists",
            ConnectorErrorClass::Permanent,
        ),
        // Paddle suggests retrying this one, and the closed class set has no
        // member that says so: it is `permanent` with its status attached
        // rather than a `5xx` the provider never sent.
        (
            409,
            "concurrent_modification",
            ConnectorErrorClass::Permanent,
        ),
        (429, "too_many_requests", ConnectorErrorClass::Http429),
        (500, "internal_error", ConnectorErrorClass::Http5xx),
        (503, "service_unavailable", ConnectorErrorClass::Http5xx),
        // A status the table does not carry takes the declared fallback.
        (418, "teapot", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/customers/{}", encoded(CUSTOMER)),
        )
        .respond_header("retry-after", "9")
        .respond_json(
            status,
            json!({
                "error": {
                    "type": "request_error",
                    "code": code,
                    "detail": format!("seller 12345 key {SECRET_SENTINEL} is not permitted"),
                    "documentation_url": "https://developer.paddle.com/errors/shared/not_found",
                },
                "meta": { "request_id": "9346b365-4cad-43a6-b7c1-48ff6a1c7836" },
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "customer.get",
                json!({ "customer_id": CUSTOMER }),
            ))
            .await
            .expect("the stub answers");

        let failure = paddle::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "12345", "9346b365", "not permitted"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `paddle_rate_limit_is_classified` (spec 026 §4 proof 3): "If you send too
/// many requests, you'll get an error with a `429` response code", and "it
/// includes a `Retry-After` response header to let you know how long to wait" —
/// which reaches `http_429` with the hint clamped at the SDK ceiling.
#[tokio::test]
async fn paddle_rate_limit_is_classified() {
    let limited = json!({
        "error": {
            "type": "api_error",
            "code": "too_many_requests",
            "detail": "You have exceeded the allowed rate limit, please retry your request after \
                       the number of seconds in the Retry-After header.",
            "documentation_url": "https://developer.paddle.com/errors/shared/too_many_requests",
        },
    });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/customers/{}", encoded(CUSTOMER)))
            .respond_header("retry-after", "60")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/customers/{}", encoded(CUSTOMER)))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "customer.get",
                json!({ "customer_id": CUSTOMER }),
            ))
            .await
            .expect("the stub answers");
        failures.push(paddle::error_map().classify_response(&response));
    }
    // "When you get this error, that IP address can't make another request for
    // 60 seconds."
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(60)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `paddle_cursor_is_opaque_and_bounded` (spec 026 §4 proof 3, ADR 058): this
/// connector declares no continuation plan and no cursor binding, so one
/// attempt is exactly one request — which the stub's own request count asserts —
/// and the continuation Paddle publishes is never followed, parsed, or turned
/// into a destination.
#[tokio::test]
async fn paddle_cursor_is_opaque_and_bounded() {
    let stub = ProviderStub::start([Expectation::new("GET", "/customers")
        .query("per_page=50&status=active")
        .respond_json(
            200,
            json!({
                "data": [{ "id": CUSTOMER }],
                "meta": { "pagination": {
                    "per_page": 50,
                    // A `next` whose bytes are hostile in every direction a
                    // value can be. It is not read here at all.
                    "next": "https://attacker.invalid/customers?after=x#/../",
                    "has_more": true,
                    "estimated_total": 2,
                } },
            }),
        )])
    .await;

    let response = stub
        .send(render(
            &stub,
            "customer.list",
            json!({ "status": "active" }),
        ))
        .await
        .expect("the stub answers");
    let decoded = operation("customer.list")
        .decode_response(response.status.as_u16(), response.body())
        .expect("the page decodes");

    // `has_more` is what a Process reads; the URL is not published at all, so
    // no caller can be handed a destination this connector will not follow.
    assert_eq!(decoded["has_more"], json!(true));
    assert_eq!(decoded["estimated_total"], json!(2));
    assert!(
        decoded.get("next").is_none(),
        "a destination nothing can reach is not published: {decoded}"
    );
    assert_eq!(
        stub.received(),
        1,
        "an operation with no declared plan spends exactly one request"
    );
    stub.assert_satisfied();

    // ADR 058: the executor walks the plan its module declares, and this module
    // declares none for any operation.
    for (id, _) in inputs() {
        assert!(
            paddle::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `paddle_pagination_is_bounded`: Paddle's continuation is a URL it returns
/// even when there is no next page, so this connector spends exactly one
/// request per attempt and publishes `has_more` for the Process that decides.
#[tokio::test]
async fn paddle_pagination_is_bounded() {
    let stub = ProviderStub::start([Expectation::new("GET", "/customers")
        .query("per_page=50&status=archived")
        .respond_json(200, page(false))])
    .await;

    stub.send(render(
        &stub,
        "customer.list",
        json!({ "status": "archived" }),
    ))
    .await
    .expect("the stub answers");
    assert_eq!(
        stub.received(),
        1,
        "one attempt of an operation with no declared plan is one request"
    );
    stub.assert_satisfied();

    // The declared page size is the connector's, on every list endpoint, and it
    // is inside the maximum Paddle publishes for that endpoint.
    for (id, sizes) in [
        ("customer.list", "50"),
        ("subscription.list", "50"),
        ("transaction.list", "30"),
        ("adjustment.list", "50"),
    ] {
        let request = operation(id)
            .plan_request(
                &donat_connectors::sdk::Origin::parse("https://api.paddle.com")
                    .expect("the published origin is valid"),
                &json!({ "status": "active", "customer_id": CUSTOMER,
                         "transaction_id": TRANSACTION }),
            )
            .expect("the declared request renders");
        assert_eq!(
            request
                .url()
                .query_pairs()
                .find(|(key, _)| key == "per_page")
                .map(|(_, value)| value.into_owned()),
            Some(sizes.to_owned()),
            "{id}"
        );
    }
}

/// `paddle_effects_are_classified`: every operation carries a class, the two
/// creates are at-most-once on a machine-checkable absence, and the refund is
/// not executable at all.
#[test]
fn paddle_effects_are_classified() {
    let connector = paddle::connector();
    let expected = [
        ("customer.list", EffectClass::ReadOnly),
        ("customer.get", EffectClass::ReadOnly),
        ("customer.create", EffectClass::AtMostOnce),
        ("customer.update", EffectClass::InventoryOnly),
        ("subscription.list", EffectClass::ReadOnly),
        ("subscription.get", EffectClass::ReadOnly),
        ("transaction.list", EffectClass::ReadOnly),
        ("transaction.get", EffectClass::ReadOnly),
        ("transaction.create", EffectClass::AtMostOnce),
        ("adjustment.list", EffectClass::ReadOnly),
        ("adjustment.create", EffectClass::InventoryOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(
            operation(id).idempotency_binding().is_none(),
            "{id}: Paddle publishes no key to bind"
        );
    }

    // The refund is the operation this batch is most careful with: it is
    // refused, and the recorded reason says why an at-most-once opt-in is not
    // the answer for it either (spec 026 §3).
    assert_eq!(
        connector.admit_operation("adjustment.create"),
        Err(OperationRejection::InventoryOnly)
    );
    let refund = operation("adjustment.create")
        .effect()
        .cloned()
        .expect("classified");
    let reason = refund.inventory_reason().expect("a recorded reason");
    assert!(reason.contains("pending_approval"), "{reason}");
    assert!(refund.no_idempotency_evidence().is_none());

    let create = operation("customer.create")
        .effect()
        .cloned()
        .expect("classified");
    let evidence = create
        .no_idempotency_evidence()
        .expect("an at-most-once class carries the search that found no key");
    assert!(
        evidence
            .searched_documentation()
            .contains("does not occur anywhere"),
    );
    assert!(evidence.repeat_produces().contains("a new `ctm_` id"));
    assert_eq!(evidence.search().as_str(), "machine_readable_description");
}

/// `paddle_output_contract`: the declared pointers read the fields Paddle
/// documents as required, and a response missing one is a validation failure.
#[test]
fn paddle_output_contract() {
    let get = operation("customer.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&customer()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": CUSTOMER,
            "email": "jo@example.com",
            "name": "Jo Brown",
            "status": "active",
            "created_at": "2024-04-11T15:57:24.813Z",
            "updated_at": "2024-04-11T15:57:24.813Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"data":{"id":"ctm_1"}}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // "If successful, your response includes a copy of the new customer
    // entity" — with a `201`, and a `200` there is not the documented success.
    let create = operation("customer.create");
    assert!(create.is_success(201) && !create.is_success(200));
    // A list response always carries `meta.pagination`, so `has_more` is
    // required; `estimated_total` is not, because Paddle documents that it is
    // omitted or `-1` when the count is skipped.
    assert_eq!(
        operation("customer.list")
            .decode_response(200, &serde_json::to_vec(&page(false)).expect("serializes"))
            .expect("a last page decodes"),
        json!({ "data": [], "has_more": false, "estimated_total": 1 })
    );
}

/// `paddle_amounts_survive` (spec 026 §4 proof 4): "Monetary values are
/// returned as strings in the lowest denomination for a currency", and this
/// connector keeps them strings in both directions.
///
/// The failure this holds shut is a connector that reads `"1000"` into a number:
/// for `JPY`, where Paddle documents zero decimals, ¥1000 would become 1000.0
/// and then, on the way out, an amount Paddle never wrote.
#[tokio::test]
async fn paddle_amounts_survive() {
    // Outbound: an amount given as a JSON string reaches the wire as one.
    let stub = ProviderStub::start([Expectation::new("POST", "/adjustments")
        .json_body(json!({
            "action": "refund", "type": "partial", "transaction_id": TRANSACTION,
            "reason": "error",
            "items": [{ "item_id": "txnitm_01", "type": "partial", "amount": "2499" }],
        }))
        .respond_json(201, adjustment())])
    .await;
    let request = render(
        &stub,
        "adjustment.create",
        json!({ "action": "refund", "type": "partial", "transaction_id": TRANSACTION,
                "reason": "error",
                "items": [{ "item_id": "txnitm_01", "type": "partial", "amount": "2499" }] }),
    );
    let sent = std::str::from_utf8(request.body()).expect("the body is UTF-8");
    assert!(
        sent.contains(r#""amount":"2499""#),
        "the amount stays the string Paddle documents: {sent}"
    );
    assert!(!sent.contains("2499.0"), "{sent}");
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    // Inbound: a total is published as the exact string Paddle sent, for a
    // zero-decimal currency and for a two-decimal one.
    let decoded = operation("transaction.get")
        .decode_response(
            200,
            &serde_json::to_vec(&transaction()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(decoded["grand_total"], json!("1000"));
    assert_eq!(decoded["currency_code"], json!("JPY"));
    assert!(
        decoded["grand_total"].is_string(),
        "a total is never decoded into a number"
    );

    let refund = operation("adjustment.create")
        .decode_response(
            201,
            &serde_json::to_vec(&adjustment()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(refund["total"], json!("2499"));

    // And the declaration itself says so, so a later edit cannot quietly retype
    // an amount as a number.
    for (id, field) in [
        ("transaction.get", "grand_total"),
        ("adjustment.create", "total"),
    ] {
        let projection = operation(id).project();
        let output = projection
            .outputs()
            .iter()
            .find(|output| output.name() == field)
            .unwrap_or_else(|| panic!("{id} publishes {field}"));
        assert_eq!(
            *output.scalar(),
            donat_value_contract::ValueScalar::String,
            "{id}.{field}"
        );
    }
}
