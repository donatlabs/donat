//! Grafana connector proofs (spec 027 §3, which adopts spec 023 §4), against the
//! SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::grafana;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const RULE_UID: &str = "cIBgcSjkk";
const DASHBOARD_UID: &str = "cIBgcSjkl";

fn operation(id: &str) -> &'static Operation {
    grafana::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the grafana declaration publishes {id}"))
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

fn alert_rule() -> JsonValue {
    json!({
        "uid": RULE_UID, "title": "Disk is full", "folderUID": "nErXDvCkzz",
        "ruleGroup": "eval_group_1", "condition": "B", "isPaused": false,
        "updated": "2026-08-10T09:00:00Z",
        "data": [{ "refId": "A" }],
    })
}

fn dashboard() -> JsonValue {
    json!({
        "dashboard": { "uid": DASHBOARD_UID, "title": "Production overview", "version": 3 },
        "meta": { "folderUid": "nErXDvCkzz", "url": "/d/cIBgcSjkl/production-overview",
                  "updated": "2026-08-10T09:00:00Z" },
    })
}

fn hit() -> JsonValue {
    json!({ "id": 163, "uid": DASHBOARD_UID, "title": "Production overview",
            "type": "dash-db", "url": "/d/cIBgcSjkl/production-overview" })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("alert_rule.list", json!({})),
        ("alert_rule.get", json!({ "uid": RULE_UID })),
        (
            "alert_rule.update",
            json!({ "uid": RULE_UID, "title": "Disk is full", "folderUID": "nErXDvCkzz",
                    "ruleGroup": "eval_group_1", "condition": "B",
                    "data": [{ "refId": "A" }], "noDataState": "NoData",
                    "execErrState": "Error", "for": "5m", "isPaused": false }),
        ),
        ("dashboard.get", json!({ "uid": DASHBOARD_UID })),
        ("dashboard.search", json!({ "query": "" })),
    ]
}

/// `grafana_request_shape`: exact method, path, query, headers and body for every
/// operation, all under the published `/api` base path.
#[tokio::test]
async fn grafana_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v1/provisioning/alert-rules")
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, json!([alert_rule()])),
        Expectation::new(
            "GET",
            &format!("/api/v1/provisioning/alert-rules/{RULE_UID}"),
        )
        .query("")
        .respond_json(200, alert_rule()),
        Expectation::new(
            "PUT",
            &format!("/api/v1/provisioning/alert-rules/{RULE_UID}"),
        )
        .json_body(json!({
            "title": "Disk is full", "folderUID": "nErXDvCkzz",
            "ruleGroup": "eval_group_1", "condition": "B",
            "data": [{ "refId": "A" }], "noDataState": "NoData",
            "execErrState": "Error", "for": "5m", "isPaused": false,
        }))
        .respond_json(200, alert_rule()),
        Expectation::new("GET", &format!("/api/dashboards/uid/{DASHBOARD_UID}"))
            .query("")
            .respond_json(200, dashboard()),
        Expectation::new("GET", "/api/search")
            .query("query=&page=1&limit=100")
            .respond_json(200, json!([hit()])),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/"),
            "{id} renders the published base path: {}",
            request.url().path()
        );
        // The search's page parameters come from its declared plan, exactly as
        // the executor would prime them.
        if let Some(plan) = grafana::pagination(id) {
            let primed = plan
                .collect(
                    request,
                    &stub.origin(),
                    &PaginationBudget::new(1, 1, 1_000, 1 << 20, Duration::from_secs(5)),
                    undeclared_status_gate,
                    |request| stub.send(request),
                )
                .await
                .expect("the first page is the whole answer");
            assert_eq!(primed, vec![hit()]);
            continue;
        }
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `grafana_auth_is_applied`: the service account token reaches the wire as the
/// `Bearer` header Grafana's own examples publish.
#[tokio::test]
async fn grafana_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/v1/provisioning/alert-rules/{RULE_UID}"),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, alert_rule())])
    .await;

    let request = render(&stub, "alert_rule.get", json!({ "uid": RULE_UID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("an ASCII header"),
        format!("Bearer {SECRET_SENTINEL}")
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        grafana::connector().credential(),
        grafana::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `grafana_host_comes_only_from_deploy_time_configuration` (spec 023 §4 proof
/// 1): the instance's whole origin is one configuration value, and input, a
/// provider body, and a continuation each fail to move it.
#[tokio::test]
async fn grafana_host_comes_only_from_deploy_time_configuration() {
    assert_eq!(
        grafana::connector().origin().host_variable(),
        Some(grafana::INSTANCE_ORIGIN)
    );

    let origin = grafana::connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            grafana::INSTANCE_ORIGIN,
            "https://grafana.example.test",
        )]))
        .expect("a configured instance origin resolves");
    assert_eq!(origin.as_url().as_str(), "https://grafana.example.test/");

    // 1. Operation input. A uid that spells another authority stays inside its
    //    own path segment on the configured host.
    let request = operation("alert_rule.get")
        .plan_request(&origin, &json!({ "uid": "https://attacker.invalid/x" }))
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("grafana.example.test"));
    assert_eq!(
        request.url().path(),
        "/api/v1/provisioning/alert-rules/https%3A%2F%2Fattacker%2Einvalid%2Fx"
    );

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("dashboard.get")
        .extract_output(&json!({
            "dashboard": { "uid": DASHBOARD_UID },
            "meta": { "url": "https://attacker.invalid/d/x" },
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("url"),
        Some(&json!("https://attacker.invalid/d/x"))
    );

    // 3. A continuation cannot move it either: the one declared plan derives its
    //    next page from the walk rather than from anything the provider sent, so
    //    a body naming another origin changes nothing about where page two goes.
    let full: Vec<JsonValue> = (0..100).map(|_| hit()).collect();
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/search")
            .query("query=&page=1&limit=100")
            .respond_header(
                "link",
                "<https://attacker.invalid/api/search>; rel=\"next\"",
            )
            .respond_json(200, json!(full)),
        Expectation::new("GET", "/api/search")
            .query("query=&page=2&limit=100")
            .respond_json(200, json!([])),
    ])
    .await;
    let hits = grafana::pagination("dashboard.search")
        .expect("the search declares a plan")
        .collect(
            render(&stub, "dashboard.search", json!({ "query": "" })),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 1_000, 1 << 20, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk ignores a header no declared plan reads");
    assert_eq!(hits.len(), 100);
    stub.assert_satisfied();

    // And the configured value is checked at deploy time.
    grafana::validate_instance_origin("https://grafana.example.test")
        .expect("a plain https instance origin is admitted");
    grafana::validate_instance_origin("https://grafana.example.test:3000")
        .expect("a port is part of an origin");
    for refused in [
        "http://grafana.example.test",
        "https://example.test/grafana",
        "https://user:pass@grafana.example.test",
        "grafana.example.test",
        "ftp://grafana.example.test",
        "",
    ] {
        assert!(
            grafana::validate_instance_origin(refused).is_err(),
            "`{refused}` is not an instance origin this connector may send a token to"
        );
    }
}

/// `grafana_error_map`: every documented status reaches exactly one closed
/// class, and none of Grafana's prose crosses the boundary.
#[tokio::test]
async fn grafana_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (406, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v1/provisioning/alert-rules/{RULE_UID}"),
        )
        .respond_json(
            status,
            json!({ "message": format!("grafana.example.test rejected {SECRET_SENTINEL}"),
                    "traceID": "abc123" }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "alert_rule.get", json!({ "uid": RULE_UID })))
            .await
            .expect("the stub answers");

        let failure = grafana::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "grafana.example.test", "abc123"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `grafana_rate_limit_is_classified`: Grafana publishes no rate limit for its
/// HTTP API, so a `429` from the instance's own host is retryable and its hint is
/// whatever the response carried, clamped.
#[tokio::test]
async fn grafana_rate_limit_is_classified() {
    let path = format!("/api/v1/provisioning/alert-rules/{RULE_UID}");
    let stub = ProviderStub::start([
        Expectation::new("GET", &path).respond_json(429, json!({ "message": "slow down" })),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "message": "slow down" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "alert_rule.get", json!({ "uid": RULE_UID })))
            .await
            .expect("the stub answers");
        failures.push(grafana::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Grafana publishes no Retry-After, so the connector invents none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `grafana_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the search's
/// page number is derived from the walk rather than from a provider value, and
/// the walk makes exactly the number of requests the plan declares (ADR 058).
#[tokio::test]
async fn grafana_cursor_is_opaque_and_bounded() {
    let plan = grafana::pagination("dashboard.search").expect("the search declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    let full: Vec<JsonValue> = (0..100).map(|_| hit()).collect();

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/search")
            .query("query=&page=1&limit=100")
            .respond_json(200, json!(full)),
        Expectation::new("GET", "/api/search")
            .query("query=&page=2&limit=100")
            .respond_json(200, json!([hit()])),
    ])
    .await;

    let hits = plan
        .collect(
            render(&stub, "dashboard.search", json!({ "query": "" })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk advances the page and stops on a short one");
    assert_eq!(hits.len(), 101);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `grafana_pagination_is_bounded`: the declared plan terminates and respects the
/// call, page, item and byte budgets, and only the search declares one.
#[tokio::test]
async fn grafana_pagination_is_bounded() {
    let plan = grafana::pagination("dashboard.search").expect("the search declares a plan");
    let full: Vec<JsonValue> = (0..100).map(|_| hit()).collect();
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start(
            (0..12).map(|_| Expectation::new("GET", "/api/search").respond_json(200, json!(full))),
        )
        .await;
        let failure = plan
            .collect(
                render(&stub, "dashboard.search", json!({ "query": "" })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    assert_eq!(plan.items_pointer(), "");
    for id in [
        "alert_rule.list",
        "alert_rule.get",
        "alert_rule.update",
        "dashboard.get",
    ] {
        assert!(
            grafana::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `grafana_effects_are_classified`: every operation carries a class, and the
/// `PUT` is unreachable from a Process because Grafana publishes no repeat
/// statement for it.
#[test]
fn grafana_effects_are_classified() {
    let expected = [
        ("alert_rule.list", EffectClass::ReadOnly),
        ("alert_rule.get", EffectClass::ReadOnly),
        ("alert_rule.update", EffectClass::InventoryOnly),
        ("dashboard.get", EffectClass::ReadOnly),
        ("dashboard.search", EffectClass::ReadOnly),
    ];
    assert_eq!(grafana::connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            grafana::connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        grafana::connector().admit_operation("alert_rule.update"),
        Err(OperationRejection::InventoryOnly)
    );
    // A `PUT` against a fixed uid is not evidence: the reason names what Grafana
    // did not publish.
    let reason = operation("alert_rule.update")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("Update an existing alert rule"), "{reason}");
    assert!(reason.contains("api-merged.json"), "{reason}");
}

/// `grafana_triggering_is_not_a_read` (spec 027 §3): rewriting an alert rule
/// changes what Grafana pages people about, so it is not classified `ReadOnly`
/// and no Process can reach it at all.
#[test]
fn grafana_triggering_is_not_a_read() {
    let update = operation("alert_rule.update");
    assert_ne!(update.effect_class(), Some(EffectClass::ReadOnly));
    assert_eq!(update.effect_class(), Some(EffectClass::InventoryOnly));
    assert!(!update.effect_class().expect("a class").is_executable());
    // Everything else in this module is a `GET`, and each says so with the
    // method rather than with an assertion.
    for id in [
        "alert_rule.list",
        "alert_rule.get",
        "dashboard.get",
        "dashboard.search",
    ] {
        assert_eq!(operation(id).effect_class(), Some(EffectClass::ReadOnly));
        assert!(matches!(
            operation(id)
                .effect()
                .and_then(donat_connectors::sdk::Effect::read_only_assertion),
            Some(donat_connectors::sdk::effect::ReadOnlyAssertion::Method)
        ));
    }
}

/// `grafana_output_contract`: the declared pointers read Grafana's own objects,
/// including the two-part dashboard document.
#[test]
fn grafana_output_contract() {
    let get = operation("dashboard.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&dashboard()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "uid": DASHBOARD_UID, "title": "Production overview", "version": 3,
            "folderUid": "nErXDvCkzz", "url": "/d/cIBgcSjkl/production-overview",
            "updated": "2026-08-10T09:00:00Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"meta":{"url":"/d/x"}}"#)
            .expect_err("a document without the dashboard is not this endpoint's contract")
            .class(),
        ConnectorErrorClass::Validation
    );
    // The alert-rule read demands the identity and the title, which is what
    // Grafana's own schema marks required.
    assert_eq!(
        operation("alert_rule.get")
            .decode_response(200, br#"{"uid":"cIBgcSjkk"}"#)
            .expect_err("a rule carries a title")
            .class(),
        ConnectorErrorClass::Validation
    );
    // The two collections publish the whole document, because Grafana answers
    // each with a bare array.
    assert_eq!(
        operation("alert_rule.list")
            .decode_response(200, b"[]")
            .expect("an empty instance is an empty list"),
        json!([])
    );
    assert_eq!(
        grafana::decode(
            get,
            404,
            &reqwest::header::HeaderMap::new(),
            br#"{"message":"Dashboard not found"}"#
        )
        .expect_err("a 404 is not a success")
        .class(),
        ConnectorErrorClass::Permanent
    );
}
