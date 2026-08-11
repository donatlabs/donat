//! ClickUp connector proofs (spec 024 §3, which adopts spec 023 §4), against the
//! SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::clickup;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const TASK_ID: &str = "9hx";
const LIST_ID: &str = "15505202";
const FOLDER_ID: &str = "6992470";
const TEAM_ID: &str = "7002367";

fn operation(id: &str) -> &'static Operation {
    clickup::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the clickup declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::authorization_credential()
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn task() -> JsonValue {
    json!({
        "id": TASK_ID,
        "name": "Fix login bug",
        "description": "Users are unable to log in when SSO is enabled.",
        "status": { "status": "in progress", "type": "custom" },
        "url": "https://app.clickup.com/t/9hx",
        "list": { "id": LIST_ID, "name": "Sprint Backlog" },
        "assignees": [],
        "due_date": null,
        "date_created": "1567780450202",
        "date_updated": "1567780450202",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("task.get", json!({ "task_id": TASK_ID })),
        (
            "task.list",
            json!({ "list_id": LIST_ID, "page": 0, "include_closed": true, "subtasks": true }),
        ),
        (
            "task.search",
            json!({ "team_Id": TEAM_ID, "page": 0, "include_closed": false }),
        ),
        (
            "task.create",
            json!({
                "list_id": LIST_ID, "name": "Fix login bug", "description": "SSO",
                "assignees": [], "tags": [], "status": "to do",
                "priority": 3, "due_date": null,
            }),
        ),
        (
            "task.update",
            json!({
                "task_id": TASK_ID, "name": "Fix login bug", "description": null,
                "status": "in progress", "priority": null, "due_date": null, "archived": false,
            }),
        ),
        ("task.delete", json!({ "task_id": TASK_ID })),
        ("comment.list", json!({ "task_id": TASK_ID })),
        (
            "comment.create",
            json!({
                "task_id": TASK_ID, "comment_text": "Task comment content",
                "notify_all": true, "assignee": null,
            }),
        ),
        ("list.get", json!({ "list_id": LIST_ID })),
        (
            "list.list",
            json!({ "folder_id": FOLDER_ID, "archived": false }),
        ),
        (
            "space.list",
            json!({ "team_id": TEAM_ID, "archived": false }),
        ),
    ]
}

/// `clickup_request_shape`: exact method, path, query, headers, and body for
/// every operation.
#[tokio::test]
async fn clickup_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/v2/task/{TASK_ID}"))
            .query("")
            .header("authorization", SECRET_SENTINEL)
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, task()),
        Expectation::new("GET", &format!("/api/v2/list/{LIST_ID}/task"))
            .query("page=0&include_closed=true&subtasks=true")
            .respond_json(200, json!({ "tasks": [task()], "last_page": true })),
        Expectation::new("GET", &format!("/api/v2/team/{TEAM_ID}/task"))
            .query("page=0&include_closed=false")
            .respond_json(200, json!({ "tasks": [], "last_page": true })),
        Expectation::new("POST", &format!("/api/v2/list/{LIST_ID}/task"))
            .json_body(json!({
                "name": "Fix login bug", "description": "SSO", "assignees": [],
                "tags": [], "status": "to do", "priority": 3, "due_date": null,
            }))
            .respond_json(200, task()),
        Expectation::new("PUT", &format!("/api/v2/task/{TASK_ID}"))
            .json_body(json!({
                "name": "Fix login bug", "description": null, "status": "in progress",
                "priority": null, "due_date": null, "archived": false,
            }))
            .respond_json(200, task()),
        Expectation::new("DELETE", &format!("/api/v2/task/{TASK_ID}"))
            .header("content-type", "application/json")
            .respond_bytes(204, Vec::new()),
        Expectation::new("GET", &format!("/api/v2/task/{TASK_ID}/comment"))
            .query("")
            .respond_json(200, json!({ "comments": [] })),
        Expectation::new("POST", &format!("/api/v2/task/{TASK_ID}/comment"))
            .json_body(json!({
                "comment_text": "Task comment content", "notify_all": true, "assignee": null,
            }))
            .respond_json(
                200,
                json!({ "id": "458", "hist_id": "26508", "date": 1568036964079_i64 }),
            ),
        Expectation::new("GET", &format!("/api/v2/list/{LIST_ID}"))
            .respond_json(200, json!({ "id": LIST_ID, "name": "Sprint Backlog" })),
        Expectation::new("GET", &format!("/api/v2/folder/{FOLDER_ID}/list"))
            .query("archived=false")
            .respond_json(200, json!({ "lists": [] })),
        Expectation::new("GET", &format!("/api/v2/team/{TEAM_ID}/space"))
            .query("archived=false")
            .respond_json(200, json!({ "spaces": [] })),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v2/"),
            "{id} renders a published ClickUp path: {}",
            request.url().path()
        );
        assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `clickup_auth_is_applied`: the personal token *is* the `Authorization` value,
/// with no scheme in front of it, and it appears nowhere else.
#[tokio::test]
async fn clickup_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/api/v2/task/{TASK_ID}"))
        .header("authorization", SECRET_SENTINEL)
        .respond_json(200, task())])
    .await;

    let request = render(&stub, "task.get", json!({ "task_id": TASK_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("the token is ASCII"),
        SECRET_SENTINEL,
        "ClickUp publishes the bare token, and `Bearer` would authenticate as nobody"
    );
    assert!(!request.url_carries_credential());
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{:?}", clickup::connector().credential()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", clickup::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `clickup_error_map`: every documented status reaches exactly one closed
/// class, and ClickUp's `err`/`ECODE` body never crosses the boundary.
#[tokio::test]
async fn clickup_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v2/task/{TASK_ID}"),
        )
        .respond_json(
            status,
            json!({ "err": format!("Team not authorized {SECRET_SENTINEL}"), "ECODE": "OAUTH_027" }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "task.get", json!({ "task_id": TASK_ID })))
            .await
            .expect("the stub answers");

        let failure = clickup::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "Team not authorized", "OAUTH_027"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `clickup_rate_limit_is_classified`: the documented rate-limit response
/// reaches `http_429`, and ClickUp publishes no `Retry-After` for the connector
/// to invent one from.
#[tokio::test]
async fn clickup_rate_limit_is_classified() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/api/v2/task/{TASK_ID}"))
        .respond_header("x-ratelimit-limit", "100")
        .respond_header("x-ratelimit-remaining", "0")
        .respond_header("x-ratelimit-reset", "1567780450")
        .respond_json(
            429,
            json!({ "err": "Rate limit reached", "ECODE": "RATE_001" }),
        )])
    .await;

    let response = stub
        .send(render(&stub, "task.get", json!({ "task_id": TASK_ID })))
        .await
        .expect("the stub answers");
    let failure = clickup::error_map().classify_response(&response);
    assert_eq!(failure.class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failure.retry_after(),
        None,
        "ClickUp publishes X-RateLimit-* and no Retry-After, so the connector invents none"
    );
    stub.assert_satisfied();
}

/// `clickup_pagination_is_bounded`: no operation declares a continuation plan,
/// because ClickUp ends its collections on a boolean no plan can read — and the
/// page index it publishes instead is a declared input a Process advances.
#[tokio::test]
async fn clickup_pagination_is_bounded() {
    for (id, _) in cases() {
        assert!(
            clickup::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }

    // The flag is published as an output instead, so a Process can see the end
    // of the collection the provider named.
    let page = operation("task.list")
        .decode_response(200, br#"{"tasks":[{"id":"9hx"}],"last_page":false}"#)
        .expect("the declared contract is satisfied");
    assert_eq!(page["last_page"], json!(false));

    // And the page index reaches the wire as ClickUp's own zero-based
    // parameter, which is the whole of the declared walk.
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/api/v2/list/{LIST_ID}/task"))
                .query("page=3&include_closed=true&subtasks=true")
                .respond_json(200, json!({ "tasks": [], "last_page": true })),
        ])
        .await;
    stub.send(render(
        &stub,
        "task.list",
        json!({ "list_id": LIST_ID, "page": 3, "include_closed": true, "subtasks": true }),
    ))
    .await
    .expect("the stub answers");
    assert_eq!(stub.received(), 1, "one call is one page");
    stub.assert_satisfied();
}

/// `clickup_effects_are_classified`: every operation carries a class, and the
/// update and the delete are unreachable from a Process.
#[test]
fn clickup_effects_are_classified() {
    let connector = clickup::connector();
    let expected = [
        ("task.get", EffectClass::ReadOnly),
        ("task.list", EffectClass::ReadOnly),
        ("task.search", EffectClass::ReadOnly),
        ("task.create", EffectClass::AtMostOnce),
        ("task.update", EffectClass::InventoryOnly),
        ("task.delete", EffectClass::InventoryOnly),
        ("comment.list", EffectClass::ReadOnly),
        ("comment.create", EffectClass::AtMostOnce),
        ("list.get", EffectClass::ReadOnly),
        ("list.list", EffectClass::ReadOnly),
        ("space.list", EffectClass::ReadOnly),
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
        connector.admit_operation("task.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    let evidence = operation("comment.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("518 KB"));
    assert!(evidence.repeat_produces().contains("a second comment"));
}

/// `clickup_output_contract`: the declared pointers read ClickUp's own objects,
/// its millisecond timestamps stay strings, and the documented empty delete is a
/// success.
#[test]
fn clickup_output_contract() {
    let get = operation("task.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&task()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": TASK_ID, "name": "Fix login bug",
            "description": "Users are unable to log in when SSO is enabled.",
            "status": { "status": "in progress", "type": "custom" },
            "url": "https://app.clickup.com/t/9hx",
            "list": { "id": LIST_ID, "name": "Sprint Backlog" },
            "assignees": [], "due_date": null,
            "date_created": "1567780450202", "date_updated": "1567780450202",
        })
    );
    // "date_created": "1567780450202" — a string of epoch milliseconds, which
    // the declaration publishes as one.
    assert_eq!(
        get.decode_response(200, br#"{"id":"9hx","date_created":1567780450202}"#)
            .expect_err("a numeric timestamp is not the declared string")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"name":"no id"}"#)
            .expect_err("an object with no id is not a task")
            .class(),
        ConnectorErrorClass::Validation
    );
    // "Delete a task from your Workspace" answers `204` with no body at all,
    // which the declaration admits as a success rather than as malformed JSON.
    assert_eq!(
        operation("task.delete")
            .decode_response(204, b"")
            .expect("a documented empty success is a success"),
        json!({})
    );
    assert_eq!(
        operation("task.delete")
            .decode_response(200, b"{}")
            .expect_err("200 is not a declared success for the delete")
            .code(),
        "connector_unsupported_http_status"
    );
}

/// The declared deadline is positive for every operation, so no walk or attempt
/// inherits an unbounded one.
#[test]
fn clickup_operations_declare_a_deadline() {
    for (id, _) in cases() {
        assert!(operation(id).deadline() > Duration::ZERO, "{id}");
    }
}
