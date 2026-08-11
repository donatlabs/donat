//! SurveyMonkey connector proofs (spec 028 §4, which adopts spec 023 §4),
//! against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::surveymonkey;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const SURVEY_ID: &str = "309683094";
const RESPONSE_ID: &str = "5007154325";

fn operation(id: &str) -> &'static Operation {
    surveymonkey::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the surveymonkey declaration publishes {id}"))
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

/// SurveyMonkey's own published `GET /surveys` item.
fn survey() -> JsonValue {
    json!({ "id": SURVEY_ID, "title": "My Survey", "nickname": "",
            "owner": "1234",
            "href": "https://api.surveymonkey.com/v3/surveys/309683094" })
}

fn response() -> JsonValue {
    json!({ "id": RESPONSE_ID, "survey_id": SURVEY_ID, "collector_id": "405544189",
            "response_status": "completed", "date_created": "2026-08-01T09:00:00",
            "pages": [{ "id": "1", "questions": [] }] })
}

/// The list envelope every collection publishes.
fn page(data: JsonValue, next: Option<&str>) -> JsonValue {
    let mut links =
        json!({ "self": "https://api.surveymonkey.com/v3/surveys?page=1&per_page=100" });
    if let Some(next) = next {
        links["next"] = json!(next);
    }
    json!({ "data": data, "per_page": 100, "page": 1, "total": 1, "links": links })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("survey.list", json!({})),
        ("survey.get", json!({ "survey_id": SURVEY_ID })),
        ("survey.details", json!({ "survey_id": SURVEY_ID })),
        ("response.list", json!({ "survey_id": SURVEY_ID })),
        (
            "response.get",
            json!({ "survey_id": SURVEY_ID, "response_id": RESPONSE_ID }),
        ),
        (
            "response.delete",
            json!({ "survey_id": SURVEY_ID, "response_id": RESPONSE_ID }),
        ),
    ]
}

/// `surveymonkey_request_shape`: exact method, path, query, headers and body for
/// every operation, all under the `/v3` base URL the provider publishes.
#[tokio::test]
async fn surveymonkey_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v3/surveys")
            .query("per_page=100")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, page(json!([survey()]), None)),
        Expectation::new("GET", &format!("/v3/surveys/{SURVEY_ID}"))
            .query("")
            .no_body()
            .respond_json(200, survey()),
        Expectation::new("GET", &format!("/v3/surveys/{SURVEY_ID}/details"))
            .query("")
            .respond_json(
                200,
                json!({ "id": SURVEY_ID, "title": "My Survey", "pages": [] }),
            ),
        Expectation::new("GET", &format!("/v3/surveys/{SURVEY_ID}/responses/bulk"))
            .query("per_page=100")
            .respond_json(200, page(json!([response()]), None)),
        Expectation::new(
            "GET",
            &format!("/v3/surveys/{SURVEY_ID}/responses/{RESPONSE_ID}/details"),
        )
        .query("")
        .respond_json(200, response()),
        Expectation::new(
            "DELETE",
            &format!("/v3/surveys/{SURVEY_ID}/responses/{RESPONSE_ID}"),
        )
        .query("")
        .no_body()
        .respond_bytes(204, Vec::new()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/v3/"),
            "{id} renders the published base path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `surveymonkey_auth_is_applied`: the access token reaches the wire as the
/// bearer header every published code sample carries, and never anywhere else.
#[tokio::test]
async fn surveymonkey_auth_is_applied() {
    let stub = ProviderStub::start(
        [Expectation::new("GET", &format!("/v3/surveys/{SURVEY_ID}"))
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .respond_json(200, survey())],
    )
    .await;

    let request = render(&stub, "survey.get", json!({ "survey_id": SURVEY_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("an ASCII header"),
        format!("Bearer {SECRET_SENTINEL}")
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    for operation in surveymonkey::connector().operations() {
        let projection = operation.project();
        assert!(
            !projection.inputs().iter().any(|input| {
                let name = input.name().to_ascii_lowercase();
                name.contains("token") || name.contains("api_key")
            }),
            "{} declares no credential-shaped input",
            operation.id()
        );
    }

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        surveymonkey::connector().credential(),
        surveymonkey::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `surveymonkey_secret_and_non_secret_are_separated` (spec 028 §3): the access
/// token is this connector's whole secret contract, and its one non-secret
/// deploy-time fact — the origin — is a compiled constant rather than
/// configuration at all.
#[tokio::test]
async fn surveymonkey_secret_and_non_secret_are_separated() {
    assert_eq!(
        surveymonkey::connector()
            .credential()
            .fields()
            .iter()
            .map(|field| (field.name(), field.is_secret()))
            .collect::<Vec<_>>(),
        [("secret", true)]
    );
    // The origin is not deploy-time configuration at all: a fixed origin ignores
    // it entirely, so there is no non-secret key a deployment could get wrong.
    assert_eq!(surveymonkey::connector().origin().host_variable(), None);

    let stub = ProviderStub::start(
        [
            Expectation::new("GET", &format!("/v3/surveys/{SURVEY_ID}")).respond_json(
                401,
                json!({ "error": { "docs": "https://developer.surveymonkey.com/api/v3/#error-codes",
                               "message": format!("token {SECRET_SENTINEL} is invalid"),
                               "id": "1011", "name": "Authorization Error",
                               "http_status_code": 401 } }),
            ),
        ],
    )
    .await;
    let request = render(&stub, "survey.get", json!({ "survey_id": SURVEY_ID }));
    let redacted = request.redacted_url();
    let debug = format!("{request:?}");
    let response = stub.send(request).await.expect("the stub answers");
    let failure = surveymonkey::error_map().classify_response(&response);
    let decoded = surveymonkey::decode(
        operation("survey.get"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect_err("a 401 is not a success");

    for surface in [
        redacted,
        debug,
        format!("{:?}", surveymonkey::connector().credential()),
        format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        ),
        format!(
            "{} {} {}",
            decoded.code(),
            decoded.safe_message(),
            decoded.diagnostic()
        ),
    ] {
        assert!(
            !surface.contains(SECRET_SENTINEL),
            "the secret must not appear: {surface}"
        );
    }
    assert_eq!(failure.class(), ConnectorErrorClass::Authentication);
    stub.assert_satisfied();
}

/// `surveymonkey_error_map`: every documented status and every published error
/// id reaches exactly one closed class, and none of the provider's prose crosses
/// the boundary.
#[tokio::test]
async fn surveymonkey_error_map() {
    let documented = [
        (400, "1002", ConnectorErrorClass::Validation),
        (401, "1011", ConnectorErrorClass::Authentication),
        (402, "1005", ConnectorErrorClass::Permanent),
        // "1018 — The user does not have permission to access the host in this
        // region", the refusal a deployment in another datacentre earns.
        (403, "1018", ConnectorErrorClass::Authentication),
        (404, "1020", ConnectorErrorClass::Permanent),
        (405, "1061", ConnectorErrorClass::Permanent),
        (409, "1026", ConnectorErrorClass::Permanent),
        (410, "1053", ConnectorErrorClass::Permanent),
        (413, "1030", ConnectorErrorClass::Validation),
        (429, "1040", ConnectorErrorClass::Http429),
        (500, "1050", ConnectorErrorClass::Http5xx),
        (502, "1054", ConnectorErrorClass::Http5xx),
        (503, "1051", ConnectorErrorClass::Http5xx),
        (504, "1055", ConnectorErrorClass::Http5xx),
        (418, "9999", ConnectorErrorClass::Permanent),
    ];

    for (status, id, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/v3/surveys/{SURVEY_ID}"),
        )
        .respond_json(
            status,
            json!({ "error": { "message": format!("api.surveymonkey.com rejected {SECRET_SENTINEL}"),
                               "id": id, "name": "Oh bananas!",
                               "http_status_code": status } }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "survey.get",
                json!({ "survey_id": SURVEY_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = surveymonkey::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} id {id}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "api.surveymonkey.com", "Oh bananas!"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // Each published id also decides on its own, for a status the table does
    // not name — which is what makes an error object inside a `2xx` classify.
    for (id, expected) in [
        ("1001", ConnectorErrorClass::Validation),
        ("1013", ConnectorErrorClass::Authentication),
        ("1040", ConnectorErrorClass::Http429),
        ("1051", ConnectorErrorClass::Http5xx),
    ] {
        let failure = surveymonkey::error_map().classify(
            200,
            &reqwest::header::HeaderMap::new(),
            format!(r#"{{"error":{{"id":"{id}"}}}}"#).as_bytes(),
        );
        assert_eq!(failure.class(), expected, "id {id}");
    }
}

/// `surveymonkey_rate_limit_is_classified`: "1040 — 429 Rate Limit Reached — Too
/// many requests were made, try again later", and a provider `Retry-After` is
/// clamped to the SDK ceiling.
#[tokio::test]
async fn surveymonkey_rate_limit_is_classified() {
    let path = format!("/v3/surveys/{SURVEY_ID}");
    let limited = json!({ "error": { "id": "1040", "name": "Rate Limit Reached",
                                     "message": "Too many requests were made, try again later.",
                                     "http_status_code": 429 } });
    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            // SurveyMonkey's own published rate-limit signal is a reset
            // *countdown* rather than `Retry-After`, so a delay only ever
            // arrives if it sends the standard header too.
            .respond_header("x-ratelimit-app-global-minute-limit", "120")
            .respond_header("x-ratelimit-app-global-minute-remaining", "0")
            .respond_header("x-ratelimit-app-global-minute-reset", "37")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "37")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(
                &stub,
                "survey.get",
                json!({ "survey_id": SURVEY_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(surveymonkey::error_map().classify_response(&response));
    }
    assert!(
        failures
            .iter()
            .all(|failure| failure.class() == ConnectorErrorClass::Http429)
    );
    assert_eq!(
        failures[0].retry_after(),
        None,
        "the reset countdown is not a Retry-After and is not invented into one"
    );
    assert_eq!(failures[1].retry_after(), Some(Duration::from_secs(37)));
    assert_eq!(
        failures[2].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `surveymonkey_failure_inside_a_200_never_reads_as_success` (spec 023 §4 proof
/// 4): both published failure envelopes are gated between the status check and
/// the declared output pointers.
#[test]
fn surveymonkey_failure_inside_a_200_never_reads_as_success() {
    let get = operation("survey.get");
    let headers = reqwest::header::HeaderMap::new();

    // 1. The error object, carrying its own status inside the body.
    let error_object =
        br#"{"id":"309683094","title":"My Survey","error":{"id":"1050","http_status_code":500}}"#;
    assert!(
        get.decode_response(200, error_object).is_ok(),
        "the declared pointers alone would have read this as a survey"
    );
    assert_eq!(
        surveymonkey::decode(get, 200, &headers, error_object)
            .expect_err("an error object is a failure whatever the status was")
            .class(),
        ConnectorErrorClass::Http5xx
    );

    // 2. The revocation envelope, for which SurveyMonkey publishes no HTTP
    //    status at all: "a key `status` with a value of `1` and a key `errmsg`".
    let revoked = br#"{"id":"309683094","title":"My Survey","status":1,"errmsg":"Client revoked access grant"}"#;
    assert!(get.decode_response(200, revoked).is_ok());
    let failure = surveymonkey::decode(get, 200, &headers, revoked)
        .expect_err("a revoked grant is never an activity success");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);

    // A success is still a success, and `status: 0` beside an `errmsg` is not
    // the documented envelope.
    assert!(
        surveymonkey::decode(
            get,
            200,
            &headers,
            &serde_json::to_vec(&survey()).expect("a fixture serializes")
        )
        .is_ok()
    );
    assert!(
        surveymonkey::decode(
            get,
            200,
            &headers,
            br#"{"id":"1","title":"t","status":0,"errmsg":""}"#
        )
        .is_ok()
    );
}

/// `surveymonkey_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the walk
/// follows the provider's own `links.next`, makes exactly the number of requests
/// the plan declares (ADR 058), and refuses a continuation that leaves the
/// compiled origin.
#[tokio::test]
async fn surveymonkey_cursor_is_opaque_and_bounded() {
    let plan = surveymonkey::pagination("response.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 1 << 20, Duration::from_secs(5));
    let path = format!("/v3/surveys/{SURVEY_ID}/responses/bulk");

    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .query("per_page=100")
            .respond_json(
                200,
                page(
                    json!([response()]),
                    Some("/v3/surveys/309683094/responses/bulk?page=2&per_page=100"),
                ),
            ),
        Expectation::new("GET", &path)
            .query("page=2&per_page=100")
            .respond_json(200, page(json!([response(), response()]), None)),
    ])
    .await;

    let items = plan
        .collect(
            render(&stub, "response.list", json!({ "survey_id": SURVEY_ID })),
            &stub.origin(),
            &budget,
            |status, headers, body| {
                surveymonkey::decode(operation("response.list"), status, headers, body).map(|_| ())
            },
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows links.next and stops when it is absent");
    assert_eq!(items.len(), 3);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();

    // A continuation that names another authority is refused rather than
    // followed, which is what makes `NextUriInBody` safe to declare at all.
    let hostile = ProviderStub::start([Expectation::new("GET", &path)
        .query("per_page=100")
        .respond_json(
            200,
            page(
                json!([response()]),
                Some("https://attacker.invalid/v3/surveys/1/responses/bulk?page=2"),
            ),
        )])
    .await;
    let failure = plan
        .collect(
            render(&hostile, "response.list", json!({ "survey_id": SURVEY_ID })),
            &hostile.origin(),
            &budget,
            |status, headers, body| {
                surveymonkey::decode(operation("response.list"), status, headers, body).map(|_| ())
            },
            |request| hostile.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    assert_eq!(hostile.received(), 1);
    hostile.assert_satisfied();
}

/// `surveymonkey_pagination_is_bounded`: the declared plan terminates under
/// every budget, only the two collections declare one, and no page size binds
/// from input.
#[tokio::test]
async fn surveymonkey_pagination_is_bounded() {
    for id in [
        "survey.get",
        "survey.details",
        "response.get",
        "response.delete",
    ] {
        assert!(
            surveymonkey::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    for id in ["survey.list", "response.list"] {
        assert_eq!(
            surveymonkey::pagination(id)
                .expect("a collection declares a plan")
                .items_pointer(),
            "/data",
            "{id} writes its aggregate where its declared output reads it"
        );
    }

    let plan = surveymonkey::pagination("survey.list").expect("the list declares a plan");
    let full: Vec<JsonValue> = (0..100).map(|_| survey()).collect();
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 22, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 22, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 22, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v3/surveys").respond_json(
                200,
                page(
                    JsonValue::Array(full.clone()),
                    Some("/v3/surveys?page=2&per_page=100"),
                ),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "survey.list", json!({})),
                &stub.origin(),
                &budget,
                |status, headers, body| {
                    surveymonkey::decode(operation("survey.list"), status, headers, body)
                        .map(|_| ())
                },
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget", "{budget:?}");
    }

    let queries: Vec<(String, String)> = surveymonkey::connector()
        .operations()
        .iter()
        .flat_map(|operation| {
            operation
                .project()
                .query()
                .iter()
                .map(|query| (operation.id().to_owned(), format!("{query:?}")))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        queries
            .iter()
            .any(|(id, query)| id == "response.list" && query.contains("100")),
        "the response list asks for the published maximum: {queries:?}"
    );
    assert!(
        !queries.iter().any(|(_, query)| query.contains("Input")),
        "no page size binds from input: {queries:?}"
    );
}

/// `surveymonkey_effects_are_classified`: every operation carries a class, and
/// the one `DELETE` stays unreachable because the provider publishes no repeat
/// statement for it and no consequence of a second send either.
#[test]
fn surveymonkey_effects_are_classified() {
    let expected = [
        ("survey.list", EffectClass::ReadOnly),
        ("survey.get", EffectClass::ReadOnly),
        ("survey.details", EffectClass::ReadOnly),
        ("response.list", EffectClass::ReadOnly),
        ("response.get", EffectClass::ReadOnly),
        ("response.delete", EffectClass::InventoryOnly),
    ];
    assert_eq!(surveymonkey::connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            surveymonkey::connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        surveymonkey::connector().admit_operation("response.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    let reason = operation("response.delete")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("Deletes a response."), "{reason}");
    assert!(reason.contains("NaturalMethod"), "{reason}");
    assert!(reason.contains("AtMostOnce"), "{reason}");
}

/// `surveymonkey_output_contract`: the declared pointers read SurveyMonkey's own
/// objects, with its own typing.
#[test]
fn surveymonkey_output_contract() {
    let get = operation("survey.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&survey()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({ "id": SURVEY_ID, "title": "My Survey", "nickname": "",
                "language": null, "date_created": null, "date_modified": null,
                "href": "https://api.surveymonkey.com/v3/surveys/309683094" })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":309683094,"title":"t"}"#)
            .expect_err("an id SurveyMonkey publishes as a String is not an Integer")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"1"}"#)
            .expect_err("a survey with no title is not the declared contract")
            .class(),
        ConnectorErrorClass::Validation
    );

    // The delete publishes no response schema, so an empty success is a success.
    let delete = operation("response.delete");
    assert!(delete.decode_response(204, b"").is_ok());
    assert!(delete.decode_response(200, b"").is_ok());

    let list = operation("response.list");
    assert_eq!(
        list.decode_response(
            200,
            &serde_json::to_vec(&page(json!([response()]), Some("/v3/x")))
                .expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({ "data": [response()], "page": 1, "per_page": 100, "total": 1 }),
        "the continuation is spent by the plan and is not published as output"
    );
}
