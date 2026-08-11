//! Asana connector proofs (spec 024 §3, which adopts spec 023 §4), against the
//! SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::asana;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const TASK_GID: &str = "1201234567890123";
const PROJECT_GID: &str = "1200000000000001";
const WORKSPACE_GID: &str = "1190000000000002";

fn operation(id: &str) -> &'static Operation {
    asana::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the asana declaration publishes {id}"))
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
        "data": {
            "gid": TASK_GID,
            "name": "Ship the batch",
            "notes": "with its evidence",
            "completed": false,
            "due_on": "2026-08-20",
            "assignee": { "gid": "1199", "name": "Sam" },
            "projects": [{ "gid": PROJECT_GID }],
            "permalink_url": "https://app.asana.com/0/1/2",
            "created_at": "2026-08-01T11:56:51.000Z",
            "modified_at": "2026-08-02T11:56:51.000Z",
        }
    })
}

fn project() -> JsonValue {
    json!({
        "data": {
            "gid": PROJECT_GID,
            "name": "Connectors",
            "archived": false,
            "permalink_url": "https://app.asana.com/0/1",
            "workspace": { "gid": WORKSPACE_GID },
        }
    })
}

fn story() -> JsonValue {
    json!({
        "data": {
            "gid": "1209",
            "text": "Looks right to me.",
            "created_at": "2026-08-02T11:56:51.000Z",
        }
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("task.get", json!({ "task_gid": TASK_GID })),
        (
            "task.list",
            json!({ "project": PROJECT_GID, "completed_since": "2026-01-01T00:00:00Z" }),
        ),
        (
            "task.search",
            json!({ "workspace_gid": WORKSPACE_GID, "text": "batch", "sort_by": "created_at" }),
        ),
        (
            "task.create",
            json!({
                "name": "Ship the batch", "notes": "with its evidence",
                "workspace": WORKSPACE_GID, "projects": [PROJECT_GID],
                "assignee": "me", "due_on": "2026-08-20",
            }),
        ),
        (
            "task.update",
            json!({
                "task_gid": TASK_GID, "name": "Ship the batch", "notes": null,
                "completed": true, "assignee": null, "due_on": null,
            }),
        ),
        ("task.delete", json!({ "task_gid": TASK_GID })),
        ("story.list", json!({ "task_gid": TASK_GID })),
        (
            "story.create",
            json!({ "task_gid": TASK_GID, "text": "Looks right to me." }),
        ),
        ("project.get", json!({ "project_gid": PROJECT_GID })),
        (
            "project.list",
            json!({ "workspace": WORKSPACE_GID, "archived": false }),
        ),
    ]
}

/// `asana_request_shape`: exact method, path, query, headers, and body for every
/// operation, and the envelope Asana publishes for both directions.
#[tokio::test]
async fn asana_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/1.0/tasks/{TASK_GID}"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, task()),
        Expectation::new("GET", "/api/1.0/tasks")
            .query(&format!(
                "project={PROJECT_GID}&completed_since=2026%2D01%2D01T00%3A00%3A00Z"
            ))
            .respond_json(200, json!({ "data": [], "next_page": null })),
        Expectation::new(
            "GET",
            &format!("/api/1.0/workspaces/{WORKSPACE_GID}/tasks/search"),
        )
        .query("text=batch&sort_by=created%5Fat&limit=100")
        .respond_json(200, json!({ "data": [] })),
        Expectation::new("POST", "/api/1.0/tasks")
            .json_body(json!({
                "data": {
                    "name": "Ship the batch", "notes": "with its evidence",
                    "workspace": WORKSPACE_GID, "projects": [PROJECT_GID],
                    "assignee": "me", "due_on": "2026-08-20",
                }
            }))
            .respond_json(201, task()),
        Expectation::new("PUT", &format!("/api/1.0/tasks/{TASK_GID}"))
            .json_body(json!({
                "data": { "name": "Ship the batch", "notes": null, "completed": true,
                          "assignee": null, "due_on": null }
            }))
            .respond_json(200, task()),
        Expectation::new("DELETE", &format!("/api/1.0/tasks/{TASK_GID}"))
            .respond_json(200, json!({ "data": {} })),
        Expectation::new("GET", &format!("/api/1.0/tasks/{TASK_GID}/stories"))
            .respond_json(200, json!({ "data": [], "next_page": null })),
        Expectation::new("POST", &format!("/api/1.0/tasks/{TASK_GID}/stories"))
            .json_body(json!({ "data": { "text": "Looks right to me." } }))
            .respond_json(201, story()),
        Expectation::new("GET", &format!("/api/1.0/projects/{PROJECT_GID}"))
            .respond_json(200, project()),
        Expectation::new("GET", "/api/1.0/projects")
            .query(&format!("workspace={WORKSPACE_GID}&archived=false"))
            .respond_json(200, json!({ "data": [], "next_page": null })),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/1.0/"),
            "{id} renders a published Asana path: {}",
            request.url().path()
        );
        // A fixed origin is a fixed origin: no input of any operation moves it.
        assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `asana_auth_is_applied`: the personal access token reaches the wire as the
/// `Bearer` credential Asana publishes, and appears nowhere else.
#[tokio::test]
async fn asana_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/api/1.0/tasks/{TASK_GID}"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, task()),
        ])
        .await;

    let request = render(&stub, "task.get", json!({ "task_gid": TASK_GID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!request.url_carries_credential());
    assert!(!request.redacted_url().contains(SECRET_SENTINEL));
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{:?}", asana::connector().credential()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", asana::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `asana_error_map`: every documented status reaches exactly one closed class,
/// and none of Asana's prose crosses the boundary.
#[tokio::test]
async fn asana_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (402, ConnectorErrorClass::Permanent),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (451, ConnectorErrorClass::Permanent),
        (500, ConnectorErrorClass::Http5xx),
        // A status Asana publishes nothing for still lands in one class.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/1.0/tasks/{TASK_GID}"),
        )
        .respond_json(
            status,
            json!({
                "errors": [{
                    "message": format!("task: Not a recognized ID: {SECRET_SENTINEL}"),
                    "help": "For more information on API status codes and how to handle them",
                    "phrase": "6 sad squid snuggle softly",
                }]
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "task.get", json!({ "task_gid": TASK_GID })))
            .await
            .expect("the stub answers");

        let failure = asana::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "sad squid", "Not a recognized ID"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `asana_rate_limit_is_classified`: the documented rate-limit response reaches
/// `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn asana_rate_limit_is_classified() {
    let limited = json!({ "errors": [{ "message": "Rate limit exceeded" }] });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/api/1.0/tasks/{TASK_GID}"))
            .respond_header("retry-after", "17")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/api/1.0/tasks/{TASK_GID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "task.get", json!({ "task_gid": TASK_GID })))
            .await
            .expect("the stub answers");
        failures.push(asana::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(17)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `asana_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the continuation
/// is Asana's own `next_page.offset`, it is spent as the `offset` query
/// parameter, and the walk makes exactly the number of requests the plan
/// declares.
#[tokio::test]
async fn asana_cursor_is_opaque_and_bounded() {
    let plan = asana::pagination("task.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/1.0/tasks")
            .query(&format!(
                "project={PROJECT_GID}&completed_since=2026%2D01%2D01T00%3A00%3A00Z&limit=100"
            ))
            .respond_json(
                200,
                json!({
                    "data": [{ "gid": "1" }],
                    "next_page": {
                        "offset": "eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9",
                        "path": "/tasks?offset=eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9",
                        "uri": "https://app.asana.com/api/1.0/tasks?offset=eyJ0eXAiOJiKV1iQ",
                    },
                }),
            ),
        // "If there are no more pages available, `next_page` will be null and no
        // offset will be provided", which is the absence the plan ends on.
        Expectation::new("GET", "/api/1.0/tasks")
            .query(&format!(
                "project={PROJECT_GID}&completed_since=2026%2D01%2D01T00%3A00%3A00Z&limit=100\
                 &offset=eyJ0eXAiOJiKV1iQLCJhbGciOiJIUzI1NiJ9"
            ))
            .respond_json(200, json!({ "data": [{ "gid": "2" }], "next_page": null })),
    ])
    .await;

    let tasks = plan
        .collect(
            render(
                &stub,
                "task.list",
                json!({ "project": PROJECT_GID, "completed_since": "2026-01-01T00:00:00Z" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk spends one offset and stops where `next_page` is null");
    assert_eq!(tasks, vec![json!({ "gid": "1" }), json!({ "gid": "2" })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `asana_pagination_is_bounded`: the declared plan terminates under every
/// budget, and the search — which Asana publishes as unpaginable — declares
/// none.
#[tokio::test]
async fn asana_pagination_is_bounded() {
    let plan = asana::pagination("project.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/api/1.0/projects").respond_json(
                200,
                json!({
                    "data": [{ "gid": "1" }, { "gid": "2" }],
                    "next_page": { "offset": "endless" },
                }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "project.list",
                    json!({ "workspace": WORKSPACE_GID, "archived": false }),
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
        "task.get",
        "task.search",
        "task.create",
        "task.update",
        "task.delete",
        "story.create",
        "project.get",
    ] {
        assert!(
            asana::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    for id in ["task.list", "project.list", "story.list"] {
        assert_eq!(
            asana::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            "/data"
        );
    }
}

/// `asana_effects_are_classified`: every operation carries a class, and the two
/// that cannot be reached from a Process are the update and the delete.
#[test]
fn asana_effects_are_classified() {
    let connector = asana::connector();
    let expected = [
        ("task.get", EffectClass::ReadOnly),
        ("task.list", EffectClass::ReadOnly),
        ("task.search", EffectClass::ReadOnly),
        ("task.create", EffectClass::AtMostOnce),
        ("task.update", EffectClass::InventoryOnly),
        ("task.delete", EffectClass::InventoryOnly),
        ("story.list", EffectClass::ReadOnly),
        ("story.create", EffectClass::AtMostOnce),
        ("project.get", EffectClass::ReadOnly),
        ("project.list", EffectClass::ReadOnly),
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
        connector.admit_operation("task.update"),
        Err(OperationRejection::InventoryOnly)
    );

    // The evidence is the machine-checkable absence, and the consequence names
    // what an operator accepts.
    let evidence = operation("task.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("idempot"));
    assert!(evidence.repeat_produces().contains("a second task"));
}

/// `asana_output_contract`: the declared pointers read Asana's own envelope, and
/// a compact record still satisfies the identity.
#[test]
fn asana_output_contract() {
    let get = operation("task.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&task()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "gid": TASK_GID, "name": "Ship the batch", "notes": "with its evidence",
            "completed": false, "due_on": "2026-08-20",
            "assignee": { "gid": "1199", "name": "Sam" },
            "projects": [{ "gid": PROJECT_GID }],
            "permalink_url": "https://app.asana.com/0/1/2",
            "created_at": "2026-08-01T11:56:51.000Z",
            "modified_at": "2026-08-02T11:56:51.000Z",
        })
    );
    // "This endpoint returns a resource which excludes some properties by
    // default", so only the identity is demanded.
    assert_eq!(
        get.decode_response(200, br#"{"data":{"gid":"1201234567890123"}}"#)
            .expect("only the identity is required")
            .get("gid"),
        Some(&json!(TASK_GID))
    );
    // Asana's gids are strings, and a number is not one.
    assert_eq!(
        get.decode_response(200, br#"{"data":{"gid":1201234567890123}}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // A body outside the envelope is not a task.
    assert_eq!(
        get.decode_response(200, br#"{"gid":"1201234567890123"}"#)
            .expect_err("an unenveloped body is outside the declared contract")
            .class(),
        ConnectorErrorClass::Validation
    );
    // An undeclared status is a failure rather than a silent success.
    assert_eq!(
        get.decode_response(202, br#"{"data":{"gid":"1"}}"#)
            .expect_err("202 is not a declared success")
            .code(),
        "connector_unsupported_http_status"
    );

    // A walked collection publishes the page and the continuation it has spent.
    assert_eq!(
        operation("task.list")
            .decode_response(200, br#"{"data":[{"gid":"1"}],"next_page":null}"#)
            .expect("the declared contract is satisfied"),
        json!({ "data": [{ "gid": "1" }], "next_offset": null })
    );
}
