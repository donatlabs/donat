//! Mercado Pago connector proofs (spec 026 §4), against the SDK's local
//! provider stub.

use std::time::Duration;

use donat_connectors::providers::mercado_pago;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const PAYMENT: &str = "1316588293";
const CUSTOMER: &str = "649457098-FybpOkG6zH8QRm";

fn operation(id: &str) -> &'static Operation {
    mercado_pago::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the mercado_pago declaration publishes {id}"))
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

/// A payment as the *Get payment* reference documents it — with money as the
/// **number** the field list types it.
fn payment() -> JsonValue {
    json!({
        "id": 1_316_588_293_i64,
        "status": "approved",
        "status_detail": "accredited",
        "transaction_amount": 24.50,
        "transaction_amount_refunded": 0,
        "currency_id": "BRL",
        "external_reference": "MP0001",
        "date_created": "2026-08-10T09:37:52.000-04:00",
    })
}

/// The same payment as the reference's own response *example* prints it — with
/// money as a **string**.
fn payment_with_string_amount() -> JsonValue {
    json!({
        "id": 1_316_588_293_i64,
        "status": "approved",
        "status_detail": "accredited",
        "transaction_amount": "24.50",
        "transaction_amount_refunded": 0,
        "currency_id": "BRL",
        "external_reference": "MP0001",
        "date_created": "2026-08-10T09:37:52.000-04:00",
    })
}

fn refund() -> JsonValue {
    json!({
        "id": 1_234_567_890_i64,
        "payment_id": 1_316_588_293_i64,
        "amount": 24.50,
        "status": "approved",
        "date_created": "2026-08-10T09:40:00.000-04:00",
        "unique_sequence_number": "1234567890",
    })
}

fn customer() -> JsonValue {
    json!({
        "id": CUSTOMER,
        "email": "jo@example.test",
        "first_name": "Jo",
        "last_name": "Brown",
        "date_created": "2026-08-10T09:00:00.000-04:00",
    })
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("payment.get", json!({ "payment_id": PAYMENT })),
        ("refund.list", json!({ "payment_id": PAYMENT })),
        (
            "refund.create",
            json!({ "payment_id": PAYMENT, "amount": 24.50 }),
        ),
        ("customer.get", json!({ "customer_id": CUSTOMER })),
        ("customer.search", json!({ "email": "jo@example.test" })),
        (
            "customer.create",
            json!({ "email": "jo@example.test", "first_name": "Jo", "last_name": "Brown",
                    "identification": { "type": "CPF", "number": "19119119100" } }),
        ),
        (
            "customer.update",
            json!({ "customer_id": CUSTOMER, "email": "jo@example.test", "first_name": "Jo",
                    "last_name": "Brown-Anderson" }),
        ),
    ]
}

/// `mercado_pago_request_shape`: exact method, path, query, headers, and body
/// for every operation.
#[tokio::test]
async fn mercado_pago_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v1/payments/{PAYMENT}"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, payment()),
        Expectation::new("GET", &format!("/v1/payments/{PAYMENT}/refunds"))
            .respond_json(200, json!([refund()])),
        Expectation::new("POST", &format!("/v1/payments/{PAYMENT}/refunds"))
            .header("content-type", "application/json")
            .json_body(json!({ "amount": 24.50 }))
            .respond_json(201, refund()),
        Expectation::new("GET", &format!("/v1/customers/{}", encoded(CUSTOMER)))
            .respond_json(200, customer()),
        Expectation::new("GET", "/v1/customers/search")
            .query("email=jo%40example%2Etest")
            .respond_json(
                200,
                json!({ "paging": { "total": 1, "limit": 10, "offset": 0 },
                                       "results": [customer()] }),
            ),
        Expectation::new("POST", "/v1/customers")
            .json_body(json!({
                "email": "jo@example.test", "first_name": "Jo", "last_name": "Brown",
                "identification": { "type": "CPF", "number": "19119119100" },
            }))
            .respond_json(201, customer()),
        Expectation::new("PUT", &format!("/v1/customers/{}", encoded(CUSTOMER)))
            .json_body(json!({
                "email": "jo@example.test", "first_name": "Jo", "last_name": "Brown-Anderson",
            }))
            .respond_json(200, customer()),
    ])
    .await;

    for (id, input) in inputs() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// The SDK percent-encodes every path value per segment, and a Mercado Pago
/// customer id carries a hyphen — so this is the path a request really sends.
fn encoded(id: &str) -> String {
    id.replace('-', "%2D")
}

/// `mercado_pago_auth_is_applied`: "use the **Access Token** (private
/// credential) in `Authorization: Bearer <ACCESS_TOKEN>`", and it appears
/// nowhere else.
#[tokio::test]
async fn mercado_pago_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/v1/payments/{PAYMENT}"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, payment())])
    .await;

    let request = render(&stub, "payment.get", json!({ "payment_id": PAYMENT }));
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
        mercado_pago::connector().credential(),
        mercado_pago::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `mercado_pago_error_map`: every documented status reaches exactly one closed
/// class, and none of the provider's prose or numeric cause crosses the
/// boundary.
#[tokio::test]
async fn mercado_pago_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // A status the table does not carry takes the declared fallback.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/v1/payments/{PAYMENT}"),
        )
        .respond_header("retry-after", "11")
        .respond_json(
            status,
            json!({
                "message": format!("collector 649457098 token {SECRET_SENTINEL}"),
                "error": "bad_request",
                "status": status,
                "cause": [{ "code": 2001, "description": "Already posted the same request in the \
                                                          last minute." }],
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "payment.get",
                json!({ "payment_id": PAYMENT }),
            ))
            .await
            .expect("the stub answers");

        let failure = mercado_pago::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "649457098", "Already posted"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `mercado_pago_rate_limit_is_classified` (spec 026 §4 proof 3): a `429`
/// reaches `http_429` and its retry hint is clamped at the SDK ceiling.
#[tokio::test]
async fn mercado_pago_rate_limit_is_classified() {
    let limited = json!({ "message": "Too many requests", "error": "too_many_requests",
                          "status": 429, "cause": [] });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v1/payments/{PAYMENT}"))
            .respond_header("retry-after", "5")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/v1/payments/{PAYMENT}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "payment.get",
                json!({ "payment_id": PAYMENT }),
            ))
            .await
            .expect("the stub answers");
        failures.push(mercado_pago::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(5)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `mercado_pago_cursor_is_opaque_and_bounded` (spec 026 §4 proof 3, ADR 058):
/// the one collection whose envelope Mercado Pago publishes has `paging` in the
/// response and no offset or limit in the request, so there is no cursor to
/// spend and one attempt is exactly one request — asserted on the stub's own
/// request count.
#[tokio::test]
async fn mercado_pago_cursor_is_opaque_and_bounded() {
    let stub = ProviderStub::start([Expectation::new("GET", "/v1/customers/search")
        .query("email=jo%40example%2Etest")
        .respond_json(
            200,
            // A response that claims there are 5,000 more is still one request:
            // nothing in the declaration can ask for the next page.
            json!({ "paging": { "total": 5000, "limit": 10, "offset": 0 },
                    "results": [customer()] }),
        )])
    .await;

    let response = stub
        .send(render(
            &stub,
            "customer.search",
            json!({ "email": "jo@example.test" }),
        ))
        .await
        .expect("the stub answers");
    let decoded = operation("customer.search")
        .decode_response(response.status.as_u16(), response.body())
        .expect("the page decodes");
    assert_eq!(decoded["total"], json!(5000));
    assert_eq!(decoded["offset"], json!(0));
    assert_eq!(
        stub.received(),
        1,
        "an operation with no declared plan spends exactly one request"
    );
    stub.assert_satisfied();

    for (id, _) in inputs() {
        assert!(
            mercado_pago::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `mercado_pago_pagination_is_bounded`: no operation declares a plan, so there
/// is no loop to bound — and the query a search sends is the declared one,
/// whatever a caller puts in it.
#[tokio::test]
async fn mercado_pago_pagination_is_bounded() {
    let hostile = "jo@example.test&limit=9999&offset=1#/../";
    let stub = ProviderStub::start([Expectation::new("GET", "/v1/customers/search")
        .query("email=jo%40example%2Etest%26limit%3D9999%26offset%3D1%23%2F%2E%2E%2F")
        .respond_json(200, json!({ "paging": { "total": 0 }, "results": [] }))])
    .await;

    let request = render(&stub, "customer.search", json!({ "email": hostile }));
    assert_eq!(
        request.url().query_pairs().count(),
        1,
        "a hostile filter cannot add a query parameter of its own"
    );
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `mercado_pago_effects_are_classified`: every operation carries a class, the
/// refund is not executable, and the reason recorded on it is the near-miss
/// rather than an absence.
#[test]
fn mercado_pago_effects_are_classified() {
    let connector = mercado_pago::connector();
    let expected = [
        ("payment.get", EffectClass::ReadOnly),
        ("refund.list", EffectClass::ReadOnly),
        ("refund.create", EffectClass::InventoryOnly),
        ("customer.get", EffectClass::ReadOnly),
        ("customer.search", EffectClass::ReadOnly),
        ("customer.create", EffectClass::AtMostOnce),
        ("customer.update", EffectClass::InventoryOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        // Nothing here binds a key: the one Mercado Pago publishes has no
        // documented retention, so no operation reaches `ExplicitKey`.
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
        assert_ne!(
            operation(id).effect_class(),
            Some(EffectClass::ProviderIdempotentExplicitKey),
            "{id}"
        );
    }

    assert_eq!(
        connector.admit_operation("refund.create"),
        Err(OperationRejection::InventoryOnly)
    );
}

/// `mercado_pago_idempotency_evidence_is_complete` (spec 026 §4 proof 1), in the
/// only form this connector can satisfy it: **no** operation claims
/// `ExplicitKey`, and the refund records the near-miss — the binding Mercado
/// Pago publishes, and the retention it does not — rather than an absence it
/// does not have.
#[test]
fn mercado_pago_idempotency_evidence_is_complete() {
    for operation in mercado_pago::connector().operations() {
        assert!(
            operation
                .effect()
                .and_then(donat_connectors::sdk::Effect::explicit_key_evidence)
                .is_none(),
            "{} claims a class Mercado Pago does not publish the retention for",
            operation.id()
        );
    }

    let refund = operation("refund.create")
        .effect()
        .cloned()
        .expect("classified");
    let reason = refund
        .inventory_reason()
        .expect("a near-miss records what was missing");
    // The binding is quoted, because it exists…
    assert!(reason.contains("X-Idempotency-Key"), "{reason}");
    assert!(reason.contains("(string, required)"), "{reason}");
    // …and so is the thing that is not there, which is why the class is refused.
    assert!(reason.contains("no retention"), "{reason}");
    // The at-most-once class is not available for it, and the module says so.
    assert!(
        refund.no_idempotency_evidence().is_none(),
        "a provider that publishes a mechanism has no absence to admit"
    );
    assert!(reason.contains("evidence of an absence"), "{reason}");

    // The customer create is the operation that *does* stand on an absence, and
    // it carries both halves ADR 063 requires.
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
            .contains("complete request contract"),
        "{}",
        evidence.searched_documentation()
    );
    assert!(evidence.repeat_produces().contains("a second customer"));
    assert_eq!(evidence.search().as_str(), "published_contract");
}

/// `mercado_pago_output_contract`: the declared pointers read the fields the
/// references publish, and a response missing a required one is a validation
/// failure.
#[test]
fn mercado_pago_output_contract() {
    assert_eq!(
        operation("payment.get")
            .decode_response(
                200,
                &serde_json::to_vec(&payment()).expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied"),
        json!({
            "id": 1_316_588_293_i64,
            "status": "approved",
            "status_detail": "accredited",
            "transaction_amount": 24.50,
            "transaction_amount_refunded": 0,
            "currency_id": "BRL",
            "external_reference": "MP0001",
            "date_created": "2026-08-10T09:37:52.000-04:00",
        })
    );
    assert_eq!(
        operation("payment.get")
            .decode_response(200, br#"{"status":"approved"}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // "Get all Refunds for a specific payment" — the documented response is a
    // bare JSON array, so the whole document is the output.
    assert_eq!(
        operation("refund.list")
            .decode_response(
                200,
                &serde_json::to_vec(&json!([refund()])).expect("a fixture serializes"),
            )
            .expect("a bare array is the documented response"),
        json!([refund()])
    );
    // Mercado Pago answers a create with either documented success status.
    for id in ["refund.create", "customer.create"] {
        assert!(
            operation(id).is_success(200) && operation(id).is_success(201),
            "{id}"
        );
        assert!(!operation(id).is_success(202), "{id}");
    }
}

/// `mercado_pago_amounts_survive` (spec 026 §4 proof 4): Mercado Pago types
/// `transaction_amount` as a number and prints it as a string in its own
/// example, so this connector carries whichever arrived, unchanged — and sends
/// back exactly what a caller supplied.
///
/// The failure this holds shut is a connector that coerces one form into the
/// other: `"24.50"` parsed into `24.5` is a different JSON document, and `24.5`
/// rendered as `"24.5"` is an amount Mercado Pago never wrote.
#[tokio::test]
async fn mercado_pago_amounts_survive() {
    let numeric = operation("payment.get")
        .decode_response(
            200,
            &serde_json::to_vec(&payment()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(numeric["transaction_amount"], json!(24.50));
    assert!(numeric["transaction_amount"].is_number());
    assert_eq!(
        serde_json::to_string(&numeric["transaction_amount"]).expect("serializes"),
        "24.5",
        "a JSON number carries its value, and trailing zeros are not part of one"
    );

    let stringly = operation("payment.get")
        .decode_response(
            200,
            &serde_json::to_vec(&payment_with_string_amount()).expect("a fixture serializes"),
        )
        .expect("the reference's own example is also a documented response");
    assert_eq!(stringly["transaction_amount"], json!("24.50"));
    assert!(
        stringly["transaction_amount"].is_string(),
        "the string form keeps its trailing zero because it was never parsed"
    );

    // Outbound: a partial refund amount reaches the wire as the JSON value the
    // caller supplied — "If the amount field is filled, it will create a
    // partial refund" — and is never re-formatted.
    let stub = ProviderStub::start([
        Expectation::new("POST", &format!("/v1/payments/{PAYMENT}/refunds"))
            .json_body(json!({ "amount": 24.50 }))
            .respond_json(201, refund()),
        // The full refund is the same operation with a null amount, which is
        // the documented way to ask for one.
        Expectation::new("POST", &format!("/v1/payments/{PAYMENT}/refunds"))
            .json_body(json!({ "amount": null }))
            .respond_json(201, refund()),
    ])
    .await;
    let partial = render(
        &stub,
        "refund.create",
        json!({ "payment_id": PAYMENT, "amount": 24.50 }),
    );
    assert_eq!(
        std::str::from_utf8(partial.body()).expect("the body is UTF-8"),
        r#"{"amount":24.5}"#
    );
    stub.send(partial).await.expect("the stub answers");
    let full = render(
        &stub,
        "refund.create",
        json!({ "payment_id": PAYMENT, "amount": null }),
    );
    stub.send(full).await.expect("the stub answers");
    stub.assert_satisfied();

    // And the declaration says so: a money field is `Json`, because Mercado
    // Pago sends both forms and neither may be coerced into the other.
    for (id, field) in [
        ("payment.get", "transaction_amount"),
        ("refund.create", "amount"),
    ] {
        let projection = operation(id).project();
        let output = projection
            .outputs()
            .iter()
            .find(|output| output.name() == field)
            .unwrap_or_else(|| panic!("{id} publishes {field}"));
        assert_eq!(
            *output.scalar(),
            donat_value_contract::ValueScalar::Json,
            "{id}.{field}"
        );
    }
}
