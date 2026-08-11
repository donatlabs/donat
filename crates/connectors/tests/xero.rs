//! Xero connector proofs (spec 026 §4), against the SDK's local provider stub.
//!
//! No test here reaches Xero, and no test carries a real credential: the stored
//! access token is [`SECRET_SENTINEL`], which doubles as the value every
//! redaction assertion looks for.

use std::time::Duration;

use donat_connectors::providers::xero;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation,
    PaginationBudget, RequestPlan,
};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value as JsonValue, json};

const TENANT: &str = "00000000-0000-0000-0000-000000000042";
const CONTACT: &str = "fe61ead1-8afc-4f0b-beda-066620227aad";
const INVOICE: &str = "220ddca8-3144-4085-9a88-2d72c5133734";
const PAYMENT: &str = "297c2dc5-cc47-4afd-8ec8-74990b8761e9";
const ACTIVITY_KEY: &str = "activity-00000000-0000-4000-8000-000000000042-1";

/// The SDK percent-encodes every path value per segment, and a GUID's hyphen is
/// not in its unreserved set — so this is the path a request really sends.
fn encoded(id: &str) -> String {
    id.replace('-', "%2D")
}

fn operation(id: &str) -> &'static Operation {
    xero::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the xero declaration publishes {id}"))
}

/// The one deploy-time header every request of one instance carries.
fn tenant_headers(tenant: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static(xero::TENANT_HEADER),
        HeaderValue::from_str(tenant).expect("a validated tenant identifier is a header value"),
    );
    headers
}

/// Render one operation the way a deployment would: the request from the
/// declaration, the organisation from configuration, the activity's stable key
/// in the binding its class was admitted on, and the credential from the
/// source-local store.
fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let operation = operation(id);
    let mut request = operation
        .plan_configured_request(&stub.origin(), &input, &tenant_headers(TENANT))
        .expect("the declared request renders");
    operation
        .apply_idempotency_key(&mut request, ACTIVITY_KEY)
        .expect("the declared binding takes the activity key");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn contacts() -> JsonValue {
    json!({
        "Id": "1a2b3c",
        "Status": "OK",
        "DateTimeUTC": "/Date(1439434356790)/",
        "Contacts": [{
            "ContactID": CONTACT,
            "Name": "Liam Gallagher",
            "EmailAddress": "liam@example.test",
            "ContactStatus": "ACTIVE",
            "UpdatedDateUTC": "/Date(1573755038314)/",
        }],
    })
}

/// One invoice, with the money fields Xero types as doubles.
fn invoices() -> JsonValue {
    json!({
        "Id": "1a2b3c",
        "Status": "OK",
        "Invoices": [{
            "InvoiceID": INVOICE,
            "InvoiceNumber": "INV-0001",
            "Type": "ACCREC",
            "Status": "AUTHORISED",
            "CurrencyCode": "NZD",
            "SubTotal": 1234.56,
            "TotalTax": 185.18,
            "Total": 1419.74,
            "AmountDue": 1419.74,
            "AmountPaid": 0.0,
            "UpdatedDateUTC": "/Date(1573755038314)/",
        }],
    })
}

fn payments() -> JsonValue {
    json!({
        "Id": "1a2b3c",
        "Status": "OK",
        "Payments": [{
            "PaymentID": PAYMENT,
            "Amount": 200.00,
            "Date": "/Date(1573689600000+0000)/",
            "Status": "AUTHORISED",
            "Reference": "Direct Debit",
        }],
    })
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    let contact_body = json!([{ "Name": "Liam Gallagher", "EmailAddress": "liam@example.test" }]);
    vec![
        (
            "contact.list",
            json!({ "where": "ContactStatus==\"ACTIVE\"" }),
        ),
        ("contact.get", json!({ "contact_id": CONTACT })),
        ("contact.create", json!({ "contacts": contact_body })),
        (
            "contact.update",
            json!({ "contact_id": CONTACT, "contacts": contact_body }),
        ),
        ("invoice.list", json!({ "where": "Status==\"AUTHORISED\"" })),
        ("invoice.get", json!({ "invoice_id": INVOICE })),
        (
            "invoice.create",
            json!({ "invoices": [{ "Type": "ACCREC", "Contact": { "ContactID": CONTACT },
                                   "LineItems": [{ "Description": "Consulting",
                                                   "Quantity": 1.0, "UnitAmount": 1234.5678 }] }] }),
        ),
        ("payment.list", json!({ "where": "Status==\"AUTHORISED\"" })),
        ("payment.get", json!({ "payment_id": PAYMENT })),
        (
            "payment.create",
            json!({ "invoice": { "InvoiceID": INVOICE }, "account": { "Code": "090" },
                    "amount": 200.00, "date": "2026-08-10", "reference": "Direct Debit" }),
        ),
    ]
}

/// `xero_request_shape`: exact method, path, query, headers, and body for every
/// operation.
#[tokio::test]
async fn xero_request_shape() {
    let contact_body = json!([{ "Name": "Liam Gallagher", "EmailAddress": "liam@example.test" }]);
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api.xro/2.0/Contacts")
            .query("where=ContactStatus%3D%3D%22ACTIVE%22")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("xero-tenant-id", TENANT)
            .header("accept", "application/json")
            .without_header("idempotency-key")
            .no_body()
            .respond_json(200, contacts()),
        Expectation::new(
            "GET",
            &format!("/api.xro/2.0/Contacts/{}", encoded(CONTACT)),
        )
        .respond_json(200, contacts()),
        Expectation::new("PUT", "/api.xro/2.0/Contacts")
            .header("idempotency-key", ACTIVITY_KEY)
            .header("content-type", "application/json")
            .json_body(json!({ "Contacts": contact_body }))
            .respond_json(200, contacts()),
        Expectation::new(
            "POST",
            &format!("/api.xro/2.0/Contacts/{}", encoded(CONTACT)),
        )
        .header("idempotency-key", ACTIVITY_KEY)
        .json_body(json!({ "Contacts": contact_body }))
        .respond_json(200, contacts()),
        Expectation::new("GET", "/api.xro/2.0/Invoices")
            .query("where=Status%3D%3D%22AUTHORISED%22&unitdp=4")
            .respond_json(200, invoices()),
        Expectation::new(
            "GET",
            &format!("/api.xro/2.0/Invoices/{}", encoded(INVOICE)),
        )
        .query("unitdp=4")
        .respond_json(200, invoices()),
        Expectation::new("PUT", "/api.xro/2.0/Invoices")
            .query("unitdp=4")
            .header("idempotency-key", ACTIVITY_KEY)
            .json_body(json!({ "Invoices": [{
                "Type": "ACCREC",
                "Contact": { "ContactID": CONTACT },
                "LineItems": [{ "Description": "Consulting", "Quantity": 1.0,
                                "UnitAmount": 1234.5678 }],
            }] }))
            .respond_json(200, invoices()),
        Expectation::new("GET", "/api.xro/2.0/Payments")
            .query("where=Status%3D%3D%22AUTHORISED%22")
            .respond_json(200, payments()),
        Expectation::new(
            "GET",
            &format!("/api.xro/2.0/Payments/{}", encoded(PAYMENT)),
        )
        .respond_json(200, payments()),
        Expectation::new("POST", "/api.xro/2.0/Payments")
            .header("idempotency-key", ACTIVITY_KEY)
            .json_body(json!({
                "Invoice": { "InvoiceID": INVOICE },
                "Account": { "Code": "090" },
                "Amount": 200.0,
                "Date": "2026-08-10",
                "Reference": "Direct Debit",
            }))
            .respond_json(200, payments()),
    ])
    .await;

    for (id, input) in inputs() {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `xero_auth_is_applied`: the stored OAuth2 access token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else — not in the URL, not in
/// a `Debug`, not in a classified failure.
#[tokio::test]
async fn xero_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api.xro/2.0/Contacts/{}", encoded(CONTACT)),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, contacts())])
    .await;

    let request = render(&stub, "contact.get", json!({ "contact_id": CONTACT }));
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
        xero::connector().credential(),
        xero::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    // The declaration configures no secret of its own: the credential is the
    // deployment's stored grant, and this connector holds none.
    assert!(xero::connector().credential().fields().is_empty());
    stub.assert_satisfied();
}

/// `xero_error_map`: every status in Xero's own "Codes summary" reaches exactly
/// one closed class, and none of its prose crosses the boundary.
#[tokio::test]
async fn xero_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (412, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (501, ConnectorErrorClass::Permanent),
        (503, ConnectorErrorClass::Http5xx),
        // A status the table does not carry takes the declared fallback.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api.xro/2.0/Contacts/{}", encoded(CONTACT)),
        )
        .respond_header("retry-after", "13")
        .respond_json(
            status,
            json!({
                "ErrorNumber": 10,
                "Type": "ValidationException",
                "Message": format!("tenant {TENANT} token {SECRET_SENTINEL} shard db-7"),
                "Elements": [{ "ValidationErrors": [{ "Message": "Email address must be valid" }] }],
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "contact.get",
                json!({ "contact_id": CONTACT }),
            ))
            .await
            .expect("the stub answers");

        let failure = xero::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, TENANT, "db-7", "ValidationException"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `xero_rate_limit_is_classified` (spec 026 §4 proof 3): "Exceeding a rate
/// limit will result in an HTTP 429 (too many requests) response … you will also
/// receive a Retry-After http header that tells you how many seconds to wait
/// before making another request", clamped at the SDK ceiling.
#[tokio::test]
async fn xero_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new(
            "GET",
            &format!("/api.xro/2.0/Contacts/{}", encoded(CONTACT)),
        )
        .respond_header("retry-after", "30")
        .respond_header("x-rate-limit-problem", "minute")
        .respond_bytes(429, "oauth_problem=rate limit exceeded"),
        Expectation::new(
            "GET",
            &format!("/api.xro/2.0/Contacts/{}", encoded(CONTACT)),
        )
        .respond_header("retry-after", "604800")
        .respond_bytes(429, "oauth_problem=rate limit exceeded"),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "contact.get",
                json!({ "contact_id": CONTACT }),
            ))
            .await
            .expect("the stub answers");
        failures.push(xero::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(30)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `xero_cursor_is_opaque_and_bounded` (spec 026 §4 proof 3, ADR 058): Xero's
/// continuation is a page *number*, so there is no provider value to echo — the
/// walk derives the next page from itself and stops on a short page — and the
/// stub's own request count is what proves the executor's walk is the declared
/// one.
#[tokio::test]
async fn xero_cursor_is_opaque_and_bounded() {
    let full_page = |page: u32| {
        let contacts = (0..100)
            .map(|index| json!({ "ContactID": CONTACT, "Name": format!("p{page}-{index}") }))
            .collect::<Vec<_>>();
        json!({ "Id": "1a", "Status": "OK", "pagination": { "page": page, "pageSize": 100,
                "pageCount": 3, "itemCount": 250 }, "Contacts": contacts })
    };

    let plan = xero::pagination("contact.list").expect("the contact list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 1 << 20, Duration::from_secs(5));
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api.xro/2.0/Contacts")
            .query("where=&page=1&pageSize=100")
            .respond_json(200, full_page(1)),
        Expectation::new("GET", "/api.xro/2.0/Contacts")
            .query("where=&page=2&pageSize=100")
            .respond_json(200, full_page(2)),
        // A short page is the documented end of the collection.
        Expectation::new("GET", "/api.xro/2.0/Contacts")
            .query("where=&page=3&pageSize=100")
            .respond_json(
                200,
                json!({ "Id": "1a", "Status": "OK", "Contacts": [{ "ContactID": CONTACT }] }),
            ),
    ])
    .await;

    let items = plan
        .collect(
            render(&stub, "contact.list", json!({ "where": "" })),
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

    // The page number comes from the walk, never from the provider: a response
    // that claims to be on page 99 cannot restart or rewind it.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api.xro/2.0/Contacts")
            .query("where=&page=1&pageSize=100")
            .respond_json(
                200,
                json!({ "Id": "1a", "Status": "OK",
                        "pagination": { "page": 99, "pageSize": 100, "pageCount": 100,
                                        "itemCount": 9900 },
                        "Contacts": (0..100).map(|index| json!({ "ContactID": index }))
                            .collect::<Vec<_>>() }),
            ),
        Expectation::new("GET", "/api.xro/2.0/Contacts")
            .query("where=&page=2&pageSize=100")
            .respond_json(200, json!({ "Id": "1a", "Status": "OK", "Contacts": [] })),
    ])
    .await;
    plan.collect(
        render(&stub, "contact.list", json!({ "where": "" })),
        &stub.origin(),
        &budget,
        undeclared_status_gate,
        |request| stub.send(request),
    )
    .await
    .expect("the walk ignores the provider's own page number");
    stub.assert_satisfied();
}

/// `xero_pagination_is_bounded`: the declared plan terminates and respects the
/// call, page, item, and byte budgets, and the single-resource reads declare no
/// plan at all.
#[tokio::test]
async fn xero_pagination_is_bounded() {
    let plan = xero::pagination("invoice.list").expect("the invoice list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 150, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api.xro/2.0/Invoices").respond_json(
                200,
                json!({ "Id": "1a", "Status": "OK",
                        "Invoices": (0..100).map(|index| json!({ "InvoiceID": index }))
                            .collect::<Vec<_>>() }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "invoice.list", json!({ "where": "" })),
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
        "contact.get",
        "contact.create",
        "contact.update",
        "invoice.get",
        "invoice.create",
        "payment.get",
        "payment.create",
    ] {
        assert!(
            xero::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `xero_effects_are_classified`: every operation carries a class, and every
/// mutation is `ExplicitKey` on the key Xero publishes rather than on its
/// method.
#[test]
fn xero_effects_are_classified() {
    let connector = xero::connector();
    let expected = [
        ("contact.list", EffectClass::ReadOnly),
        ("contact.get", EffectClass::ReadOnly),
        ("contact.create", EffectClass::ProviderIdempotentExplicitKey),
        ("contact.update", EffectClass::ProviderIdempotentExplicitKey),
        ("invoice.list", EffectClass::ReadOnly),
        ("invoice.get", EffectClass::ReadOnly),
        ("invoice.create", EffectClass::ProviderIdempotentExplicitKey),
        ("payment.list", EffectClass::ReadOnly),
        ("payment.get", EffectClass::ReadOnly),
        ("payment.create", EffectClass::ProviderIdempotentExplicitKey),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert!(connector.admit_operation(id).is_ok(), "{id}");
        assert_eq!(
            operation(id).idempotency_binding().is_some(),
            class == EffectClass::ProviderIdempotentExplicitKey,
            "{id}"
        );
        // No operation here is at-most-once: Xero publishes a mechanism, so the
        // class admitted on an *absence* is not available for any of them.
        assert!(
            operation(id)
                .effect()
                .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
                .is_none(),
            "{id}"
        );
    }
}

/// `xero_idempotency_evidence_is_complete` (spec 026 §4 proof 1): for every
/// `ExplicitKey` operation the binding, the scope, and the retention each trace
/// to a quotation, and the declared clock margin is strictly smaller than the
/// retention.
#[test]
fn xero_idempotency_evidence_is_complete() {
    let mut keyed = 0;
    for operation in xero::connector().operations() {
        let Some(effect) = operation.effect() else {
            panic!("{} carries no class", operation.id());
        };
        let Some(evidence) = effect.explicit_key_evidence() else {
            continue;
        };
        keyed += 1;
        let id = operation.id();

        // Binding: the header Xero documents, and the SDK's own — a
        // declaration that names it does not build.
        assert_eq!(
            evidence
                .binding()
                .as_header()
                .map(reqwest::header::HeaderName::as_str),
            Some("idempotency-key"),
            "{id}"
        );
        // Scope: "Key re-use is procesed per app."
        assert_eq!(
            evidence.retention().scope(),
            "the Xero app whose OAuth2 client made the request",
            "{id}"
        );
        // Retention: "keys are stored for 6 minutes from the time of the first
        // call, after which they expire."
        assert_eq!(
            evidence.retention().minimum(),
            Duration::from_secs(360),
            "{id}"
        );
        assert_eq!(
            evidence.retention().clock_safety_margin(),
            Duration::from_secs(60),
            "{id}"
        );
        assert!(
            evidence.retention().clock_safety_margin() < evidence.retention().minimum(),
            "{id}"
        );

        // Every one of the three traces to a quotation in the citation, and the
        // citation is what a reviewer checks.
        let citation = evidence.citation();
        for quoted in [
            "‘Idempotency-Key’ header",
            "Key re-use is procesed per app",
            "keys are stored for 6 minutes",
        ] {
            assert!(citation.contains(quoted), "{id}: {citation}");
        }

        // The class is only reachable on a mutating method, and the key really
        // reaches the wire.
        assert!(operation.method().mutates(), "{id}");
        let mut request = operation
            .plan_configured_request(
                &donat_connectors::sdk::Origin::parse("https://api.xero.com")
                    .expect("the published origin is valid"),
                &request_input(id),
                &tenant_headers(TENANT),
            )
            .expect("the declared request renders");
        assert!(
            request.headers().get("idempotency-key").is_none(),
            "{id}: the key is the SDK's to write, not the declaration's"
        );
        operation
            .apply_idempotency_key(&mut request, ACTIVITY_KEY)
            .expect("the declared binding takes the activity key");
        assert_eq!(
            request
                .headers()
                .get("idempotency-key")
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
    }
    assert_eq!(keyed, 4, "every Xero mutation carries the published key");
}

/// The input each operation renders with, for the proofs that render one
/// operation at a time.
fn request_input(id: &str) -> JsonValue {
    inputs()
        .into_iter()
        .find(|(candidate, _)| *candidate == id)
        .map(|(_, input)| input)
        .unwrap_or_else(|| panic!("{id} has a declared input"))
}

/// `xero_tenant_comes_only_from_deploy_time_configuration`: the organisation
/// every write lands in is deploy-time material, and neither operation input nor
/// a provider response can move it.
#[test]
fn xero_tenant_comes_only_from_deploy_time_configuration() {
    let origin = donat_connectors::sdk::Origin::parse("https://api.xero.com")
        .expect("the published origin is valid");
    let hostile = json!({
        "contact_id": CONTACT,
        // Every shape an input could take to reach the header.
        "tenant_id": "11111111-1111-1111-1111-111111111111",
        "xero-tenant-id": "11111111-1111-1111-1111-111111111111",
        "headers": { "xero-tenant-id": "11111111-1111-1111-1111-111111111111" },
    });
    let request = operation("contact.get")
        .plan_configured_request(&origin, &hostile, &tenant_headers(TENANT))
        .expect("the declared request renders");
    assert_eq!(
        request
            .headers()
            .get(xero::TENANT_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(TENANT),
        "the configured organisation is the one on the wire"
    );
    assert_eq!(
        request
            .headers()
            .get_all(xero::TENANT_HEADER)
            .iter()
            .count(),
        1
    );
    assert!(!request.url().as_str().contains("11111111"));

    // No operation declares the header itself, so a later edit cannot make the
    // declaration and the configuration disagree about whose books are written.
    for operation in xero::connector().operations() {
        let projection = operation.project();
        assert!(
            !projection
                .headers()
                .iter()
                .any(|header| header.name().eq_ignore_ascii_case(xero::TENANT_HEADER)),
            "{} declares the tenant header",
            operation.id()
        );
        assert!(
            !projection
                .inputs()
                .iter()
                .any(|input| input.name().contains("tenant")),
            "{} publishes the tenant as a Process input",
            operation.id()
        );
    }
}

/// `xero_output_contract`: the declared pointers read the fields Xero
/// documents, and a response missing a required one is a validation failure.
#[test]
fn xero_output_contract() {
    let get = operation("contact.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&contacts()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "contact_id": CONTACT,
            "name": "Liam Gallagher",
            "email_address": "liam@example.test",
            "contact_status": "ACTIVE",
            "updated_date_utc": "/Date(1573755038314)/",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"Contacts":[{"ContactID":"x"}]}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // Xero answers `200` for a create as well as for a read, and nothing else
    // is a documented success.
    for id in ["contact.create", "invoice.create", "payment.create"] {
        let operation = operation(id);
        assert!(operation.is_success(200), "{id}");
        assert!(!operation.is_success(201), "{id}");
        assert!(!operation.is_success(204), "{id}");
    }
}

/// `xero_amounts_survive` (spec 026 §4 proof 4): Xero types every money field
/// as `"number"` with `"format": "double"` and its own `x-is-money` marker, and
/// this connector carries the digits it was sent — in both directions.
///
/// The failures this holds shut are a connector that rounds an amount to two
/// decimal places, one that stringifies it, and one that truncates it to an
/// integer. All three are ways to pay the wrong amount.
#[tokio::test]
async fn xero_amounts_survive() {
    // Inbound: the exact digits Xero sent, for a two-decimal total and a
    // four-decimal unit amount.
    let decoded = operation("invoice.get")
        .decode_response(
            200,
            &serde_json::to_vec(&invoices()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(decoded["total"], json!(1419.74));
    assert_eq!(decoded["amount_due"], json!(1419.74));
    assert_eq!(decoded["amount_paid"], json!(0.0));
    assert_eq!(
        serde_json::to_string(&decoded["total"]).expect("a decoded amount serializes"),
        "1419.74",
        "an amount is published with the digits Xero sent"
    );
    assert!(
        decoded["total"].is_number() && !decoded["total"].is_string(),
        "a Xero amount is a JSON number, and turning it into a string would change its type on \
         the way into a Process"
    );

    let payment = operation("payment.get")
        .decode_response(
            200,
            &serde_json::to_vec(&payments()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(payment["amount"], json!(200.0));
    assert_eq!(payment["amount"].as_f64(), Some(200.0));

    // Outbound: an amount reaches the wire as the number a caller supplied,
    // never reformatted, and the request asks Xero for four decimal places
    // wherever Xero offers them.
    let stub = ProviderStub::start([Expectation::new("PUT", "/api.xro/2.0/Invoices")
        .query("unitdp=4")
        .json_body(json!({ "Invoices": [{
            "Type": "ACCREC",
            "Contact": { "ContactID": CONTACT },
            "LineItems": [{ "Description": "Consulting", "Quantity": 1.0,
                            "UnitAmount": 1234.5678 }],
        }] }))
        .respond_json(200, invoices())])
    .await;
    let request = render(&stub, "invoice.create", request_input("invoice.create"));
    let sent = std::str::from_utf8(request.body()).expect("the body is UTF-8");
    assert!(
        sent.contains("1234.5678"),
        "a four-decimal unit amount is sent whole: {sent}"
    );
    assert!(!sent.contains("1234.57"), "{sent}");
    assert!(!sent.contains("\"1234.5678\""), "{sent}");
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    // And the declaration says so, so a later edit cannot quietly retype an
    // amount as a string or an integer.
    for (id, field) in [
        ("invoice.get", "total"),
        ("invoice.get", "amount_due"),
        ("payment.get", "amount"),
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
            "{id}.{field}: a Xero amount is a JSON number, and `Json` is the one scalar that \
             carries one unchanged"
        );
    }

    // And the reason it is not `Decimal`: this workspace's `Decimal` is a
    // decimal *string*, so a declaration that used it would refuse Xero's own
    // documented response rather than protect it.
    let decimal_typed = donat_connectors::sdk::Operation::get("probe.get", "/api.xro/2.0/Invoices")
        .version("1.0.0")
        .success_statuses([reqwest::StatusCode::OK])
        .output_pointer(
            "total",
            "/Invoices/0/Total",
            donat_value_contract::ValueScalar::Decimal,
            donat_connectors::sdk::Required::Yes,
        )
        .effect(donat_connectors::sdk::Effect::read_only())
        .build()
        .expect("the probe declaration is valid");
    assert_eq!(
        decimal_typed
            .decode_response(
                200,
                &serde_json::to_vec(&invoices()).expect("a fixture serializes"),
            )
            .expect_err("a decimal-typed money field refuses a JSON number")
            .class(),
        ConnectorErrorClass::Validation
    );
}
