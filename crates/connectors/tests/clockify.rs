//! Clockify connector proofs (spec 028 §4, which adopts spec 023 §4), against
//! the SDK's local provider stub.
//!
//! Clockify is this half of the batch's third shape of deploy-time value: the
//! workspace is a *path* segment rather than a header or a host, so the proof
//! that nothing but configuration can move it is
//! `clockify_workspace_comes_only_from_deploy_time_configuration`, the analogue
//! of spec 023 §4's templated-host proof.

use std::time::Duration;

use donat_connectors::providers::clockify;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    Connector, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const WORKSPACE: &str = "64a687e29ae1f428e7ebe303";
const USER_ID: &str = "5a0ab5acb07987125438b60f";
const TIME_ENTRY_ID: &str = "64c777ddd3fcab07cfbb210c";
const PROJECT_ID: &str = "25b687e29ae1f428e7ebe123";

fn connector() -> Connector {
    clockify::connector(WORKSPACE).expect("a configured Clockify workspace declares")
}

fn operation(connector: &Connector, id: &str) -> Operation {
    connector
        .operation(id)
        .unwrap_or_else(|| panic!("the clockify declaration publishes {id}"))
        .clone()
}

fn render(stub: &ProviderStub, connector: &Connector, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(connector, id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    connector
        .credential()
        .plan()
        .expect("clockify declares a credential plan")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the configured key");
    request
}

fn time_entry() -> JsonValue {
    json!({
        "id": TIME_ENTRY_ID,
        "description": "Batch L",
        "billable": true,
        "projectId": PROJECT_ID,
        "taskId": JsonValue::Null,
        "userId": USER_ID,
        "workspaceId": WORKSPACE,
        "tagIds": [],
        "timeInterval": {
            "start": "2026-08-03T09:00:00Z",
            "end": "2026-08-03T11:00:00Z",
            "duration": "PT2H",
        },
    })
}

fn scoped(suffix: &str) -> String {
    format!("/api/v1/workspaces/{WORKSPACE}{suffix}")
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("user.me", json!({})),
        ("time_entry.get", json!({ "time_entry_id": TIME_ENTRY_ID })),
        (
            "time_entry.list",
            json!({
                "user_id": USER_ID, "start": "2026-08-01T00:00:00Z",
                "end": "2026-08-31T23:59:59Z",
            }),
        ),
        (
            "time_entry.create",
            json!({
                "start": "2026-08-03T09:00:00Z", "end": "2026-08-03T11:00:00Z",
                "billable": true, "description": "Batch L", "projectId": PROJECT_ID,
                "taskId": JsonValue::Null, "tagIds": [],
            }),
        ),
        (
            "time_entry.update",
            json!({
                "time_entry_id": TIME_ENTRY_ID,
                "start": "2026-08-03T09:00:00Z", "end": "2026-08-03T12:00:00Z",
                "billable": true, "description": "Batch L, revised",
                "projectId": PROJECT_ID, "taskId": JsonValue::Null, "tagIds": [],
            }),
        ),
        ("project.get", json!({ "project_id": PROJECT_ID })),
        (
            "project.list",
            json!({ "name": "Connectors", "archived": false }),
        ),
        ("client.list", json!({ "name": "ACME", "archived": false })),
    ]
}

/// `clockify_request_shape`: exact method, path, query, headers, and body for
/// every operation, each workspace-scoped one under this deployment's
/// workspace.
#[tokio::test]
async fn clockify_request_shape() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v1/user")
            .query("")
            .header("x-api-key", SECRET_SENTINEL)
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, json!({ "id": USER_ID, "email": "kim@example.test" })),
        Expectation::new("GET", &scoped(&format!("/time-entries/{TIME_ENTRY_ID}")))
            .respond_json(200, time_entry()),
        Expectation::new("GET", &scoped(&format!("/user/{USER_ID}/time-entries")))
            .query(
                "start=2026%2D08%2D01T00%3A00%3A00Z&end=2026%2D08%2D31T23%3A59%3A59Z&page-size=50",
            )
            .respond_json(200, json!([time_entry()])),
        Expectation::new("POST", &scoped("/time-entries"))
            .json_body(json!({
                "start": "2026-08-03T09:00:00Z", "end": "2026-08-03T11:00:00Z",
                "billable": true, "description": "Batch L", "projectId": PROJECT_ID,
                "taskId": JsonValue::Null, "tagIds": [],
            }))
            .respond_json(201, time_entry()),
        Expectation::new("PUT", &scoped(&format!("/time-entries/{TIME_ENTRY_ID}")))
            .json_body(json!({
                "start": "2026-08-03T09:00:00Z", "end": "2026-08-03T12:00:00Z",
                "billable": true, "description": "Batch L, revised",
                "projectId": PROJECT_ID, "taskId": JsonValue::Null, "tagIds": [],
            }))
            .respond_json(200, time_entry()),
        Expectation::new("GET", &scoped(&format!("/projects/{PROJECT_ID}")))
            .respond_json(200, json!({ "id": PROJECT_ID, "name": "Connectors" })),
        Expectation::new("GET", &scoped("/projects"))
            .query("name=Connectors&archived=false&page-size=50")
            .respond_json(200, json!([])),
        Expectation::new("GET", &scoped("/clients"))
            .query("name=ACME&archived=false&page-size=50")
            .respond_json(200, json!([])),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, &connector, id, input);
        assert!(
            request.url().path().starts_with("/api/v1/"),
            "{id} renders under Clockify's own version prefix: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `clockify_auth_is_applied`: the configured key reaches the wire as the
/// `X-Api-Key` header Clockify publishes, it is redacted everywhere else, and a
/// request with no key never renders.
#[tokio::test]
async fn clockify_auth_is_applied() {
    let connector = connector();
    let stub = ProviderStub::start([Expectation::new("GET", "/api/v1/user")
        .header("x-api-key", SECRET_SENTINEL)
        .respond_json(200, json!({ "id": USER_ID }))])
    .await;

    let request = render(&stub, &connector, "user.me", json!({}));
    let applied = request
        .headers()
        .get("x-api-key")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!request.url_carries_credential());
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{connector:?}").contains(SECRET_SENTINEL));

    // The declared credential contract names the secret and the non-secret
    // workspace, and carries neither value.
    let fields = connector.credential().fields();
    assert_eq!(fields.len(), 2, "{fields:?}");
    assert!(
        fields
            .iter()
            .any(|field| field.name() == "secret" && field.is_secret())
    );
    assert!(
        fields
            .iter()
            .any(|field| field.name() == clockify::WORKSPACE_ID && !field.is_secret())
    );

    let mut unauthenticated = operation(&connector, "user.me")
        .plan_request(&stub.origin(), &json!({}))
        .expect("the declared request renders");
    let failure = connector
        .credential()
        .plan()
        .expect("clockify declares a plan")
        .apply(&Credential::from_fields([]), &mut unauthenticated, None)
        .expect_err("a connector with no configured key cannot send");
    assert_eq!(failure.code(), "connector_credential_missing_field");

    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `clockify_workspace_comes_only_from_deploy_time_configuration` (spec 023 §4
/// proof 1, in the shape Clockify's URL grammar gives it): input, a provider
/// body, and a continuation each fail to move the workspace a request reaches.
#[tokio::test]
async fn clockify_workspace_comes_only_from_deploy_time_configuration() {
    let connector = connector();
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &scoped(&format!("/time-entries/{TIME_ENTRY_ID}")),
    )
    .respond_json(200, time_entry())])
    .await;
    stub.send(render(
        &stub,
        &connector,
        "time_entry.get",
        json!({ "time_entry_id": TIME_ENTRY_ID }),
    ))
    .await
    .expect("the stub answers on this deployment's workspace");

    // 1. Operation input. Every scoped path renders under the configured
    //    workspace whatever the input says, and no operation publishes the
    //    workspace as a slot at all.
    for (id, mut input) in cases() {
        if let Some(fields) = input.as_object_mut() {
            fields.insert(
                clockify::WORKSPACE_ID.to_owned(),
                json!("00000000000000000000dead"),
            );
        }
        let rendered = operation(&connector, id)
            .plan_request(&stub.origin(), &input)
            .expect("the declared request renders");
        assert!(
            rendered.url().path() == "/api/v1/user"
                || rendered
                    .url()
                    .path()
                    .starts_with(&format!("/api/v1/workspaces/{WORKSPACE}/")),
            "{id} renders under this deployment's workspace: {}",
            rendered.url().path()
        );
        assert!(
            !operation(&connector, id)
                .project()
                .inputs()
                .iter()
                .any(|input| input.name() == clockify::WORKSPACE_ID),
            "{id} must not publish the workspace as a Process input"
        );
    }

    // A path value that spells another workspace stays one percent-encoded
    // segment under the configured prefix.
    let escaped = operation(&connector, "time_entry.get")
        .plan_request(
            &stub.origin(),
            &json!({ "time_entry_id": "../../00000000000000000000dead/time-entries/1" }),
        )
        .expect("the declared request renders");
    assert!(
        escaped
            .url()
            .path()
            .starts_with(&format!("/api/v1/workspaces/{WORKSPACE}/time-entries/")),
        "{}",
        escaped.url().path()
    );
    assert!(!escaped.url().path().contains("dead/time-entries/1"));

    // 2. A provider response naming another workspace is data, not a
    //    destination.
    let output = operation(&connector, "time_entry.get")
        .extract_output(&json!({
            "id": TIME_ENTRY_ID, "workspaceId": "00000000000000000000dead",
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("workspaceId"),
        Some(&json!("00000000000000000000dead"))
    );

    // 3. A continuation is a page number the walk derives, never a value the
    //    provider chose, so a body cannot restart or redirect it.
    let walk_stub = ProviderStub::start([
        Expectation::new("GET", &scoped("/projects"))
            .query("name=Connectors&archived=false&page-size=50&page=1")
            .respond_json(
                200,
                json!(
                    (0..50)
                        .map(|index| json!({ "id": index, "workspaceId": "00000000000000000000dead" }))
                        .collect::<Vec<_>>()
                ),
            ),
        Expectation::new("GET", &scoped("/projects"))
            .query("name=Connectors&archived=false&page-size=50&page=2")
            .respond_json(200, json!([])),
    ])
    .await;
    let walked = clockify::pagination("project.list")
        .expect("project.list declares a plan")
        .collect(
            render(
                &walk_stub,
                &connector,
                "project.list",
                json!({ "name": "Connectors", "archived": false }),
            ),
            &walk_stub.origin(),
            &PaginationBudget::new(8, 8, 1_000, 1 << 20, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| walk_stub.send(request),
        )
        .await
        .expect("the walk ends on a short page");
    assert_eq!(walked.len(), 50);
    walk_stub.assert_satisfied();

    // And the configuration itself admits Clockify's own grammar and nothing
    // else.
    for hostile in [
        "",
        "64a687e29ae1f428e7ebe303/../deadbeefdeadbeefdeadbeef",
        "64A687E29AE1F428E7EBE303",
        "64a687e29ae1f428e7ebe30",
        "64a687e29ae1f428e7ebe303x",
        "64a687e2 ae1f428e7ebe303",
    ] {
        assert!(
            clockify::validate_workspace_id(hostile).is_err(),
            "`{hostile}` is not a Clockify workspace id"
        );
        assert!(clockify::connector(hostile).is_err());
    }
    assert!(clockify::validate_workspace_id(WORKSPACE).is_ok());

    stub.assert_satisfied();
}

/// `clockify_error_map`: every documented status reaches exactly one closed
/// class, and neither the key nor Clockify's prose crosses the boundary.
#[tokio::test]
async fn clockify_error_map() {
    let connector = connector();
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
        let stub = ProviderStub::start([Expectation::new("GET", "/api/v1/user").respond_json(
            status,
            json!({ "code": 501, "message": format!("Full authentication for {SECRET_SENTINEL}") }),
        )])
        .await;
        let response = stub
            .send(render(&stub, &connector, "user.me", json!({})))
            .await
            .expect("the stub answers");

        let failure = clockify::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "Full authentication"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `clockify_rate_limit_is_classified`: Clockify's documented throttle reaches
/// `http_429`, and a retry hint it sends is read and clamped.
#[tokio::test]
async fn clockify_rate_limit_is_classified() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v1/user")
            .respond_header("retry-after", "1")
            .respond_json(429, json!({ "message": "Too many requests" })),
        Expectation::new("GET", "/api/v1/user")
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "message": "Too many requests" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, &connector, "user.me", json!({})))
            .await
            .expect("the stub answers");
        failures.push(clockify::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(1)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `clockify_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3, in the shape a
/// page-number regime gives it): the page number is derived by the walk, the
/// walk ends on a short page, and it makes exactly the number of requests the
/// plan declares.
#[tokio::test]
async fn clockify_cursor_is_opaque_and_bounded() {
    let connector = connector();
    let plan = clockify::pagination("client.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let full_page: Vec<JsonValue> = (0..50).map(|index| json!({ "id": index })).collect();
    let stub = ProviderStub::start([
        Expectation::new("GET", &scoped("/clients"))
            .query("name=ACME&archived=false&page-size=50&page=1")
            .respond_json(200, json!(full_page)),
        // Clockify numbers its pages from 1, so the second request is page 2.
        Expectation::new("GET", &scoped("/clients"))
            .query("name=ACME&archived=false&page-size=50&page=2")
            .respond_json(200, json!([{ "id": 50 }])),
    ])
    .await;

    let clients = plan
        .collect(
            render(
                &stub,
                &connector,
                "client.list",
                json!({ "name": "ACME", "archived": false }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk ends on a page shorter than the declared size");
    assert_eq!(clients.len(), 51);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `clockify_pagination_is_bounded`: the declared plan terminates under every
/// budget, and no write or single-record read declares one.
#[tokio::test]
async fn clockify_pagination_is_bounded() {
    let connector = connector();
    let plan = clockify::pagination("project.list").expect("the list declares a plan");
    let full_page: Vec<JsonValue> = (0..50).map(|index| json!({ "id": index })).collect();
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &scoped("/projects")).respond_json(200, json!(full_page))
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    &connector,
                    "project.list",
                    json!({ "name": "Connectors", "archived": false }),
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
        "user.me",
        "time_entry.get",
        "time_entry.create",
        "time_entry.update",
        "project.get",
    ] {
        assert!(
            clockify::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    // Every walked collection is a bare array at the document root.
    for id in ["time_entry.list", "project.list", "client.list"] {
        assert_eq!(
            clockify::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            ""
        );
    }
}

/// `clockify_effects_are_classified`: the create is `AtMostOnce` on a recorded
/// absence and a named consequence, and the `PUT` its provider never described a
/// repeat of stays unreachable.
#[test]
fn clockify_effects_are_classified() {
    let connector = connector();
    let expected = [
        ("user.me", EffectClass::ReadOnly),
        ("time_entry.get", EffectClass::ReadOnly),
        ("time_entry.list", EffectClass::ReadOnly),
        ("time_entry.create", EffectClass::AtMostOnce),
        ("time_entry.update", EffectClass::InventoryOnly),
        ("project.get", EffectClass::ReadOnly),
        ("project.list", EffectClass::ReadOnly),
        ("client.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(&connector, id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation.idempotency_binding().is_none(), "{id}");
    }

    // The sharpest entry in this half of the batch: a `PUT` against a fixed
    // identity that the gate still does not admit, because `NaturalMethod` is
    // evidence and not a method.
    assert_eq!(
        connector.admit_operation("time_entry.update"),
        Err(OperationRejection::InventoryOnly)
    );
    let reason = operation(&connector, "time_entry.update")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records its reason")
        .to_owned();
    assert!(reason.contains("*method* half"), "{reason}");
    assert!(reason.contains("no consequence to record"), "{reason}");

    let evidence = operation(&connector, "time_entry.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on")
        .clone();
    assert!(evidence.searched_documentation().contains("tagIds"));
    assert!(evidence.repeat_produces().contains("a second time entry"));
}

/// `clockify_output_contract`: the declared pointers read Clockify's own
/// objects, and a collection is a bare array at the document root.
#[test]
fn clockify_output_contract() {
    let connector = connector();
    let get = operation(&connector, "time_entry.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&time_entry()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        time_entry()
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"64c777ddd3fcab07cfbb210c"}"#)
            .expect("only the identity is required")
            .get("id"),
        Some(&json!(TIME_ENTRY_ID))
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":42}"#)
            .expect_err("a Clockify id is a string")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        operation(&connector, "project.list")
            .decode_response(200, br#"[{"id":"25b687e29ae1f428e7ebe123"}]"#)
            .expect("a bare array is the whole output"),
        json!([{ "id": PROJECT_ID }])
    );
}
