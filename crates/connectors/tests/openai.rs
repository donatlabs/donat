//! OpenAI connector proofs (spec 012 §3), against the SDK's local provider
//! stub.  No test reaches OpenAI, and no test carries a real credential.

use std::time::Duration;

use donat_connectors::providers::openai;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    openai::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the openai declaration publishes {id}"))
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

fn chat_input() -> JsonValue {
    json!({
        "model": "gpt-4o-mini",
        "messages": [{ "role": "user", "content": "Summarise order 1001" }],
    })
}

fn chat_response() -> JsonValue {
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "created": 1_786_000_000_i64,
        "model": "gpt-4o-mini",
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "Shipped." }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11 },
    })
}

/// `openai_request_shape`: exact method, path, query, headers, and body for
/// every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn openai_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", "/v1/chat/completions")
            .query("")
            .header("content-type", "application/json")
            .json_body(json!({
                "model": "gpt-4o-mini",
                "messages": [{ "role": "user", "content": "Summarise order 1001" }],
            }))
            .respond_json(200, chat_response()),
        Expectation::new("POST", "/v1/embeddings")
            .json_body(json!({ "model": "text-embedding-3-small", "input": "order 1001" }))
            .respond_json(
                200,
                json!({
                    "object": "list",
                    "model": "text-embedding-3-small",
                    "data": [{ "object": "embedding", "index": 0, "embedding": [0.1, 0.2] }],
                    "usage": { "prompt_tokens": 3, "total_tokens": 3 },
                }),
            ),
        Expectation::new("GET", "/v1/models")
            .query("")
            .no_body()
            .respond_json(200, json!({ "object": "list", "data": [] })),
        Expectation::new("GET", "/v1/files/file%2Dabc123")
            .query("")
            .no_body()
            .respond_json(
                200,
                json!({
                    "id": "file-abc123",
                    "object": "file",
                    "bytes": 175,
                    "created_at": 1_613_677_385_i64,
                    "filename": "salesOverview.pdf",
                    "purpose": "assistants",
                    "status": "processed",
                }),
            ),
        // A hostile file identifier stays one percent-encoded path segment.
        Expectation::new("GET", "/v1/files/%2E%2E%2Fmodels%3Fx%3D1%23y").respond_json(
            200,
            json!({
                "id": "file-abc123",
                "object": "file",
                "bytes": 1,
                "created_at": 1_613_677_385_i64,
                "filename": "f",
                "purpose": "assistants",
                "status": "processed",
            }),
        ),
    ])
    .await;

    for (id, input) in [
        ("chat.complete", chat_input()),
        (
            "embedding.create",
            json!({ "model": "text-embedding-3-small", "input": "order 1001" }),
        ),
        ("model.list", json!({})),
        ("file.get", json!({ "file_id": "file-abc123" })),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(&stub, "file.get", json!({ "file_id": "../models?x=1#y" }));
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();

    // The listing carries its `limit` from the declared pagination plan rather
    // than from input, so its shape is proven where the plan puts it.
    let stub = ProviderStub::start([Expectation::new("GET", "/v1/files")
        .query("limit=100")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .no_body()
        .respond_json(
            200,
            json!({ "object": "list", "data": [], "first_id": null, "last_id": null, "has_more": false }),
        )])
    .await;
    let plan = openai::pagination("file.list").expect("file.list declares a plan");
    assert!(
        plan.collect(
            render(&stub, "file.list", json!({})),
            &stub.origin(),
            &PaginationBudget::new(4, 4, 16, 16 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("an empty first page is a complete walk")
        .is_empty()
    );
    stub.assert_satisfied();
}

/// `openai_auth_is_applied`: the API key reaches the wire as
/// `Authorization: Bearer <key>` and appears nowhere else.
#[tokio::test]
async fn openai_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/v1/models")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .without_header("openai-organization")
        .without_header("x-api-key")
        .respond_json(200, json!({ "object": "list", "data": [] }))])
    .await;

    let request = render(&stub, "model.list", json!({}));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(
        !request.url().as_str().contains(SECRET_SENTINEL),
        "the API key is not a query value"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = openai::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        openai::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `openai_error_map`: the documented statuses and error `type`s each reach
/// exactly one of the eight closed classes, and OpenAI's prose never crosses
/// the boundary.
#[tokio::test]
async fn openai_error_map() {
    let documented = [
        (
            400,
            "invalid_request_error",
            ConnectorErrorClass::Validation,
        ),
        // A rejected key is an authentication failure even though OpenAI types
        // it `invalid_request_error`: the status rules are declared first.
        (
            401,
            "invalid_request_error",
            ConnectorErrorClass::Authentication,
        ),
        (
            403,
            "invalid_request_error",
            ConnectorErrorClass::Authentication,
        ),
        (404, "invalid_request_error", ConnectorErrorClass::Permanent),
        (429, "insufficient_quota", ConnectorErrorClass::Http429),
        (500, "server_error", ConnectorErrorClass::Http5xx),
        (503, "server_error", ConnectorErrorClass::Http5xx),
        // An undocumented status carrying a documented type is classified by
        // the type; an undocumented pair takes the declared fallback.
        (418, "server_error", ConnectorErrorClass::Http5xx),
        (418, "teapot_error", ConnectorErrorClass::Permanent),
    ];

    for (status, error_type, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", "/v1/models")
            .respond_header("x-request-id", "req_01H")
            .respond_json(
                status,
                json!({
                    "error": {
                        "type": error_type,
                        "code": "invalid_api_key",
                        "param": null,
                        "message": format!("shard db-7.internal rejected key {SECRET_SENTINEL}"),
                    }
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "model.list", json!({})))
            .await
            .expect("the stub answers");

        let failure = openai::error_map().classify_response(&response);
        assert_eq!(
            failure.class(),
            expected,
            "status {status} type {error_type}"
        );
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("request_id")
                .map(String::as_str),
            Some("req_01H"),
            "OpenAI's own support handle is the one provider value retained"
        );
        assert!(
            operation("model.list")
                .decode_response(status, response.body())
                .is_err(),
            "status {status} is not a declared success"
        );

        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [
            SECRET_SENTINEL,
            "db-7.internal",
            "invalid_api_key",
            error_type,
        ] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // A `Retry-After` a provider sends is clamped to the SDK's ceiling.
    let stub = ProviderStub::start([Expectation::new("GET", "/v1/models")
        .respond_header("retry-after", "172800")
        .respond_json(429, json!({ "error": { "type": "rate_limit_error" } }))])
    .await;
    let response = stub
        .send(render(&stub, "model.list", json!({})))
        .await
        .expect("the stub answers");
    assert_eq!(
        openai::error_map()
            .classify_response(&response)
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
    stub.assert_satisfied();
}

/// `openai_pagination_is_bounded`: the declared `after` cursor terminates,
/// respects the budget, and cannot leave the compiled origin.
#[tokio::test]
async fn openai_pagination_is_bounded() {
    let plan = openai::pagination("file.list").expect("file.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));

    fn page(ids: &[&str], last: Option<&str>, has_more: bool) -> JsonValue {
        json!({
            "object": "list",
            "data": ids.iter().map(|id| json!({ "id": id, "object": "file" })).collect::<Vec<_>>(),
            "first_id": ids.first().copied(),
            "last_id": last,
            "has_more": has_more,
        })
    }

    // OpenAI's documented protocol: `after` is the previous page's object ID.
    // The walk stops at the page that carries no cursor — which, because the
    // SDK's plan set cannot read `has_more`, is one call past the last full
    // page.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v1/files")
            .query("limit=100")
            .respond_json(200, page(&["file-1", "file-2"], Some("file-2"), true)),
        Expectation::new("GET", "/v1/files")
            .query("limit=100&after=file%2D2")
            .respond_json(200, page(&["file-3"], Some("file-3"), false)),
        Expectation::new("GET", "/v1/files")
            .query("limit=100&after=file%2D3")
            .respond_json(200, page(&[], None, false)),
    ])
    .await;
    let files = plan
        .collect(
            render(&stub, "file.list", json!({})),
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
        .expect("the declared plan walks to the end and stops");
    assert_eq!(files.len(), 3);
    stub.assert_satisfied();

    // A cursor that spells another origin stays a percent-encoded query value.
    let elsewhere = ProviderStub::start([
        Expectation::new("GET", "/v1/files").respond_json(200, page(&[], None, false))
    ])
    .await;
    let hostile = format!("{}/v1/files", elsewhere.base_url());
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v1/files")
            .respond_json(200, page(&["file-1"], Some(&hostile), true)),
        Expectation::new("GET", "/v1/files").respond_json(200, page(&[], None, false)),
    ])
    .await;
    plan.collect(
        render(&stub, "file.list", json!({})),
        &stub.origin(),
        &budget,
        undeclared_status_gate,
        |request| {
            assert_eq!(
                request.url().host_str(),
                stub.origin().as_url().host_str(),
                "a hostile cursor is a query value, not a destination"
            );
            stub.send(request)
        },
    )
    .await
    .expect("the hostile cursor never becomes a destination");
    assert_eq!(
        elsewhere.mismatches().len(),
        1,
        "the other origin was never contacted"
    );

    // A provider that keeps offering a cursor exhausts each ceiling instead of
    // looping, and returns no partial aggregate.
    for budget in [
        PaginationBudget::new(2, 8, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 4, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 64, 200, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v1/files")
                .respond_json(200, page(&["file-1", "file-2"], Some("file-2"), true))
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "file.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    for id in [
        "chat.complete",
        "embedding.create",
        "model.list",
        "file.get",
    ] {
        assert!(
            openai::pagination(id).is_none(),
            "{id} is not a paginated endpoint"
        );
    }
}

/// `openai_effects_are_classified`: every operation carries a class, and a
/// generative call is inventory-only whatever its method.
#[test]
fn openai_effects_are_classified() {
    let connector = openai::connector();
    let expected = [
        ("chat.complete", EffectClass::InventoryOnly),
        ("embedding.create", EffectClass::InventoryOnly),
        ("model.list", EffectClass::ReadOnly),
        ("file.list", EffectClass::ReadOnly),
        ("file.get", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: OpenAI's published OpenAPI document declares no key to bind"
        );
        if class == EffectClass::InventoryOnly {
            assert_eq!(
                connector.admit_operation(id),
                Err(OperationRejection::InventoryOnly),
                "{id} must not be enablable by a deployment"
            );
            assert!(
                operation
                    .effect()
                    .and_then(donat_connectors::sdk::Effect::inventory_reason)
                    .is_some_and(|reason| reason.contains("billed")),
                "{id} records that a repeat is a second charge"
            );
        } else {
            assert!(connector.admit_operation(id).is_ok(), "{id}");
        }
    }

    assert_eq!(
        connector.admit_operation("file.upload"),
        Err(OperationRejection::Undeclared),
        "multipart upload is out of this batch's scope"
    );
}

/// `openai_output_contract`: the declared pointers are complete and typed, and
/// a missing required pointer is a validation failure rather than a null.
#[test]
fn openai_output_contract() {
    let chat = operation("chat.complete");
    let body = serde_json::to_vec(&chat_response()).expect("a fixture serializes");
    assert_eq!(
        chat.decode_response(200, &body)
            .expect("the declared contract is satisfied"),
        json!({
            "id": "chatcmpl-1",
            "model": "gpt-4o-mini",
            "created": 1_786_000_000_i64,
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": "Shipped." }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 2, "total_tokens": 11 },
        })
    );
    for body in [
        br#"{"object":"chat.completion","created":1,"model":"m","choices":[]}"#.as_slice(),
        br#"{"id":null,"created":1,"model":"m","choices":[]}"#.as_slice(),
        br#"{"id":"c1","created":"1","model":"m","choices":[]}"#.as_slice(),
        br#"{"id":"c1","created":1,"model":"m"}"#.as_slice(),
    ] {
        assert_eq!(
            chat.decode_response(200, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    // A file listing publishes its page and its cursor, and the cursor is
    // optional exactly as an empty page needs.
    assert_eq!(
        operation("file.list")
            .decode_response(
                200,
                br#"{"object":"list","data":[],"first_id":null,"last_id":null,"has_more":false}"#,
            )
            .expect("an empty page satisfies the declared contract"),
        json!({ "object": "list", "data": [], "last_id": null, "has_more": false })
    );
    assert_eq!(
        operation("file.get")
            .decode_response(
                200,
                br#"{"id":"file-abc123","object":"file","bytes":175,"created_at":1613677385,"filename":"salesOverview.pdf","purpose":"assistants","status":"processed"}"#,
            )
            .expect("the declared file contract is satisfied"),
        json!({
            "id": "file-abc123",
            "object": "file",
            "bytes": 175,
            "created_at": 1_613_677_385_i64,
            "filename": "salesOverview.pdf",
            "purpose": "assistants",
            "status": "processed",
        })
    );

    // A streamed answer is not a declared success: this connector never asks
    // for one, and an event stream is not the declared JSON contract.
    assert_eq!(
        chat.decode_response(200, b"data: {\"id\":\"chatcmpl-1\"}\n\n")
            .expect_err("an event stream does not satisfy the declared contract")
            .class(),
        ConnectorErrorClass::Validation
    );
    for operation in openai::connector().operations() {
        assert!(
            operation.is_success(200) && !operation.is_success(201),
            "{}: OpenAI documents \"200 OK\" for each of these",
            operation.id()
        );
    }
}
