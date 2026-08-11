//! GitLab connector proofs (spec 027 §3, which adopts spec 023 §4), against the
//! SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::gitlab;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

/// "The ID or URL-encoded path of the project": this connector is tested with
/// the path form, because that is the one an encoding mistake would break.
const PROJECT: &str = "acme/platform";
const PROJECT_ENCODED: &str = "acme%2Fplatform";
const ISSUE_IID: i64 = 7;
const MR_IID: i64 = 12;
const PIPELINE_ID: i64 = 4242;

fn operation(id: &str) -> &'static Operation {
    gitlab::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the gitlab declaration publishes {id}"))
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

fn issue() -> JsonValue {
    json!({
        "id": 91, "iid": ISSUE_IID, "project_id": 3, "title": "Disk is full",
        "state": "opened", "web_url": "https://gitlab.example.test/acme/platform/-/issues/7",
        "created_at": "2026-08-10T09:00:00.000Z",
    })
}

fn merge_request() -> JsonValue {
    json!({
        "id": 55, "iid": MR_IID, "project_id": 3, "title": "Rotate the key",
        "state": "opened", "source_branch": "rotate", "target_branch": "main",
        "web_url": "https://gitlab.example.test/acme/platform/-/merge_requests/12",
        "created_at": "2026-08-10T09:00:00.000Z",
    })
}

fn note() -> JsonValue {
    json!({ "id": 302, "body": "Deployed.", "system": false,
            "created_at": "2026-08-10T09:05:00.000Z" })
}

fn pipeline() -> JsonValue {
    json!({
        "id": PIPELINE_ID, "iid": 9, "project_id": 3,
        "sha": "b83d6e391c22777fca1ed3012fce84f633d7fed0", "ref": "main",
        "status": "pending",
        "web_url": "https://gitlab.example.test/acme/platform/-/pipelines/4242",
        "created_at": "2026-08-10T09:00:00.000Z",
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("project.get", json!({ "project_id": PROJECT })),
        (
            "issue.get",
            json!({ "project_id": PROJECT, "issue_iid": ISSUE_IID }),
        ),
        (
            "issue.list",
            json!({ "project_id": PROJECT, "state": "all" }),
        ),
        (
            "issue.create",
            json!({ "project_id": PROJECT, "title": "Disk is full", "description": null,
                    "labels": null, "assignee_ids": [], "issue_type": null,
                    "confidential": false }),
        ),
        (
            "issue_note.create",
            json!({ "project_id": PROJECT, "issue_iid": ISSUE_IID, "body": "Deployed." }),
        ),
        (
            "merge_request.get",
            json!({ "project_id": PROJECT, "merge_request_iid": MR_IID }),
        ),
        (
            "merge_request.list",
            json!({ "project_id": PROJECT, "state": "all" }),
        ),
        (
            "merge_request.create",
            json!({ "project_id": PROJECT, "source_branch": "rotate", "target_branch": "main",
                    "title": "Rotate the key", "description": null, "labels": null,
                    "remove_source_branch": true }),
        ),
        (
            "merge_request_note.create",
            json!({ "project_id": PROJECT, "merge_request_iid": MR_IID, "body": "Deployed." }),
        ),
        (
            "pipeline.get",
            json!({ "project_id": PROJECT, "pipeline_id": PIPELINE_ID }),
        ),
        ("pipeline.list", json!({ "project_id": PROJECT })),
        (
            "pipeline.trigger",
            json!({ "project_id": PROJECT, "ref": "main",
                    "variables": [{ "key": "TEST", "value": "1" }] }),
        ),
    ]
}

/// `gitlab_request_shape`: exact method, path, query, headers and body for every
/// operation, all under the published `/api/v4` prefix, with the project path
/// percent-encoded into one segment.
#[tokio::test]
async fn gitlab_request_shape() {
    let base = format!("/api/v4/projects/{PROJECT_ENCODED}");
    let stub = ProviderStub::start([
        Expectation::new("GET", &base)
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(
                200,
                json!({ "id": 3, "name": "platform", "path_with_namespace": PROJECT,
                        "default_branch": "main", "web_url": "https://gitlab.example.test/acme/platform",
                        "visibility": "private" }),
            ),
        Expectation::new("GET", &format!("{base}/issues/{ISSUE_IID}"))
            .query("")
            .respond_json(200, issue()),
        Expectation::new("GET", &format!("{base}/issues"))
            .query("state=all&per_page=100")
            .respond_json(200, json!([issue()])),
        Expectation::new("POST", &format!("{base}/issues"))
            .json_body(json!({ "title": "Disk is full", "description": null, "labels": null,
                               "assignee_ids": [], "issue_type": null, "confidential": false }))
            .respond_json(201, issue()),
        Expectation::new("POST", &format!("{base}/issues/{ISSUE_IID}/notes"))
            .json_body(json!({ "body": "Deployed." }))
            .respond_json(201, note()),
        Expectation::new("GET", &format!("{base}/merge_requests/{MR_IID}"))
            .query("")
            .respond_json(200, merge_request()),
        Expectation::new("GET", &format!("{base}/merge_requests"))
            .query("state=all&per_page=100")
            .respond_json(200, json!([merge_request()])),
        Expectation::new("POST", &format!("{base}/merge_requests"))
            .json_body(json!({ "source_branch": "rotate", "target_branch": "main",
                               "title": "Rotate the key", "description": null, "labels": null,
                               "remove_source_branch": true }))
            .respond_json(201, merge_request()),
        Expectation::new("POST", &format!("{base}/merge_requests/{MR_IID}/notes"))
            .json_body(json!({ "body": "Deployed." }))
            .respond_json(201, note()),
        Expectation::new("GET", &format!("{base}/pipelines/{PIPELINE_ID}"))
            .query("")
            .respond_json(200, pipeline()),
        Expectation::new("GET", &format!("{base}/pipelines"))
            .query("per_page=100")
            .respond_json(200, json!([pipeline()])),
        Expectation::new("POST", &format!("{base}/pipeline"))
            .json_body(json!({ "ref": "main", "variables": [{ "key": "TEST", "value": "1" }] }))
            .respond_json(201, pipeline()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v4/"),
            "{id} renders the published prefix: {}",
            request.url().path()
        );
        assert!(
            request.url().path().contains(PROJECT_ENCODED),
            "{id} keeps the project path inside one segment: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `gitlab_auth_is_applied`: the token reaches the wire as the OAuth-compliant
/// header GitLab publishes, and never as a query value.
#[tokio::test]
async fn gitlab_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/v4/projects/{PROJECT_ENCODED}/issues/{ISSUE_IID}"),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .respond_json(200, issue())])
    .await;

    let request = render(
        &stub,
        "issue.get",
        json!({ "project_id": PROJECT, "issue_iid": ISSUE_IID }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("an ASCII header"),
        format!("Bearer {SECRET_SENTINEL}")
    );
    // GitLab publishes `?access_token=` as an alternative for OAuth tokens; this
    // connector declares the header form and only that one.
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        gitlab::connector().credential(),
        gitlab::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `gitlab_host_comes_only_from_deploy_time_configuration` (spec 023 §4 proof
/// 1): the instance's whole origin is one configuration value, and input, a
/// provider body, and a continuation each fail to move it.
#[tokio::test]
async fn gitlab_host_comes_only_from_deploy_time_configuration() {
    assert_eq!(
        gitlab::connector().origin().host_variable(),
        Some(gitlab::INSTANCE_ORIGIN)
    );

    let origin = gitlab::connector()
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            gitlab::INSTANCE_ORIGIN,
            "https://gitlab.example.test",
        )]))
        .expect("a configured instance origin resolves");
    assert_eq!(origin.as_url().as_str(), "https://gitlab.example.test/");

    // 1. Operation input. A project id that spells another authority stays
    //    inside its own path segment on the configured host.
    let request = operation("issue.get")
        .plan_request(
            &origin,
            &json!({ "project_id": "https://attacker.invalid/x", "issue_iid": ISSUE_IID }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("gitlab.example.test"));
    assert_eq!(
        request.url().path(),
        "/api/v4/projects/https%3A%2F%2Fattacker%2Einvalid%2Fx/issues/7"
    );
    // The same for a traversal attempt.
    let request = operation("issue.get")
        .plan_request(
            &origin,
            &json!({ "project_id": "../../admin", "issue_iid": ISSUE_IID }),
        )
        .expect("the declared request renders");
    assert_eq!(
        request.url().path(),
        "/api/v4/projects/%2E%2E%2F%2E%2E%2Fadmin/issues/7"
    );

    // 2. A provider response naming another host is data, not a destination.
    let output = operation("issue.get")
        .extract_output(&json!({ "id": 1, "iid": 1, "web_url": "https://attacker.invalid" }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("web_url"),
        Some(&json!("https://attacker.invalid"))
    );

    // 3. A `Link` continuation to another origin is refused rather than
    //    followed, on a deployment origin exactly as on a fixed one.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/v4/projects/{PROJECT_ENCODED}/issues"),
    )
    .respond_header(
        "link",
        "<https://attacker.invalid/api/v4/projects/1/issues?page=2>; rel=\"next\"",
    )
    .respond_json(200, json!([]))])
    .await;
    let failure = gitlab::pagination("issue.list")
        .expect("issue.list declares a plan")
        .collect(
            render(
                &stub,
                "issue.list",
                json!({ "project_id": PROJECT, "state": "all" }),
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

    // And the configured value is checked at deploy time.
    gitlab::validate_instance_origin("https://gitlab.example.test")
        .expect("a plain https instance origin is admitted");
    gitlab::validate_instance_origin("https://gitlab.example.test:8443")
        .expect("a port is part of an origin");
    for refused in [
        // The credential is a bearer token; an http instance would carry it in
        // clear.
        "http://gitlab.example.test",
        // GitLab supports a relative-URL install; an origin is a scheme, a host
        // and a port, so such a deployment is refused by name.
        "https://example.test/gitlab",
        "https://user:pass@gitlab.example.test",
        "https://gitlab.example.test/?x=1",
        "gitlab.example.test",
        "ftp://gitlab.example.test",
        "",
    ] {
        assert!(
            gitlab::validate_instance_origin(refused).is_err(),
            "`{refused}` is not an instance origin this connector may send a token to"
        );
    }
}

/// `gitlab_error_map`: every documented status reaches exactly one closed class,
/// and none of GitLab's prose crosses the boundary.
#[tokio::test]
async fn gitlab_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (412, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v4/projects/{PROJECT_ENCODED}/issues/{ISSUE_IID}"),
        )
        .respond_json(
            status,
            json!({ "message": format!("gitlab.example.test rejected {SECRET_SENTINEL}"),
                    "error": "insufficient_scope" }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "issue.get",
                json!({ "project_id": PROJECT, "issue_iid": ISSUE_IID }),
            ))
            .await
            .expect("the stub answers");

        let failure = gitlab::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "gitlab.example.test", "insufficient_scope"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `gitlab_rate_limit_is_classified`: "The user exceeded the application rate
/// limits" is retryable, and the hint is clamped.
#[tokio::test]
async fn gitlab_rate_limit_is_classified() {
    let path = format!("/api/v4/projects/{PROJECT_ENCODED}/issues/{ISSUE_IID}");
    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .respond_header("ratelimit-remaining", "0")
            .respond_json(429, json!({ "message": "Retry later" })),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "message": "Retry later" })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "issue.get",
                json!({ "project_id": PROJECT, "issue_iid": ISSUE_IID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(gitlab::error_map().classify_response(&response));
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

/// `gitlab_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the continuation
/// is GitLab's own `Link` header, followed as a destination on this origin, and
/// the walk makes exactly the number of requests the plan declares (ADR 058).
#[tokio::test]
async fn gitlab_cursor_is_opaque_and_bounded() {
    let plan = gitlab::pagination("issue.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    let path = format!("/api/v4/projects/{PROJECT_ENCODED}/issues");

    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .query("state=all&per_page=100")
            .respond_header("x-next-page", "2")
            .respond_header(
                "link",
                &format!(
                    "<{path}?state=all&per_page=100&page=2>; rel=\"next\", \
                     <{path}?state=all&per_page=100&page=1>; rel=\"first\""
                ),
            )
            .respond_json(200, json!([{ "id": 1 }])),
        Expectation::new("GET", &path)
            .query("state=all&per_page=100&page=2")
            .respond_json(200, json!([{ "id": 2 }])),
    ])
    .await;

    let issues = plan
        .collect(
            render(
                &stub,
                "issue.list",
                json!({ "project_id": PROJECT, "state": "all" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one link and stops where the header stops");
    assert_eq!(issues, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `gitlab_pagination_is_bounded`: the declared plan terminates and respects the
/// call, page, item and byte budgets, and only the three collections declare it.
#[tokio::test]
async fn gitlab_pagination_is_bounded() {
    let plan = gitlab::pagination("issue.list").expect("the list declares a plan");
    let path = format!("/api/v4/projects/{PROJECT_ENCODED}/issues");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &path)
                .respond_header("link", &format!("<{path}?page=9>; rel=\"next\""))
                .respond_json(200, json!([{ "id": 1 }, { "id": 2 }]))
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "issue.list",
                    json!({ "project_id": PROJECT, "state": "all" }),
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

    for id in ["issue.list", "merge_request.list", "pipeline.list"] {
        assert_eq!(
            gitlab::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            "",
            "{id} collects the bare array GitLab answers with"
        );
    }
    for id in [
        "project.get",
        "issue.get",
        "issue.create",
        "issue_note.create",
        "merge_request.get",
        "merge_request.create",
        "merge_request_note.create",
        "pipeline.get",
        "pipeline.trigger",
    ] {
        assert!(
            gitlab::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `gitlab_effects_are_classified`: every operation carries a class, and every
/// write is reachable only through an activity that says what an unknown outcome
/// means.
#[test]
fn gitlab_effects_are_classified() {
    let expected = [
        ("project.get", EffectClass::ReadOnly),
        ("issue.get", EffectClass::ReadOnly),
        ("issue.list", EffectClass::ReadOnly),
        ("issue.create", EffectClass::AtMostOnce),
        ("issue_note.create", EffectClass::AtMostOnce),
        ("merge_request.get", EffectClass::ReadOnly),
        ("merge_request.list", EffectClass::ReadOnly),
        ("merge_request.create", EffectClass::AtMostOnce),
        ("merge_request_note.create", EffectClass::AtMostOnce),
        ("pipeline.get", EffectClass::ReadOnly),
        ("pipeline.list", EffectClass::ReadOnly),
        ("pipeline.trigger", EffectClass::AtMostOnce),
    ];
    assert_eq!(gitlab::connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            gitlab::connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }

    let evidence = operation("pipeline.trigger")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(
        evidence.searched_documentation().contains("idempot"),
        "the search is recorded, not asserted"
    );
    assert!(
        evidence.repeat_produces().contains("second pipeline run"),
        "{}",
        evidence.repeat_produces()
    );
}

/// `gitlab_triggering_is_not_a_read` (spec 027 §3): starting a pipeline is a
/// `POST` with a body of two fields and it runs every job in the project's CI
/// configuration. It is not a read, and the class it carries says what a second
/// one costs.
#[test]
fn gitlab_triggering_is_not_a_read() {
    let trigger = operation("pipeline.trigger");
    let class = trigger.effect_class().expect("every operation has one");
    assert_ne!(class, EffectClass::ReadOnly);
    assert_eq!(class, EffectClass::AtMostOnce);
    assert!(
        class.requires_at_most_once_opt_in(),
        "a Process reaches it only by declaring what an unknown outcome means"
    );
    assert!(
        trigger
            .effect()
            .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
            .expect("the evidence is there")
            .repeat_produces()
            .contains("deployment"),
        "the consequence names what a second run actually does"
    );
    // And every other write in the module is likewise not a read.
    for id in [
        "issue.create",
        "issue_note.create",
        "merge_request.create",
        "merge_request_note.create",
    ] {
        assert_ne!(
            operation(id).effect_class(),
            Some(EffectClass::ReadOnly),
            "{id}"
        );
    }
    // A deployment cannot enable an operation the gate refuses, and the gate
    // refused none here — every class in this module is executable, which is
    // exactly why the classes have to be right.
    assert!(
        !gitlab::connector()
            .operations()
            .iter()
            .any(|operation| operation.effect_class() == Some(EffectClass::InventoryOnly))
    );
    assert_eq!(
        gitlab::connector().admit_operation("issue.delete"),
        Err(OperationRejection::Undeclared)
    );
}

/// `gitlab_output_contract`: the declared pointers read GitLab's own objects,
/// with its own typing — `id` and `iid` are integers and a missing one is a
/// contract violation rather than a null.
#[test]
fn gitlab_output_contract() {
    let get = operation("issue.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&issue()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": 91, "iid": ISSUE_IID, "project_id": 3, "title": "Disk is full",
            "state": "opened", "web_url": "https://gitlab.example.test/acme/platform/-/issues/7",
            "created_at": "2026-08-10T09:00:00.000Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"91","iid":7}"#)
            .expect_err("an id that is not an integer is not an issue")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":91}"#)
            .expect_err("the internal id is part of the identity")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":91,"iid":7}"#)
            .expect("only the identity is required")
            .get("title"),
        Some(&json!(null))
    );
    assert_eq!(
        gitlab::decode(
            get,
            404,
            &reqwest::header::HeaderMap::new(),
            br#"{"message":"404 Not found"}"#
        )
        .expect_err("a 404 is not a success")
        .class(),
        ConnectorErrorClass::Permanent
    );
}
