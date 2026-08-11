//! GitHub connector proofs (spec 013 §4), against the SDK's local provider stub
//! and against signatures this test generates itself. No test reaches GitHub,
//! and no test carries a real credential.

mod webhook_support;

use std::time::Duration;

use donat_connectors::providers::github;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan, WebhookRejection,
};
use reqwest::header::HeaderMap;
use serde_json::{Value as JsonValue, json};

use webhook_support as inbound;

const OWNER: &str = "donat";
const REPO: &str = "engine";

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    github::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the github declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn issue() -> JsonValue {
    json!({
        "id": 42,
        "number": 7,
        "title": "A defect",
        "state": "open",
        "body": "steps to reproduce",
        "html_url": "https://github.test/donat/engine/issues/7",
    })
}

/// `github_request_shape`: exact method, path, query, headers, and body for
/// every operation, including the percent-encoding of a hostile path value.
#[tokio::test]
async fn github_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues/7"))
            .query("")
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2026-03-10")
            .header("user-agent", "donat-connector-github")
            .no_body()
            .respond_json(200, issue()),
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues"))
            .query("state=open&per_page=100")
            .no_body()
            .respond_json(200, json!([issue()])),
        Expectation::new("POST", &format!("/repos/{OWNER}/{REPO}/issues"))
            .json_body(json!({ "title": "A defect", "body": "steps" }))
            .respond_json(201, issue()),
        Expectation::new("PATCH", &format!("/repos/{OWNER}/{REPO}/issues/7"))
            .json_body(json!({ "title": "A defect", "state": "closed" }))
            .respond_json(200, issue()),
        Expectation::new("POST", &format!("/repos/{OWNER}/{REPO}/issues/7/comments"))
            .json_body(json!({ "body": "a comment" }))
            .respond_json(
                201,
                json!({ "id": 9, "html_url": "https://github.test/c/9" }),
            ),
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/pulls/3"))
            .query("")
            .respond_json(
                200,
                json!({
                    "id": 3, "number": 3, "title": "A change", "state": "open",
                    "draft": false, "html_url": "https://github.test/p/3",
                }),
            ),
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/pulls"))
            .query("state=open&per_page=100")
            .respond_json(200, json!([])),
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}"))
            .query("")
            .respond_json(
                200,
                json!({
                    "id": 1, "full_name": "donat/engine",
                    "default_branch": "main", "private": false,
                }),
            ),
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/releases/11"))
            .query("")
            .respond_json(
                200,
                json!({
                    "id": 11, "tag_name": "v1.0.0", "name": "one",
                    "draft": false, "prerelease": false,
                }),
            ),
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/releases"))
            .query("per_page=100")
            .respond_json(200, json!([])),
        // A nested content path arrives as one percent-encoded segment, exactly
        // as `aws_s3` sends an object key.
        Expectation::new(
            "GET",
            &format!("/repos/{OWNER}/{REPO}/contents/docs%2Freadme%2Emd"),
        )
        .query("ref=main")
        .respond_json(
            200,
            json!({
                "type": "file", "path": "docs/readme.md", "sha": "abc",
                "size": 12, "content": "aGk=", "encoding": "base64",
            }),
        ),
        Expectation::new(
            "PUT",
            &format!("/repos/{OWNER}/{REPO}/contents/docs%2Freadme%2Emd"),
        )
        .json_body(json!({ "message": "update", "content": "aGk=", "sha": "abc" }))
        .respond_json(
            200,
            json!({
                "content": { "path": "docs/readme.md", "sha": "def" },
                "commit": { "sha": "c0ffee" },
            }),
        ),
        Expectation::new(
            "POST",
            &format!("/repos/{OWNER}/{REPO}/actions/workflows/ci%2Eyml/dispatches"),
        )
        .json_body(json!({ "ref": "main" }))
        .respond_json(
            200,
            json!({
                "workflow_run_id": 5, "run_url": "https://api.github.test/r/5",
                "html_url": "https://github.test/r/5",
            }),
        ),
    ])
    .await;

    let repo = json!({ "owner": OWNER, "repo": REPO });
    let with = |extra: JsonValue| {
        let mut merged = repo.clone();
        for (key, value) in extra.as_object().expect("a test input is an object") {
            merged[key] = value.clone();
        }
        merged
    };

    for (id, input) in [
        ("issue.get", with(json!({ "issue_number": 7 }))),
        ("issue.list", with(json!({ "state": "open" }))),
        (
            "issue.create",
            with(json!({ "title": "A defect", "body": "steps" })),
        ),
        (
            "issue.update",
            with(json!({ "issue_number": 7, "title": "A defect", "state": "closed" })),
        ),
        (
            "issue.comment_create",
            with(json!({ "issue_number": 7, "body": "a comment" })),
        ),
        ("pull_request.get", with(json!({ "pull_number": 3 }))),
        ("pull_request.list", with(json!({ "state": "open" }))),
        ("repository.get", repo.clone()),
        ("release.get", with(json!({ "release_id": 11 }))),
        ("release.list", repo.clone()),
        (
            "file.get",
            with(json!({ "path": "docs/readme.md", "ref": "main" })),
        ),
        (
            "file.put",
            with(json!({
                "path": "docs/readme.md", "message": "update",
                "content": "aGk=", "sha": "abc",
            })),
        ),
        (
            "workflow.dispatch",
            with(json!({ "workflow_id": "ci.yml", "ref": "main" })),
        ),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }
    stub.assert_satisfied();

    // A hostile repository name stays inside its own path segment.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        "/repos/donat/%2E%2E%2F%2E%2E%2Fadmin%3Fx%3D1/issues/7",
    )
    .respond_json(200, issue())])
    .await;
    let hostile = render(
        &stub,
        "issue.get",
        json!({ "owner": OWNER, "repo": "../../admin?x=1", "issue_number": 7 }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `github_auth_is_applied`: the fine-grained token reaches the wire as
/// `Authorization: Bearer <token>` and appears nowhere else.
#[tokio::test]
async fn github_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues/7"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .without_header("x-api-key")
                .respond_json(200, issue()),
        ])
        .await;

    let request = render(
        &stub,
        "issue.get",
        json!({ "owner": OWNER, "repo": REPO, "issue_number": 7 }),
    );
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(
        !request.url_carries_credential(),
        "a bearer plan leaves the URL printable"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = github::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        github::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `github_error_map`: the documented statuses each reach exactly one of the
/// eight closed classes, GitHub's own prose never crosses the boundary, and the
/// support handle it publishes does.
#[tokio::test]
async fn github_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        // "you will receive a `403` or `429` response" — the closed map cannot
        // read `x-ratelimit-remaining`, so `403` takes the class that does not
        // retry.
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (406, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (410, ConnectorErrorClass::Permanent),
        (422, ConnectorErrorClass::Validation),
        (451, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // Undocumented: the declared fallback answers.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/repos/{OWNER}/{REPO}/issues/7"),
        )
        .respond_header("x-github-request-id", "REQ:0123:4567")
        .respond_json(
            status,
            json!({
                "message": format!("Validation Failed for {SECRET_SENTINEL} on shard db-7"),
                "documentation_url": "https://docs.github.test/rest",
                "status": status.to_string(),
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "issue.get",
                json!({ "owner": OWNER, "repo": REPO, "issue_number": 7 }),
            ))
            .await
            .expect("the stub answers");

        let failure = github::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure.correlation_ids().get("github_request_id"),
            Some(&"REQ:0123:4567".to_owned()),
            "the one support handle GitHub publishes is retained"
        );
        assert!(
            operation("issue.get")
                .decode_response(status, response.body())
                .is_err(),
            "status {status} is not a declared success"
        );

        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7", "Validation Failed"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `github_pagination_is_bounded`: the `link` walk terminates when GitHub omits
/// the header, never leaves the compiled origin, and stops at each ceiling.
#[tokio::test]
async fn github_pagination_is_bounded() {
    let plan = github::pagination("issue.list").expect("issue.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues"))
            .query("state=open&per_page=100")
            .respond_header(
                "link",
                "<{base_url}/repos/donat/engine/issues?per_page=100&after=cursor>; rel=\"next\"",
            )
            .respond_json(200, json!([issue()])),
        // The second page carries no `link` header at all, which is GitHub's
        // documented end of the walk.
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues"))
            .query("per_page=100&after=cursor")
            .respond_json(200, json!([issue()])),
    ])
    .await;
    let items = plan
        .collect(
            render(
                &stub,
                "issue.list",
                json!({ "owner": OWNER, "repo": REPO, "state": "open" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| {
                assert_eq!(
                    request.url().host_str(),
                    stub.origin().as_url().host_str(),
                    "a continuation never leaves the compiled origin"
                );
                stub.send(request)
            },
        )
        .await
        .expect("the walk follows one continuation and stops");
    assert_eq!(items.len(), 2);
    stub.assert_satisfied();

    // A cross-origin continuation is refused rather than followed.
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues"))
                .respond_header(
                    "link",
                    "<https://attacker.invalid/repos/donat/engine/issues>; rel=\"next\"",
                )
                .respond_json(200, json!([])),
        ])
        .await;
    let failure = plan
        .collect(
            render(
                &stub,
                "issue.list",
                json!({ "owner": OWNER, "repo": REPO, "state": "open" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.code(), "connector_pagination_cross_origin");

    // An endless provider exhausts a ceiling instead of looping.
    let stub = ProviderStub::start((0..12).map(|_| {
        Expectation::new("GET", &format!("/repos/{OWNER}/{REPO}/issues"))
            .respond_header(
                "link",
                "<{base_url}/repos/donat/engine/issues?after=more>; rel=\"next\"",
            )
            .respond_json(200, json!([issue()]))
    }))
    .await;
    let failure = plan
        .collect(
            render(
                &stub,
                "issue.list",
                json!({ "owner": OWNER, "repo": REPO, "state": "open" }),
            ),
            &stub.origin(),
            &PaginationBudget::new(3, 3, 1_000, 1_024 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("an endless provider exhausts the budget");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert_eq!(failure.code(), "connector_pagination_budget");

    // Only the collections declare a plan.
    for id in [
        "issue.get",
        "repository.get",
        "file.put",
        "workflow.dispatch",
    ] {
        assert!(
            github::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `github_effects_are_classified`: every operation carries a class, the one
/// executable mutation carries GitHub's own replace statement, and everything
/// GitHub publishes no key for is inventory-only.
#[test]
fn github_effects_are_classified() {
    let connector = github::connector();
    let expected = [
        ("issue.get", EffectClass::ReadOnly),
        ("issue.list", EffectClass::ReadOnly),
        ("issue.create", EffectClass::AtMostOnce),
        ("issue.update", EffectClass::InventoryOnly),
        ("issue.comment_create", EffectClass::AtMostOnce),
        ("pull_request.get", EffectClass::ReadOnly),
        ("pull_request.list", EffectClass::ReadOnly),
        ("repository.get", EffectClass::ReadOnly),
        ("release.get", EffectClass::ReadOnly),
        ("release.list", EffectClass::ReadOnly),
        ("file.get", EffectClass::ReadOnly),
        ("file.put", EffectClass::ProviderIdempotentNaturalMethod),
        ("workflow.dispatch", EffectClass::AtMostOnce),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: GitHub publishes no idempotency key to bind"
        );
        if class == EffectClass::InventoryOnly {
            assert_eq!(
                connector.admit_operation(id),
                Err(OperationRejection::InventoryOnly)
            );
            assert!(
                operation
                    .effect()
                    .and_then(donat_connectors::sdk::Effect::inventory_reason)
                    .is_some_and(
                        |reason| reason.contains("idempotency key") || reason.contains("PATCH")
                    ),
                "{id}: an inventory-only class records why"
            );
        }
    }

    assert_eq!(
        connector.admit_operation("issue.delete"),
        Err(OperationRejection::Undeclared)
    );
}

/// `github_output_contract`: the declared pointers are complete and typed, and a
/// missing required pointer is a validation failure rather than a null.
#[test]
fn github_output_contract() {
    let get = operation("issue.get");
    assert_eq!(
        get.decode_response(
            200,
            br#"{"id":42,"number":7,"title":"A defect","state":"open","body":null,"html_url":"https://github.test/i/7","assignees":[]}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": 42, "number": 7, "title": "A defect", "state": "open",
            "body": null, "html_url": "https://github.test/i/7",
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );
    for body in [
        br#"{"number":7,"title":"t","state":"open","html_url":"u"}"#.as_slice(),
        br#"{"id":null,"number":7,"title":"t","state":"open","html_url":"u"}"#.as_slice(),
        br#"{"id":"42","number":7,"title":"t","state":"open","html_url":"u"}"#.as_slice(),
    ] {
        assert_eq!(
            get.decode_response(200, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    // The contents write reads GitHub's nested `content`/`commit` envelope, and
    // both of its documented success statuses decode the same way.
    let put = operation("file.put");
    for status in [200, 201] {
        assert_eq!(
            put.decode_response(
                status,
                br#"{"content":{"path":"a.md","sha":"s1"},"commit":{"sha":"c1"}}"#,
            )
            .expect("the declared contract is satisfied"),
            json!({ "path": "a.md", "sha": "s1", "commit_sha": "c1" })
        );
        assert!(put.is_success(status));
    }
    assert!(
        !put.is_success(409),
        "a documented conflict is not a success"
    );

    // The pinned API version's dispatch answers `200` with the run details, and
    // the `204` of the older version is deliberately not a success here.
    let dispatch = operation("workflow.dispatch");
    assert!(dispatch.is_success(200) && !dispatch.is_success(204));
}

// ---------------------------------------------------------------------------
// Inbound (spec 013 §4)
// ---------------------------------------------------------------------------

/// GitHub's published scheme, transcribed here: "the HMAC hex digest of the
/// request body … generated using the SHA-256 hash function and the `secret` as
/// the HMAC `key`", with "The hash signature always starts with `sha256=`".
fn sign(body: &[u8]) -> HeaderMap {
    inbound::headers(&[
        (
            "X-Hub-Signature-256",
            &format!("sha256={}", inbound::hex(&inbound::digest(body))),
        ),
        ("X-GitHub-Event", "issues"),
        ("X-GitHub-Delivery", "72d3162e-cc78-11e3-81ab-4c9367dc0958"),
    ])
}

#[test]
fn github_signature_precedes_parse() {
    inbound::signature_precedes_parse(
        github::connector(),
        sign,
        inbound::headers(&[(
            "X-Hub-Signature-256",
            &format!("sha256={}", inbound::hex(&[0u8; 32])),
        )]),
        WebhookRejection::InvalidSignature,
    );
    inbound::nothing_prints_the_secret(github::connector());

    // A digest without GitHub's declared prefix offers no candidate at all, so
    // the legacy `X-Hub-Signature` SHA-1 header cannot be mistaken for one.
    let body = br#"{"action":"opened"}"#;
    assert_eq!(
        inbound::verify(
            github::connector(),
            &inbound::headers(&[("X-Hub-Signature-256", &inbound::hex(&inbound::digest(body)))]),
            body,
        )
        .expect_err("an unprefixed digest is not a candidate"),
        WebhookRejection::InvalidSignature
    );
}

#[test]
fn github_signature_is_exact() {
    const BODY: &[u8] =
        br#"{"action":"opened","issue":{"id":1,"number":7},"repository":{"full_name":"donat/engine"}}"#;
    inbound::signature_is_exact(github::connector(), BODY, sign, |headers| {
        let value = headers
            .get("x-hub-signature-256")
            .and_then(|value| value.to_str().ok())
            .expect("the fixture is signed");
        let flipped = format!(
            "{}{}",
            &value[..value.len() - 1],
            if value.ends_with('0') { '1' } else { '0' }
        );
        inbound::headers(&[("X-Hub-Signature-256", &flipped)])
    });
    inbound::triggers_share_one_scheme(github::connector());
    inbound::events_match_triggers(github::connector(), github::events());

    // Every declared event names GitHub's own delivery GUID as its identifier.
    for event in github::events() {
        assert_eq!(
            event.event_identifier(),
            &donat_connectors::providers::inbound::EventIdentifier::Header("X-GitHub-Delivery")
        );
    }
    // `push` is the one event with no `action` field, and its declaration does
    // not claim one.
    let push = github::events()
        .iter()
        .find(|event| event.provider_event() == "push")
        .expect("the push trigger is declared");
    assert!(push.fields().iter().all(|field| field.name() != "action"));
}

#[test]
fn github_comparison_is_constant_time() {
    inbound::comparison_is_constant_time("github.rs", &inbound::module_source("github"));
}

#[test]
fn github_body_limit_precedes_verification() {
    inbound::body_limit_precedes_verification(github::connector(), sign);
    assert_eq!(
        inbound::trigger(github::connector()).raw_body_max_bytes(),
        github::RAW_BODY_MAX_BYTES
    );
}
