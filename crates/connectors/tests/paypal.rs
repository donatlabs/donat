//! PayPal connector proofs (spec 026 §4), against the SDK's local provider stub.
//!
//! No test here reaches PayPal, and no test carries a real credential: the
//! minted access token and the configured client secret are both
//! [`SECRET_SENTINEL`], which doubles as the value every redaction assertion
//! looks for.

use std::time::Duration;

use donat_connectors::providers::paypal;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, Origin,
    PaginationBudget, RequestPlan, Secret,
};
use serde_json::{Value as JsonValue, json};

const ORDER: &str = "5O190127TN364715T";
const CAPTURE: &str = "2GG279541U471931P";
const INVOICE: &str = "INV2-Z56S-5LLA-Q52L-CPZ5";
const SUBSCRIPTION: &str = "I-BW452GLLEP1G";

/// The SDK percent-encodes every path value per segment, and a PayPal
/// identifier's hyphen is not in its unreserved set — so this is the path a
/// request really sends.
fn encoded(id: &str) -> String {
    id.replace('-', "%2D")
}
const ACTIVITY_KEY: &str = "activity-00000000-0000-4000-8000-000000000042-1";

fn operation(id: &str) -> &'static Operation {
    paypal::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the paypal declaration publishes {id}"))
}

/// The credential a deployment configures: PayPal's client id and secret, and
/// nothing else.
fn credential() -> Credential {
    Credential::from_fields([
        ("client_id", Secret::new("AeA1QIZXi-test-client-id")),
        ("client_secret", Secret::new(SECRET_SENTINEL)),
    ])
}

/// Render one operation the way the executor does: the request from the
/// declaration, the activity's stable key in the binding its class was admitted
/// on, and the access token the executor minted for this attempt.
fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let operation = operation(id);
    let mut request = operation
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    operation
        .apply_idempotency_key(&mut request, ACTIVITY_KEY)
        .expect("the declared binding takes the activity key");
    paypal::connector()
        .credential()
        .plan()
        .expect("PayPal declares a credential plan")
        .apply(
            &credential(),
            &mut request,
            Some(&AccessToken::new(SECRET_SENTINEL)),
        )
        .expect("the declared plan applies the minted token");
    request
}

fn order() -> JsonValue {
    json!({
        "id": ORDER,
        "intent": "CAPTURE",
        "status": "COMPLETED",
        "create_time": "2026-08-10T21:20:49Z",
        "purchase_units": [{
            "reference_id": "default",
            "amount": { "currency_code": "USD", "value": "100.00" },
            "payments": { "captures": [{
                "id": CAPTURE,
                "status": "COMPLETED",
                "amount": { "currency_code": "USD", "value": "100.00" },
            }] },
        }],
    })
}

fn capture() -> JsonValue {
    json!({
        "id": CAPTURE,
        "status": "COMPLETED",
        "amount": { "currency_code": "USD", "value": "10.99" },
        "final_capture": true,
        "create_time": "2026-08-10T21:20:49Z",
    })
}

fn refund() -> JsonValue {
    json!({
        "id": "1JU08902781691411",
        "status": "COMPLETED",
        "amount": { "currency_code": "USD", "value": "10.99" },
        "create_time": "2026-08-10T21:22:03Z",
    })
}

fn invoice() -> JsonValue {
    json!({
        "id": INVOICE,
        "status": "SENT",
        "detail": { "invoice_number": "0001", "invoice_date": "2026-08-10",
                    "currency_code": "USD" },
        "amount": { "currency_code": "USD", "value": "74.21" },
        "due_amount": { "currency_code": "USD", "value": "74.21" },
    })
}

fn invoices() -> JsonValue {
    json!({ "total_pages": 1, "total_items": 1, "items": [invoice()], "links": [] })
}

fn subscription() -> JsonValue {
    json!({
        "id": SUBSCRIPTION,
        "plan_id": "P-5ML4271244454362WXNWU5NQ",
        "status": "ACTIVE",
        "start_time": "2026-09-01T00:00:00Z",
        "quantity": "20",
    })
}

fn transactions() -> JsonValue {
    json!({
        "transaction_details": [{
            "transaction_info": {
                "transaction_id": "5TY05013RG002845M",
                "transaction_status": "S",
                "transaction_amount": { "currency_code": "USD", "value": "10.99" },
            },
        }],
        "page": 1,
        "total_items": 1,
        "total_pages": 1,
    })
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "order.create",
            json!({
                "intent": "CAPTURE",
                "purchase_units": [{
                    "amount": { "currency_code": "USD", "value": "100.00" },
                }],
            }),
        ),
        ("order.get", json!({ "id": ORDER })),
        ("order.capture", json!({ "id": ORDER })),
        ("capture.get", json!({ "id": CAPTURE })),
        (
            "refund.create",
            json!({
                "id": CAPTURE,
                "amount": { "currency_code": "USD", "value": "10.99" },
                "note_to_payer": "Defective product",
                "invoice_id": "INVOICE-123",
            }),
        ),
        ("invoice.list", json!({})),
        ("invoice.get", json!({ "invoice_id": INVOICE })),
        (
            "invoice.create",
            json!({
                "detail": { "invoice_number": "0001", "currency_code": "USD" },
                "invoicer": { "name": { "given_name": "Ada" } },
                "primary_recipients": [{ "billing_info": { "email_address": "a@example.test" } }],
                "items": [{ "name": "Consulting",
                            "unit_amount": { "currency_code": "USD", "value": "74.21" },
                            "quantity": "1" }],
            }),
        ),
        (
            "subscription.create",
            json!({
                "plan_id": "P-5ML4271244454362WXNWU5NQ",
                "quantity": "20",
                "subscriber": { "name": { "given_name": "Ada" } },
                "application_context": { "brand_name": "Donat" },
            }),
        ),
        ("subscription.get", json!({ "id": SUBSCRIPTION })),
        (
            "transaction.list",
            json!({
                "start_date": "2026-08-01T00:00:00-0700",
                "end_date": "2026-08-10T23:59:59-0700",
                "transaction_status": "S",
                "fields": "transaction_info",
            }),
        ),
    ]
}

/// The input one operation renders with, for the proofs that render one at a
/// time.
fn request_input(id: &str) -> JsonValue {
    inputs()
        .into_iter()
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, input)| input)
        .unwrap_or_else(|| panic!("{id} has a declared input"))
}

/// `paypal_request_shape`: every declared operation renders the exact method,
/// path, query, headers, and body PayPal's own description publishes.
#[tokio::test]
async fn paypal_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", "/v2/checkout/orders")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("paypal-request-id", ACTIVITY_KEY)
            .header("content-type", "application/json")
            .json_body(json!({
                "intent": "CAPTURE",
                "purchase_units": [{ "amount": { "currency_code": "USD", "value": "100.00" } }],
            }))
            .respond_json(201, order()),
        Expectation::new("GET", &format!("/v2/checkout/orders/{ORDER}"))
            .without_header("paypal-request-id")
            .no_body()
            .respond_json(200, order()),
        Expectation::new("POST", &format!("/v2/checkout/orders/{ORDER}/capture"))
            .header("paypal-request-id", ACTIVITY_KEY)
            .no_body()
            .respond_json(201, order()),
        Expectation::new("GET", &format!("/v2/payments/captures/{CAPTURE}"))
            .respond_json(200, capture()),
        Expectation::new("POST", &format!("/v2/payments/captures/{CAPTURE}/refund"))
            .without_header("paypal-request-id")
            .json_body(json!({
                "amount": { "currency_code": "USD", "value": "10.99" },
                "note_to_payer": "Defective product",
                "invoice_id": "INVOICE-123",
            }))
            .respond_json(201, refund()),
        Expectation::new("GET", "/v2/invoicing/invoices")
            .query("total_required=true")
            .respond_json(200, invoices()),
        Expectation::new(
            "GET",
            &format!("/v2/invoicing/invoices/{}", encoded(INVOICE)),
        )
        .respond_json(200, invoice()),
        Expectation::new("POST", "/v2/invoicing/invoices")
            .without_header("paypal-request-id")
            .respond_json(201, invoice()),
        Expectation::new("POST", "/v1/billing/subscriptions")
            .header("paypal-request-id", ACTIVITY_KEY)
            .json_body(json!({
                "plan_id": "P-5ML4271244454362WXNWU5NQ",
                "quantity": "20",
                "subscriber": { "name": { "given_name": "Ada" } },
                "application_context": { "brand_name": "Donat" },
            }))
            .respond_json(201, subscription()),
        Expectation::new(
            "GET",
            &format!("/v1/billing/subscriptions/{}", encoded(SUBSCRIPTION)),
        )
        .respond_json(200, subscription()),
        Expectation::new("GET", "/v1/reporting/transactions").respond_json(200, transactions()),
    ])
    .await;

    for (id, input) in inputs() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `paypal_auth_is_applied`: the client-credentials plan buys a token at
/// PayPal's own published token endpoint, and it is the *token* — never the
/// configured client secret — that reaches a provider request.
#[tokio::test]
async fn paypal_auth_is_applied() {
    let plan = paypal::connector()
        .credential()
        .plan()
        .expect("PayPal declares a credential plan");
    assert!(
        plan.issues_its_own_token(),
        "PayPal's REST API is authorized only by a client-credentials token"
    );
    assert_eq!(plan.required_fields(), ["client_id", "client_secret"]);

    // "POST /v1/oauth2/token", Basic over `CLIENT_ID:CLIENT_SECRET`, with the
    // body `grant_type=client_credentials`.
    let token_request = plan
        .token_request(&credential())
        .expect("the token request renders")
        .expect("this plan fetches a token");
    assert_eq!(token_request.method(), reqwest::Method::POST);
    assert_eq!(
        token_request.url().as_str(),
        "https://api-m.paypal.com/v1/oauth2/token"
    );
    assert_eq!(
        std::str::from_utf8(token_request.body()).expect("the token body is ASCII"),
        "grant_type=client_credentials"
    );
    let authorization = token_request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .expect("the token request authenticates the client")
        .to_owned();
    assert!(authorization.starts_with("Basic "), "{authorization}");
    assert!(
        !authorization.contains(SECRET_SENTINEL),
        "the client secret is encoded, never echoed"
    );
    assert!(!format!("{token_request:?}").contains(SECRET_SENTINEL));

    // ...and the provider request carries the minted bearer token.
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/v2/checkout/orders/{ORDER}"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, order()),
        ])
        .await;
    let request = render(&stub, "order.get", json!({ "id": ORDER }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    // The plan refuses to send without a token: there is no path in which the
    // header is merely absent.
    let mut unauthenticated = operation("order.get")
        .plan_request(&stub.origin(), &json!({ "id": ORDER }))
        .expect("the declared request renders");
    let refusal = plan
        .apply(&credential(), &mut unauthenticated, None)
        .expect_err("a client-credentials request without a token is refused");
    assert_eq!(refusal.class(), ConnectorErrorClass::Invariant);
    assert!(unauthenticated.headers().get("authorization").is_none());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        paypal::connector().credential(),
        paypal::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `paypal_error_map`: every status in PayPal's own table reaches exactly one
/// closed class, and none of its prose crosses the boundary.
#[tokio::test]
async fn paypal_error_map() {
    let body = json!({
        "name": "UNPROCESSABLE_ENTITY",
        "message": "The requested action could not be performed, do not forward this",
        "debug_id": "debug-000000000042",
        "details": [{ "issue": "INSTRUMENT_DECLINED", "description": "do not forward this" }],
    });
    for (status, expected) in [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (415, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // A status PayPal does not publish still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ] {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", &format!("/v2/checkout/orders/{ORDER}"))
                    .respond_header("paypal-debug-id", "debug-000000000042")
                    .respond_json(status, body.clone()),
            ])
            .await;
        let response = stub
            .send(render(&stub, "order.get", json!({ "id": ORDER })))
            .await
            .expect("the stub answers");
        let failure = paypal::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert!(
            !failure.diagnostic().contains("do not forward this"),
            "status {status}: {}",
            failure.diagnostic()
        );
        // PayPal's own correlation identifier survives, because it is the one
        // thing its support asks for.
        assert_eq!(
            failure.correlation_ids().get("paypal_debug_id"),
            Some(&"debug-000000000042".to_owned()),
            "status {status}"
        );
        stub.assert_satisfied();
    }
}

/// `paypal_rate_limit_is_classified`: PayPal's documented rate-limit response is
/// `http_429`, and its retry hint is clamped rather than trusted.
#[tokio::test]
async fn paypal_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v2/checkout/orders/{ORDER}"))
            .respond_header("retry-after", "17")
            .respond_json(429, json!({ "name": "RATE_LIMIT_REACHED" })),
        Expectation::new("GET", &format!("/v2/checkout/orders/{ORDER}"))
            .respond_header("retry-after", "999999")
            .respond_json(429, json!({ "name": "RATE_LIMIT_REACHED" })),
    ])
    .await;

    let response = stub
        .send(render(&stub, "order.get", json!({ "id": ORDER })))
        .await
        .expect("the stub answers");
    let failure = paypal::error_map().classify_response(&response);
    assert_eq!(failure.class(), ConnectorErrorClass::Http429);
    assert_eq!(failure.retry_after(), Some(Duration::from_secs(17)));

    let response = stub
        .send(render(&stub, "order.get", json!({ "id": ORDER })))
        .await
        .expect("the stub answers");
    let clamped = paypal::error_map()
        .classify_response(&response)
        .retry_after()
        .expect("a documented hint is read");
    assert!(
        clamped < Duration::from_secs(999_999),
        "a provider's retry hint is clamped, not trusted: {clamped:?}"
    );
    stub.assert_satisfied();
}

/// `paypal_cursor_is_opaque_and_bounded`: the declared page-number walks are
/// the executor's walks, and the number of requests the stub received is
/// asserted rather than inferred (ADR 058).
#[tokio::test]
async fn paypal_cursor_is_opaque_and_bounded() {
    let budget = PaginationBudget::new(8, 8, 1_000, 1 << 20, Duration::from_secs(5));
    let full_page = |marker: u32| {
        json!({
            "total_pages": 3,
            "total_items": 201,
            "items": (0..100)
                .map(|index| json!({ "id": format!("INV2-{marker}-{index}"), "status": "DRAFT" }))
                .collect::<Vec<_>>(),
        })
    };
    let plan = paypal::pagination("invoice.list").expect("the invoice list declares a plan");
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v2/invoicing/invoices")
            .query("total_required=true&page=1&page_size=100")
            .respond_json(200, full_page(1)),
        Expectation::new("GET", "/v2/invoicing/invoices")
            .query("total_required=true&page=2&page_size=100")
            .respond_json(200, full_page(2)),
        // A short page is the end of the collection.
        Expectation::new("GET", "/v2/invoicing/invoices")
            .query("total_required=true&page=3&page_size=100")
            .respond_json(200, json!({ "items": [invoice()] })),
    ])
    .await;

    let items = plan
        .collect(
            render(&stub, "invoice.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows the declared page numbers and stops on a short page");
    assert_eq!(items.len(), 201);
    assert_eq!(
        stub.received(),
        3,
        "the executor spends exactly the pages the plan declared — a plan that sent one request \
         would fail here (ADR 058)"
    );
    stub.assert_satisfied();

    // The page number is the walk's, never the provider's: a response that
    // claims to be on page 99 cannot restart or rewind it.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v1/reporting/transactions")
            .query(
                "start_date=2026%2D08%2D01T00%3A00%3A00%2D0700&end_date=2026%2D08%2D10T23%3A59%3A\
                 59%2D0700&transaction_status=S&fields=transaction%5Finfo&page=1&page_size=100",
            )
            .respond_json(
                200,
                json!({ "page": 99, "total_pages": 100, "total_items": 9900,
                        "transaction_details": (0..100)
                            .map(|index| json!({ "transaction_info": { "transaction_id": index } }))
                            .collect::<Vec<_>>() }),
            ),
        Expectation::new("GET", "/v1/reporting/transactions")
            .query(
                "start_date=2026%2D08%2D01T00%3A00%3A00%2D0700&end_date=2026%2D08%2D10T23%3A59%3A\
                 59%2D0700&transaction_status=S&fields=transaction%5Finfo&page=2&page_size=100",
            )
            .respond_json(200, json!({ "transaction_details": [] })),
    ])
    .await;
    paypal::pagination("transaction.list")
        .expect("the transaction list declares a plan")
        .collect(
            render(&stub, "transaction.list", request_input("transaction.list")),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk ignores the provider's own page number");
    assert_eq!(stub.received(), 2);
    stub.assert_satisfied();
}

/// `paypal_pagination_is_bounded`: the declared plan terminates under every
/// ceiling, and every single-resource operation declares no plan at all.
#[tokio::test]
async fn paypal_pagination_is_bounded() {
    let plan = paypal::pagination("invoice.list").expect("the invoice list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 150, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v2/invoicing/invoices").respond_json(
                200,
                json!({ "items": (0..100).map(|index| json!({ "id": index }))
                        .collect::<Vec<_>>() }),
            )
        }))
        .await;
        let walked = plan
            .collect(
                render(&stub, "invoice.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await;
        assert!(
            walked.is_err() || stub.received() <= 8,
            "an endless collection is stopped by the budget"
        );
        assert!(stub.received() <= 8, "received {}", stub.received());
    }

    for id in [
        "order.get",
        "order.create",
        "order.capture",
        "capture.get",
        "refund.create",
        "invoice.get",
        "invoice.create",
        "subscription.create",
        "subscription.get",
    ] {
        assert!(
            paypal::pagination(id).is_none(),
            "{id} is one request, and its declaration says so"
        );
    }
}

/// `paypal_effects_are_classified`: every operation carries a class, and each
/// class is the one PayPal's own documentation supports.
#[test]
fn paypal_effects_are_classified() {
    let expected = [
        ("order.create", EffectClass::ProviderIdempotentExplicitKey),
        ("order.get", EffectClass::ReadOnly),
        ("order.capture", EffectClass::ProviderIdempotentExplicitKey),
        ("capture.get", EffectClass::ReadOnly),
        ("refund.create", EffectClass::InventoryOnly),
        ("invoice.list", EffectClass::ReadOnly),
        ("invoice.get", EffectClass::ReadOnly),
        ("invoice.create", EffectClass::AtMostOnce),
        (
            "subscription.create",
            EffectClass::ProviderIdempotentExplicitKey,
        ),
        ("subscription.get", EffectClass::ReadOnly),
        ("transaction.list", EffectClass::ReadOnly),
    ];
    assert_eq!(
        paypal::connector().operations().len(),
        expected.len(),
        "every declared operation is classified here"
    );
    for (id, class) in expected {
        assert_eq!(
            operation(id).effect_class(),
            Some(class),
            "{id} is classified on PayPal's own evidence"
        );
    }

    // The refund is the operation spec 026 §3 asks for most care with. It is
    // `InventoryOnly` rather than at-most-once, and the module says why in a
    // reason a reviewer reads.
    let reason = operation("refund.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("the refund records why it is not executable");
    for quoted in [
        "A unique ID identifying the request header for idempotency purposes",
        "see the reference for your API",
    ] {
        assert!(reason.contains(quoted), "{reason}");
    }

    // The one at-most-once operation records both halves of ADR 063's bar: what
    // was searched, and what a second send produces.
    let evidence = operation("invoice.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the invoice create records the search that found no key");
    assert_eq!(
        evidence.search(),
        donat_connectors::sdk::AbsenceSearch::MachineReadableDescription
    );
    assert!(evidence.searched_documentation().contains("invoicing_v2"));
    assert!(evidence.repeat_produces().contains("second draft invoice"));
}

/// `paypal_idempotency_evidence_is_complete` (spec 026 §4 proof 1): for every
/// `ExplicitKey` operation the binding, scope and retention each trace to a
/// quotation, and the declared clock margin is strictly smaller than the
/// retention.
#[test]
fn paypal_idempotency_evidence_is_complete() {
    let expected_retention = [
        ("order.create", paypal::ORDERS_KEY_RETENTION),
        ("order.capture", paypal::ORDERS_KEY_RETENTION),
        ("subscription.create", paypal::SUBSCRIPTIONS_KEY_RETENTION),
    ];
    let mut keyed = 0;
    for operation in paypal::connector().operations() {
        let Some(evidence) = operation
            .effect()
            .and_then(donat_connectors::sdk::Effect::explicit_key_evidence)
        else {
            continue;
        };
        keyed += 1;
        let id = operation.id();

        // Binding: the header PayPal documents, and the SDK's own — a
        // declaration that names it does not build.
        assert_eq!(
            evidence
                .binding()
                .as_header()
                .map(reqwest::header::HeaderName::as_str),
            Some("paypal-request-id"),
            "{id}"
        );
        // Scope: "must be unique for both each request and an API call type".
        assert_eq!(
            evidence.retention().scope(),
            "the PayPal account whose client credentials made the request, separated by API call \
             type",
            "{id}"
        );
        // Retention: the one this operation's *own* reference publishes.
        let expected = expected_retention
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, retention)| *retention)
            .unwrap_or_else(|| panic!("{id} has a recorded retention"));
        assert_eq!(evidence.retention().minimum(), expected, "{id}");
        assert!(
            evidence.retention().clock_safety_margin() < evidence.retention().minimum(),
            "{id}: the margin is strictly smaller than the retention"
        );
        assert!(
            paypal::SEND_HORIZON
                <= evidence.retention().minimum() - evidence.retention().clock_safety_margin(),
            "{id}: the deployment-wide horizon fits inside this operation's window"
        );

        // All three trace to a quotation in the citation.
        let citation = evidence.citation();
        for quoted in [
            "PayPal-Request-Id contains a unique user-generated ID",
            "unique for both each request and an API call type",
            "The server stores keys for",
        ] {
            assert!(citation.contains(quoted), "{id}: {citation}");
        }

        // The class is only reachable on a mutating method, the key is the
        // SDK's to write, and it really reaches the wire.
        assert!(operation.method().mutates(), "{id}");
        let origin = Origin::parse("https://api-m.paypal.com").expect("the published origin");
        let mut request = operation
            .plan_request(&origin, &request_input(id))
            .expect("the declared request renders");
        assert!(
            request.headers().get("paypal-request-id").is_none(),
            "{id}: the key is the SDK's to write, not the declaration's"
        );
        operation
            .apply_idempotency_key(&mut request, ACTIVITY_KEY)
            .expect("the declared binding takes the activity key");
        assert_eq!(
            request
                .headers()
                .get("paypal-request-id")
                .and_then(|value| value.to_str().ok()),
            Some(ACTIVITY_KEY),
            "{id}"
        );
        // A key that could forge a second header field is refused rather than
        // truncated or escaped.
        assert!(
            operation
                .apply_idempotency_key(&mut request, "key\r\nx-injected: 1")
                .is_err(),
            "{id}"
        );

        // A replayed write answers `200` where the first answered `201`, so the
        // declaration admits both. Admitting only one would read a successful
        // deduplication as a failure.
        assert!(operation.is_success(200), "{id}");
        assert!(operation.is_success(201), "{id}");
    }
    assert_eq!(keyed, 3, "three PayPal writes carry a published window");
}

/// `paypal_output_contract`: each operation publishes exactly the declared
/// fields, and a response missing a required one is a failure rather than a
/// partial success.
#[test]
fn paypal_output_contract() {
    assert_eq!(
        operation("capture.get")
            .decode_response(
                200,
                &serde_json::to_vec(&capture()).expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied"),
        json!({
            "id": CAPTURE,
            "status": "COMPLETED",
            "amount_currency_code": "USD",
            "amount_value": "10.99",
            "create_time": "2026-08-10T21:20:49Z",
        })
    );
    assert_eq!(
        operation("subscription.get")
            .decode_response(
                200,
                &serde_json::to_vec(&subscription()).expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied"),
        json!({
            "id": SUBSCRIPTION,
            "status": "ACTIVE",
            "plan_id": "P-5ML4271244454362WXNWU5NQ",
            "start_time": "2026-09-01T00:00:00Z",
            "quantity": "20",
        })
    );
    assert_eq!(
        operation("capture.get")
            .decode_response(200, br#"{"status":"COMPLETED"}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // The single-resource reads answer `200` and nothing else; `204` is not a
    // documented success anywhere in this surface.
    for id in ["order.get", "invoice.get", "subscription.get"] {
        assert!(operation(id).is_success(200), "{id}");
        assert!(!operation(id).is_success(201), "{id}");
        assert!(!operation(id).is_success(204), "{id}");
    }
    // Invoicing's create is the one write PayPal answers `201` to and never
    // deduplicates, so it admits `201` alone.
    assert!(operation("invoice.create").is_success(201));
    assert!(!operation("invoice.create").is_success(200));
}

/// `paypal_amounts_survive` (spec 026 §4 proof 4): PayPal types `Money.value` as
/// a **string**, and this connector carries the characters it was sent — in both
/// directions.
///
/// The failures this holds shut are a connector that parses `"10.99"` into a
/// float, one that renders a caller's `"10.99"` as the JSON number `10.99`, and
/// one that reformats `"1000"` — a whole-unit JPY amount — as `1000`. All three
/// are ways to refund or charge the wrong amount.
#[tokio::test]
async fn paypal_amounts_survive() {
    // Inbound: the exact characters PayPal sent, as a string.
    let decoded = operation("capture.get")
        .decode_response(
            200,
            &serde_json::to_vec(&capture()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(decoded["amount_value"], json!("10.99"));
    assert!(
        decoded["amount_value"].is_string(),
        "a PayPal amount is a JSON string, and turning it into a number would lose the \
         provider's own representation"
    );
    assert_eq!(decoded["amount_currency_code"], json!("USD"));

    // A currency PayPal writes without a fractional part stays without one.
    let jpy = operation("capture.get")
        .decode_response(
            200,
            br#"{"id":"2GG279541U471931P","amount":{"currency_code":"JPY","value":"1000"}}"#,
        )
        .expect("the declared contract is satisfied");
    assert_eq!(jpy["amount_value"], json!("1000"));

    // A provider that started sending a *number* is a contract change this
    // connector fails on rather than absorbs.
    assert_eq!(
        operation("capture.get")
            .decode_response(
                200,
                br#"{"id":"2GG279541U471931P","amount":{"currency_code":"USD","value":10.99}}"#,
            )
            .expect_err("a numeric amount is refused where a string was documented")
            .class(),
        ConnectorErrorClass::Validation
    );

    // Outbound: a caller's amount reaches the wire as the string it supplied.
    let stub = ProviderStub::start([Expectation::new(
        "POST",
        &format!("/v2/payments/captures/{CAPTURE}/refund"),
    )
    .json_body(json!({
        "amount": { "currency_code": "USD", "value": "10.99" },
        "note_to_payer": "Defective product",
        "invoice_id": "INVOICE-123",
    }))
    .respond_json(201, refund())])
    .await;
    let request = render(&stub, "refund.create", request_input("refund.create"));
    let sent = std::str::from_utf8(request.body()).expect("the body is UTF-8");
    assert!(sent.contains(r#""value":"10.99""#), "{sent}");
    assert!(!sent.contains(r#""value":10.99"#), "{sent}");
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    // And the declaration says so, so a later edit cannot quietly retype an
    // amount as a number.
    for (id, field) in [
        ("capture.get", "amount_value"),
        ("invoice.get", "amount_value"),
        ("invoice.get", "due_amount_value"),
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
            "{id}.{field}: PayPal publishes a money value as a string"
        );
    }
}

/// The origin is PayPal's own and nothing a caller supplies can move it: the
/// token endpoint and every operation are on the one compiled host.
#[test]
fn paypal_origin_is_the_connectors_own() {
    let origin = Origin::parse("https://api-m.paypal.com").expect("the published origin");
    for (id, input) in inputs() {
        let request = operation(id)
            .plan_request(&origin, &input)
            .expect("the declared request renders");
        assert_eq!(request.url().host_str(), Some("api-m.paypal.com"), "{id}");
        assert_eq!(request.url().scheme(), "https", "{id}");
    }
    let token_request = paypal::connector()
        .credential()
        .plan()
        .expect("PayPal declares a credential plan")
        .token_request(&credential())
        .expect("the token request renders")
        .expect("this plan fetches a token");
    assert!(origin.contains(token_request.url()));
}

/// A hostile input cannot re-aim a request: a path value stays one segment and a
/// query value stays one value.
#[test]
fn paypal_input_cannot_move_a_request() {
    let origin = Origin::parse("https://api-m.paypal.com").expect("the published origin");
    let request = operation("order.get")
        .plan_request(&origin, &json!({ "id": "../../v1/oauth2/token?x=1#y" }))
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("api-m.paypal.com"));
    assert!(
        !request.url().path().contains("oauth2/token"),
        "{}",
        request.url().path()
    );
    assert_eq!(request.url().query(), None);
    assert_eq!(request.url().fragment(), None);

    let _ = AuthPlan::bearer();
}
