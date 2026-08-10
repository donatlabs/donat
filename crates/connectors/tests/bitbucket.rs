//! Bitbucket connector proofs (spec 027 §3, which adopts spec 023 §4), against
//! the SDK's local provider stub.

use std::sync::LazyLock;
use std::time::Duration;

use base64::Engine;
use donat_connectors::providers::bitbucket;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation, PaginationBudget,
    RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const EMAIL: &str = "ci@example.test";
const WORKSPACE: &str = "acme";
const REPO: &str = "platform";
const ISSUE_ID: i64 = 42;
const PR_ID: i64 = 8;

fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> =
        LazyLock::new(|| bitbucket::connector(EMAIL).expect("a valid account address declares"));
    &CONNECTOR
}

fn operation(id: &str) -> &'static Operation {
    connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the bitbucket declaration publishes {id}"))
}

/// "use Basic HTTP Authentication as per RFC-2617, where the username is your
/// Atlassian email and password is the API token."
fn expected_authorization() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{EMAIL}:{SECRET_SENTINEL}"))
    )
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::basic(EMAIL)
        .expect("the published username form is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn base() -> String {
    format!("/2.0/repositories/{WORKSPACE}/{REPO}")
}

fn issue() -> JsonValue {
    json!({
        "id": ISSUE_ID, "title": "Disk is full", "state": "new", "kind": "bug",
        "priority": "major", "created_on": "2026-08-10T09:00:00.000000+00:00",
        "links": { "html": { "href": "https://bitbucket.org/acme/platform/issues/42" } },
    })
}

fn pull_request() -> JsonValue {
    json!({
        "id": PR_ID, "title": "Rotate the key", "state": "OPEN",
        "source": { "branch": { "name": "rotate" } },
        "destination": { "branch": { "name": "main" } },
        "created_on": "2026-08-10T09:00:00.000000+00:00",
        "links": { "html": { "href": "https://bitbucket.org/acme/platform/pull-requests/8" } },
    })
}

fn comment() -> JsonValue {
    json!({ "id": 991, "content": { "raw": "Lorem ipsum." },
            "created_on": "2026-08-10T09:05:00.000000+00:00" })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "repository.get",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
        ),
        (
            "issue.get",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO, "issue_id": ISSUE_ID }),
        ),
        (
            "issue.list",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
        ),
        (
            "issue.create",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO, "title": "Disk is full",
                    "content": "It is full.", "kind": "bug", "priority": "major" }),
        ),
        (
            "issue_comment.create",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO, "issue_id": ISSUE_ID,
                    "content": "Lorem ipsum." }),
        ),
        (
            "pull_request.get",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO, "pull_request_id": PR_ID }),
        ),
        (
            "pull_request.list",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
        ),
        (
            "pull_request.create",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO, "title": "Rotate the key",
                    "source_branch": "rotate", "destination_branch": "main",
                    "summary": "Rotates it.", "close_source_branch": true }),
        ),
        (
            "pull_request_comment.create",
            json!({ "workspace": WORKSPACE, "repo_slug": REPO, "pull_request_id": PR_ID,
                    "content": "Lorem ipsum." }),
        ),
    ]
}

/// `bitbucket_request_shape`: exact method, path, query, headers and body for
/// every operation, all under the published `/2.0` prefix.
#[tokio::test]
async fn bitbucket_request_shape() {
    let base = base();
    let stub = ProviderStub::start([
        Expectation::new("GET", &base)
            .query("")
            .header("authorization", &expected_authorization())
            .no_body()
            .respond_json(
                200,
                json!({ "uuid": "{repo-uuid}", "name": "platform",
                        "full_name": "acme/platform",
                        "mainbranch": { "name": "main" }, "is_private": true,
                        "links": { "html": { "href": "https://bitbucket.org/acme/platform" } } }),
            ),
        Expectation::new("GET", &format!("{base}/issues/{ISSUE_ID}"))
            .query("")
            .respond_json(200, issue()),
        Expectation::new("GET", &format!("{base}/issues"))
            .query("pagelen=100")
            .respond_json(
                200,
                json!({ "values": [issue()], "size": 1, "pagelen": 100 }),
            ),
        Expectation::new("POST", &format!("{base}/issues"))
            .json_body(
                json!({ "title": "Disk is full", "content": { "raw": "It is full." },
                               "kind": "bug", "priority": "major" }),
            )
            .respond_json(201, issue()),
        Expectation::new("POST", &format!("{base}/issues/{ISSUE_ID}/comments"))
            .json_body(json!({ "content": { "raw": "Lorem ipsum." } }))
            .respond_json(201, comment()),
        Expectation::new("GET", &format!("{base}/pullrequests/{PR_ID}"))
            .query("")
            .respond_json(200, pull_request()),
        Expectation::new("GET", &format!("{base}/pullrequests"))
            .query("pagelen=100")
            .respond_json(
                200,
                json!({ "values": [pull_request()], "size": 1, "pagelen": 100 }),
            ),
        Expectation::new("POST", &format!("{base}/pullrequests"))
            .json_body(json!({
                "title": "Rotate the key",
                "source": { "branch": { "name": "rotate" } },
                "destination": { "branch": { "name": "main" } },
                "summary": { "raw": "Rotates it." },
                "close_source_branch": true,
            }))
            .respond_json(201, pull_request()),
        Expectation::new("POST", &format!("{base}/pullrequests/{PR_ID}/comments"))
            .json_body(json!({ "content": { "raw": "Lorem ipsum." } }))
            .respond_json(201, comment()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/2.0/"),
            "{id} renders the published prefix: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `bitbucket_auth_is_applied`: the API token reaches the wire as the Basic
/// password under the configured Atlassian address, never as a query value.
#[tokio::test]
async fn bitbucket_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &base())
        .header("authorization", &expected_authorization())
        .respond_json(200, json!({ "uuid": "{repo-uuid}" }))])
    .await;

    let request = render(
        &stub,
        "repository.get",
        json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("a base64 header is ASCII"),
        expected_authorization()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        connector().credential(),
        bitbucket::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();

    // The Basic username is a deployment's, refused here rather than at the
    // provider's first `401`.
    for hostile in [
        "",
        "ci",
        "ci@example",
        "ci:token@example.test",
        "ci @example.test",
    ] {
        assert!(bitbucket::connector(hostile).is_err(), "`{hostile}`");
    }
    assert!(bitbucket::declaration_shape().is_ok());
}

/// `bitbucket_error_map`: every documented status reaches exactly one closed
/// class, and none of Bitbucket's prose crosses the boundary.
#[tokio::test]
async fn bitbucket_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (410, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &base()).respond_json(
            status,
            json!({
                "type": "error",
                "error": {
                    "message": format!("bitbucket.org rejected {SECRET_SENTINEL}"),
                    "detail": "repository not found",
                },
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "repository.get",
                json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
            ))
            .await
            .expect("the stub answers");

        let failure = bitbucket::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "bitbucket.org", "repository not found"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `bitbucket_rate_limit_is_classified`: a throttle is retryable and its hint is
/// clamped.
#[tokio::test]
async fn bitbucket_rate_limit_is_classified() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &base()).respond_json(429, json!({ "type": "error" })),
        Expectation::new("GET", &base())
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "type": "error" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "repository.get",
                json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
            ))
            .await
            .expect("the stub answers");
        failures.push(bitbucket::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), None);
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `bitbucket_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the
/// continuation is the `next` URL Bitbucket asks callers to follow, it is a
/// destination checked against the compiled origin, and the walk makes exactly
/// the number of requests the plan declares (ADR 058).
#[tokio::test]
async fn bitbucket_cursor_is_opaque_and_bounded() {
    let plan = bitbucket::pagination("issue.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    let path = format!("{}/issues", base());

    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .query("pagelen=100")
            .respond_json(
                200,
                json!({ "values": [{ "id": 1 }], "size": 2, "pagelen": 100,
                        "next": format!("{path}?pagelen=100&page=2") }),
            ),
        Expectation::new("GET", &path)
            .query("pagelen=100&page=2")
            .respond_json(200, json!({ "values": [{ "id": 2 }], "size": 2 })),
    ])
    .await;

    let issues = plan
        .collect(
            render(
                &stub,
                "issue.list",
                json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one `next` and stops where the provider stops");
    assert_eq!(issues, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();

    // A `next` outside the compiled origin is refused rather than followed.
    let hostile = ProviderStub::start([Expectation::new("GET", &path).respond_json(
        200,
        json!({ "values": [], "next": "https://attacker.invalid/2.0/x" }),
    )])
    .await;
    let failure = plan
        .collect(
            render(
                &hostile,
                "issue.list",
                json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
            ),
            &hostile.origin(),
            &budget,
            undeclared_status_gate,
            |request| hostile.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    hostile.assert_satisfied();
}

/// `bitbucket_pagination_is_bounded`: the declared plan terminates and respects
/// the call, page, item and byte budgets, and only the two collections declare
/// it.
#[tokio::test]
async fn bitbucket_pagination_is_bounded() {
    let plan = bitbucket::pagination("issue.list").expect("the list declares a plan");
    let path = format!("{}/issues", base());
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &path).respond_json(
                200,
                json!({ "values": [{ "id": 1 }, { "id": 2 }],
                        "next": format!("{path}?page=9") }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "issue.list",
                    json!({ "workspace": WORKSPACE, "repo_slug": REPO }),
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

    for id in ["issue.list", "pull_request.list"] {
        assert_eq!(
            bitbucket::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            "/values",
            "{id} collects the wrapper Bitbucket answers with"
        );
    }
    for id in [
        "repository.get",
        "issue.get",
        "issue.create",
        "issue_comment.create",
        "pull_request.get",
        "pull_request.create",
        "pull_request_comment.create",
    ] {
        assert!(
            bitbucket::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `bitbucket_effects_are_classified`: every operation carries a class, and the
/// four creates are reachable only through an activity that says what an unknown
/// outcome means.
#[test]
fn bitbucket_effects_are_classified() {
    let expected = [
        ("repository.get", EffectClass::ReadOnly),
        ("issue.get", EffectClass::ReadOnly),
        ("issue.list", EffectClass::ReadOnly),
        ("issue.create", EffectClass::AtMostOnce),
        ("issue_comment.create", EffectClass::AtMostOnce),
        ("pull_request.get", EffectClass::ReadOnly),
        ("pull_request.list", EffectClass::ReadOnly),
        ("pull_request.create", EffectClass::AtMostOnce),
        ("pull_request_comment.create", EffectClass::AtMostOnce),
    ];
    assert_eq!(connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }

    let evidence = operation("pull_request.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(
        evidence
            .searched_documentation()
            .contains("default reviewers"),
        "the two idempotency statements Bitbucket does publish are named, and they are elsewhere"
    );
    assert!(
        evidence.repeat_produces().contains("second pull request"),
        "{}",
        evidence.repeat_produces()
    );
}

/// `bitbucket_triggering_is_not_a_read` (spec 027 §3): every write here opens or
/// comments on something a human is notified about, so none of them may be
/// classified `ReadOnly`.
#[test]
fn bitbucket_triggering_is_not_a_read() {
    for id in [
        "issue.create",
        "issue_comment.create",
        "pull_request.create",
        "pull_request_comment.create",
    ] {
        let class = operation(id)
            .effect_class()
            .expect("every operation has one");
        assert_ne!(class, EffectClass::ReadOnly, "{id}");
        assert!(class.requires_at_most_once_opt_in(), "{id}");
    }
    for id in ["repository.get", "issue.get", "pull_request.list"] {
        assert_eq!(operation(id).effect_class(), Some(EffectClass::ReadOnly));
    }
}

/// `bitbucket_output_contract`: the declared pointers read Bitbucket's own
/// nested objects, and a documented empty success stays a success.
#[test]
fn bitbucket_output_contract() {
    let get = operation("issue.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&issue()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": ISSUE_ID, "title": "Disk is full", "state": "new", "kind": "bug",
            "priority": "major", "created_on": "2026-08-10T09:00:00.000000+00:00",
            "html_url": "https://bitbucket.org/acme/platform/issues/42",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"42"}"#)
            .expect_err("an id that is not an integer is not an issue")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":42}"#)
            .expect("only the identity is required")
            .get("html_url"),
        Some(&json!(null))
    );
    // Bitbucket publishes no response schema for the issue comment `201`, and
    // this declaration demands the created comment rather than admitting an
    // empty success it was never promised.
    assert_eq!(
        operation("issue_comment.create")
            .decode_response(201, b"{}")
            .expect_err("a created comment carries an id")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        bitbucket::decode(
            get,
            404,
            &reqwest::header::HeaderMap::new(),
            br#"{"type":"error","error":{"message":"Not found"}}"#
        )
        .expect_err("a 404 is not a success")
        .class(),
        ConnectorErrorClass::Permanent
    );
}
