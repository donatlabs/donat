//! Postmark connector proofs (spec 012 §3), against the SDK's local provider
//! stub.  No test reaches Postmark, and no test carries a real credential.

use std::time::Duration;

use donat_connectors::providers::postmark;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const SERVER_TOKEN_HEADER: &str = "x-postmark-server-token";
const MESSAGE_ID: &str = "0a129aee-e1cd-480d-b08d-4f48548ff48d";
/// The same identifier as the SDK renders it: every non-alphanumeric byte of a
/// path value is percent-encoded, so a value can never leave its own segment.
const MESSAGE_ID_PATH: &str = "0a129aee%2De1cd%2D480d%2Db08d%2D4f48548ff48d";

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    postmark::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the postmark declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::api_key_header(postmark::SERVER_TOKEN_HEADER)
        .expect("a static header name is valid")
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn email() -> JsonValue {
    json!({
        "from": "sales@example.test",
        "to": "buyer@example.test",
        "subject": "Your order",
        "text_body": "Thank you",
    })
}

/// `postmark_request_shape`: exact method, path, query, headers, and body for
/// every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn postmark_request_shape() {
    let sent = json!({
        "To": "buyer@example.test",
        "SubmittedAt": "2026-08-10T00:00:00.0000000-05:00",
        "MessageID": MESSAGE_ID,
        "ErrorCode": 0,
        "Message": "OK",
    });
    let stub = ProviderStub::start([
        Expectation::new("POST", "/email")
            .query("")
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .json_body(json!({
                "From": "sales@example.test",
                "To": "buyer@example.test",
                "Subject": "Your order",
                "TextBody": "Thank you",
            }))
            .respond_json(200, sent.clone()),
        Expectation::new("POST", "/email/withTemplate")
            .json_body(json!({
                "From": "sales@example.test",
                "To": "buyer@example.test",
                "TemplateAlias": "order-receipt",
                "TemplateModel": { "order": "1001" },
            }))
            .respond_json(200, sent),
        Expectation::new("GET", &format!("/messages/outbound/{MESSAGE_ID_PATH}/details"))
            .query("")
            .header("accept", "application/json")
            .no_body()
            .respond_json(
                200,
                json!({ "MessageID": MESSAGE_ID, "Status": "Sent", "To": [], "MessageEvents": [] }),
            ),
        Expectation::new("GET", "/bounces/692560173")
            .query("")
            .respond_json(
                200,
                json!({ "ID": 692_560_173_i64, "Type": "HardBounce", "Email": "buyer@example.test" }),
            ),
        Expectation::new("GET", "/templates/order%2Dreceipt")
            .query("")
            .respond_json(
                200,
                json!({ "TemplateId": 1234, "Alias": "order-receipt", "Name": "Receipt" }),
            ),
        // A hostile template alias stays one percent-encoded path segment.
        Expectation::new("GET", "/templates/%2E%2E%2Femail%3Fx%3D1%23y").respond_json(
            200,
            json!({ "TemplateId": 1234, "Alias": "x", "Name": "Receipt" }),
        ),
    ])
    .await;

    for (id, input) in [
        ("email.send", email()),
        (
            "email.send_template",
            json!({
                "from": "sales@example.test",
                "to": "buyer@example.test",
                "template_alias": "order-receipt",
                "template_model": { "order": "1001" },
            }),
        ),
        ("message.get", json!({ "message_id": MESSAGE_ID })),
        ("bounce.get", json!({ "bounce_id": 692_560_173_i64 })),
        ("template.get", json!({ "template_id": "order-receipt" })),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "template.get",
        json!({ "template_id": "../email?x=1#y" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");

    stub.assert_satisfied();

    // The three list endpoints carry Postmark's required `count`/`offset` pair
    // from their declared pagination plan rather than from input, so their
    // shape is proven where the plan puts it.
    for (id, path, items, query) in [
        (
            "message.list_outbound",
            "/messages/outbound",
            "Messages",
            "offset=0&count=100",
        ),
        ("bounce.list", "/bounces", "Bounces", "offset=0&count=100"),
        (
            "template.list",
            "/templates",
            "Templates",
            "Offset=0&Count=100",
        ),
    ] {
        let stub = ProviderStub::start([Expectation::new("GET", path)
            .query(query)
            .header("accept", "application/json")
            .header(SERVER_TOKEN_HEADER, SECRET_SENTINEL)
            .no_body()
            .respond_json(200, json!({ "TotalCount": 1, items: [{ "index": 0 }] }))])
        .await;
        let plan = postmark::pagination(id).unwrap_or_else(|| panic!("{id} declares a plan"));
        let collected = plan
            .collect(
                render(&stub, id, json!({})),
                &stub.origin(),
                &PaginationBudget::new(4, 4, 16, 16 * 1024, Duration::from_secs(5)),
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .unwrap_or_else(|failure| panic!("{id}: {failure:?}"));
        assert_eq!(collected, vec![json!({ "index": 0 })], "{id}");
        stub.assert_satisfied();
    }
}

/// `postmark_auth_is_applied`: the server token reaches the wire as the exact
/// header Postmark documents, and appears nowhere else.
#[tokio::test]
async fn postmark_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/bounces")
        .header(SERVER_TOKEN_HEADER, SECRET_SENTINEL)
        .without_header("authorization")
        .respond_json(200, json!({ "TotalCount": 0, "Bounces": [] }))])
    .await;

    let request = render(&stub, "bounce.list", json!({}));
    assert!(
        request
            .headers()
            .get(SERVER_TOKEN_HEADER)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(
        !request.url().as_str().contains(SECRET_SENTINEL),
        "the server token is not a query value"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = postmark::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        postmark::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `postmark_error_map`: Postmark's documented statuses and its numeric
/// `ErrorCode` each reach exactly one of the eight closed classes.
#[tokio::test]
async fn postmark_error_map() {
    let documented = [
        // Documented statuses.
        (422, 300, ConnectorErrorClass::Validation),
        (401, 10, ConnectorErrorClass::Authentication),
        (404, 12, ConnectorErrorClass::Permanent),
        (429, 0, ConnectorErrorClass::Http429),
        (500, 0, ConnectorErrorClass::Http5xx),
        (503, 100, ConnectorErrorClass::Http5xx),
        // Documented error codes whose class the status alone does not decide:
        // Postmark answers `422` for a bad token, an exhausted account, and an
        // inactive recipient alike.
        (422, 10, ConnectorErrorClass::Authentication),
        (422, 405, ConnectorErrorClass::Permanent),
        (422, 406, ConnectorErrorClass::Permanent),
        (422, 409, ConnectorErrorClass::Validation),
        // Undocumented: the declared fallback answers.
        (418, 999, ConnectorErrorClass::Permanent),
    ];

    for (status, error_code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("POST", "/email").respond_json(
            status,
            json!({
                "ErrorCode": error_code,
                "Message": format!("shard db-7.internal rejected token {SECRET_SENTINEL}"),
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "email.send", email()))
            .await
            .expect("the stub answers");

        let failure = postmark::error_map().classify_response(&response);
        assert_eq!(
            failure.class(),
            expected,
            "status {status} code {error_code}"
        );
        assert_eq!(failure.provider_status(), Some(status));

        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal", "shard"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // Postmark also answers `200` with a non-zero `ErrorCode` on the send
    // endpoints. The declared output contract refuses it rather than treating
    // a rejected send as a delivered one.
    let stub = ProviderStub::start([Expectation::new("POST", "/email").respond_json(
        200,
        json!({ "ErrorCode": 406, "Message": "You tried to send to recipient(s) that have been marked as inactive." }),
    )])
    .await;
    let response = stub
        .send(render(&stub, "email.send", email()))
        .await
        .expect("the stub answers");
    assert_eq!(
        operation("email.send")
            .decode_response(200, response.body())
            .expect_err("a 200 without the documented MessageID is not a delivered send")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        postmark::error_map()
            .classify(200, response.headers(), response.body())
            .class(),
        ConnectorErrorClass::Permanent,
        "the documented inactive-recipient code is permanent whatever status carries it"
    );
    stub.assert_satisfied();

    // A `Retry-After` a provider sends is clamped to the SDK's ceiling.
    let stub = ProviderStub::start([Expectation::new("GET", "/bounces")
        .respond_header("retry-after", "90000")
        .respond_json(429, json!({ "ErrorCode": 0, "Message": "rate limited" }))])
    .await;
    let response = stub
        .send(render(&stub, "bounce.list", json!({})))
        .await
        .expect("the stub answers");
    assert_eq!(
        postmark::error_map()
            .classify_response(&response)
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
    stub.assert_satisfied();
}

/// `postmark_pagination_is_bounded`: the declared offset/limit plans terminate,
/// respect the budget, and cannot leave the compiled origin.
#[tokio::test]
async fn postmark_pagination_is_bounded() {
    let budget = PaginationBudget::new(8, 8, 1_000, 64 * 1024, Duration::from_secs(5));

    for (id, path, items, page_query, next_query) in [
        (
            "message.list_outbound",
            "/messages/outbound",
            "Messages",
            "offset=0&count=100",
            "offset=100&count=100",
        ),
        (
            "bounce.list",
            "/bounces",
            "Bounces",
            "offset=0&count=100",
            "offset=100&count=100",
        ),
        (
            "template.list",
            "/templates",
            "Templates",
            "Offset=0&Count=100",
            "Offset=100&Count=100",
        ),
    ] {
        let plan = postmark::pagination(id).unwrap_or_else(|| panic!("{id} declares a plan"));
        let full: Vec<JsonValue> = (0..100).map(|index| json!({ "index": index })).collect();
        let stub = ProviderStub::start([
            Expectation::new("GET", path)
                .query(page_query)
                .respond_json(200, json!({ "TotalCount": 101, items: full })),
            Expectation::new("GET", path)
                .query(next_query)
                .respond_json(200, json!({ "TotalCount": 101, items: [{ "index": 100 }] })),
        ])
        .await;
        let collected = plan
            .collect(
                render(&stub, id, json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| {
                    assert_eq!(
                        request.url().host_str(),
                        stub.origin().as_url().host_str(),
                        "{id}: a continuation never leaves the compiled origin"
                    );
                    stub.send(request)
                },
            )
            .await
            .unwrap_or_else(|failure| panic!("{id}: {failure:?}"));
        assert_eq!(collected.len(), 101, "{id}");
        stub.assert_satisfied();

        // An endless provider exhausts each ceiling instead of looping. The
        // offset is derived from the count of items already collected, so no
        // provider value reaches the URL at all.
        for budget in [
            PaginationBudget::new(2, 8, 1_000, 64 * 1024, Duration::from_secs(5)),
            PaginationBudget::new(8, 2, 1_000, 64 * 1024, Duration::from_secs(5)),
            PaginationBudget::new(8, 8, 150, 64 * 1024, Duration::from_secs(5)),
            PaginationBudget::new(8, 8, 1_000, 1_200, Duration::from_secs(5)),
        ] {
            let stub = ProviderStub::start((0..12).map(|_| {
                Expectation::new("GET", path)
                    .respond_json(200, json!({ "TotalCount": 10_000, items: full }))
            }))
            .await;
            let failure = plan
                .collect(
                    render(&stub, id, json!({})),
                    &stub.origin(),
                    &budget,
                    undeclared_status_gate,
                    |request| stub.send(request),
                )
                .await
                .unwrap_err();
            assert_eq!(failure.class(), ConnectorErrorClass::Validation, "{id}");
            assert_eq!(failure.code(), "connector_pagination_budget", "{id}");
        }
    }

    for id in ["email.send", "message.get", "bounce.get", "template.get"] {
        assert!(
            postmark::pagination(id).is_none(),
            "{id} is not a paginated endpoint"
        );
    }
}

/// `postmark_effects_are_classified`: every operation carries a class, and an
/// inventory-only operation cannot be enabled by a deployment.
#[test]
fn postmark_effects_are_classified() {
    let connector = postmark::connector();
    let expected = [
        ("email.send", EffectClass::AtMostOnce),
        ("email.send_template", EffectClass::AtMostOnce),
        ("message.get", EffectClass::ReadOnly),
        ("message.list_outbound", EffectClass::ReadOnly),
        ("bounce.list", EffectClass::ReadOnly),
        ("bounce.get", EffectClass::ReadOnly),
        ("template.list", EffectClass::ReadOnly),
        ("template.get", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: Postmark publishes no idempotency key to bind"
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
                    .is_some_and(|reason| !reason.is_empty())
            );
        } else {
            assert!(connector.admit_operation(id).is_ok(), "{id}");
        }
    }

    assert_eq!(
        connector.admit_operation("email.send_batch"),
        Err(OperationRejection::Undeclared)
    );
}

/// `postmark_output_contract`: the declared pointers are complete and typed,
/// and a missing required pointer is a validation failure rather than a null.
#[test]
fn postmark_output_contract() {
    let send = operation("email.send");
    assert_eq!(
        send.decode_response(
            200,
            br#"{"To":"buyer@example.test","SubmittedAt":"2026-08-10T00:00:00Z","MessageID":"m1","ErrorCode":0,"Message":"OK"}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "message_id": "m1",
            "submitted_at": "2026-08-10T00:00:00Z",
            "to": "buyer@example.test",
            "error_code": 0,
        })
    );
    for body in [
        br#"{"To":"buyer@example.test","SubmittedAt":"2026-08-10T00:00:00Z","ErrorCode":0}"#
            .as_slice(),
        br#"{"To":"buyer@example.test","SubmittedAt":"2026-08-10T00:00:00Z","MessageID":null,"ErrorCode":0}"#
            .as_slice(),
        br#"{"To":"buyer@example.test","SubmittedAt":"2026-08-10T00:00:00Z","MessageID":7,"ErrorCode":0}"#
            .as_slice(),
        br#"{"To":"buyer@example.test","SubmittedAt":"2026-08-10T00:00:00Z","MessageID":"m1","ErrorCode":"0"}"#
            .as_slice(),
    ] {
        assert_eq!(
            send.decode_response(200, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    assert_eq!(
        operation("bounce.list")
            .decode_response(200, br#"{"TotalCount":1,"Bounces":[{"ID":1}]}"#)
            .expect("the declared list contract is satisfied"),
        json!({ "total_count": 1, "bounces": [{ "ID": 1 }] })
    );
    assert_eq!(
        operation("template.get")
            .decode_response(
                200,
                br#"{"TemplateId":1234,"Alias":"order-receipt","Name":"Receipt","Subject":"s"}"#,
            )
            .expect("the declared template contract is satisfied"),
        json!({ "template_id": 1234, "alias": "order-receipt", "name": "Receipt" })
    );

    // An undeclared status is never a silent success.
    assert_eq!(
        send.decode_response(202, br#"{"MessageID":"m1"}"#)
            .expect_err("an undeclared status is a failure")
            .class(),
        ConnectorErrorClass::Permanent
    );
    for operation in postmark::connector().operations() {
        assert!(
            operation.is_success(200) && !operation.is_success(201),
            "{}: Postmark documents 200 as its one success status",
            operation.id()
        );
    }
}
