//! Jira connector proofs (spec 016 §3), against the SDK's local provider stub.
//!
//! The proof this connector owes on its own is `jira_site_host_is_deploy_time`:
//! the templated Atlassian host comes only from connector configuration, and
//! input, response, and pagination each get a turn at moving it.

use std::time::Duration;

use donat_connectors::providers::jira;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    Operation, OperationRejection, PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const EMAIL: &str = "integrations@example.test";
const ISSUE_KEY: &str = "ACM-42";
/// The same key as the SDK renders it into one path segment.
const ISSUE_SEGMENT: &str = "ACM%2D42";
const ACCOUNT_ID: &str = "5b10ac8d82e05b22cc7d4ef5";

fn declaration() -> Connector {
    jira::connector(EMAIL).expect("a valid account address declares")
}

fn operation(connector: &Connector, id: &str) -> Operation {
    connector
        .operation(id)
        .unwrap_or_else(|| panic!("the jira declaration publishes {id}"))
        .clone()
}

fn render(stub: &ProviderStub, connector: &Connector, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(connector, id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::basic(EMAIL)
        .expect("a static username is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn issue() -> JsonValue {
    json!({
        "id": "10002",
        "key": ISSUE_KEY,
        "self": "https://acme.atlassian.net/rest/api/3/issue/10002",
        "fields": { "summary": "Ship it", "attachment": [] },
    })
}

/// Every operation, with an input that satisfies it.
fn inputs() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "issue.get",
            json!({ "issueIdOrKey": ISSUE_KEY, "fields": "summary,status" }),
        ),
        (
            "issue.search",
            json!({ "jql": "project = ACM ORDER BY created DESC", "fields": "summary" }),
        ),
        (
            "issue.create",
            json!({ "fields": { "project": { "key": "ACM" }, "summary": "New" }, "update": {} }),
        ),
        (
            "issue.update",
            json!({ "issueIdOrKey": ISSUE_KEY, "fields": { "summary": "Renamed" }, "update": {} }),
        ),
        (
            "issue.transition",
            json!({ "issueIdOrKey": ISSUE_KEY, "transition": { "id": "5" }, "fields": {} }),
        ),
        ("comment.list", json!({ "issueIdOrKey": ISSUE_KEY })),
        (
            "comment.add",
            json!({ "issueIdOrKey": ISSUE_KEY,
                    "body": { "type": "doc", "version": 1, "content": [] } }),
        ),
        ("attachment.list", json!({ "issueIdOrKey": ISSUE_KEY })),
        ("user.get", json!({ "user_account_id": ACCOUNT_ID })),
    ]
}

/// `jira_request_shape`: exact method, path, query, headers, and body for every
/// operation the declaration publishes.
#[tokio::test]
async fn jira_request_shape() {
    let connector = declaration();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"))
            .query("fields=summary%2Cstatus")
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, issue()),
        Expectation::new("GET", "/rest/api/3/search/jql")
            .query(
                "jql=project%20%3D%20ACM%20ORDER%20BY%20created%20DESC&fields=summary&maxResults=100",
            )
            .respond_json(200, json!({ "issues": [], "isLast": true })),
        Expectation::new("POST", "/rest/api/3/issue")
            .header("content-type", "application/json")
            .json_body(json!({ "fields": { "project": { "key": "ACM" }, "summary": "New" },
                               "update": {} }))
            .respond_json(
                201,
                json!({ "id": "10002", "key": ISSUE_KEY, "self": "https://acme/rest/api/3/issue/10002" }),
            ),
        Expectation::new("PUT", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"))
            .json_body(json!({ "fields": { "summary": "Renamed" }, "update": {} }))
            .respond_bytes(204, Vec::new()),
        Expectation::new(
            "POST",
            &format!("/rest/api/3/issue/{ISSUE_SEGMENT}/transitions"),
        )
        .json_body(json!({ "transition": { "id": "5" }, "fields": {} }))
        .respond_bytes(204, Vec::new()),
        Expectation::new("GET", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}/comment"))
            .query("maxResults=100")
            .respond_json(200, json!({ "comments": [], "startAt": 0, "maxResults": 100, "total": 0 })),
        Expectation::new("POST", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}/comment"))
            .json_body(json!({ "body": { "type": "doc", "version": 1, "content": [] } }))
            .respond_json(
                201,
                json!({ "id": "10000", "self": "https://acme/rest/api/3/issue/10002/comment/10000",
                        "created": "2026-08-10T10:00:00.000+0000" }),
            ),
        Expectation::new("GET", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"))
            .query("fields=attachment")
            .respond_json(200, issue()),
        Expectation::new("GET", "/rest/api/3/user")
            .query(&format!("accountId={ACCOUNT_ID}"))
            .respond_json(
                200,
                json!({ "accountId": ACCOUNT_ID, "displayName": "Mia", "active": true }),
            ),
    ])
    .await;

    for (id, input) in inputs() {
        stub.send(render(&stub, &connector, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `jira_site_host_is_deploy_time` (spec 016 §3 proof 3): the templated host
/// comes only from connector configuration; input, response, and pagination
/// cannot change it.
#[tokio::test]
async fn jira_site_host_is_deploy_time() {
    let connector = declaration();
    assert_eq!(connector.origin().host_variable(), Some(jira::SITE));

    let origin = connector
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            jira::SITE,
            "acme",
        )]))
        .expect("a configured site resolves");
    assert_eq!(origin.as_url().as_str(), "https://acme.atlassian.net/");

    // 1. Operation input. A path value that spells another authority stays one
    //    percent-encoded segment on the configured host.
    let request = operation(&connector, "issue.get")
        .plan_request(
            &origin,
            &json!({ "issueIdOrKey": "../../..//evil.example.test/x", "fields": "summary" }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("acme.atlassian.net"));
    assert_eq!(request.url().scheme(), "https");
    assert!(
        request.url().path().starts_with("/rest/api/3/issue/"),
        "{}",
        request.url().path()
    );
    assert!(!request.url().path().contains("evil.example.test/x"));

    // 2. A provider response naming another host is data, not a destination.
    let output = operation(&connector, "issue.get")
        .extract_output(&json!({
            "id": "1", "key": "ACM-1",
            "self": "https://attacker.invalid/rest/api/3/issue/1",
            "fields": {},
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output["self"],
        json!("https://attacker.invalid/rest/api/3/issue/1")
    );
    assert_eq!(
        operation(&connector, "issue.get")
            .plan_request(
                &origin,
                &json!({ "issueIdOrKey": "ACM-2", "fields": "summary" })
            )
            .expect("the next request renders")
            .url()
            .host_str(),
        Some("acme.atlassian.net")
    );

    // 3. A continuation cursor is a query value on the compiled origin, never a
    //    destination: a token that spells a URL comes back percent-encoded.
    let plan = jira::pagination("issue.search").expect("the search declares a plan");
    let stub = ProviderStub::start([
        Expectation::new("GET", "/rest/api/3/search/jql").respond_json(
            200,
            json!({ "issues": [{ "id": "1" }],
                    "nextPageToken": "https://attacker.invalid/rest/api/3/search/jql" }),
        ),
        Expectation::new("GET", "/rest/api/3/search/jql")
            .query(
                "jql=x&fields=summary&maxResults=100&nextPageToken=https%3A%2F%2Fattacker%2Einvalid%2Frest%2Fapi%2F3%2Fsearch%2Fjql",
            )
            .respond_json(200, json!({ "issues": [], "isLast": true })),
    ])
    .await;
    let issues = plan
        .collect(
            render(
                &stub,
                &connector,
                "issue.search",
                json!({ "jql": "x", "fields": "summary" }),
            ),
            &stub.origin(),
            &PaginationBudget::new(8, 8, 100, 256 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a token that spells a URL is spent as a query value");
    assert_eq!(issues.len(), 1);
    stub.assert_satisfied();

    // And the configuration itself admits one host label and nothing else.
    for hostile in [
        "acme.atlassian.net",
        "acme/../evil",
        "acme:8080",
        "user@acme",
        "",
        "-acme",
        "ACME",
    ] {
        assert!(
            connector
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    jira::SITE,
                    hostile
                )]))
                .is_err(),
            "configuration value {hostile} must not resolve"
        );
    }
    assert!(
        connector
            .resolve_origin(&ConnectorConfiguration::default())
            .is_err(),
        "an unconfigured site is a startup failure"
    );
}

/// The Basic username is deploy-time material too, and a value that is not an
/// account address does not build a declaration at all.
#[test]
fn jira_basic_username_is_deploy_time() {
    for hostile in [
        "",
        "not-an-email",
        "two@at@signs.test",
        "user:password@example.test",
        "user name@example.test",
        "user@localhost",
    ] {
        assert!(
            jira::connector(hostile).is_err(),
            "a Basic username of `{hostile}` must not declare"
        );
    }
    assert!(jira::connector(EMAIL).is_ok());
    assert!(
        jira::declaration_shape().is_ok(),
        "the module table's declaration shape builds"
    );
}

/// `jira_auth_is_applied`: the API token reaches the wire inside a Basic
/// credential and appears nowhere else, in particular not in clear.
#[tokio::test]
async fn jira_auth_is_applied() {
    let connector = declaration();
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"),
    )
    .without_header("x-atlassian-token")
    .respond_json(200, issue())])
    .await;

    let request = render(
        &stub,
        &connector,
        "issue.get",
        json!({ "issueIdOrKey": ISSUE_KEY, "fields": "summary" }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied")
        .to_str()
        .expect("a basic credential is visible ASCII")
        .to_owned();
    assert!(applied.starts_with("Basic "), "{applied}");
    assert!(
        !applied.contains(SECRET_SENTINEL),
        "the API token is encoded, never echoed"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        connector.credential(),
        jira::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `jira_error_map`: every documented status reaches exactly one closed class,
/// and none of Jira's `errorMessages` prose crosses the boundary.
#[tokio::test]
async fn jira_error_map() {
    let connector = declaration();
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (413, ConnectorErrorClass::Validation),
        (422, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (410, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"),
        )
        .respond_header("retry-after", "4")
        .respond_json(
            status,
            json!({
                "errorMessages": [format!("site acme node jira-7 token {SECRET_SENTINEL}")],
                "errors": { "summary": "Field 'summary' cannot be set." },
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                &connector,
                "issue.get",
                json!({ "issueIdOrKey": ISSUE_KEY, "fields": "summary" }),
            ))
            .await
            .expect("the stub answers");

        let failure = jira::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "jira-7", "cannot be set"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `jira_rate_limit_is_classified` (spec 016 §3 proof 5): "When any limit is
/// exceeded, Jira returns an HTTP `429 Too Many Requests` response", and its
/// `Retry-After` is clamped.
#[tokio::test]
async fn jira_rate_limit_is_classified() {
    let connector = declaration();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"))
            .respond_header("retry-after", "12")
            .respond_json(429, json!({ "errorMessages": ["Rate limit exceeded"] })),
        // "Some transient 5xx responses (such as 503) may also include a
        // `Retry-After` header", and the ceiling applies there too.
        Expectation::new("GET", &format!("/rest/api/3/issue/{ISSUE_SEGMENT}"))
            .respond_header("retry-after", "604800")
            .respond_json(503, json!({ "errorMessages": ["Service unavailable"] })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                &connector,
                "issue.get",
                json!({ "issueIdOrKey": ISSUE_KEY, "fields": "summary" }),
            ))
            .await
            .expect("the stub answers");
        failures.push(jira::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(12)));
    assert_eq!(failures[1].class(), ConnectorErrorClass::Http5xx);
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `jira_cursor_is_opaque_and_bounded` (spec 016 §3 proof 4): the search cursor
/// is echoed verbatim and never parsed, the comment offset is derived from the
/// walk rather than from the provider, and both stop at every budget.
#[tokio::test]
async fn jira_cursor_is_opaque_and_bounded() {
    let connector = declaration();
    let search = jira::pagination("issue.search").expect("the search declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const TOKEN: &str = "CAEaAggD&maxResults=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/rest/api/3/search/jql")
            .query("jql=x&fields=summary&maxResults=100")
            .respond_json(
                200,
                json!({ "issues": [{ "id": "1" }], "nextPageToken": TOKEN, "isLast": false }),
            ),
        Expectation::new("GET", "/rest/api/3/search/jql")
            .query(
                "jql=x&fields=summary&maxResults=100&nextPageToken=CAEaAggD%26maxResults%3D9999%23%2F%2E%2E%2F",
            )
            // "If this result represents the last or the only page this token
            // will be null."
            .respond_json(200, json!({ "issues": [{ "id": "2" }], "isLast": true })),
    ])
    .await;
    let issues = search
        .collect(
            render(
                &stub,
                &connector,
                "issue.search",
                json!({ "jql": "x", "fields": "summary" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one token and stops on the absent one");
    assert_eq!(issues.len(), 2);
    stub.assert_satisfied();
}

/// `jira_pagination_is_bounded`: both declared plans terminate and respect
/// the call, page, item, and byte budgets.
#[tokio::test]
async fn jira_pagination_is_bounded() {
    let connector = declaration();
    let search = jira::pagination("issue.search").expect("the search declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/rest/api/3/search/jql").respond_json(
                200,
                json!({ "issues": [{ "id": "1" }, { "id": "2" }], "nextPageToken": "more" }),
            )
        }))
        .await;
        let failure = search
            .collect(
                render(
                    &stub,
                    &connector,
                    "issue.search",
                    json!({ "jql": "x", "fields": "summary" }),
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

    // The offset walk: `startAt` is the walk's own running total, never the
    // provider's `startAt`, so a provider that lies about it cannot rewind.
    let comments = jira::pagination("comment.list").expect("the comments declare a plan");
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/rest/api/3/issue/{ISSUE_SEGMENT}/comment"),
    )
    .query("maxResults=100&startAt=0")
    .respond_json(
        200,
        json!({ "comments": [{ "id": "1" }], "startAt": 900, "maxResults": 100, "total": 5000 }),
    )])
    .await;
    let page = comments
        .collect(
            render(
                &stub,
                &connector,
                "comment.list",
                json!({ "issueIdOrKey": ISSUE_KEY }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a short page ends the walk");
    assert_eq!(
        page.len(),
        1,
        "a provider `startAt` of 900 cannot restart it"
    );
    stub.assert_satisfied();

    for id in [
        "issue.get",
        "issue.create",
        "issue.update",
        "issue.transition",
        "comment.add",
        "attachment.list",
        "user.get",
    ] {
        assert!(
            jira::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `jira_effects_are_classified`: every operation carries a class, and every
/// write is inventory-only on a machine-checkable absence.
#[test]
fn jira_effects_are_classified() {
    let connector = declaration();
    let expected = [
        ("issue.get", EffectClass::ReadOnly),
        ("issue.search", EffectClass::ReadOnly),
        ("issue.create", EffectClass::AtMostOnce),
        ("issue.update", EffectClass::InventoryOnly),
        ("issue.transition", EffectClass::AtMostOnce),
        ("comment.list", EffectClass::ReadOnly),
        ("comment.add", EffectClass::AtMostOnce),
        ("attachment.list", EffectClass::ReadOnly),
        ("user.get", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(
            operation(&connector, id).effect_class(),
            Some(class),
            "{id}"
        );
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(
            operation(&connector, id).idempotency_binding().is_none(),
            "{id}"
        );
    }
    assert_eq!(
        connector.admit_operation("issue.update"),
        Err(OperationRejection::InventoryOnly),
        "a documented partial edit is not the class ADR 063 admits"
    );
    assert!(
        operation(&connector, "issue.create")
            .effect()
            .and_then(|effect| effect.no_idempotency_evidence())
            .is_some_and(|evidence| evidence.repeat_produces().contains("a second issue")),
        "the create records what a second send would leave behind"
    );
    assert!(
        operation(&connector, "issue.update")
            .effect()
            .and_then(|effect| effect.inventory_reason())
            .is_some_and(|reason| reason.contains("Edits an issue")),
        "the PUT records Atlassian's own description as the reason the gate refuses it"
    );
}

/// `jira_output_contract`: the declared pointers read Jira's own objects, and a
/// documented empty-bodied success is a success rather than a validation
/// failure.
#[test]
fn jira_output_contract() {
    let connector = declaration();
    let get = operation(&connector, "issue.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&issue()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": "10002",
            "key": ISSUE_KEY,
            "self": "https://acme.atlassian.net/rest/api/3/issue/10002",
            "fields": { "summary": "Ship it", "attachment": [] },
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"1"}"#)
            .expect_err("a missing required pointer is a failure")
            .class(),
        ConnectorErrorClass::Validation
    );

    // "Edits an issue" answers `204 No Content`, and silence is the documented
    // success.
    let update = operation(&connector, "issue.update");
    assert!(update.is_success(204) && update.is_no_content_success(204));
    assert_eq!(
        update
            .decode_response(204, b"")
            .expect("the documented empty success decodes"),
        json!({})
    );
    assert!(
        !update.is_success(200),
        "the `200` form needs `returnIssue=true`, which this declaration does not send"
    );

    // The attachment read is the issue read narrowed to one field.
    assert_eq!(
        operation(&connector, "attachment.list")
            .decode_response(
                200,
                br#"{"key":"ACM-42","fields":{"attachment":[{"id":"10000","filename":"a.png"}]}}"#,
            )
            .expect("the declared contract is satisfied"),
        json!({ "key": ISSUE_KEY, "attachments": [{ "id": "10000", "filename": "a.png" }] })
    );
}
