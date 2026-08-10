//! Harvest connector proofs (spec 028 §4, which adopts spec 023 §4), against
//! the SDK's local provider stub.
//!
//! Harvest is this batch's two-value credential: a Personal Access Token that is
//! a secret, and an account identifier that is not. The proof spec 028 §3 asks
//! for is `harvest_account_id_is_configuration_and_the_token_is_a_secret`, and
//! it holds both halves at once — the token reaches no `Debug`, diagnostic, or
//! error, while the account id is visible on the wire because it has to be.

use std::time::Duration;

use donat_connectors::providers::harvest;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, Secret, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const ACCOUNT_ID: &str = "1234567";
const USER_AGENT: &str = "Donat (integrations@example.test)";
const TIME_ENTRY_ID: i64 = 636_708_723;
const PROJECT_ID: i64 = 14_307_913;
const CLIENT_ID: i64 = 5_735_774;
const TASK_ID: i64 = 8_083_365;

/// The declared filters of each collection, and the exact query they render.
/// Every declared query slot is bound on every call, because a declared input
/// the caller omits is a failure rather than an absent parameter.
fn time_entry_filters() -> JsonValue {
    json!({
        "user_id": 1_782_959, "project_id": PROJECT_ID, "client_id": CLIENT_ID,
        "from": "2026-08-01", "to": "2026-08-31", "is_billed": false,
        "updated_since": "2026-08-01T00:00:00Z",
    })
}

const TIME_ENTRY_QUERY: &str = "user_id=1782959&project_id=14307913&client_id=5735774\
                                &from=2026%2D08%2D01&to=2026%2D08%2D31&is_billed=false\
                                &updated_since=2026%2D08%2D01T00%3A00%3A00Z&per_page=100";

fn project_filters() -> JsonValue {
    json!({ "is_active": true, "client_id": CLIENT_ID, "updated_since": "2026-08-01T00:00:00Z" })
}

const PROJECT_QUERY: &str = "is_active=true&client_id=5735774\
                             &updated_since=2026%2D08%2D01T00%3A00%3A00Z&per_page=100";

fn client_filters() -> JsonValue {
    json!({ "is_active": true, "updated_since": "2026-08-01T00:00:00Z" })
}

const CLIENT_QUERY: &str = "is_active=true&updated_since=2026%2D08%2D01T00%3A00%3A00Z&per_page=100";

fn connector() -> Connector {
    harvest::connector(ACCOUNT_ID, USER_AGENT).expect("a configured Harvest account declares")
}

fn operation(connector: &Connector, id: &str) -> Operation {
    connector
        .operation(id)
        .unwrap_or_else(|| panic!("the harvest declaration publishes {id}"))
        .clone()
}

fn credential() -> Credential {
    // The two values one deployment resolves: the secret the plan spends, and
    // the non-secret account the declaration compiled into every header.
    Credential::from_fields([
        ("secret", Secret::new(SECRET_SENTINEL)),
        (harvest::ACCOUNT_ID, Secret::new(ACCOUNT_ID)),
    ])
}

fn render(stub: &ProviderStub, connector: &Connector, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(connector, id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the configured token");
    request
}

fn time_entry() -> JsonValue {
    json!({
        "id": TIME_ENTRY_ID,
        "spent_date": "2026-08-03",
        "hours": 2.11,
        "rounded_hours": 2.25,
        "notes": "Batch L",
        "is_running": false,
        "is_billed": false,
        "billable": true,
        "user": { "id": 1_782_959, "name": "Kim Allen" },
        "client": { "id": CLIENT_ID, "name": "ACME" },
        "project": { "id": PROJECT_ID, "name": "Marketing Website" },
        "task": { "id": TASK_ID, "name": "Graphic Design" },
        "external_reference": JsonValue::Null,
        "created_at": "2026-08-03T15:00:00Z",
        "updated_at": "2026-08-03T15:30:00Z",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("time_entry.get", json!({ "time_entry_id": TIME_ENTRY_ID })),
        ("time_entry.list", time_entry_filters()),
        (
            "time_entry.create",
            json!({
                "project_id": PROJECT_ID, "task_id": TASK_ID, "spent_date": "2026-08-03",
                "user_id": 1_782_959, "hours": 2.11, "notes": "Batch L",
                "external_reference": { "id": "1", "service": "example.test" },
            }),
        ),
        (
            "time_entry.update",
            json!({
                "time_entry_id": TIME_ENTRY_ID, "project_id": PROJECT_ID, "task_id": TASK_ID,
                "spent_date": "2026-08-03", "hours": 3.0, "notes": "Batch L, revised",
            }),
        ),
        ("project.get", json!({ "project_id": PROJECT_ID })),
        ("project.list", project_filters()),
        ("client.get", json!({ "client_id": CLIENT_ID })),
        ("client.list", client_filters()),
    ]
}

/// `harvest_request_shape`: exact method, path, query, headers, and body for
/// every operation, each one carrying this deployment's account and identity.
#[tokio::test]
async fn harvest_request_shape() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v2/time_entries/{TIME_ENTRY_ID}"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("harvest-account-id", ACCOUNT_ID)
            .header("user-agent", USER_AGENT)
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, time_entry()),
        Expectation::new("GET", "/v2/time_entries")
            .query(TIME_ENTRY_QUERY)
            .header("harvest-account-id", ACCOUNT_ID)
            .respond_json(200, json!({ "time_entries": [time_entry()], "links": {} })),
        Expectation::new("POST", "/v2/time_entries")
            .json_body(json!({
                "project_id": PROJECT_ID, "task_id": TASK_ID, "spent_date": "2026-08-03",
                "user_id": 1_782_959, "hours": 2.11, "notes": "Batch L",
                "external_reference": { "id": "1", "service": "example.test" },
            }))
            .respond_json(201, time_entry()),
        Expectation::new("PATCH", &format!("/v2/time_entries/{TIME_ENTRY_ID}"))
            .json_body(json!({
                "project_id": PROJECT_ID, "task_id": TASK_ID, "spent_date": "2026-08-03",
                "hours": 3.0, "notes": "Batch L, revised",
            }))
            .respond_json(200, time_entry()),
        Expectation::new("GET", &format!("/v2/projects/{PROJECT_ID}")).respond_json(
            200,
            json!({ "id": PROJECT_ID, "name": "Marketing Website" }),
        ),
        Expectation::new("GET", "/v2/projects")
            .query(PROJECT_QUERY)
            .respond_json(200, json!({ "projects": [], "links": {} })),
        Expectation::new("GET", &format!("/v2/clients/{CLIENT_ID}"))
            .respond_json(200, json!({ "id": CLIENT_ID, "name": "ACME" })),
        Expectation::new("GET", "/v2/clients")
            .query(CLIENT_QUERY)
            .respond_json(200, json!({ "clients": [], "links": {} })),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, &connector, id, input);
        // "All API requests … https://api.harvestapp.com/v2/".
        assert!(
            request.url().path().starts_with("/v2/"),
            "{id} renders under Harvest's own version prefix: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `harvest_auth_is_applied`: the configured Personal Access Token reaches the
/// wire as the `Bearer` credential Harvest publishes, the account and identity
/// headers travel with it, and a request with no token never renders.
#[tokio::test]
async fn harvest_auth_is_applied() {
    let connector = connector();
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/v2/time_entries/{TIME_ENTRY_ID}"),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .header("harvest-account-id", ACCOUNT_ID)
    .header("user-agent", USER_AGENT)
    .respond_json(200, time_entry())])
    .await;

    let request = render(
        &stub,
        &connector,
        "time_entry.get",
        json!({ "time_entry_id": TIME_ENTRY_ID }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!request.url_carries_credential());

    // The declared credential contract names both fields and carries neither
    // value, and it says which of the two is a secret.
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
            .any(|field| field.name() == harvest::ACCOUNT_ID && !field.is_secret())
    );

    // A declared credential that cannot be applied fails the attempt before a
    // byte leaves (ADR 043).
    let mut unauthenticated = operation(&connector, "time_entry.get")
        .plan_request(&stub.origin(), &json!({ "time_entry_id": TIME_ENTRY_ID }))
        .expect("the declared request renders");
    let failure = AuthPlan::bearer()
        .apply(
            &Credential::from_fields([(harvest::ACCOUNT_ID, Secret::new(ACCOUNT_ID))]),
            &mut unauthenticated,
            None,
        )
        .expect_err("an account id alone is not a credential");
    assert_eq!(failure.code(), "connector_credential_missing_field");

    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `harvest_account_id_is_configuration_and_the_token_is_a_secret` (spec 028
/// §3): the secret half reaches no `Debug`, no diagnostic, and no error, while
/// the non-secret half is deploy-time configuration on every request and cannot
/// be moved by input, a provider body, or a continuation.
#[tokio::test]
async fn harvest_account_id_is_configuration_and_the_token_is_a_secret() {
    let connector = connector();
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/v2/time_entries/{TIME_ENTRY_ID}"),
    )
    .header("harvest-account-id", ACCOUNT_ID)
    .respond_json(200, time_entry())])
    .await;
    let request = render(
        &stub,
        &connector,
        "time_entry.get",
        json!({ "time_entry_id": TIME_ENTRY_ID }),
    );

    // 1. The secret half. It is on the wire and nowhere else: not in the
    //    request's `Debug`, not in its redacted URL, not in the declaration.
    let printed = format!("{request:?}");
    assert!(!printed.contains(SECRET_SENTINEL), "{printed}");
    assert!(!request.redacted_url().contains(SECRET_SENTINEL));
    assert!(!format!("{connector:?}").contains(SECRET_SENTINEL));
    assert!(!format!("{:?}", connector.credential()).contains(SECRET_SENTINEL));
    assert!(!format!("{:?}", credential()).contains(SECRET_SENTINEL));

    // 2. The non-secret half. It is a header on every request, it is legible in
    //    a diagnostic, and that is the point: an operator debugging "which
    //    account did this reach" must be able to read it.
    assert_eq!(
        request
            .headers()
            .get("harvest-account-id")
            .and_then(|value| value.to_str().ok()),
        Some(ACCOUNT_ID)
    );
    assert!(
        !request
            .headers()
            .get("harvest-account-id")
            .expect("the account header is applied")
            .is_sensitive(),
        "the account id is not a secret, so it is not redacted"
    );
    assert!(printed.contains(ACCOUNT_ID), "{printed}");

    // 3. And nothing but deploy-time configuration can move it. Input cannot:
    //    no operation publishes the header or a slot behind it.
    for (id, mut input) in cases() {
        if let Some(fields) = input.as_object_mut() {
            fields.insert(harvest::ACCOUNT_ID.to_owned(), json!("7654321"));
        }
        let rendered = operation(&connector, id)
            .plan_request(&stub.origin(), &input)
            .expect("the declared request renders");
        assert_eq!(
            rendered
                .headers()
                .get("harvest-account-id")
                .and_then(|value| value.to_str().ok()),
            Some(ACCOUNT_ID),
            "{id} sends the configured account whatever the input says"
        );
        assert!(
            !operation(&connector, id)
                .project()
                .inputs()
                .iter()
                .any(|input| input.name() == harvest::ACCOUNT_ID),
            "{id} must not publish the account as a Process input"
        );
    }

    // A provider body naming another account is data, not a destination.
    let output = operation(&connector, "time_entry.get")
        .extract_output(&json!({ "id": TIME_ENTRY_ID, "notes": "account 7654321" }))
        .expect("the declared contract is satisfied");
    assert_eq!(output.get("notes"), Some(&json!("account 7654321")));

    // A continuation off this origin is refused rather than followed.
    let walk_stub = ProviderStub::start([Expectation::new("GET", "/v2/time_entries")
        .query(TIME_ENTRY_QUERY)
        .respond_json(
            200,
            json!({
                "time_entries": [],
                "links": { "next": "https://attacker.invalid/v2/time_entries?cursor=2" },
            }),
        )])
    .await;
    let failure = harvest::pagination("time_entry.list")
        .expect("time_entry.list declares a plan")
        .collect(
            render(
                &walk_stub,
                &connector,
                "time_entry.list",
                time_entry_filters(),
            ),
            &walk_stub.origin(),
            &PaginationBudget::new(4, 4, 16, 64 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| walk_stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    walk_stub.assert_satisfied();

    // And the configuration itself admits Harvest's own grammar and nothing
    // else, in both directions.
    for hostile in ["", "1234567\r\nX-Evil: 1", "acct-1234567", "12 34567", "-1"] {
        assert!(
            harvest::validate_account_id(hostile).is_err(),
            "`{hostile}` is not a Harvest account id"
        );
        assert!(harvest::connector(hostile, USER_AGENT).is_err());
    }
    assert!(harvest::validate_account_id(ACCOUNT_ID).is_ok());
    for hostile in ["", "Donat", "(only@contact.test)", "Donat ()"] {
        assert!(
            harvest::validate_user_agent(hostile).is_err(),
            "`{hostile}` is not the identification Harvest demands"
        );
        assert!(harvest::connector(ACCOUNT_ID, hostile).is_err());
    }
    assert!(harvest::validate_user_agent(USER_AGENT).is_ok());

    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `harvest_error_map`: every documented status reaches exactly one closed
/// class, and neither the token nor Harvest's prose crosses the boundary.
#[tokio::test]
async fn harvest_error_map() {
    let connector = connector();
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
            &format!("/v2/time_entries/{TIME_ENTRY_ID}"),
        )
        .respond_json(
            status,
            json!({ "message": format!("Spent date can't be blank for {SECRET_SENTINEL}") }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                &connector,
                "time_entry.get",
                json!({ "time_entry_id": TIME_ENTRY_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = harvest::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "can't be blank"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `harvest_rate_limit_is_classified`: "When the rate limit is exceeded Harvest
/// will send an HTTP 429 status code. The number of seconds until the throttle
/// is lifted is sent via the `Retry-After` HTTP header" — read, and clamped.
#[tokio::test]
async fn harvest_rate_limit_is_classified() {
    let connector = connector();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v2/time_entries/{TIME_ENTRY_ID}"))
            .respond_header("retry-after", "15")
            .respond_json(429, json!({ "message": "Throttled" })),
        Expectation::new("GET", &format!("/v2/time_entries/{TIME_ENTRY_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "message": "Throttled" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                &connector,
                "time_entry.get",
                json!({ "time_entry_id": TIME_ENTRY_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(harvest::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(15)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `harvest_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is Harvest's own `links.next`, it is followed as a destination
/// on this origin, and the walk makes exactly the number of requests the plan
/// declares.
#[tokio::test]
async fn harvest_cursor_is_opaque_and_bounded() {
    let connector = connector();
    let plan = harvest::pagination("time_entry.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/v2/time_entries")
            .query(TIME_ENTRY_QUERY)
            .respond_json(
                200,
                json!({
                    "time_entries": [{ "id": 1 }],
                    "per_page": 100, "total_pages": 2, "total_entries": 2,
                    "page": JsonValue::Null,
                    "next_page": JsonValue::Null,
                    "previous_page": JsonValue::Null,
                    "links": {
                        "first": "/v2/time_entries?per_page=100",
                        "next": "/v2/time_entries?cursor=eyJhZnRlciI6MX0&per_page=100",
                        "previous": JsonValue::Null,
                        "last": JsonValue::Null,
                    },
                }),
            ),
        // "If the response is using cursor based pagination, `page`,
        // `next_page`, and `previous_page` will always return `null`", and the
        // walk ends where `links.next` is absent.
        Expectation::new("GET", "/v2/time_entries")
            .query("cursor=eyJhZnRlciI6MX0&per_page=100")
            .respond_json(
                200,
                json!({
                    "time_entries": [{ "id": 2 }],
                    "links": { "next": JsonValue::Null },
                }),
            ),
    ])
    .await;

    let entries = plan
        .collect(
            render(&stub, &connector, "time_entry.list", time_entry_filters()),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one link and stops where the link stops");
    assert_eq!(entries, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `harvest_pagination_is_bounded`: the declared plan terminates under every
/// budget, no write declares one, and each plan reads the collection its own
/// operation publishes.
#[tokio::test]
async fn harvest_pagination_is_bounded() {
    let connector = connector();
    let plan = harvest::pagination("project.list").expect("the list declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v2/projects").respond_json(
                200,
                json!({
                    "projects": [{ "id": 1 }, { "id": 2 }],
                    "links": { "next": "/v2/projects?cursor=endless&per_page=100" },
                }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, &connector, "project.list", project_filters()),
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
        "time_entry.get",
        "time_entry.create",
        "time_entry.update",
        "project.get",
        "client.get",
    ] {
        assert!(
            harvest::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    for (id, items) in [
        ("time_entry.list", "/time_entries"),
        ("project.list", "/projects"),
        ("client.list", "/clients"),
    ] {
        assert_eq!(
            harvest::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            items,
            "{id}"
        );
    }
    // The aggregate a walk writes is the pointer the operation publishes.
    assert!(
        operation(&connector, "time_entry.list")
            .project()
            .outputs()
            .iter()
            .any(|output| output.name() == "time_entries")
    );
}

/// `harvest_effects_are_classified`: the create is `AtMostOnce` on a recorded
/// absence and a named consequence, the partial update stays unreachable, and
/// every read is a read.
#[test]
fn harvest_effects_are_classified() {
    let connector = connector();
    let expected = [
        ("time_entry.get", EffectClass::ReadOnly),
        ("time_entry.list", EffectClass::ReadOnly),
        ("time_entry.create", EffectClass::AtMostOnce),
        ("time_entry.update", EffectClass::InventoryOnly),
        ("project.get", EffectClass::ReadOnly),
        ("project.list", EffectClass::ReadOnly),
        ("client.get", EffectClass::ReadOnly),
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
        // Harvest publishes no idempotency mechanism at all, so nothing here
        // binds a key.
        assert!(operation.idempotency_binding().is_none(), "{id}");
    }

    assert_eq!(
        connector.admit_operation("time_entry.update"),
        Err(OperationRejection::InventoryOnly)
    );
    let reason = operation(&connector, "time_entry.update")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records its reason")
        .to_owned();
    assert!(reason.contains("PUT and DELETE only"), "{reason}");
    assert!(reason.contains("no consequence to record"), "{reason}");

    let evidence = operation(&connector, "time_entry.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on")
        .clone();
    assert!(
        evidence
            .searched_documentation()
            .contains("external_reference"),
        "the one credential-adjacent field that could have been a key is named"
    );
    assert!(evidence.repeat_produces().contains("a second time entry"));
}

/// `harvest_output_contract`: the declared pointers read Harvest's own objects,
/// and `hours` keeps the shape the provider publishes it in.
#[test]
fn harvest_output_contract() {
    let connector = connector();
    let get = operation(&connector, "time_entry.get");
    let decoded = get
        .decode_response(
            200,
            &serde_json::to_vec(&time_entry()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(decoded.get("id"), Some(&json!(TIME_ENTRY_ID)));
    // ADR 071: Harvest publishes `hours` as a JSON number, so the contract
    // carries a number rather than a string.
    assert_eq!(decoded.get("hours"), Some(&json!(2.11)));
    assert_eq!(decoded.get("rounded_hours"), Some(&json!(2.25)));
    assert_eq!(decoded.get("external_reference"), Some(&JsonValue::Null));

    // Only the identity is required.
    assert_eq!(
        get.decode_response(200, br#"{"id":636708723}"#)
            .expect("only the identity is required")
            .get("id"),
        Some(&json!(TIME_ENTRY_ID))
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"636708723"}"#)
            .expect_err("a Harvest id is a number")
            .class(),
        ConnectorErrorClass::Validation
    );

    // A collection is the envelope Harvest publishes, with its continuation
    // carried as data.
    let list = operation(&connector, "time_entry.list")
        .decode_response(
            200,
            br#"{"time_entries":[{"id":1}],"total_entries":1,"total_pages":1,
                 "links":{"next":null}}"#,
        )
        .expect("the declared contract is satisfied");
    assert_eq!(list.get("time_entries"), Some(&json!([{ "id": 1 }])));
    assert_eq!(list.get("total_entries"), Some(&json!(1)));
    assert_eq!(list.get("links"), Some(&json!({ "next": JsonValue::Null })));
}
