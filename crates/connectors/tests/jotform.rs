//! Jotform connector proofs (spec 028 §4, which adopts spec 023 §4), against
//! the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::jotform;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const FORM_ID: &str = "31504059977966";
const SUBMISSION_ID: &str = "237955080346633702";

/// A deployment's non-secret half. It names one of Jotform's three published
/// regions and nothing else, and it is deliberately distinguishable from the
/// secret sentinel so a leak test cannot pass by accident.
const REGION: &str = "eu";

fn declaration() -> &'static Connector {
    static DECLARATION: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        jotform::connector(jotform::Region::parse(REGION).expect("a published region"))
            .expect("the Jotform declaration is valid")
    });
    &DECLARATION
}

fn operation(id: &str) -> &'static Operation {
    declaration()
        .operation(id)
        .unwrap_or_else(|| panic!("the jotform declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_header(jotform::API_KEY_HEADER)
        .expect("the declared header name is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

/// Jotform's own published `/user/forms` sample, trimmed to the fields declared.
fn form() -> JsonValue {
    json!({
        "id": FORM_ID, "username": "johnsmith", "title": "Contact Us",
        "status": "ENABLED", "created_at": "2013-06-24 18:43:21",
        "updated_at": "2013-06-25 19:01:52", "new": "5", "count": "755",
    })
}

fn submission() -> JsonValue {
    json!({
        "id": SUBMISSION_ID, "form_id": FORM_ID, "ip": "123.123.123.123",
        "created_at": "2013-06-25 03:38:00", "status": "ACTIVE", "new": "1",
        "answers": { "4": { "text": "Your Message", "type": "control_textarea",
                            "answer": "¡Ay, caramba!" } },
    })
}

/// The envelope every Jotform response carries.
fn envelope(content: JsonValue) -> JsonValue {
    json!({ "responseCode": 200, "message": "success", "content": content, "limit-left": 4986 })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("form.list", json!({})),
        ("form.get", json!({ "form_id": FORM_ID })),
        ("question.list", json!({ "form_id": FORM_ID })),
        ("submission.list", json!({ "form_id": FORM_ID })),
        ("submission.get", json!({ "submission_id": SUBMISSION_ID })),
        (
            "submission.delete",
            json!({ "submission_id": SUBMISSION_ID }),
        ),
    ]
}

/// `jotform_request_shape`: exact method, path, query, headers and body for
/// every operation, all against Jotform's own published paths.
#[tokio::test]
async fn jotform_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/user/forms")
            .query("limit=1000")
            .header("apikey", SECRET_SENTINEL)
            .no_body()
            .respond_json(200, envelope(json!([form()]))),
        Expectation::new("GET", &format!("/form/{FORM_ID}"))
            .query("")
            .no_body()
            .respond_json(200, envelope(json!([form()]))),
        Expectation::new("GET", &format!("/form/{FORM_ID}/questions"))
            .query("")
            .respond_json(200, envelope(json!({ "1": { "qid": "1" } }))),
        Expectation::new("GET", &format!("/form/{FORM_ID}/submissions"))
            .query("limit=1000")
            .respond_json(200, envelope(json!([submission()]))),
        Expectation::new("GET", &format!("/submission/{SUBMISSION_ID}"))
            .query("")
            .respond_json(200, envelope(submission())),
        Expectation::new("DELETE", &format!("/submission/{SUBMISSION_ID}"))
            .query("")
            .no_body()
            .respond_json(
                200,
                envelope(json!(
                    "Submission #237955080346633702 deleted successfully."
                )),
            ),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            !request.url().as_str().contains("apiKey"),
            "{id} never spends the credential in the query Jotform also publishes"
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `jotform_auth_is_applied`: the API key reaches the wire as the `APIKEY`
/// header Jotform publishes, is marked sensitive, and never lands in the URL —
/// even though Jotform publishes a query form of the same credential.
#[tokio::test]
async fn jotform_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/form/{FORM_ID}"))
        .header("apikey", SECRET_SENTINEL)
        .respond_json(200, envelope(json!([form()])))])
    .await;

    let request = render(&stub, "form.get", json!({ "form_id": FORM_ID }));
    let applied = request
        .headers()
        .get("apikey")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(applied.to_str().expect("an ASCII header"), SECRET_SENTINEL);
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    // No operation publishes a slot the credential could reach: Jotform's query
    // form of the same key is not declared anywhere in this connector.
    for operation in declaration().operations() {
        let projection = operation.project();
        assert!(
            !projection
                .inputs()
                .iter()
                .any(|input| input.name().to_ascii_lowercase().contains("apikey")),
            "{} declares no credential-shaped input",
            operation.id()
        );
        assert!(
            !format!("{:?}", projection.query())
                .to_ascii_lowercase()
                .contains("apikey"),
            "{} sends no credential in its query",
            operation.id()
        );
    }

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        declaration().credential(),
        jotform::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `jotform_secret_and_non_secret_are_separated` (spec 028 §3): the API key is
/// the deployment's only secret, the region is ordinary non-secret deploy-time
/// configuration, and the two are told apart by where each is allowed to appear.
#[tokio::test]
async fn jotform_secret_and_non_secret_are_separated() {
    // The declared credential contract is one secret field and no other.
    assert_eq!(
        declaration()
            .credential()
            .fields()
            .iter()
            .map(|field| (field.name(), field.is_secret()))
            .collect::<Vec<_>>(),
        [("secret", true)]
    );

    // The non-secret half is public by construction: it *is* the compiled
    // origin, so it appears in every diagnostic that names where a request went.
    let origin = declaration()
        .resolve_origin(&Default::default())
        .expect("a region-compiled origin resolves without configuration");
    assert_eq!(origin.as_url().as_str(), "https://eu-api.jotform.com/");
    assert!(format!("{declaration:?}", declaration = declaration()).contains("eu-api.jotform.com"));

    // The secret half reaches no declaration, no request debug, no diagnostic,
    // no error, and no failure message.
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/form/{FORM_ID}"))
        .respond_json(
            401,
            json!({ "responseCode": 401,
                    "message": format!("Invalid API Key {SECRET_SENTINEL}"), "content": "" }),
        )])
    .await;
    let request = render(&stub, "form.get", json!({ "form_id": FORM_ID }));
    let redacted = request.redacted_url();
    let debug = format!("{request:?}");
    let response = stub.send(request).await.expect("the stub answers");
    let failure = jotform::error_map().classify_response(&response);
    let decoded = jotform::decode(
        operation("form.get"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect_err("a 401 is not a success");

    for surface in [
        redacted,
        debug,
        format!("{:?}", declaration().credential()),
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
            "the secret half must not appear: {surface}"
        );
    }
    assert_eq!(failure.class(), ConnectorErrorClass::Authentication);
    stub.assert_satisfied();
}

/// `jotform_region_comes_only_from_deploy_time_configuration`: the region is a
/// closed compiled table, and nothing outside deployment metadata names one.
#[test]
fn jotform_region_comes_only_from_deploy_time_configuration() {
    assert_eq!(
        jotform::Region::ALL
            .iter()
            .map(|region| (region.name(), region.api_origin()))
            .collect::<Vec<_>>(),
        [
            ("us", "https://api.jotform.com"),
            ("eu", "https://eu-api.jotform.com"),
            ("hipaa", "https://hipaa-api.jotform.com"),
        ]
    );
    for named in ["us", "eu", "hipaa"] {
        assert!(jotform::Region::parse(named).is_ok());
    }
    for hostile in [
        "",
        "US",
        "attacker.invalid",
        "https://attacker.invalid",
        "eu-api.jotform.com",
        "enterprise",
    ] {
        let refusal = jotform::Region::parse(hostile)
            .expect_err("a region Jotform does not publish does not resolve");
        assert!(refusal.message().contains("us, eu, hipaa"), "{hostile}");
    }

    // Every declared origin is fixed once the region is chosen, so no input, no
    // provider body, and no continuation can move it.
    for region in jotform::Region::ALL {
        let connector = jotform::connector(region).expect("a published region declares");
        let origin = connector
            .resolve_origin(&Default::default())
            .expect("a fixed origin ignores configuration entirely");
        let request = connector
            .operation("form.get")
            .expect("the declaration publishes form.get")
            .plan_request(
                &origin,
                &json!({ "form_id": "https://attacker.invalid/form/1" }),
            )
            .expect("a hostile input renders");
        assert_eq!(
            request.url().host_str(),
            origin.as_url().host_str(),
            "input cannot reach the host"
        );
        assert_eq!(request.url().scheme(), "https");
    }
}

/// `jotform_error_map`: every documented status reaches exactly one closed
/// class, and none of Jotform's prose crosses the boundary.
#[tokio::test]
async fn jotform_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &format!("/form/{FORM_ID}"))
            .respond_json(
                status,
                json!({ "responseCode": status,
                        "message": format!("eu-api.jotform.com rejected {SECRET_SENTINEL}"),
                        "content": "" }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "form.get", json!({ "form_id": FORM_ID })))
            .await
            .expect("the stub answers");

        let failure = jotform::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "eu-api.jotform.com", "Invalid API Key"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `jotform_rate_limit_is_classified`: the daily call limit — "1000 requests per
/// day for the starter plan", whose exceeded state is an "'API-Limit exceeded'
/// error message" — classifies as `http_429`, and a provider `Retry-After` is
/// clamped to the SDK ceiling.
#[tokio::test]
async fn jotform_rate_limit_is_classified() {
    let path = format!("/form/{FORM_ID}");
    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .respond_header("retry-after", "42")
            .respond_json(
                429,
                json!({ "responseCode": 429, "message": "API-Limit exceeded", "content": "" }),
            ),
        // The limit "resets at midnight, Eastern Standard Time", which can be a
        // whole day away; the SDK's ceiling is what a durable activity waits.
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(
                429,
                json!({ "responseCode": 429, "message": "API-Limit exceeded", "content": "" }),
            ),
        // And the same failure reported inside a `200` is still a rate limit.
        Expectation::new("GET", &path).respond_json(
            200,
            json!({ "responseCode": 429, "message": "API-Limit exceeded", "content": "" }),
        ),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "form.get", json!({ "form_id": FORM_ID })))
            .await
            .expect("the stub answers");
        failures.push(jotform::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(42)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );

    let response = stub
        .send(render(&stub, "form.get", json!({ "form_id": FORM_ID })))
        .await
        .expect("the stub answers");
    assert_eq!(
        jotform::decode(
            operation("form.get"),
            response.status.as_u16(),
            response.headers(),
            response.body()
        )
        .expect_err("a 200 carrying a rate limit is not a success")
        .class(),
        ConnectorErrorClass::Http429
    );
    stub.assert_satisfied();
}

/// `jotform_failure_inside_a_200_never_reads_as_success` (spec 023 §4 proof 4):
/// the body gate sits between the status check and the declared output pointers.
#[test]
fn jotform_failure_inside_a_200_never_reads_as_success() {
    let get = operation("form.get");
    let headers = reqwest::header::HeaderMap::new();

    // The declared success statuses alone would have admitted this body, and
    // its `content` would have decoded through the declared pointer.
    let reported_failure =
        br#"{"responseCode":401,"message":"Invalid API Key","content":[{"id":"1"}]}"#;
    assert!(get.decode_response(200, reported_failure).is_ok());
    let failure = jotform::decode(get, 200, &headers, reported_failure)
        .expect_err("a failure inside a 200 is a failure");
    assert_eq!(failure.class(), ConnectorErrorClass::Authentication);

    // A body that is not Jotform's envelope at all is an invariant, not a
    // success with missing fields.
    assert_eq!(
        jotform::decode(get, 200, &headers, br#"{"content":[]}"#)
            .expect_err("an envelope with no responseCode is outside the contract")
            .class(),
        ConnectorErrorClass::Invariant
    );
    assert_eq!(
        jotform::decode(get, 200, &headers, b"not json")
            .expect_err("a non-JSON body is outside the contract")
            .class(),
        ConnectorErrorClass::Invariant
    );

    // And the documented success still decodes.
    assert!(
        jotform::decode(
            get,
            200,
            &headers,
            &serde_json::to_vec(&envelope(json!([form()]))).expect("a fixture serializes")
        )
        .is_ok()
    );
}

/// `jotform_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the offset is
/// derived from the walk rather than from `resultSet`, and the walk makes
/// exactly the number of requests the plan declares (ADR 058).
#[tokio::test]
async fn jotform_cursor_is_opaque_and_bounded() {
    let plan = jotform::pagination("submission.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 4_000, 4 << 20, Duration::from_secs(5));
    let path = format!("/form/{FORM_ID}/submissions");
    let full: Vec<JsonValue> = (0..1000).map(|_| submission()).collect();

    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .query("limit=1000&offset=0")
            .respond_json(
                200,
                json!({ "responseCode": 200, "message": "success", "content": full,
                        // The provider's own offset is deliberately wrong here:
                        // no plan reads it, so a body cannot rewind the walk.
                        "resultSet": { "offset": 900, "limit": 1000, "count": 1000 } }),
            ),
        Expectation::new("GET", &path)
            .query("limit=1000&offset=1000")
            .respond_json(
                200,
                json!({ "responseCode": 200, "message": "success", "content": [submission()],
                        "resultSet": { "offset": 1000, "limit": 1000, "count": 1 } }),
            ),
    ])
    .await;

    let items = plan
        .collect(
            render(&stub, "submission.list", json!({ "form_id": FORM_ID })),
            &stub.origin(),
            &budget,
            |status, headers, body| {
                jotform::decode(operation("submission.list"), status, headers, body).map(|_| ())
            },
            |request| stub.send(request),
        )
        .await
        .expect("the walk advances the offset and stops on a short page");
    assert_eq!(items.len(), 1001);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `jotform_pagination_is_bounded`: the declared plan terminates under every
/// budget, only the two collections declare one, and no page size binds from
/// input.
#[tokio::test]
async fn jotform_pagination_is_bounded() {
    for id in [
        "form.get",
        "question.list",
        "submission.get",
        "submission.delete",
    ] {
        assert!(
            jotform::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    for id in ["form.list", "submission.list"] {
        assert_eq!(
            jotform::pagination(id)
                .expect("a collection declares a plan")
                .items_pointer(),
            "/content",
            "{id} writes its aggregate where its declared output reads it"
        );
    }

    let plan = jotform::pagination("form.list").expect("the list declares a plan");
    let full: Vec<JsonValue> = (0..1000).map(|_| form()).collect();
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 24, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 24, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 24, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/user/forms").respond_json(
                200,
                json!({ "responseCode": 200, "message": "success", "content": full }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "form.list", json!({})),
                &stub.origin(),
                &budget,
                |status, headers, body| {
                    jotform::decode(operation("form.list"), status, headers, body).map(|_| ())
                },
                |request| stub.send(request),
            )
            .await
            .expect_err("an unbounded provider is stopped by the budget");
        assert_eq!(failure.code(), "connector_pagination_budget", "{budget:?}");
    }

    // Every declared page size is a static, never an input a Process could grow
    // past the provider's published maximum of 1000.
    let queries: Vec<(String, String)> = declaration()
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
            .any(|(id, query)| id == "form.list" && query.contains("1000")),
        "the form list asks for the published maximum: {queries:?}"
    );
    assert!(
        !queries.iter().any(|(_, query)| query.contains("Input")),
        "no page size binds from input: {queries:?}"
    );
}

/// `jotform_effects_are_classified`: every operation carries a class, and the
/// one `DELETE` stays unreachable because Jotform publishes no repeat statement
/// for it and no consequence of a second send either.
#[test]
fn jotform_effects_are_classified() {
    let expected = [
        ("form.list", EffectClass::ReadOnly),
        ("form.get", EffectClass::ReadOnly),
        ("question.list", EffectClass::ReadOnly),
        ("submission.list", EffectClass::ReadOnly),
        ("submission.get", EffectClass::ReadOnly),
        ("submission.delete", EffectClass::InventoryOnly),
    ];
    assert_eq!(declaration().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            declaration().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        declaration().admit_operation("submission.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    let reason = operation("submission.delete")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("Delete a single submission."), "{reason}");
    assert!(reason.contains("NaturalMethod"), "{reason}");
    assert!(
        reason.contains("AtMostOnce"),
        "the reason says why the weaker class is wrong too: {reason}"
    );
}

/// `jotform_output_contract`: the declared pointers read Jotform's own envelope,
/// with its own typing.
#[test]
fn jotform_output_contract() {
    let get = operation("submission.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&envelope(submission())).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({ "content": submission(), "limit_left": 4986 })
    );
    // `limit-left` is optional: Jotform publishes it on every sample, and a
    // response without it is still a submission.
    assert_eq!(
        get.decode_response(200, br#"{"responseCode":200,"content":{"id":"1"}}"#)
            .expect("only the payload is required")
            .get("limit_left"),
        Some(&json!(null))
    );
    assert_eq!(
        get.decode_response(200, br#"{"responseCode":200,"limit-left":9991}"#)
            .expect_err("an envelope with no content is not a submission")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(
            200,
            br#"{"responseCode":200,"content":{},"limit-left":"9991"}"#
        )
        .expect_err("a call budget that is not a number is not the declared type")
        .class(),
        ConnectorErrorClass::Validation
    );

    // The collection publishes its own result set beside the payload.
    let list = operation("form.list");
    let page = json!({ "responseCode": 200, "message": "success", "content": [form()],
                       "resultSet": { "offset": 0, "limit": 20, "count": 20 },
                       "limit-left": 4986 });
    assert_eq!(
        list.decode_response(
            200,
            &serde_json::to_vec(&page).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied")
        .get("result_set"),
        Some(&json!({ "offset": 0, "limit": 20, "count": 20 }))
    );
}
