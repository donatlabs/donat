//! Salesforce connector proofs (spec 023 §4), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::salesforce;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    Operation, OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const RECORD_ID: &str = "001D000000IqhSLIAZ";
const SOBJECT: &str = "Account";

fn operation(id: &str) -> &'static Operation {
    salesforce::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the salesforce declaration publishes {id}"))
}

/// The credential lifecycle's applied header for one attempt (spec 011).
fn applied_token() -> AccessToken {
    AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_token()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn path(suffix: &str) -> String {
    format!("/services/data/{}{suffix}", salesforce::API_VERSION)
}

fn write_result() -> JsonValue {
    json!({ "id": RECORD_ID, "errors": [], "success": true })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "record.get",
            json!({ "sobject": SOBJECT, "record_id": RECORD_ID }),
        ),
        ("record.query", json!({ "q": "SELECT Name FROM Account" })),
        (
            "record.query_all",
            json!({ "q": "SELECT Name FROM Account" }),
        ),
        ("record.search", json!({ "q": "FIND {joe}" })),
        (
            "record.create",
            json!({ "sobject": SOBJECT, "record": { "Name": "Acme" } }),
        ),
        (
            "record.update",
            json!({ "sobject": SOBJECT, "record_id": RECORD_ID,
                    "record": { "BillingCity": "San Francisco" } }),
        ),
        (
            "record.upsert",
            json!({ "sobject": SOBJECT, "external_field": "ExtId__c",
                    "external_value": "acme-1", "record": { "Name": "Acme" } }),
        ),
        (
            "record.delete",
            json!({ "sobject": SOBJECT, "record_id": RECORD_ID }),
        ),
    ]
}

/// `salesforce_request_shape`: exact method, path, query, headers, and body for
/// every operation, all under the pinned version segment.
#[tokio::test]
async fn salesforce_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, json!({ "Id": RECORD_ID, "Name": "Acme" })),
        Expectation::new("GET", &path("/query"))
            .query("q=SELECT%20Name%20FROM%20Account")
            .respond_json(
                200,
                json!({ "totalSize": 1, "done": true, "records": [{ "Id": RECORD_ID }] }),
            ),
        Expectation::new("GET", &path("/queryAll"))
            .query("q=SELECT%20Name%20FROM%20Account")
            .respond_json(200, json!({ "totalSize": 0, "done": true, "records": [] })),
        Expectation::new("GET", &path("/search"))
            .query("q=FIND%20%7Bjoe%7D")
            .respond_json(200, json!({ "searchRecords": [] })),
        Expectation::new("POST", &path(&format!("/sobjects/{SOBJECT}")))
            .json_body(json!({ "Name": "Acme" }))
            .respond_json(201, write_result()),
        Expectation::new("PATCH", &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")))
            .json_body(json!({ "BillingCity": "San Francisco" }))
            .respond_bytes(204, Vec::new()),
        Expectation::new(
            "PATCH",
            &path(&format!("/sobjects/{SOBJECT}/ExtId%5F%5Fc/acme%2D1")),
        )
        .json_body(json!({ "Name": "Acme" }))
        .respond_json(
            200,
            json!({ "id": RECORD_ID, "errors": [], "success": true,
                                   "created": false }),
        ),
        Expectation::new("DELETE", &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")))
            .respond_bytes(204, Vec::new()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request
                .url()
                .path()
                .starts_with(&format!("/services/data/{}/", salesforce::API_VERSION)),
            "{id} renders the pinned version segment: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `salesforce_auth_is_applied`: the stored OAuth2 token reaches the wire as the
/// `Bearer` authorization Salesforce publishes, and a request without one is
/// refused rather than sent unauthenticated.
#[tokio::test]
async fn salesforce_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, json!({ "Id": RECORD_ID }))])
    .await;

    let request = render(
        &stub,
        "record.get",
        json!({ "sobject": SOBJECT, "record_id": RECORD_ID }),
    );
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    // The declaration configures no secret at all, and refuses to render
    // without the token one attempt is given.
    assert!(salesforce::connector().credential().fields().is_empty());
    let mut unauthenticated = operation("record.get")
        .plan_request(
            &stub.origin(),
            &json!({ "sobject": SOBJECT, "record_id": RECORD_ID }),
        )
        .expect("the declared request renders");
    let refusal = AuthPlan::oauth2_authorization_code()
        .apply(&Credential::from_fields([]), &mut unauthenticated, None)
        .expect_err("a stored-credential plan never sends unauthenticated");
    assert_eq!(refusal.code(), "connector_credential_not_applicable");

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        salesforce::connector().credential(),
        salesforce::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `salesforce_host_comes_only_from_deploy_time_configuration` (spec 023 §4
/// proof 1): input, a provider body, and a continuation each fail to move the
/// org's host.
#[tokio::test]
async fn salesforce_host_comes_only_from_deploy_time_configuration() {
    assert_eq!(
        salesforce::connector().origin().host_variable(),
        Some(salesforce::MY_DOMAIN)
    );

    let origin = salesforce::connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            salesforce::MY_DOMAIN,
            "acme",
        )]))
        .expect("a configured My Domain resolves");
    assert_eq!(origin.as_url().as_str(), "https://acme.my.salesforce.com/");

    // 1. Operation input. An object name that spells another authority stays
    //    one percent-encoded segment on the configured host.
    let request = operation("record.get")
        .plan_request(
            &origin,
            &json!({ "sobject": "..//attacker.invalid", "record_id": RECORD_ID }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("acme.my.salesforce.com"));
    assert_eq!(request.url().scheme(), "https");
    assert!(
        request.url().path().starts_with(&format!(
            "/services/data/{}/sobjects/",
            salesforce::API_VERSION
        )),
        "{}",
        request.url().path()
    );
    assert!(!request.url().path().contains("attacker.invalid/"));

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("record.query")
        .extract_output(&json!({ "records": [], "done": true,
                                 "nextRecordsUrl": "https://attacker.invalid/q" }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("next_records_url"),
        Some(&json!("https://attacker.invalid/q"))
    );

    // 3. And the same value spent as a continuation is refused rather than
    //    followed: `nextRecordsUrl` is a destination, and it is checked.
    let stub = ProviderStub::start([Expectation::new("GET", &path("/query")).respond_json(
        200,
        json!({ "records": [], "done": false,
                "nextRecordsUrl": "https://attacker.invalid/services/data/v67.0/query/01g-500" }),
    )])
    .await;
    let failure = salesforce::pagination("record.query")
        .expect("record.query declares a plan")
        .collect(
            render(
                &stub,
                "record.query",
                json!({ "q": "SELECT Id FROM Account" }),
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

    for hostile in [
        "acme.my.salesforce.com",
        "acme--dev.sandbox",
        "acme/../evil",
        "acme:8080",
        "",
        "-acme",
        "ACME",
    ] {
        assert!(
            salesforce::connector()
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    salesforce::MY_DOMAIN,
                    hostile
                )]))
                .is_err(),
            "`{hostile}` is not one host label under the published production suffix"
        );
    }
}

/// `salesforce_error_map`: every documented status and `errorCode` reaches
/// exactly one closed class, and none of Salesforce's prose crosses the
/// boundary.
#[tokio::test]
async fn salesforce_error_map() {
    let documented = [
        (400, "MALFORMED_QUERY", ConnectorErrorClass::Validation),
        (
            400,
            "REQUIRED_FIELD_MISSING",
            ConnectorErrorClass::Validation,
        ),
        (
            401,
            "INVALID_SESSION_ID",
            ConnectorErrorClass::Authentication,
        ),
        (
            403,
            "INSUFFICIENT_ACCESS",
            ConnectorErrorClass::Authentication,
        ),
        (404, "NOT_FOUND", ConnectorErrorClass::Permanent),
        (405, "METHOD_NOT_ALLOWED", ConnectorErrorClass::Permanent),
        (300, "MULTIPLE_MATCHES", ConnectorErrorClass::Permanent),
        (410, "GONE", ConnectorErrorClass::Permanent),
        (414, "URI_TOO_LONG", ConnectorErrorClass::Validation),
        (500, "UNKNOWN_EXCEPTION", ConnectorErrorClass::Http5xx),
        (502, "EDGE_FAILURE", ConnectorErrorClass::Http5xx),
        (503, "SERVER_UNAVAILABLE", ConnectorErrorClass::Http5xx),
        (418, "not_a_published_code", ConnectorErrorClass::Permanent),
        // The classifications a status-only map would get wrong.
        (403, "REQUEST_LIMIT_EXCEEDED", ConnectorErrorClass::Http429),
        (500, "UNABLE_TO_LOCK_ROW", ConnectorErrorClass::Http5xx),
        (400, "QUERY_TIMEOUT", ConnectorErrorClass::Timeout),
    ];

    for (status, code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")),
        )
        .respond_json(
            status,
            // Salesforce's published error body is an array.
            json!([{
                "message": format!("acme org rejected {SECRET_SENTINEL}"),
                "errorCode": code,
                "fields": ["Id"],
            }]),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "record.get",
                json!({ "sobject": SOBJECT, "record_id": RECORD_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = salesforce::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme org", "Id"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `salesforce_rate_limit_is_classified` (spec 023 §4 proof 2): Salesforce
/// publishes its rate limit as a `403` carrying `REQUEST_LIMIT_EXCEEDED` and
/// publishes no `429` and no `Retry-After` — so the code rule is what makes the
/// refusal retryable, and a hint that did arrive is clamped.
#[tokio::test]
async fn salesforce_rate_limit_is_classified() {
    let limited = json!([{ "message": "TotalRequests Limit exceeded.",
                           "errorCode": "REQUEST_LIMIT_EXCEEDED" }]);
    let stub = ProviderStub::start([
        Expectation::new("GET", &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")))
            .respond_header("sforce-limit-info", "api-usage=100000/100000")
            .respond_json(403, limited.clone()),
        Expectation::new("GET", &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}")))
            .respond_header("retry-after", "604800")
            .respond_json(403, limited),
        // A bare `403` stays authentication: it is a permission refusal, and a
        // Process must not wait it out.
        Expectation::new("GET", &path(&format!("/sobjects/{SOBJECT}/{RECORD_ID}"))).respond_json(
            403,
            json!([{ "message": "no access",
                                        "errorCode": "INSUFFICIENT_ACCESS" }]),
        ),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(
                &stub,
                "record.get",
                json!({ "sobject": SOBJECT, "record_id": RECORD_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(salesforce::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Salesforce publishes no Retry-After, so the connector invents none"
    );
    assert_eq!(failures[1].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a hint that did arrive is clamped to the SDK ceiling"
    );
    assert_eq!(failures[2].class(), ConnectorErrorClass::Authentication);
    stub.assert_satisfied();
}

/// `salesforce_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the query
/// locator is spent verbatim as a destination on this org's origin, the walk
/// stops on the absence Salesforce publishes, and it makes exactly the number of
/// requests the plan declares.
#[tokio::test]
async fn salesforce_cursor_is_opaque_and_bounded() {
    let plan = salesforce::pagination("record.query").expect("the query declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    let locator = path("/query/01gRO0000016PIAYA2-500");

    let stub = ProviderStub::start([
        Expectation::new("GET", &path("/query"))
            .query("q=SELECT%20Id%20FROM%20Account")
            .respond_json(
                200,
                json!({ "totalSize": 2, "done": false, "nextRecordsUrl": locator,
                        "records": [{ "Id": "a" }] }),
            ),
        // "You can continue retrieving results from the initial query until
        // `done` is true", and the locator is absent there.
        Expectation::new("GET", &locator).query("").respond_json(
            200,
            json!({ "totalSize": 2, "done": true, "records": [{ "Id": "b" }] }),
        ),
    ])
    .await;

    let records = plan
        .collect(
            render(
                &stub,
                "record.query",
                json!({ "q": "SELECT Id FROM Account" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one locator and stops where the field stops");
    assert_eq!(records, vec![json!({ "Id": "a" }), json!({ "Id": "b" })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `salesforce_pagination_is_bounded`: the declared plan terminates and respects
/// the call, page, item, and byte budgets, and a short page never ends a walk —
/// only the absent locator does.
#[tokio::test]
async fn salesforce_pagination_is_bounded() {
    let plan = salesforce::pagination("record.query").expect("the query declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &path("/query")).respond_json(
                200,
                json!({ "done": false, "nextRecordsUrl": path("/query/01g-500"),
                        "records": [{ "Id": "a" }, { "Id": "b" }] }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "record.query",
                    json!({ "q": "SELECT Id FROM Account" }),
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

    assert_eq!(
        salesforce::pagination("record.query_all")
            .expect("the queryAll declares the same plan")
            .items_pointer(),
        "/records"
    );
    for id in [
        "record.get",
        "record.search",
        "record.create",
        "record.update",
        "record.upsert",
        "record.delete",
    ] {
        assert!(
            salesforce::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `salesforce_effects_are_classified`: every operation carries a class, and the
/// three writes Salesforce documents differently are each refused for their own
/// recorded reason.
#[test]
fn salesforce_effects_are_classified() {
    let connector = salesforce::connector();
    let expected = [
        ("record.get", EffectClass::ReadOnly),
        ("record.query", EffectClass::ReadOnly),
        ("record.query_all", EffectClass::ReadOnly),
        ("record.search", EffectClass::ReadOnly),
        ("record.create", EffectClass::AtMostOnce),
        ("record.update", EffectClass::InventoryOnly),
        ("record.upsert", EffectClass::InventoryOnly),
        ("record.delete", EffectClass::InventoryOnly),
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
        connector.admit_operation("record.upsert"),
        Err(OperationRejection::InventoryOnly)
    );

    let reason = |id: &str| {
        operation(id)
            .effect()
            .and_then(donat_connectors::sdk::Effect::inventory_reason)
            .unwrap_or_default()
            .to_owned()
    };
    // The upsert is the one Salesforce itself calls idempotent, and it is
    // refused because the class it wants keeps the retry.
    assert!(reason("record.upsert").contains("(idempotent)"));
    assert!(reason("record.upsert").contains("keeps the retry"));
    // The delete is refused on a silence that Salesforce's own Big Object
    // statement proves is meaningful.
    assert!(reason("record.delete").contains("deleteByExample"));
    // And the update is refused on the method plus the partial write.
    assert!(reason("record.update").contains("PATCH"));

    let evidence = operation("record.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("ui-api"));
    assert!(evidence.repeat_produces().contains("second record"));
}

/// `salesforce_output_contract`: the declared pointers read Salesforce's own
/// write result and query envelope, and the empty-bodied successes decode as the
/// documented silence rather than as a failure.
#[test]
fn salesforce_output_contract() {
    assert_eq!(
        operation("record.create")
            .decode_response(
                201,
                &serde_json::to_vec(&write_result()).expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied"),
        json!({ "id": RECORD_ID, "success": true, "errors": [] })
    );
    // "Example response body for updating fields in an Account object — none
    // returned", and the same for the delete.
    for id in ["record.update", "record.delete"] {
        assert_eq!(
            operation(id)
                .decode_response(204, b"")
                .expect("a documented empty success is not a failure"),
            json!({})
        );
    }

    let query = operation("record.query");
    assert_eq!(
        query
            .decode_response(
                200,
                br#"{"totalSize":3222,"done":false,
                     "nextRecordsUrl":"/services/data/v67.0/query/01g-500",
                     "records":[{"Id":"a"}]}"#,
            )
            .expect("the declared contract is satisfied"),
        json!({
            "records": [{ "Id": "a" }],
            "done": false,
            "total_size": 3222,
            "next_records_url": "/services/data/v67.0/query/01g-500",
        })
    );
    assert_eq!(
        query
            .decode_response(200, br#"{"done":true}"#)
            .expect_err("a query answer with no records is not a query answer")
            .class(),
        ConnectorErrorClass::Validation
    );
    // The `created` flag Salesforce publishes for an upsert is composed rather
    // than demanded, because it "doesn't appear in earlier versions".
    assert_eq!(
        operation("record.upsert")
            .decode_response(
                200,
                br#"{"id":"001","errors":[],"success":true,"created":false}"#
            )
            .expect("the declared contract is satisfied")
            .get("success"),
        Some(&json!(true))
    );
}
