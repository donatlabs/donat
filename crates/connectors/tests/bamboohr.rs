//! BambooHR connector proofs (spec 028 §4, which adopts spec 023 §4), against
//! the SDK's local provider stub.
//!
//! BambooHR is spec 028 §3's second case: a provider whose *only* published
//! credential wire form puts the secret in the HTTP Basic username with a fixed
//! password beside it. The proof that matters here is
//! `bamboohr_api_key_is_the_basic_username_and_the_password_is_not_a_secret` —
//! the key reaches the wire only through the plan, the constant password is
//! declaration material a deployment does not choose, and neither the
//! declaration nor any diagnostic carries the key.

use std::time::Duration;

use donat_connectors::providers::bamboohr;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const COMPANY: &str = "acme";
const EMPLOYEE_ID: i64 = 43;
const FIELDS: &str = "firstName,lastName,workEmail,jobTitle,department,hireDate";

fn operation(id: &str) -> Operation {
    bamboohr::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the bamboohr declaration publishes {id}"))
        .clone()
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    bamboohr::connector()
        .credential()
        .plan()
        .expect("bamboohr declares a credential plan")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the configured key");
    request
}

/// `base64("<key>:x")`, which is the whole of what BambooHR's example sends.
fn expected_basic() -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{SECRET_SENTINEL}:x"))
    )
}

fn employee() -> JsonValue {
    json!({
        "id": "43",
        "firstName": "Ava",
        "lastName": "Chen",
        "workEmail": "ava.chen@example.test",
        "jobTitle": "Staff Engineer",
        "department": "Engineering",
        "hireDate": "2024-02-01",
        "employmentHistoryStatus": "Full-Time",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "employee.get",
            json!({ "employee_id": EMPLOYEE_ID, "fields": FIELDS }),
        ),
        (
            "employee.list",
            json!({ "fields": FIELDS, "sort": "-hireDate" }),
        ),
        (
            "employee.create",
            json!({
                "firstName": "Ava", "lastName": "Chen",
                "workEmail": "ava.chen@example.test", "jobTitle": "Staff Engineer",
                "department": "Engineering", "hireDate": "2024-02-01",
            }),
        ),
        (
            "employee.update",
            json!({
                "employee_id": EMPLOYEE_ID, "workEmail": "ava@example.test",
                "jobTitle": "Principal Engineer", "department": "Engineering",
                "status": "Full-Time",
            }),
        ),
        (
            "time_off_request.list",
            json!({ "start": "2026-08-01", "end": "2026-08-31", "status": "approved" }),
        ),
    ]
}

/// `bamboohr_request_shape`: exact method, path, query, headers, and body for
/// every operation.
#[tokio::test]
async fn bamboohr_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v1/employees/{EMPLOYEE_ID}"))
            .query("fields=firstName%2ClastName%2CworkEmail%2CjobTitle%2Cdepartment%2ChireDate")
            .header("authorization", &expected_basic())
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, employee()),
        Expectation::new("GET", "/api/v1/employees")
            .query(
                "fields=firstName%2ClastName%2CworkEmail%2CjobTitle%2Cdepartment%2ChireDate\
                 &sort=%2DhireDate&page[limit]=100",
            )
            .respond_json(200, json!({ "data": [employee()], "_links": {} })),
        Expectation::new("POST", "/api/v1/employees")
            .json_body(json!({
                "firstName": "Ava", "lastName": "Chen",
                "workEmail": "ava.chen@example.test", "jobTitle": "Staff Engineer",
                "department": "Engineering", "hireDate": "2024-02-01",
            }))
            .respond_bytes(201, Vec::new()),
        Expectation::new("POST", &format!("/api/v1/employees/{EMPLOYEE_ID}"))
            .json_body(json!({
                "workEmail": "ava@example.test", "jobTitle": "Principal Engineer",
                "department": "Engineering", "employmentHistoryStatus": "Full-Time",
            }))
            .respond_bytes(200, Vec::new()),
        Expectation::new("GET", "/api/v1/time_off/requests")
            .query("start=2026%2D08%2D01&end=2026%2D08%2D31&status=approved")
            .respond_json(200, json!([])),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        // Every documented endpoint is under BambooHR's own `/api/v1` prefix.
        assert!(
            request.url().path().starts_with("/api/v1/"),
            "{id} renders under BambooHR's own version prefix: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `bamboohr_auth_is_applied`, and spec 028 §3's second case:
/// `bamboohr_api_key_is_the_basic_username_and_the_password_is_not_a_secret`.
///
/// The key is the connector's one secret and reaches the wire only through the
/// plan; the password beside it is a compile-time constant a deployment does not
/// choose; and neither the declaration, its `Debug`, nor a rendered request
/// carries the key in the clear.
#[tokio::test]
async fn bamboohr_api_key_is_the_basic_username_and_the_password_is_not_a_secret() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/api/v1/employees/{EMPLOYEE_ID}"))
                .header("authorization", &expected_basic())
                .respond_json(200, employee()),
        ])
        .await;

    let request = render(
        &stub,
        "employee.get",
        json!({ "employee_id": EMPLOYEE_ID, "fields": FIELDS }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!request.url_carries_credential());

    // The secret half reaches no `Debug`, no redacted URL, and no declaration.
    // It is the *username* half of the Basic pair, which is exactly why
    // `AuthPlan::basic` is not the plan here: that one takes its username where
    // the declaration is built.
    let printed = format!("{request:?}");
    assert!(!printed.contains(SECRET_SENTINEL), "{printed}");
    assert!(!request.redacted_url().contains(SECRET_SENTINEL));
    let declaration = format!("{:?}", bamboohr::connector());
    assert!(!declaration.contains(SECRET_SENTINEL), "{declaration}");

    // The declared credential contract names one field and it is a secret: the
    // constant password is not configuration and is not published as a field a
    // deployment binds.
    let fields = bamboohr::connector().credential().fields();
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0].name(), "secret");
    assert!(fields[0].is_secret());
    assert_eq!(
        bamboohr::connector()
            .credential()
            .plan()
            .expect("bamboohr declares a plan")
            .required_fields(),
        ["secret"]
    );

    // A declared credential that cannot be applied fails the attempt before a
    // byte leaves (ADR 043).
    let mut unauthenticated = operation("employee.get")
        .plan_request(
            &stub.origin(),
            &json!({ "employee_id": EMPLOYEE_ID, "fields": FIELDS }),
        )
        .expect("the declared request renders");
    let failure = bamboohr::connector()
        .credential()
        .plan()
        .expect("bamboohr declares a plan")
        .apply(&Credential::from_fields([]), &mut unauthenticated, None)
        .expect_err("a connector with no configured key cannot send");
    assert_eq!(failure.code(), "connector_credential_missing_field");

    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `bamboohr_host_comes_only_from_deploy_time_configuration` (spec 023 §4 proof
/// 1): input, a provider body, and a continuation each fail to move the host.
#[tokio::test]
async fn bamboohr_host_comes_only_from_deploy_time_configuration() {
    let specification = bamboohr::connector().origin();
    assert_eq!(
        specification.host_variable(),
        Some(bamboohr::COMPANY_DOMAIN)
    );

    let origin = bamboohr::connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            bamboohr::COMPANY_DOMAIN,
            COMPANY,
        )]))
        .expect("a configured company resolves");
    assert_eq!(origin.as_url().as_str(), "https://acme.bamboohr.com/");

    // 1. Operation input. A value that spells another authority stays a query
    //    or path value on the configured host.
    let request = operation("employee.get")
        .plan_request(
            &origin,
            &json!({
                "employee_id": EMPLOYEE_ID,
                "fields": "https://attacker.invalid/api/v1/employees",
            }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("acme.bamboohr.com"));
    assert_eq!(request.url().scheme(), "https");
    assert_eq!(
        request.url().path(),
        format!("/api/v1/employees/{EMPLOYEE_ID}")
    );
    // The one path slot is typed as an integer, so a value spelling another
    // company is not a value the declaration admits at all.
    assert!(
        operation("employee.get")
            .plan_request(
                &origin,
                &json!({ "employee_id": "../../evil.bamboohr.com", "fields": FIELDS }),
            )
            .is_err()
    );

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("employee.get")
        .extract_output(&json!({ "id": "43", "workEmail": "ops@attacker.invalid" }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("workEmail"),
        Some(&json!("ops@attacker.invalid"))
    );

    // 3. A `_links.next` continuation to another origin is refused rather than
    //    followed, on a templated origin exactly as on a fixed one.
    let stub = ProviderStub::start([Expectation::new("GET", "/api/v1/employees").respond_json(
        200,
        json!({
            "data": [],
            "_links": { "next": "https://attacker.invalid/api/v1/employees?page[after]=2" },
        }),
    )])
    .await;
    let failure = bamboohr::pagination("employee.list")
        .expect("employee.list declares a plan")
        .collect(
            render(
                &stub,
                "employee.list",
                json!({ "fields": FIELDS, "sort": "-hireDate" }),
            ),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    stub.assert_satisfied();

    // And the configuration itself admits one host label and nothing else: the
    // company subdomain, never a whole host and never another authority.
    for hostile in [
        "acme.bamboohr.com",
        "acme/../evil",
        "acme:8080",
        "user@acme",
        "ACME",
        "",
    ] {
        assert!(
            bamboohr::connector()
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    bamboohr::COMPANY_DOMAIN,
                    hostile,
                )]))
                .is_err(),
            "`{hostile}` is not one lowercase host label"
        );
    }
    // A deployment that configured nothing is refused by name rather than
    // served a default company.
    assert!(
        bamboohr::connector()
            .resolve_origin(&ConnectorConfiguration::default())
            .is_err()
    );
}

/// `bamboohr_error_map`: every documented status reaches exactly one closed
/// class, and neither the key nor BambooHR's prose crosses the boundary.
#[tokio::test]
async fn bamboohr_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (406, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v1/employees/{EMPLOYEE_ID}"),
        )
        .respond_json(
            status,
            json!({ "message": format!("duplicate email for {SECRET_SENTINEL}") }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "employee.get",
                json!({ "employee_id": EMPLOYEE_ID, "fields": FIELDS }),
            ))
            .await
            .expect("the stub answers");

        let failure = bamboohr::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "duplicate email"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `bamboohr_rate_limit_is_classified`: "API requests can be throttled if
/// BambooHR deems them to be too frequent. Implementations should always be
/// ready for a `503 Service Unavailable` response" — and the `Retry-After` it
/// may carry, clamped.
#[tokio::test]
async fn bamboohr_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v1/employees/{EMPLOYEE_ID}"))
            .respond_header("retry-after", "30")
            .respond_json(429, json!({ "message": "Too many requests" })),
        Expectation::new("GET", &format!("/api/v1/employees/{EMPLOYEE_ID}"))
            .respond_header("retry-after", "60")
            .respond_json(503, json!({ "message": "Service Unavailable" })),
        Expectation::new("GET", &format!("/api/v1/employees/{EMPLOYEE_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(503, json!({ "message": "Service Unavailable" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(
                &stub,
                "employee.get",
                json!({ "employee_id": EMPLOYEE_ID, "fields": FIELDS }),
            ))
            .await
            .expect("the stub answers");
        failures.push(bamboohr::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(30)));
    assert_eq!(failures[1].class(), ConnectorErrorClass::Http5xx);
    assert_eq!(failures[1].retry_after(), Some(Duration::from_secs(60)));
    assert_eq!(
        failures[2].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `bamboohr_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is BambooHR's own `_links.next`, it is followed as a destination
/// on this origin, and the walk makes exactly the number of requests the plan
/// declares.
#[tokio::test]
async fn bamboohr_cursor_is_opaque_and_bounded() {
    let plan = bamboohr::pagination("employee.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v1/employees")
            .query(
                "fields=firstName%2ClastName%2CworkEmail%2CjobTitle%2Cdepartment%2ChireDate\
                 &sort=%2DhireDate&page[limit]=100",
            )
            .respond_json(
                200,
                json!({
                    "data": [{ "id": "1" }],
                    "meta": { "total": 2 },
                    "_links": {
                        "self": "/api/v1/employees?page%5Blimit%5D=100",
                        "next": "/api/v1/employees?page%5Blimit%5D=100&page%5Bafter%5D=1",
                        "prev": JsonValue::Null,
                    },
                }),
            ),
        // The walk ends where `next` is absent.
        Expectation::new("GET", "/api/v1/employees")
            .query("page%5Blimit%5D=100&page%5Bafter%5D=1")
            .respond_json(
                200,
                json!({ "data": [{ "id": "2" }], "_links": { "next": JsonValue::Null } }),
            ),
    ])
    .await;

    let employees = plan
        .collect(
            render(
                &stub,
                "employee.list",
                json!({ "fields": FIELDS, "sort": "-hireDate" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one link and stops where the link stops");
    assert_eq!(employees, vec![json!({ "id": "1" }), json!({ "id": "2" })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `bamboohr_pagination_is_bounded`: the declared plan terminates under every
/// budget, and every other operation declares none — including the time-off
/// listing, whose bound is BambooHR's own required date window.
#[tokio::test]
async fn bamboohr_pagination_is_bounded() {
    let plan = bamboohr::pagination("employee.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/v1/employees").respond_json(
                200,
                json!({
                    "data": [{ "id": "1" }, { "id": "2" }],
                    "_links": { "next": "/api/v1/employees?page%5Bafter%5D=endless" },
                }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "employee.list",
                    json!({ "fields": FIELDS, "sort": "-hireDate" }),
                ),
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
        "employee.get",
        "employee.create",
        "employee.update",
        "time_off_request.list",
    ] {
        assert!(
            bamboohr::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    assert_eq!(plan.items_pointer(), "/data");
}

/// `bamboohr_effects_are_classified`: the create is `AtMostOnce` on a recorded
/// absence and a named consequence, the `POST` partial update stays unreachable,
/// and every read is a read.
#[test]
fn bamboohr_effects_are_classified() {
    let connector = bamboohr::connector();
    let expected = [
        ("employee.get", EffectClass::ReadOnly),
        ("employee.list", EffectClass::ReadOnly),
        ("employee.create", EffectClass::AtMostOnce),
        ("employee.update", EffectClass::InventoryOnly),
        ("time_off_request.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation.idempotency_binding().is_none(), "{id}");
    }

    assert_eq!(
        connector.admit_operation("employee.update"),
        Err(OperationRejection::InventoryOnly)
    );
    let reason = operation("employee.update")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records its reason")
        .to_owned();
    assert!(reason.contains("PUT and DELETE only"), "{reason}");
    assert!(reason.contains("no consequence to record"), "{reason}");

    let evidence = operation("employee.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on")
        .clone();
    assert!(
        evidence
            .searched_documentation()
            .contains("Technical Overview")
    );
    assert!(
        evidence
            .repeat_produces()
            .contains("a second employee record")
    );
}

/// `bamboohr_output_contract`: the declared pointers read BambooHR's own
/// objects, its documented empty successes are successes, and the time-off
/// listing is a bare array at the document root.
#[test]
fn bamboohr_output_contract() {
    let get = operation("employee.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&employee()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        employee()
    );
    // BambooHR publishes the employee id as a JSON *string* in this
    // representation and as a number elsewhere, so the declared pointer is the
    // one scalar that carries the provider's own value through
    // (`knowledgebase/declarative-saas/decisions/071-*`).
    assert_eq!(
        get.decode_response(200, br#"{"id":43}"#)
            .expect("a numeric id is the provider's own value too")
            .get("id"),
        Some(&json!(43))
    );

    // "The ID of the newly created employee is included in the `Location`
    // header of the response": the documented success carries no body.
    assert_eq!(
        operation("employee.create")
            .decode_response(201, b"")
            .expect("a documented empty success is a success"),
        json!({})
    );
    assert_eq!(
        operation("employee.update")
            .decode_response(200, b"")
            .expect("a documented empty success is a success"),
        json!({})
    );

    // The time-off listing is a bare array at the document root, published
    // whole.
    assert_eq!(
        operation("time_off_request.list")
            .decode_response(200, br#"[{"id":1,"status":{"status":"approved"}}]"#)
            .expect("the declared contract is satisfied"),
        json!([{ "id": 1, "status": { "status": "approved" } }])
    );

    // And a collection is the envelope BambooHR publishes, with its
    // continuation carried as data.
    let list = operation("employee.list")
        .decode_response(
            200,
            br#"{"data":[{"id":"1"}],"meta":{"total":1},"_links":{}}"#,
        )
        .expect("the declared contract is satisfied");
    assert_eq!(list.get("data"), Some(&json!([{ "id": "1" }])));
    assert_eq!(list.get("meta"), Some(&json!({ "total": 1 })));
}
