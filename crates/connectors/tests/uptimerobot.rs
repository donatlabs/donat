//! UptimeRobot connector proofs (spec 027 §3, which adopts spec 023 §4),
//! against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::uptimerobot;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const MONITOR_ID: i64 = 777_712_345;
const INCIDENT_ID: &str = "01J9Z2X6Q4";

fn operation(id: &str) -> &'static Operation {
    uptimerobot::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the uptimerobot declaration publishes {id}"))
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

fn monitor() -> JsonValue {
    json!({
        "id": MONITOR_ID, "friendlyName": "Storefront", "url": "https://shop.example.test",
        "status": "UP", "type": "HTTP", "interval": 300,
        "lastIncidentId": INCIDENT_ID, "createDateTime": "2026-08-01T09:00:00Z",
    })
}

fn incident() -> JsonValue {
    json!({
        "id": INCIDENT_ID, "status": "RESOLVED", "reason": "Connection Timeout",
        "duration": 420, "startedAt": "2026-08-10T09:00:00Z",
        "resolvedAt": "2026-08-10T09:07:00Z",
    })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("monitor.list", json!({})),
        ("monitor.get", json!({ "monitor_id": MONITOR_ID })),
        ("monitor.pause", json!({ "monitor_id": MONITOR_ID })),
        ("incident.list", json!({})),
        ("incident.get", json!({ "incident_id": INCIDENT_ID })),
        ("incident_alert.list", json!({ "incident_id": INCIDENT_ID })),
        (
            "incident_comment.list",
            json!({ "incident_id": INCIDENT_ID }),
        ),
        (
            "incident_comment.create",
            json!({ "incident_id": INCIDENT_ID, "content": "Root cause found." }),
        ),
    ]
}

/// `uptimerobot_request_shape`: exact method, path, query, headers and body for
/// every operation, all under the `/v3` server the published OpenAPI declares.
#[tokio::test]
async fn uptimerobot_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v3/monitors")
            .query("limit=200")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, json!({ "data": [monitor()], "nextLink": null })),
        Expectation::new("GET", &format!("/v3/monitors/{MONITOR_ID}"))
            .query("")
            .respond_json(200, monitor()),
        Expectation::new("POST", &format!("/v3/monitors/{MONITOR_ID}/pause"))
            .respond_json(200, json!({})),
        Expectation::new("GET", "/v3/incidents")
            .query("")
            .respond_json(200, json!({ "data": [incident()], "nextLink": null })),
        Expectation::new("GET", &format!("/v3/incidents/{INCIDENT_ID}"))
            .query("")
            .respond_json(200, incident()),
        Expectation::new("GET", &format!("/v3/incidents/{INCIDENT_ID}/alerts"))
            .query("")
            .respond_json(
                200,
                json!({ "data": [{ "timestamp": "2026-08-10T09:00:05Z",
                                   "recipientName": "oncall", "recipientValue": "x@example.test",
                                   "channelType": "EMAIL", "status": "SUCCESS" }] }),
            ),
        Expectation::new("GET", &format!("/v3/incidents/{INCIDENT_ID}/comments"))
            .query("limit=100")
            .respond_json(200, json!({ "data": [], "nextLink": null })),
        Expectation::new("POST", &format!("/v3/incidents/{INCIDENT_ID}/comments"))
            .json_body(json!({ "content": "Root cause found." }))
            .respond_bytes(201, Vec::new()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/v3/"),
            "{id} renders the published server path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `uptimerobot_auth_is_applied`: the API token reaches the wire as the bearer
/// header UptimeRobot's own v3 OpenAPI declares — and never in a body, which is
/// the whole point of declaring v3 rather than v2 (spec 027 §1).
#[tokio::test]
async fn uptimerobot_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/v3/monitors/{MONITOR_ID}"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, monitor()),
        ])
        .await;

    let request = render(&stub, "monitor.get", json!({ "monitor_id": MONITOR_ID }));
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

    // The v2 form put `api_key` in the request body. Nothing in this connector's
    // compiled request contract has a slot a credential could reach: no
    // operation declares an `api_key` input, and none of the bodies name one.
    for operation in uptimerobot::connector().operations() {
        let projection = operation.project();
        assert!(
            !projection
                .inputs()
                .iter()
                .any(|input| input.name().to_ascii_lowercase().contains("api_key")),
            "{} declares no credential-shaped input",
            operation.id()
        );
        assert!(
            !format!("{:?}", projection.body())
                .to_ascii_lowercase()
                .contains("api_key"),
            "{} sends no credential in its body",
            operation.id()
        );
    }

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        uptimerobot::connector().credential(),
        uptimerobot::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `uptimerobot_error_map`: every documented status reaches exactly one closed
/// class, and none of UptimeRobot's prose crosses the boundary.
#[tokio::test]
async fn uptimerobot_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", &format!("/v3/monitors/{MONITOR_ID}")).respond_json(
                    status,
                    json!({ "message": format!("api.uptimerobot.com rejected {SECRET_SENTINEL}"),
                    "error": "monitor_not_found" }),
                ),
            ])
            .await;
        let response = stub
            .send(render(
                &stub,
                "monitor.get",
                json!({ "monitor_id": MONITOR_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = uptimerobot::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "api.uptimerobot.com", "monitor_not_found"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `uptimerobot_rate_limit_is_classified`: "We will return 429 HTTP status code
/// in the response from API, when you hit the rate limits", with the documented
/// "Retry-After - Number of second after you should retry the call", clamped.
#[tokio::test]
async fn uptimerobot_rate_limit_is_classified() {
    let path = format!("/v3/monitors/{MONITOR_ID}");
    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .respond_header("x-ratelimit-limit", "10")
            .respond_header("x-ratelimit-remaining", "0")
            .respond_header("retry-after", "42")
            .respond_json(429, json!({ "message": "rate limited" })),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "message": "rate limited" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "monitor.get",
                json!({ "monitor_id": MONITOR_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(uptimerobot::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(42)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `uptimerobot_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): UptimeRobot
/// publishes a `nextLink` it never describes, so no plan is declared, the
/// continuation never becomes a destination, and one attempt is exactly one
/// request.
#[tokio::test]
async fn uptimerobot_cursor_is_opaque_and_bounded() {
    for id in [
        "monitor.list",
        "monitor.get",
        "monitor.pause",
        "incident.list",
        "incident.get",
        "incident_alert.list",
        "incident_comment.list",
        "incident_comment.create",
    ] {
        assert!(
            uptimerobot::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }

    // The continuation the provider does publish is *data*: it lands in the
    // declared output, and nothing in the connector spends it.
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/monitors")
        .query("limit=200")
        .respond_json(
            200,
            json!({ "data": [monitor()],
                    "nextLink": "https://attacker.invalid/v3/monitors?cursor=2" }),
        )])
    .await;
    let response = stub
        .send(render(&stub, "monitor.list", json!({})))
        .await
        .expect("the stub answers");
    let output = uptimerobot::decode(
        operation("monitor.list"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("nextLink"),
        Some(&json!("https://attacker.invalid/v3/monitors?cursor=2")),
        "the continuation is published as data a Process can read"
    );
    assert_eq!(
        stub.received(),
        1,
        "one attempt is one request: nothing follows a link this connector declared no plan for"
    );
    stub.assert_satisfied();
}

/// `uptimerobot_pagination_is_bounded`: with no plan declared, every collection
/// asks for one page of the provider's own published maximum.
#[test]
fn uptimerobot_pagination_is_bounded() {
    let queries: Vec<(String, String)> = uptimerobot::connector()
        .operations()
        .iter()
        .flat_map(|operation| {
            operation
                .project()
                .query()
                .iter()
                .map(|query| (operation.id().to_owned(), format!("{query:?}")))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        queries
            .iter()
            .any(|(id, query)| id == "monitor.list" && query.contains("200")),
        "the monitor list asks for the published maximum: {queries:?}"
    );
    assert!(
        queries
            .iter()
            .any(|(id, query)| id == "incident_comment.list" && query.contains("100")),
        "the comment list asks for the published maximum: {queries:?}"
    );
    // And every declared page size is a static, never an input a Process could
    // grow past the provider's ceiling.
    assert!(
        !queries.iter().any(|(_, query)| query.contains("Input")),
        "no page size binds from input: {queries:?}"
    );
}

/// `uptimerobot_effects_are_classified`: every operation carries a class, and
/// the write the provider documents as repeat-safe over a `POST` stays
/// unreachable rather than trading the retry away.
#[test]
fn uptimerobot_effects_are_classified() {
    let expected = [
        ("monitor.list", EffectClass::ReadOnly),
        ("monitor.get", EffectClass::ReadOnly),
        ("monitor.pause", EffectClass::InventoryOnly),
        ("incident.list", EffectClass::ReadOnly),
        ("incident.get", EffectClass::ReadOnly),
        ("incident_alert.list", EffectClass::ReadOnly),
        ("incident_comment.list", EffectClass::ReadOnly),
        ("incident_comment.create", EffectClass::AtMostOnce),
    ];
    assert_eq!(uptimerobot::connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            uptimerobot::connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        uptimerobot::connector().admit_operation("monitor.pause"),
        Err(OperationRejection::InventoryOnly)
    );

    // The reason quotes UptimeRobot's own repeat statement and says why a
    // documented repeat-safe `POST` is not admitted by either executable class.
    let reason = operation("monitor.pause")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("This operation is idempotent"), "{reason}");
    assert!(reason.contains("NaturalMethod"), "{reason}");
    assert!(
        reason.contains("trade away a retry"),
        "the reason says why AtMostOnce is the wrong home too: {reason}"
    );

    let evidence = operation("incident_comment.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(
        evidence
            .searched_documentation()
            .contains("occurs four times"),
        "{}",
        evidence.searched_documentation()
    );
    assert!(evidence.repeat_produces().contains("second comment"));
}

/// `uptimerobot_triggering_is_not_a_read` (spec 027 §3): pausing a monitor stops
/// the checks that page a human, so it is not a read — and this connector
/// classifies it unreachable rather than merely non-`ReadOnly`.
#[test]
fn uptimerobot_triggering_is_not_a_read() {
    for id in ["monitor.pause", "incident_comment.create"] {
        let class = operation(id)
            .effect_class()
            .expect("every operation has one");
        assert_ne!(class, EffectClass::ReadOnly, "{id} has a consequence");
    }
    assert!(
        !operation("monitor.pause")
            .effect_class()
            .expect("a class")
            .is_executable(),
        "stopping the monitoring is unreachable from a Process"
    );
    assert!(
        operation("incident_comment.create")
            .effect_class()
            .expect("a class")
            .requires_at_most_once_opt_in()
    );
    for id in [
        "monitor.list",
        "monitor.get",
        "incident.list",
        "incident.get",
    ] {
        assert_eq!(operation(id).effect_class(), Some(EffectClass::ReadOnly));
    }
}

/// `uptimerobot_output_contract`: the declared pointers read UptimeRobot's own
/// v3 objects, with its own typing.
#[test]
fn uptimerobot_output_contract() {
    let get = operation("monitor.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&monitor()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": MONITOR_ID, "friendlyName": "Storefront", "url": "https://shop.example.test",
            "status": "UP", "type": "HTTP", "interval": 300,
            "lastIncidentId": INCIDENT_ID, "createDateTime": "2026-08-01T09:00:00Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"777712345"}"#)
            .expect_err("an id that is not a number is not a monitor")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":777712345}"#)
            .expect("only the identity is required")
            .get("status"),
        Some(&json!(null))
    );
    // The pause publishes no response schema, so an empty success is a success.
    assert!(operation("monitor.pause").decode_response(200, b"").is_ok());
    assert_eq!(
        uptimerobot::decode(
            get,
            404,
            &reqwest::header::HeaderMap::new(),
            br#"{"message":"Monitor not found"}"#
        )
        .expect_err("a 404 is not a success")
        .class(),
        ConnectorErrorClass::Permanent
    );
}
