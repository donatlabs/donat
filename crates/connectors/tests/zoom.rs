//! Zoom connector proofs (spec 025 §4), against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::zoom;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation,
    PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const MEETING_ID: i64 = 97_654_321_012_i64;
const USER_ID: &str = "me";

fn operation(id: &str) -> &'static Operation {
    zoom::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the zoom declaration publishes {id}"))
}

fn applied_token() -> AccessToken {
    AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_token()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn meeting() -> JsonValue {
    json!({
        "id": MEETING_ID,
        "uuid": "aDYlohsHRtCd4ii1uC2+hA==",
        "topic": "Quarterly review",
        "type": 2,
        "start_time": "2026-08-15T10:00:00Z",
        "duration": 60,
        "timezone": "Europe/Berlin",
        "join_url": "https://example.zoom.us/j/97654321012",
        "host_id": "30R7kT7bTIKSNUFEuH_Qlg",
    })
}

/// Every operation, with an input that satisfies it and the documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("meeting.get", json!({ "meeting_id": MEETING_ID })),
        (
            "meeting.list",
            json!({ "user_id": USER_ID, "type": "scheduled" }),
        ),
        (
            "meeting.create",
            json!({
                "user_id": USER_ID, "topic": "Quarterly review", "type": 2,
                "start_time": "2026-08-15T10:00:00Z", "duration": 60,
                "timezone": "Europe/Berlin", "agenda": null,
            }),
        ),
        ("meeting.delete", json!({ "meeting_id": MEETING_ID })),
    ]
}

/// `zoom_request_shape`: exact method, path, query, headers, and body for every
/// operation, each under the published `/v2` prefix.
#[tokio::test]
async fn zoom_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v2/meetings/{MEETING_ID}"))
            .query("")
            .no_body()
            .respond_json(200, meeting()),
        Expectation::new("GET", &format!("/v2/users/{USER_ID}/meetings"))
            .query("type=scheduled")
            .respond_json(
                200,
                json!({ "meetings": [meeting()], "total_records": 1, "next_page_token": "" }),
            ),
        Expectation::new("POST", &format!("/v2/users/{USER_ID}/meetings"))
            .json_body(json!({
                "topic": "Quarterly review", "type": 2,
                "start_time": "2026-08-15T10:00:00Z", "duration": 60,
                "timezone": "Europe/Berlin", "agenda": null,
            }))
            .respond_json(201, meeting()),
        Expectation::new("DELETE", &format!("/v2/meetings/{MEETING_ID}"))
            .query("")
            .respond_json(204, json!(null)),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/v2/"),
            "{id} renders a published Zoom path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `zoom_auth_is_applied`: the stored OAuth2 token reaches the wire as
/// `Authorization: Bearer …`, and the server-to-server grant this connector does
/// not serve is not reachable from the declaration either.
#[tokio::test]
async fn zoom_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/v2/meetings/{MEETING_ID}"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, meeting()),
        ])
        .await;

    let request = render(&stub, "meeting.get", json!({ "meeting_id": MEETING_ID }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    // The declared plan is the stored authorization-code one, and it issues no
    // token of its own: Zoom's `account_credentials` grant is not RFC 6749 §4.4
    // and this connector does not send one.
    let plan = zoom::connector()
        .credential()
        .plan()
        .expect("Zoom declares a plan");
    assert!(!plan.issues_its_own_token());
    assert_eq!(
        plan.oauth2_authorization_scheme(),
        Some(donat_connectors::sdk::BEARER_SCHEME)
    );
    assert!(plan.required_fields().is_empty(), "no secret is configured");

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", zoom::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `zoom_error_map`: every documented status reaches exactly one closed class,
/// and none of Zoom's prose or numeric error codes crosses the boundary.
#[tokio::test]
async fn zoom_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Permanent),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", &format!("/v2/meetings/{MEETING_ID}"))
                    .respond_header("x-zm-trackingid", "v=2.0;clid=aw1;rid=WEB_42")
                    .respond_json(
                        status,
                        json!({
                            "code": 3001,
                            "message": format!("Meeting does not exist: acme {SECRET_SENTINEL}"),
                        }),
                    ),
            ])
            .await;
        let response = stub
            .send(render(
                &stub,
                "meeting.get",
                json!({ "meeting_id": MEETING_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = zoom::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("request_id")
                .map(String::as_str),
            Some("v=2.0;clid=aw1;rid=WEB_42")
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme", "Meeting does not exist"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `zoom_rate_limit_is_classified`: the documented `429` is retryable, and Zoom
/// publishes no `Retry-After` for the per-second limit — so the hint is absent
/// unless the response carried one, and a hostile one is clamped.
#[tokio::test]
async fn zoom_rate_limit_is_classified() {
    let limited = json!({
        "code": 429,
        "message": "You have reached the maximum per-second rate limit for this API. Try again \
                    later.",
    });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v2/meetings/{MEETING_ID}"))
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/v2/meetings/{MEETING_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "meeting.get",
                json!({ "meeting_id": MEETING_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(zoom::error_map().classify_response(&response));
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

/// `zoom_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the page token is
/// echoed back verbatim, is never parsed or constructed here, and the walk makes
/// exactly the number of requests the plan declares — ending on the empty string
/// Zoom publishes for a last page.
#[tokio::test]
async fn zoom_cursor_is_opaque_and_bounded() {
    let plan = zoom::pagination("meeting.list").expect("the listing declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const TOKEN: &str = "Tva2CuIdTgsv8wAnhyAdU3m06Y2HuLQtlh3&page_size=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v2/users/{USER_ID}/meetings"))
            .query("type=scheduled&page_size=100")
            .respond_json(
                200,
                json!({ "meetings": [{ "id": 1 }], "next_page_token": TOKEN }),
            ),
        Expectation::new("GET", &format!("/v2/users/{USER_ID}/meetings"))
            .query(&format!(
                "type=scheduled&page_size=100&next_page_token={}",
                TOKEN
                    .chars()
                    .map(|character| if character.is_ascii_alphanumeric() {
                        character.to_string()
                    } else {
                        format!("%{:02X}", character as u32)
                    })
                    .collect::<String>()
            ))
            // Zoom answers the last page with an empty token, which the SDK's
            // cursor plan reads as absent.
            .respond_json(
                200,
                json!({ "meetings": [{ "id": 2 }], "next_page_token": "" }),
            ),
    ])
    .await;

    let meetings = plan
        .collect(
            render(
                &stub,
                "meeting.list",
                json!({ "user_id": USER_ID, "type": "scheduled" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one token and stops on the empty one");
    assert_eq!(meetings, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `zoom_pagination_is_bounded`: the declared plan terminates and respects the
/// call, page, item, and byte budgets, and the operations that declare none send
/// one request.
#[tokio::test]
async fn zoom_pagination_is_bounded() {
    let plan = zoom::pagination("meeting.list").expect("the listing declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &format!("/v2/users/{USER_ID}/meetings")).respond_json(
                200,
                json!({ "meetings": [{ "id": 1 }, { "id": 2 }], "next_page_token": "more" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "meeting.list",
                    json!({ "user_id": USER_ID, "type": "scheduled" }),
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

    for id in ["meeting.get", "meeting.create", "meeting.delete"] {
        assert!(
            zoom::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `zoom_effects_are_classified`: every operation carries a class, the create is
/// at-most-once with both halves of ADR 063's evidence, and the delete carries
/// Zoom's own repeat statement.
#[test]
fn zoom_effects_are_classified() {
    let connector = zoom::connector();
    let expected = [
        ("meeting.get", EffectClass::ReadOnly),
        ("meeting.list", EffectClass::ReadOnly),
        ("meeting.create", EffectClass::AtMostOnce),
        (
            "meeting.delete",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
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

    let evidence = operation("meeting.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("OpenAPI"));
    assert!(
        evidence
            .repeat_produces()
            .contains("a second scheduled meeting")
    );
    assert!(evidence.repeat_produces().contains("100 create/update"));

    let citation = format!("{:?}", operation("meeting.delete").effect());
    assert!(citation.contains("Meeting does not exist"), "{citation}");
    // The occurrence filter is not declared, so the identity a delete names is
    // always the whole meeting.
    let projection = operation("meeting.delete").project();
    assert!(
        projection
            .inputs()
            .iter()
            .all(|input| input.name() != "occurrence_id"),
        "an operation whose identity a caller could narrow is not one fixed identity"
    );
}

/// `zoom_output_contract`: the declared pointers read Zoom's own meeting object,
/// including the long meeting id it publishes as an integer.
#[test]
fn zoom_output_contract() {
    let get = operation("meeting.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&meeting()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": MEETING_ID,
            "uuid": "aDYlohsHRtCd4ii1uC2+hA==",
            "topic": "Quarterly review",
            "type": 2,
            "start_time": "2026-08-15T10:00:00Z",
            "duration": 60,
            "timezone": "Europe/Berlin",
            "join_url": "https://example.zoom.us/j/97654321012",
            "host_id": "30R7kT7bTIKSNUFEuH_Qlg",
        })
    );
    // "store it as a long format integer and **not** an integer. Meeting IDs
    // can exceed 10 digits" — the declaration reads it as a 64-bit integer, and
    // a string there is a contract violation.
    assert_eq!(
        get.decode_response(200, br#"{"id":"97654321012"}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"topic":"Quarterly review"}"#)
            .expect_err("an answer with no id is not a meeting")
            .class(),
        ConnectorErrorClass::Validation
    );

    // A delete answers `204` with no body, and the declaration says so.
    let delete = operation("meeting.delete");
    assert!(delete.is_no_content_success(204));
    assert_eq!(
        delete
            .decode_response(204, b"")
            .expect("an empty success is the provider's own answer"),
        json!({})
    );

    let list = operation("meeting.list");
    assert_eq!(
        list.decode_response(
            200,
            br#"{"meetings":[{"id":1}],"total_records":1,"next_page_token":""}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "meetings": [{ "id": 1 }],
            "total_records": 1,
            "next_page_token": "",
        })
    );
}
