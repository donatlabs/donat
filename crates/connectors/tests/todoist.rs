//! Todoist connector proofs (spec 024 §3, which adopts spec 023 §4), against the
//! SDK's local provider stub.
//!
//! Todoist is the batch's published-mechanism case, so the effect proof here
//! asserts on the recorded evidence: the mechanism exists, it is the Sync
//! endpoint's command `uuid`, and no retention is published for it.

use std::time::Duration;

use donat_connectors::providers::todoist;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const TASK_ID: &str = "6X7rfFVPjhvv84XG";
const PROJECT_ID: &str = "6Cx9RG9Qh7Wr2WjX";

fn operation(id: &str) -> &'static Operation {
    todoist::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the todoist declaration publishes {id}"))
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

fn task() -> JsonValue {
    json!({
        "id": TASK_ID,
        "content": "Ship the batch",
        "description": "with its evidence",
        "project_id": PROJECT_ID,
        "section_id": null,
        "priority": 1,
        "labels": ["connectors"],
        "due": { "date": "2026-08-20", "is_recurring": false, "string": "Aug 20" },
        "checked": false,
        "added_at": "2026-08-01T11:56:51.000000Z",
        "updated_at": "2026-08-02T11:56:51.000000Z",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("task.get", json!({ "task_id": TASK_ID })),
        ("task.list", json!({ "project_id": PROJECT_ID })),
        ("task.search", json!({ "query": "today | overdue" })),
        (
            "task.create",
            json!({
                "content": "Ship the batch", "description": "with its evidence",
                "project_id": PROJECT_ID, "section_id": null,
                "labels": ["connectors"], "priority": 1, "due_string": "Aug 20",
            }),
        ),
        (
            "task.update",
            json!({
                "task_id": TASK_ID, "content": "Ship the batch", "description": null,
                "labels": [], "priority": 2, "due_string": null,
            }),
        ),
        ("task.close", json!({ "task_id": TASK_ID })),
        ("task.delete", json!({ "task_id": TASK_ID })),
        ("project.get", json!({ "project_id": PROJECT_ID })),
        ("project.list", json!({})),
        ("comment.list", json!({ "task_id": TASK_ID })),
        (
            "comment.create",
            json!({
                "content": "Looks right to me.", "task_id": TASK_ID, "project_id": null,
            }),
        ),
    ]
}

/// `todoist_request_shape`: exact method, path, query, headers, and body for
/// every operation.
#[tokio::test]
async fn todoist_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v1/tasks/{TASK_ID}"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, task()),
        Expectation::new("GET", "/api/v1/tasks")
            .query(&format!("project_id={PROJECT_ID}"))
            .respond_json(200, json!({ "results": [], "next_cursor": null })),
        Expectation::new("GET", "/api/v1/tasks/filter")
            .query("query=today%20%7C%20overdue")
            .respond_json(200, json!({ "results": [], "next_cursor": null })),
        Expectation::new("POST", "/api/v1/tasks")
            .json_body(json!({
                "content": "Ship the batch", "description": "with its evidence",
                "project_id": PROJECT_ID, "section_id": null,
                "labels": ["connectors"], "priority": 1, "due_string": "Aug 20",
            }))
            .respond_json(200, task()),
        Expectation::new("POST", &format!("/api/v1/tasks/{TASK_ID}"))
            .json_body(json!({
                "content": "Ship the batch", "description": null,
                "labels": [], "priority": 2, "due_string": null,
            }))
            .respond_json(200, task()),
        Expectation::new("POST", &format!("/api/v1/tasks/{TASK_ID}/close"))
            .respond_json(200, json!(true)),
        Expectation::new("DELETE", &format!("/api/v1/tasks/{TASK_ID}"))
            .respond_json(200, json!(true)),
        Expectation::new("GET", &format!("/api/v1/projects/{PROJECT_ID}")).respond_json(
            200,
            json!({ "id": PROJECT_ID, "name": "Connectors", "color": "charcoal",
                        "is_archived": false, "is_favorite": true }),
        ),
        Expectation::new("GET", "/api/v1/projects")
            .query("")
            .respond_json(200, json!({ "results": [], "next_cursor": null })),
        Expectation::new("GET", "/api/v1/comments")
            .query(&format!("task_id={TASK_ID}"))
            .respond_json(200, json!({ "results": [], "next_cursor": null })),
        Expectation::new("POST", "/api/v1/comments")
            .json_body(json!({
                "content": "Looks right to me.", "task_id": TASK_ID, "project_id": null,
            }))
            .respond_json(
                200,
                json!({ "id": "2992679862", "content": "Looks right to me.",
                        "posted_at": "2026-08-02T11:56:51.000000Z" }),
            ),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v1/"),
            "{id} renders a published Todoist path: {}",
            request.url().path()
        );
        assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `todoist_auth_is_applied`: the token reaches the wire as the `Bearer`
/// credential Todoist publishes, and appears nowhere else.
#[tokio::test]
async fn todoist_auth_is_applied() {
    let stub = ProviderStub::start(
        [Expectation::new("GET", &format!("/api/v1/tasks/{TASK_ID}"))
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .respond_json(200, task())],
    )
    .await;

    let request = render(&stub, "task.get", json!({ "task_id": TASK_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!request.url_carries_credential());
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{:?}", todoist::connector().credential()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", todoist::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `todoist_error_map`: every documented status reaches exactly one closed
/// class, and none of Todoist's prose crosses the boundary.
#[tokio::test]
async fn todoist_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v1/tasks/{TASK_ID}"),
        )
        .respond_json(
            status,
            json!({
                "detail": [{
                    "loc": ["body", "content"],
                    "msg": format!("field required for {SECRET_SENTINEL}"),
                    "type": "value_error.missing",
                }]
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "task.get", json!({ "task_id": TASK_ID })))
            .await
            .expect("the stub answers");

        let failure = todoist::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "field required", "value_error"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `todoist_rate_limit_is_classified`: the documented rate-limit response
/// reaches `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn todoist_rate_limit_is_classified() {
    let limited = json!({ "error": "Rate limit exceeded", "error_code": 43, "retry_after": 3 });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v1/tasks/{TASK_ID}"))
            .respond_header("retry-after", "3")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/api/v1/tasks/{TASK_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "task.get", json!({ "task_id": TASK_ID })))
            .await
            .expect("the stub answers");
        failures.push(todoist::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(3)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `todoist_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is Todoist's own `next_cursor`, it is passed back verbatim as
/// `cursor`, and the walk makes exactly the number of requests the plan
/// declares.
#[tokio::test]
async fn todoist_cursor_is_opaque_and_bounded() {
    let plan = todoist::pagination("task.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const CURSOR: &str = "eyJwYWdlIjoyLCJsaW1pdCI6NTB9.aGFzaA";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v1/tasks")
            .query(&format!("project_id={PROJECT_ID}&limit=200"))
            .respond_json(
                200,
                json!({ "results": [{ "id": "1" }], "next_cursor": CURSOR }),
            ),
        // "When `next_cursor` is `null`, you've reached the end of the results."
        Expectation::new("GET", "/api/v1/tasks")
            .query(&format!(
                "project_id={PROJECT_ID}&limit=200&cursor=eyJwYWdlIjoyLCJsaW1pdCI6NTB9%2EaGFzaA"
            ))
            .respond_json(
                200,
                json!({ "results": [{ "id": "2" }], "next_cursor": null }),
            ),
    ])
    .await;

    let tasks = plan
        .collect(
            render(&stub, "task.list", json!({ "project_id": PROJECT_ID })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk spends one cursor and stops where `next_cursor` is null");
    assert_eq!(tasks, vec![json!({ "id": "1" }), json!({ "id": "2" })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `todoist_pagination_is_bounded`: the declared plan terminates under every
/// budget, and no write declares one.
#[tokio::test]
async fn todoist_pagination_is_bounded() {
    let plan = todoist::pagination("comment.list").expect("the collection declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/v1/comments").respond_json(
                200,
                json!({ "results": [{ "id": "1" }, { "id": "2" }], "next_cursor": "endless" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "comment.list", json!({ "task_id": TASK_ID })),
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
        "task.get",
        "task.create",
        "task.update",
        "task.close",
        "task.delete",
        "project.get",
        "comment.create",
    ] {
        assert!(
            todoist::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    for id in ["task.list", "task.search", "project.list", "comment.list"] {
        assert_eq!(
            todoist::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            "/results"
        );
    }
}

/// `todoist_effects_are_classified`: every operation carries a class, and the
/// evidence records the mechanism Todoist publishes and the window it does not.
#[test]
fn todoist_effects_are_classified() {
    let connector = todoist::connector();
    let expected = [
        ("task.get", EffectClass::ReadOnly),
        ("task.list", EffectClass::ReadOnly),
        ("task.search", EffectClass::ReadOnly),
        ("task.create", EffectClass::AtMostOnce),
        ("task.update", EffectClass::InventoryOnly),
        ("task.close", EffectClass::AtMostOnce),
        ("task.delete", EffectClass::InventoryOnly),
        ("project.get", EffectClass::ReadOnly),
        ("project.list", EffectClass::ReadOnly),
        ("comment.list", EffectClass::ReadOnly),
        ("comment.create", EffectClass::AtMostOnce),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        // The mechanism Todoist publishes is not one this connector binds, so
        // no operation carries a binding for a runtime to write.
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector.admit_operation("task.update"),
        Err(OperationRejection::InventoryOnly)
    );

    // The recorded evidence names the mechanism, where it lives, and the
    // retention that is missing — which is what makes the class reviewable.
    let evidence = operation("task.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    let searched = evidence.searched_documentation();
    assert!(searched.contains("Sync"), "{searched}");
    assert!(searched.contains("uuid"), "{searched}");
    assert!(searched.contains("retention"), "{searched}");

    // The close is the one write whose repeat consequence Todoist publishes
    // outright, and it is the recurrence rather than a duplicate record.
    let close = operation("task.close")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(close.repeat_produces().contains("recurring"));

    // And the delete stays inventory-only on Todoist's own published second
    // send, which is a refusal rather than the same one absent task.
    assert!(
        operation("task.delete")
            .effect()
            .and_then(donat_connectors::sdk::Effect::inventory_reason)
            .expect("an inventory-only operation records its reason")
            .contains("NOT_FOUND")
    );
}

/// `todoist_output_contract`: the declared pointers read Todoist's own objects,
/// and its opaque string ids stay strings.
#[test]
fn todoist_output_contract() {
    let get = operation("task.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&task()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": TASK_ID, "content": "Ship the batch", "description": "with its evidence",
            "project_id": PROJECT_ID, "section_id": null, "priority": 1,
            "labels": ["connectors"],
            "due": { "date": "2026-08-20", "is_recurring": false, "string": "Aug 20" },
            "checked": false,
            "added_at": "2026-08-01T11:56:51.000000Z",
            "updated_at": "2026-08-02T11:56:51.000000Z",
        })
    );
    // "IDs have been opaque strings almost everywhere … This version officially
    // makes them non-number opaque strings."
    assert_eq!(
        get.decode_response(200, br#"{"id":2995104339}"#)
            .expect_err("a numeric id is not the declared opaque string")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"content":"no id"}"#)
            .expect_err("an object with no id is not a task")
            .class(),
        ConnectorErrorClass::Validation
    );
    // A paginated collection publishes the page and the continuation.
    assert_eq!(
        operation("task.list")
            .decode_response(200, br#"{"results":[{"id":"1"}],"next_cursor":null}"#)
            .expect("the declared contract is satisfied"),
        json!({ "results": [{ "id": "1" }], "next_cursor": null })
    );
    // The close answers a bare `true`, which the declaration publishes whole.
    assert_eq!(
        operation("task.close")
            .decode_response(200, b"true")
            .expect("a bare boolean is the whole output"),
        json!(true)
    );
}
