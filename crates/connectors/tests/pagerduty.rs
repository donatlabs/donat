//! PagerDuty connector proofs (spec 027 §3, which adopts spec 023 §4), against
//! the SDK's local provider stub.

use std::sync::LazyLock;
use std::time::Duration;

use donat_connectors::providers::pagerduty;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const FROM: &str = "oncall@example.test";
const INCIDENT_ID: &str = "PT4KHLK";
const ALERT_ID: &str = "PT4KHLA";
const SERVICE_ID: &str = "PIJ90N7";
const ACCEPT: &str = "application/vnd.pagerduty+json;version=2";

fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> =
        LazyLock::new(|| pagerduty::connector(FROM).expect("a valid From address declares"));
    &CONNECTOR
}

fn operation(id: &str) -> &'static Operation {
    connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the pagerduty declaration publishes {id}"))
}

/// "The API Key with format `Token token=<API_KEY>`."
fn expected_authorization() -> String {
    format!("Token token={SECRET_SENTINEL}")
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_authorization_parameter("Token", "token")
        .expect("the published credential form is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn incident() -> JsonValue {
    json!({
        "incident": {
            "id": INCIDENT_ID,
            "incident_number": 1234,
            "title": "The server is on fire",
            "status": "triggered",
            "urgency": "high",
            "incident_key": "srv01-disk",
            "created_at": "2026-08-10T09:00:00Z",
            "html_url": "https://acme.pagerduty.com/incidents/PT4KHLK",
        }
    })
}

fn alert() -> JsonValue {
    json!({
        "alert": {
            "id": ALERT_ID,
            "summary": "disk full",
            "status": "triggered",
            "alert_key": "srv01-disk",
            "severity": "critical",
            "created_at": "2026-08-10T09:00:00Z",
        }
    })
}

fn note() -> JsonValue {
    json!({
        "note": { "id": "PWL7QXS", "content": "Firefighters are on the scene.",
                  "created_at": "2026-08-10T09:05:00Z" }
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("incident.get", json!({ "incident_id": INCIDENT_ID })),
        (
            "incident.list",
            json!({ "status": "triggered", "service_id": SERVICE_ID }),
        ),
        (
            "incident.create",
            json!({ "title": "The server is on fire", "service_id": SERVICE_ID,
                    "urgency": "high", "incident_key": "srv01-disk", "details": "disk full" }),
        ),
        (
            "incident.update",
            json!({ "incident_id": INCIDENT_ID, "status": "resolved",
                    "resolution": "restarted", "urgency": null }),
        ),
        ("incident_note.list", json!({ "incident_id": INCIDENT_ID })),
        (
            "incident_note.create",
            json!({ "incident_id": INCIDENT_ID, "content": "Firefighters are on the scene." }),
        ),
        (
            "alert.get",
            json!({ "incident_id": INCIDENT_ID, "alert_id": ALERT_ID }),
        ),
        (
            "alert.list",
            json!({ "incident_id": INCIDENT_ID, "status": "triggered" }),
        ),
    ]
}

/// `pagerduty_request_shape`: exact method, path, query, headers, and body for
/// every operation, with the published versioning header on all of them and the
/// deployment's `From` identity on exactly the four writes.
#[tokio::test]
async fn pagerduty_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}"))
            .query("")
            .header("accept", ACCEPT)
            .header("authorization", &expected_authorization())
            .without_header("from")
            .no_body()
            .respond_json(200, incident()),
        Expectation::new("GET", "/incidents")
            .query(&format!("statuses[]=triggered&service_ids[]={SERVICE_ID}"))
            .without_header("from")
            .respond_json(200, json!({ "incidents": [], "more": false })),
        Expectation::new("POST", "/incidents")
            .header("from", FROM)
            .json_body(json!({
                "incident": {
                    "type": "incident",
                    "title": "The server is on fire",
                    "service": { "id": SERVICE_ID, "type": "service_reference" },
                    "urgency": "high",
                    "incident_key": "srv01-disk",
                    "body": { "type": "incident_body", "details": "disk full" },
                }
            }))
            .respond_json(201, incident()),
        Expectation::new("PUT", &format!("/incidents/{INCIDENT_ID}"))
            .header("from", FROM)
            .json_body(json!({
                "incident": { "type": "incident", "status": "resolved",
                              "resolution": "restarted", "urgency": null }
            }))
            .respond_json(200, incident()),
        Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}/notes"))
            .query("")
            .respond_json(200, json!({ "notes": [] })),
        Expectation::new("POST", &format!("/incidents/{INCIDENT_ID}/notes"))
            .header("from", FROM)
            .json_body(json!({ "note": { "content": "Firefighters are on the scene." } }))
            .respond_json(200, note()),
        Expectation::new(
            "GET",
            &format!("/incidents/{INCIDENT_ID}/alerts/{ALERT_ID}"),
        )
        .query("")
        .respond_json(200, alert()),
        Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}/alerts"))
            .query("statuses[]=triggered")
            .respond_json(200, json!({ "alerts": [], "more": false })),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert_eq!(
            request
                .headers()
                .get("accept")
                .and_then(|value| value.to_str().ok()),
            Some(ACCEPT),
            "{id} sends the published versioning header"
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `pagerduty_auth_is_applied`: the key reaches the wire as PagerDuty's own
/// authentication parameter and nowhere else.
#[tokio::test]
async fn pagerduty_auth_is_applied() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}"))
            .header("authorization", &expected_authorization())
            .respond_json(200, incident()),
    ])
    .await;

    let request = render(&stub, "incident.get", json!({ "incident_id": INCIDENT_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("an ASCII header"),
        expected_authorization()
    );
    // Not `Bearer <key>` and not `Token <key>`: PagerDuty publishes one form and
    // the other two authenticate as nobody.
    assert!(!applied.to_str().expect("ASCII").starts_with("Bearer "));
    assert_eq!(
        applied.to_str().expect("ASCII"),
        format!("Token token={SECRET_SENTINEL}")
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        connector().credential(),
        pagerduty::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();

    // The `From` identity is a deployment's, refused here rather than at the
    // provider's first `400`.
    for hostile in [
        "",
        "   ",
        "oncall",
        "oncall@example",
        "on call@example.test",
        "a@b.c\r\n",
    ] {
        assert!(pagerduty::connector(hostile).is_err(), "`{hostile}`");
    }
    assert!(pagerduty::connector("oncall@example.test").is_ok());
    assert!(pagerduty::declaration_shape().is_ok());
}

/// The `From` header is a compiled identity: nothing in an input, a provider
/// body, or a continuation can move it, because there is no slot for it.
#[tokio::test]
async fn pagerduty_from_identity_comes_only_from_deploy_time_configuration() {
    let stub = ProviderStub::start([Expectation::new("POST", "/incidents")
        .header("from", FROM)
        .respond_json(201, incident())])
    .await;

    // 1. Operation input. An input named `From` is not a declared slot, and the
    //    rendered header is still the compiled one.
    let request = render(
        &stub,
        "incident.create",
        json!({
            "title": "t", "service_id": SERVICE_ID, "urgency": null,
            "incident_key": null, "details": null,
            "From": "attacker@example.invalid", "from": "attacker@example.invalid",
        }),
    );
    assert_eq!(
        request
            .headers()
            .get("from")
            .and_then(|value| value.to_str().ok()),
        Some(FROM)
    );
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    // 2. The declaration publishes no input slot that could reach it.
    let projection = operation("incident.create").project();
    assert!(
        !projection
            .inputs()
            .iter()
            .any(|input| input.name().eq_ignore_ascii_case("from")),
        "the compiled identity is not an input a Process can choose"
    );

    // 3. A provider body naming another address is data, not a destination.
    let output = operation("incident.create")
        .extract_output(&json!({ "incident": { "id": "PX", "title": "attacker@example.invalid" } }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("title"),
        Some(&json!("attacker@example.invalid"))
    );
}

/// `pagerduty_error_map`: every documented status reaches exactly one closed
/// class, and none of PagerDuty's prose crosses the boundary.
#[tokio::test]
async fn pagerduty_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (402, ConnectorErrorClass::Permanent),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (413, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}")).respond_json(
                    status,
                    json!({
                        "error": {
                            "message": format!("acme.pagerduty.com rejected {SECRET_SENTINEL}"),
                            "code": 2001,
                            "errors": { "incident.title": ["can't be blank"] },
                        }
                    }),
                ),
            ])
            .await;
        let response = stub
            .send(render(
                &stub,
                "incident.get",
                json!({ "incident_id": INCIDENT_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = pagerduty::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme.pagerduty.com", "can't be blank"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `pagerduty_rate_limit_is_classified`: "Too many requests have been made, the
/// rate limit has been reached" is retryable, and its hint is clamped.
#[tokio::test]
async fn pagerduty_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}")).respond_json(
            429,
            json!({ "error": { "message": "slow down", "code": 2020 } }),
        ),
        Expectation::new("GET", &format!("/incidents/{INCIDENT_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(
                429,
                json!({ "error": { "message": "slow down", "code": 2020 } }),
            ),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "incident.get",
                json!({ "incident_id": INCIDENT_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(pagerduty::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "PagerDuty publishes no Retry-After for the REST API, so the connector invents none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `pagerduty_cursor_is_opaque_and_bounded`: the continuation is PagerDuty's own
/// `offset`/`limit` regime, it ends on the absence of a full page rather than on
/// the `more` flag no plan reads, and the walk makes exactly the number of
/// requests the plan declares (ADR 058).
#[tokio::test]
async fn pagerduty_cursor_is_opaque_and_bounded() {
    let plan = pagerduty::pagination("incident.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let full: Vec<JsonValue> = (0..100).map(|index| json!({ "id": index })).collect();
    let stub = ProviderStub::start([
        Expectation::new("GET", "/incidents")
            .query(&format!(
                "statuses[]=triggered&service_ids[]={SERVICE_ID}&offset=0&limit=100"
            ))
            .respond_json(200, json!({ "incidents": full, "more": true, "offset": 0 })),
        Expectation::new("GET", "/incidents")
            .query(&format!(
                "statuses[]=triggered&service_ids[]={SERVICE_ID}&offset=100&limit=100"
            ))
            .respond_json(
                200,
                json!({ "incidents": [{ "id": 100 }], "more": false, "offset": 100 }),
            ),
    ])
    .await;

    let incidents = plan
        .collect(
            render(
                &stub,
                "incident.list",
                json!({ "status": "triggered", "service_id": SERVICE_ID }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows the offset and stops on a short page");
    assert_eq!(incidents.len(), 101);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `pagerduty_pagination_is_bounded`: the declared plan terminates and respects
/// the call, page, item, and byte budgets, and the collections that publish no
/// regime declare no plan.
#[tokio::test]
async fn pagerduty_pagination_is_bounded() {
    let plan = pagerduty::pagination("incident.list").expect("the list declares a plan");
    let full: Vec<JsonValue> = (0..100).map(|index| json!({ "id": index })).collect();
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/incidents")
                .respond_json(200, json!({ "incidents": full.clone(), "more": true }))
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "incident.list",
                    json!({ "status": "triggered", "service_id": SERVICE_ID }),
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
        pagerduty::pagination("incident.list")
            .expect("the list declares a plan")
            .items_pointer(),
        "/incidents"
    );
    assert_eq!(
        pagerduty::pagination("alert.list")
            .expect("the alert list declares a plan")
            .items_pointer(),
        "/alerts"
    );
    for id in [
        "incident.get",
        "incident.create",
        "incident.update",
        // The note reference publishes no `limit` and no `offset`, so there is
        // no regime to walk.
        "incident_note.list",
        "incident_note.create",
        "alert.get",
    ] {
        assert!(
            pagerduty::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `pagerduty_effects_are_classified`: every operation carries a class, and the
/// deduplication key PagerDuty publishes on the create endpoint is quoted inside
/// the class that refused it.
#[test]
fn pagerduty_effects_are_classified() {
    let expected = [
        ("incident.get", EffectClass::ReadOnly),
        ("incident.list", EffectClass::ReadOnly),
        ("incident.create", EffectClass::AtMostOnce),
        ("incident.update", EffectClass::InventoryOnly),
        ("incident_note.list", EffectClass::ReadOnly),
        ("incident_note.create", EffectClass::AtMostOnce),
        ("alert.get", EffectClass::ReadOnly),
        ("alert.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        // Nothing here binds a key: `ExplicitKey` is the only class that does,
        // and no operation reached it.
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector().admit_operation("incident.update"),
        Err(OperationRejection::InventoryOnly)
    );

    // The evidence carries PagerDuty's own mechanism and the sentence that
    // disqualifies it, so a reviewer sees both in one place (ADR 067's shape,
    // recorded for this provider in ADR 080).
    let evidence = operation("incident.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    let searched = evidence.searched_documentation();
    assert!(searched.contains("incident_key"), "{searched}");
    assert!(
        searched.contains("rejected if an open incident matches"),
        "the escape clause is quoted, not summarised: {searched}"
    );
    assert!(searched.contains("no published retention"), "{searched}");
    assert!(
        evidence.repeat_produces().contains("second incident"),
        "{}",
        evidence.repeat_produces()
    );

    // `incident_key` is still a declared input — a deployment may send
    // PagerDuty's own key — and nothing in the runtime writes it.
    assert!(
        operation("incident.create")
            .project()
            .inputs()
            .iter()
            .any(|input| input.name() == "incident_key"),
        "the provider's own key stays available to a caller"
    );
}

/// `pagerduty_triggering_is_not_a_read` (spec 027 §3): opening an incident,
/// commenting on one, and resolving one all look like small requests and each
/// has a consequence, so none of them may be classified `ReadOnly`.
#[test]
fn pagerduty_triggering_is_not_a_read() {
    for id in ["incident.create", "incident_note.create", "incident.update"] {
        let class = operation(id)
            .effect_class()
            .expect("every operation has one");
        assert_ne!(class, EffectClass::ReadOnly, "{id} pages a human");
        assert!(
            matches!(class, EffectClass::AtMostOnce | EffectClass::InventoryOnly),
            "{id} carries {class}"
        );
        // And each executable one is reachable only through an activity that
        // says what an unknown outcome means.
        if class.is_executable() {
            assert!(class.requires_at_most_once_opt_in(), "{id}");
        }
    }
}

/// `pagerduty_output_contract`: the declared pointers read PagerDuty's own
/// wrapped objects, with its own typing.
#[test]
fn pagerduty_output_contract() {
    let get = operation("incident.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&incident()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": INCIDENT_ID, "incident_number": 1234, "title": "The server is on fire",
            "status": "triggered", "urgency": "high", "incident_key": "srv01-disk",
            "created_at": "2026-08-10T09:00:00Z",
            "html_url": "https://acme.pagerduty.com/incidents/PT4KHLK",
        })
    );
    // A response that is not the wrapped incident does not satisfy the contract.
    assert_eq!(
        get.decode_response(200, br#"{"id":"PT4KHLK"}"#)
            .expect_err("an unwrapped body is not this endpoint's contract")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"incident":{"id":1234}}"#)
            .expect_err("an id that is not a string is not an incident")
            .class(),
        ConnectorErrorClass::Validation
    );
    // Only the identity is demanded; PagerDuty omits fields a request did not
    // ask to include.
    assert_eq!(
        get.decode_response(200, br#"{"incident":{"id":"PT4KHLK"}}"#)
            .expect("only the identity is required")
            .get("urgency"),
        Some(&json!(null))
    );
    // The declared decoder answers a declared failure status through the error
    // map rather than through the pointers.
    assert_eq!(
        pagerduty::decode(
            get,
            404,
            &reqwest::header::HeaderMap::new(),
            br#"{"error":{"message":"Not Found","code":2100}}"#
        )
        .expect_err("a 404 is not a success")
        .class(),
        ConnectorErrorClass::Permanent
    );
}
