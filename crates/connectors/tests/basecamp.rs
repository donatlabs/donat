//! Basecamp connector proofs (spec 024 §3, which adopts spec 023 §4), against
//! the SDK's local provider stub.
//!
//! Basecamp is the batch's per-deployment declaration, so the proof that
//! nothing but deploy-time configuration can move its account prefix is its own,
//! and it is the analogue of spec 023 §4's templated-host proof.

use std::time::Duration;

use donat_connectors::providers::basecamp;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const ACCOUNT: &str = "999999999";
const USER_AGENT: &str = "Donat (integrations@example.test)";
const TODO_ID: i64 = 2;
const TODOLIST_ID: i64 = 3;
const TODOSET_ID: i64 = 5;
const RECORDING_ID: i64 = 3;
const PROJECT_ID: i64 = 1;

fn connector() -> Connector {
    basecamp::connector(ACCOUNT, USER_AGENT).expect("a configured Basecamp account declares")
}

fn operation(connector: &Connector, id: &str) -> Operation {
    connector
        .operation(id)
        .unwrap_or_else(|| panic!("the basecamp declaration publishes {id}"))
        .clone()
}

fn render(stub: &ProviderStub, connector: &Connector, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(connector, id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn todo() -> JsonValue {
    json!({
        "id": TODO_ID,
        "title": "Ship the batch",
        "content": "Ship the batch",
        "description": "<div>with its evidence</div>",
        "completed": false,
        "due_on": "2026-08-20",
        "status": "active",
        "app_url": "https://3.basecamp.com/999999999/buckets/1/todos/2",
        "assignees": [],
        "bucket": { "id": PROJECT_ID, "name": "Connectors" },
        "created_at": "2026-08-01T11:56:51.000Z",
        "updated_at": "2026-08-02T11:56:51.000Z",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("todo.get", json!({ "todo_id": TODO_ID })),
        (
            "todo.list",
            json!({ "todolist_id": TODOLIST_ID, "status": "active", "completed": false }),
        ),
        (
            "todo.create",
            json!({
                "todolist_id": TODOLIST_ID, "content": "Ship the batch",
                "description": "<div>with its evidence</div>",
                "assignee_ids": [], "notify": true, "due_on": "2026-08-20",
            }),
        ),
        (
            "todo.replace",
            json!({
                "todo_id": TODO_ID, "content": "Ship the batch",
                "description": "<div>with its evidence</div>",
                "assignee_ids": [], "notify": false, "due_on": "2026-08-20",
            }),
        ),
        ("todo.complete", json!({ "todo_id": TODO_ID })),
        ("todo.uncomplete", json!({ "todo_id": TODO_ID })),
        ("comment.list", json!({ "recording_id": RECORDING_ID })),
        (
            "comment.create",
            json!({ "recording_id": RECORDING_ID, "content": "<div>Looks right.</div>" }),
        ),
        (
            "todolist.list",
            json!({ "todoset_id": TODOSET_ID, "status": "active" }),
        ),
        ("project.get", json!({ "project_id": PROJECT_ID })),
        ("project.list", json!({ "status": "active" })),
    ]
}

/// `basecamp_request_shape`: exact method, path, query, headers, and body for
/// every operation, each one under this deployment's account prefix.
#[tokio::test]
async fn basecamp_request_shape() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/{ACCOUNT}/todos/{TODO_ID}.json"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("accept", "application/json")
            .header("user-agent", USER_AGENT)
            .no_body()
            .respond_json(200, todo()),
        Expectation::new(
            "GET",
            &format!("/{ACCOUNT}/todolists/{TODOLIST_ID}/todos.json"),
        )
        .query("status=active&completed=false")
        .respond_json(200, json!([todo()])),
        Expectation::new(
            "POST",
            &format!("/{ACCOUNT}/todolists/{TODOLIST_ID}/todos.json"),
        )
        .json_body(json!({
            "content": "Ship the batch", "description": "<div>with its evidence</div>",
            "assignee_ids": [], "notify": true, "due_on": "2026-08-20",
        }))
        .respond_json(201, todo()),
        Expectation::new("PUT", &format!("/{ACCOUNT}/todos/{TODO_ID}.json"))
            .json_body(json!({
                "content": "Ship the batch", "description": "<div>with its evidence</div>",
                "assignee_ids": [], "notify": false, "due_on": "2026-08-20",
            }))
            .respond_json(200, todo()),
        Expectation::new(
            "POST",
            &format!("/{ACCOUNT}/todos/{TODO_ID}/completion.json"),
        )
        .respond_bytes(204, Vec::new()),
        Expectation::new(
            "DELETE",
            &format!("/{ACCOUNT}/todos/{TODO_ID}/completion.json"),
        )
        .respond_bytes(204, Vec::new()),
        Expectation::new(
            "GET",
            &format!("/{ACCOUNT}/recordings/{RECORDING_ID}/comments.json"),
        )
        .respond_json(200, json!([])),
        Expectation::new(
            "POST",
            &format!("/{ACCOUNT}/recordings/{RECORDING_ID}/comments.json"),
        )
        .json_body(json!({ "content": "<div>Looks right.</div>" }))
        .respond_json(
            201,
            json!({ "id": 7, "content": "<div>Looks right.</div>",
                    "app_url": "https://3.basecamp.com/999999999/buckets/1/comments/7",
                    "created_at": "2026-08-02T11:56:51.000Z" }),
        ),
        Expectation::new(
            "GET",
            &format!("/{ACCOUNT}/todosets/{TODOSET_ID}/todolists.json"),
        )
        .query("status=active")
        .respond_json(200, json!([])),
        Expectation::new("GET", &format!("/{ACCOUNT}/projects/{PROJECT_ID}.json")).respond_json(
            200,
            json!({ "id": PROJECT_ID, "name": "Connectors", "dock": [] }),
        ),
        Expectation::new("GET", &format!("/{ACCOUNT}/projects.json"))
            .query("status=active")
            .respond_json(200, json!([])),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, &connector, id, input);
        assert!(
            request.url().path().starts_with(&format!("/{ACCOUNT}/")),
            "{id} renders under this deployment's account prefix: {}",
            request.url().path()
        );
        // "All API URLs end in `.json` to indicate that they return JSON."
        assert!(request.url().path().ends_with(".json"), "{id}");
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `basecamp_auth_is_applied`: the stored OAuth2 token reaches the wire as the
/// `Bearer` credential Basecamp publishes, the deployment's `User-Agent` travels
/// with it, and neither the token nor a request without a credential gets out.
#[tokio::test]
async fn basecamp_auth_is_applied() {
    let connector = connector();
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/{ACCOUNT}/todos/{TODO_ID}.json"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .header("user-agent", USER_AGENT)
                .respond_json(200, todo()),
        ])
        .await;

    let request = render(&stub, &connector, "todo.get", json!({ "todo_id": TODO_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!request.url_carries_credential());
    assert!(!format!("{request:?}").contains(SECRET_SENTINEL));

    // "If you don't include a `User-Agent` header, you'll get a `400 Bad
    // Request` response", so the header is on every request rather than on the
    // ones a caller remembered.
    assert_eq!(
        request
            .headers()
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some(USER_AGENT)
    );

    // The credential seam refuses before it sends: a stored-credential
    // connector with no issued token renders nothing at all (ADR 043).
    let mut unauthenticated = operation(&connector, "todo.get")
        .plan_request(&stub.origin(), &json!({ "todo_id": TODO_ID }))
        .expect("the declared request renders");
    let failure = AuthPlan::oauth2_authorization_code()
        .apply(&Credential::from_fields([]), &mut unauthenticated, None)
        .expect_err("a stored-credential connector cannot send without a token");
    assert_eq!(failure.code(), "connector_credential_not_applicable");

    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `basecamp_account_prefix_comes_only_from_deploy_time_configuration` (spec 023
/// §4 proof 1, in the shape Basecamp's URL grammar gives it): input, a provider
/// body, and a continuation each fail to move the account the request reaches.
#[tokio::test]
async fn basecamp_account_prefix_comes_only_from_deploy_time_configuration() {
    let connector = connector();

    // 1. Operation input. A path value that spells another account stays one
    //    percent-encoded segment under the configured prefix.
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/{ACCOUNT}/todos")).respond_json(200, json!([]))
    ])
    .await;
    let request = operation(&connector, "todo.get")
        .plan_request(&stub.origin(), &json!({ "todo_id": 7 }))
        .expect("the declared request renders");
    assert_eq!(request.url().path(), format!("/{ACCOUNT}/todos/7.json"));
    // The one path slot is typed as an integer, so a value spelling another
    // account is not even a value the declaration admits.
    assert!(
        operation(&connector, "todo.get")
            .plan_request(
                &stub.origin(),
                &json!({ "todo_id": "../../111111111/todos/7" })
            )
            .is_err(),
        "a path slot Basecamp types as an id takes an id"
    );

    // 2. A provider response naming another account is data, not a destination.
    let output = operation(&connector, "todo.get")
        .extract_output(&json!({
            "id": TODO_ID,
            "app_url": "https://3.basecampapi.com/111111111/buckets/1/todos/2",
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("app_url"),
        Some(&json!(
            "https://3.basecampapi.com/111111111/buckets/1/todos/2"
        ))
    );

    // 3. A `Link` continuation to another origin is refused rather than
    //    followed, which is what stops a walk leaving this deployment's host.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/{ACCOUNT}/todolists/{TODOLIST_ID}/todos.json"),
    )
    .respond_header(
        "link",
        "<https://attacker.invalid/111111111/todos.json?page=2>; rel=\"next\"",
    )
    .respond_json(200, json!([]))])
    .await;
    let failure = basecamp::pagination("todo.list")
        .expect("todo.list declares a plan")
        .collect(
            render(
                &stub,
                &connector,
                "todo.list",
                json!({ "todolist_id": TODOLIST_ID, "status": "active", "completed": false }),
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

    // And the configuration itself admits Basecamp's own grammar and nothing
    // else: "Basecamp account ID (numeric string)".
    for hostile in [
        "",
        "999999999/../111111111",
        "99999999a",
        "9999 99999",
        "99999999999999999999999",
        "-1",
    ] {
        assert!(
            basecamp::validate_account_id(hostile).is_err(),
            "`{hostile}` is not a Basecamp account id"
        );
        assert!(basecamp::connector(hostile, USER_AGENT).is_err());
    }
    assert!(basecamp::validate_account_id(ACCOUNT).is_ok());

    // The `User-Agent` has its own grammar for the same reason: it identifies
    // this deployment to Basecamp, and a request may not choose it.
    for hostile in ["", "Donat", "(only@contact.test)", "Donat ()"] {
        assert!(
            basecamp::validate_user_agent(hostile).is_err(),
            "`{hostile}` is not the identification Basecamp demands"
        );
    }
    assert!(basecamp::validate_user_agent(USER_AGENT).is_ok());
}

/// `basecamp_error_map`: every documented status reaches exactly one closed
/// class, and none of Basecamp's prose crosses the boundary.
#[tokio::test]
async fn basecamp_error_map() {
    let connector = connector();
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (415, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/{ACCOUNT}/todos/{TODO_ID}.json"),
        )
        .respond_json(
            status,
            json!({ "error": format!("Content can't be blank for {SECRET_SENTINEL}") }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                &connector,
                "todo.get",
                json!({ "todo_id": TODO_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = basecamp::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "can't be blank", ACCOUNT] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `basecamp_rate_limit_is_classified`: the documented rate-limit response
/// reaches `http_429` with its published retry hint, clamped.
#[tokio::test]
async fn basecamp_rate_limit_is_classified() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/{ACCOUNT}/todos/{TODO_ID}.json"))
            .respond_header("retry-after", "10")
            .respond_json(429, json!({ "error": "Rate limit exceeded" })),
        Expectation::new("GET", &format!("/{ACCOUNT}/todos/{TODO_ID}.json"))
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "error": "Rate limit exceeded" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                &connector,
                "todo.get",
                json!({ "todo_id": TODO_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(basecamp::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(10)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `basecamp_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is Basecamp's own `Link` header, it is followed as a destination
/// on this origin, and the walk makes exactly the number of requests the plan
/// declares.
#[tokio::test]
async fn basecamp_cursor_is_opaque_and_bounded() {
    let connector = connector();
    let plan = basecamp::pagination("todo.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new(
            "GET",
            &format!("/{ACCOUNT}/todolists/{TODOLIST_ID}/todos.json"),
        )
        .query("status=active&completed=false")
        .respond_header(
            "link",
            &format!("</{ACCOUNT}/todolists/{TODOLIST_ID}/todos.json?page=2>; rel=\"next\""),
        )
        .respond_header("x-total-count", "2")
        .respond_json(200, json!([{ "id": 1 }])),
        // "If the `Link` header is blank, that's the last page."
        Expectation::new(
            "GET",
            &format!("/{ACCOUNT}/todolists/{TODOLIST_ID}/todos.json"),
        )
        .query("page=2")
        .respond_json(200, json!([{ "id": 2 }])),
    ])
    .await;

    let todos = plan
        .collect(
            render(
                &stub,
                &connector,
                "todo.list",
                json!({ "todolist_id": TODOLIST_ID, "status": "active", "completed": false }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one link and stops where the header stops");
    assert_eq!(todos, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `basecamp_pagination_is_bounded`: the declared plan terminates under every
/// budget, and no write declares one.
#[tokio::test]
async fn basecamp_pagination_is_bounded() {
    let connector = connector();
    let plan = basecamp::pagination("project.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &format!("/{ACCOUNT}/projects.json"))
                .respond_header(
                    "link",
                    &format!("</{ACCOUNT}/projects.json?page=9>; rel=\"next\""),
                )
                .respond_json(200, json!([{ "id": 1 }, { "id": 2 }]))
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    &connector,
                    "project.list",
                    json!({ "status": "active" }),
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
        "todo.get",
        "todo.create",
        "todo.replace",
        "todo.complete",
        "todo.uncomplete",
        "comment.create",
        "project.get",
    ] {
        assert!(
            basecamp::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    // Every walked collection is a bare array at the document root.
    for id in ["todo.list", "todolist.list", "comment.list", "project.list"] {
        assert_eq!(
            basecamp::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            ""
        );
    }
}

/// `basecamp_effects_are_classified`: the two writes Basecamp marks repeat-safe
/// over `PUT` and `DELETE` are `NaturalMethod`, the one it marks repeat-safe over
/// `POST` stays unreachable, and the two it marks not at all are `AtMostOnce`.
#[test]
fn basecamp_effects_are_classified() {
    let connector = connector();
    let expected = [
        ("todo.get", EffectClass::ReadOnly),
        ("todo.list", EffectClass::ReadOnly),
        ("todo.create", EffectClass::AtMostOnce),
        ("todo.replace", EffectClass::ProviderIdempotentNaturalMethod),
        ("todo.complete", EffectClass::InventoryOnly),
        (
            "todo.uncomplete",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        ("comment.list", EffectClass::ReadOnly),
        ("comment.create", EffectClass::AtMostOnce),
        ("todolist.list", EffectClass::ReadOnly),
        ("project.get", EffectClass::ReadOnly),
        ("project.list", EffectClass::ReadOnly),
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
        // `NaturalMethod` is a provider guarantee about the method, not a key,
        // so nothing here binds one.
        assert!(operation.idempotency_binding().is_none(), "{id}");
    }

    // The sharpest entry in the batch: a write Basecamp itself marks repeat-safe
    // that the gate still does not admit, because the method is a `POST`.
    assert_eq!(
        connector.admit_operation("todo.complete"),
        Err(OperationRejection::InventoryOnly)
    );
    let reason = operation(&connector, "todo.complete")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records its reason")
        .to_owned();
    assert!(reason.contains("x-basecamp-idempotent"), "{reason}");
    assert!(reason.contains("PUT and DELETE only"), "{reason}");

    // And the two the provider does mark, over methods the gate admits.
    for id in ["todo.replace", "todo.uncomplete"] {
        assert_eq!(
            operation(&connector, id).effect_class(),
            Some(EffectClass::ProviderIdempotentNaturalMethod),
            "{id}"
        );
    }

    let evidence = operation(&connector, "todo.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on")
        .clone();
    assert!(
        evidence
            .searched_documentation()
            .contains("x-basecamp-idempotent")
    );
    assert!(evidence.repeat_produces().contains("a second to-do"));
}

/// `basecamp_output_contract`: the declared pointers read Basecamp's own
/// objects, and its documented empty successes are successes.
#[test]
fn basecamp_output_contract() {
    let connector = connector();
    let get = operation(&connector, "todo.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&todo()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": TODO_ID, "title": "Ship the batch", "content": "Ship the batch",
            "description": "<div>with its evidence</div>", "completed": false,
            "due_on": "2026-08-20", "status": "active",
            "app_url": "https://3.basecamp.com/999999999/buckets/1/todos/2",
            "assignees": [], "bucket": { "id": PROJECT_ID, "name": "Connectors" },
            "created_at": "2026-08-01T11:56:51.000Z",
            "updated_at": "2026-08-02T11:56:51.000Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":2}"#)
            .expect("only the identity is required")
            .get("id"),
        Some(&json!(TODO_ID))
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"2"}"#)
            .expect_err("a Basecamp id is a number")
            .class(),
        ConnectorErrorClass::Validation
    );
    // The completion endpoints answer `204` with no body at all.
    assert_eq!(
        operation(&connector, "todo.complete")
            .decode_response(204, b"")
            .expect("a documented empty success is a success"),
        json!({})
    );
    assert_eq!(
        operation(&connector, "todo.uncomplete")
            .decode_response(204, b"")
            .expect("a documented empty success is a success"),
        json!({})
    );
    // And a collection is a bare array at the document root.
    assert_eq!(
        operation(&connector, "todo.list")
            .decode_response(200, br#"[{"id":2}]"#)
            .expect("a bare array is the whole output"),
        json!([{ "id": 2 }])
    );
}
