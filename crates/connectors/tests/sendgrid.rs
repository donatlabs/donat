//! SendGrid connector proofs (spec 012 §3), against the SDK's local provider
//! stub.  No test reaches SendGrid, and no test carries a real credential.

use std::time::Duration;

use donat_connectors::providers::sendgrid;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const LIST_ID: &str = "01HDONATLIST0000000000000";
const CONTACT_ID: &str = "01HDONATCONTACT000000000";

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    sendgrid::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the sendgrid declaration publishes {id}"))
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

fn contacts() -> JsonValue {
    json!({ "contacts": [{ "email": "buyer@example.test" }] })
}

/// `sendgrid_request_shape`: exact method, path, query, headers, and body for
/// every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn sendgrid_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v3/marketing/contacts")
            .query("")
            .no_body()
            .respond_json(200, json!({ "result": [], "contact_count": 0 })),
        Expectation::new("GET", &format!("/v3/marketing/contacts/{CONTACT_ID}"))
            .query("")
            .no_body()
            .respond_json(
                200,
                json!({ "id": CONTACT_ID, "email": "buyer@example.test", "created_at": "2026-08-10T00:00:00Z", "updated_at": "2026-08-10T00:00:00Z", "list_ids": [] }),
            ),
        Expectation::new("PUT", "/v3/marketing/contacts")
            .header("content-type", "application/json")
            .json_body(json!({ "contacts": [{ "email": "buyer@example.test" }] }))
            .respond_json(202, json!({ "job_id": "job_1" })),
        Expectation::new("DELETE", "/v3/marketing/contacts")
            .query(&format!("ids={CONTACT_ID}"))
            .respond_json(202, json!({ "job_id": "job_2" })),
        Expectation::new("POST", "/v3/marketing/lists")
            .json_body(json!({ "name": "Buyers" }))
            .respond_json(
                201,
                json!({ "id": LIST_ID, "name": "Buyers", "contact_count": 0 }),
            ),
        Expectation::new("GET", &format!("/v3/marketing/lists/{LIST_ID}"))
            .query("")
            .respond_json(
                200,
                json!({ "id": LIST_ID, "name": "Buyers", "contact_count": 0 }),
            ),
        Expectation::new("GET", "/v3/marketing/lists")
            .query("page_size=100")
            .respond_json(
                200,
                json!({ "result": [], "_metadata": { "self": "https://api.sendgrid.com/v3/marketing/lists" } }),
            ),
        Expectation::new("PATCH", &format!("/v3/marketing/lists/{LIST_ID}"))
            .json_body(json!({ "name": "Buyers 2026" }))
            .respond_json(
                200,
                json!({ "id": LIST_ID, "name": "Buyers 2026", "contact_count": 0 }),
            ),
        Expectation::new("DELETE", &format!("/v3/marketing/lists/{LIST_ID}"))
            .query("delete_contacts=false")
            .respond_json(200, json!({ "job_id": "job_3" })),
        Expectation::new("POST", "/v3/mail/send")
            .json_body(json!({
                "personalizations": [{ "to": [{ "email": "buyer@example.test" }] }],
                "from": { "email": "sales@example.test" },
                "subject": "Your order",
                "content": [{ "type": "text/plain", "value": "Thank you" }],
            }))
            .respond_bytes(202, Vec::new()),
        // A hostile list identifier stays one percent-encoded path segment.
        Expectation::new("GET", "/v3/marketing/lists/%2E%2E%2Fmail%2Fsend%3Fx%3D1")
            .respond_json(200, json!({ "id": LIST_ID, "name": "x", "contact_count": 0 })),
    ])
    .await;

    for (id, input) in [
        ("contact.list", json!({})),
        ("contact.get", json!({ "contact_id": CONTACT_ID })),
        ("contact.upsert", contacts()),
        ("contact.delete", json!({ "ids": CONTACT_ID })),
        ("list.create", json!({ "name": "Buyers" })),
        ("list.get", json!({ "list_id": LIST_ID })),
        ("list.list", json!({})),
        (
            "list.update",
            json!({ "list_id": LIST_ID, "name": "Buyers 2026" }),
        ),
        (
            "list.delete",
            json!({ "list_id": LIST_ID, "delete_contacts": false }),
        ),
        (
            "mail.send",
            json!({
                "personalizations": [{ "to": [{ "email": "buyer@example.test" }] }],
                "from": { "email": "sales@example.test" },
                "subject": "Your order",
                "content": [{ "type": "text/plain", "value": "Thank you" }],
            }),
        ),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(&stub, "list.get", json!({ "list_id": "../mail/send?x=1" }));
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");

    stub.assert_satisfied();
}

/// `sendgrid_auth_is_applied`: the API key reaches the wire as
/// `Authorization: Bearer <key>` and appears nowhere else.
#[tokio::test]
async fn sendgrid_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/marketing/lists")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .without_header("x-api-key")
        .respond_json(200, json!({ "result": [] }))])
    .await;

    let request = render(&stub, "list.list", json!({}));
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
    let failure = sendgrid::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        sendgrid::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `sendgrid_error_map`: every documented failure status reaches exactly one of
/// the eight closed classes, and SendGrid's prose never crosses the boundary.
#[tokio::test]
async fn sendgrid_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (406, ConnectorErrorClass::Validation),
        (413, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        // Undocumented: the declared fallback answers.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", "/v3/marketing/lists")
            .respond_header("x-message-id", "msg_01H")
            .respond_json(
                status,
                json!({
                    "errors": [{
                        "field": "from.email",
                        "message": format!("shard db-7.internal rejected key {SECRET_SENTINEL}"),
                        "help": "https://docs.example.test",
                    }]
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "list.list", json!({})))
            .await
            .expect("the stub answers");

        let failure = sendgrid::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("message_id")
                .map(String::as_str),
            Some("msg_01H"),
            "the documented X-Message-Id is the support handle SendGrid publishes"
        );
        assert!(
            operation("list.list")
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
            "from.email",
            "docs.example",
        ] {
            assert!(
                !surface.contains(leaked),
                "status {status} leaked {leaked} in {surface}"
            );
        }
        stub.assert_satisfied();
    }

    // A `Retry-After` a provider sends is clamped to the SDK's ceiling.
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/marketing/lists")
        .respond_header("retry-after", "100000")
        .respond_json(429, json!({ "errors": [] }))])
    .await;
    let response = stub
        .send(render(&stub, "list.list", json!({})))
        .await
        .expect("the stub answers");
    assert_eq!(
        sendgrid::error_map()
            .classify_response(&response)
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
    stub.assert_satisfied();
}

/// `sendgrid_pagination_is_bounded`: SendGrid's Marketing Campaigns lists
/// publish their continuation as an absolute URL in `_metadata.next`, which the
/// SDK's body-carried next-URI plan resolves against the compiled origin. A
/// continuation that names another origin is rejected, not followed, and the
/// endpoints SendGrid publishes no continuation for declare no plan at all.
#[tokio::test]
async fn sendgrid_pagination_is_bounded() {
    // "pagination of the contacts has been deprecated", and a single resource
    // has nothing to continue.
    for id in ["contact.list", "contact.get", "list.get", "mail.send"] {
        assert!(
            sendgrid::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    let plan = sendgrid::pagination("list.list").expect("list.list declares a continuation plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    // The documented walk: `_metadata.next` is an absolute URL on SendGrid's
    // own origin, and `page_size` stays part of the declaration.
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v3/marketing/lists")
            .query("page_size=100")
            .respond_json(
                200,
                json!({
                    "result": [{ "id": "l1", "name": "Buyers", "contact_count": 1 }],
                    "_metadata": { "self": "{base_url}/v3/marketing/lists", "next": "{base_url}/v3/marketing/lists?page_size=100&page_token=zzz" },
                }),
            ),
        Expectation::new("GET", "/v3/marketing/lists")
            .query("page_size=100&page_token=zzz")
            .respond_json(
                200,
                json!({
                    "result": [{ "id": "l2", "name": "Sellers", "contact_count": 2 }],
                    "_metadata": { "self": "{base_url}/v3/marketing/lists" },
                }),
            ),
    ])
    .await;
    let lists = plan
        .collect(
            render(&stub, "list.list", json!({})),
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
        .expect("the declared plan walks both pages and stops");
    assert_eq!(
        lists
            .iter()
            .filter_map(|list| list.get("id").and_then(JsonValue::as_str))
            .collect::<Vec<_>>(),
        vec!["l1", "l2"]
    );
    stub.assert_satisfied();

    // A `_metadata.next` naming another origin is refused rather than followed,
    // and that origin is never contacted.
    let elsewhere =
        ProviderStub::start([Expectation::new("GET", "/v3/marketing/lists")
            .respond_json(200, json!({ "result": [] }))])
        .await;
    let next = format!("{}/v3/marketing/lists?page_token=zzz", elsewhere.base_url());
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/marketing/lists")
        .query("page_size=100")
        .respond_json(200, json!({ "result": [], "_metadata": { "next": next } }))])
    .await;
    let failure = plan
        .collect(
            render(&stub, "list.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect_err("a cross-origin continuation is not followed");
    assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
    assert_eq!(failure.code(), "connector_pagination_cross_origin");
    assert_eq!(
        elsewhere.mismatches().len(),
        1,
        "the origin named by the provider body was never contacted"
    );
    stub.assert_satisfied();

    // The continuation is also published as declared output, and reading it
    // there never turns it into a request.
    let stub = ProviderStub::start([Expectation::new("GET", "/v3/marketing/lists")
        .query("page_size=100")
        .respond_json(200, json!({ "result": [], "_metadata": { "next": next } }))])
    .await;
    let response = stub
        .send(render(&stub, "list.list", json!({})))
        .await
        .expect("the stub answers");
    let page = operation("list.list")
        .decode_response(200, response.body())
        .expect("the declared contract is satisfied");
    assert_eq!(
        page.get("next_page").and_then(JsonValue::as_str),
        Some(next.as_str())
    );
    let again = render(&stub, "list.list", json!({}));
    assert_eq!(again.url().host_str(), stub.origin().as_url().host_str());
    assert_eq!(again.url().query(), Some("page_size=100"));

    // An endless provider exhausts each ceiling instead of looping.
    for budget in [
        PaginationBudget::new(2, 8, 10_000, 1_024 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 10_000, 1_024 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 3, 1_024 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 10_000, 200, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v3/marketing/lists").respond_json(
                200,
                json!({
                    "result": [{ "id": "l1" }, { "id": "l2" }],
                    "_metadata": { "next": "{base_url}/v3/marketing/lists?page_token=zzz" },
                }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "list.list", json!({})),
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
}

/// `sendgrid_effects_are_classified`: every operation carries a class, and an
/// inventory-only operation cannot be enabled by a deployment.
#[test]
fn sendgrid_effects_are_classified() {
    let connector = sendgrid::connector();
    let expected = [
        ("contact.list", EffectClass::ReadOnly),
        ("contact.get", EffectClass::ReadOnly),
        (
            "contact.upsert",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        (
            "contact.delete",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        ("list.create", EffectClass::AtMostOnce),
        ("list.get", EffectClass::ReadOnly),
        ("list.list", EffectClass::ReadOnly),
        ("list.update", EffectClass::InventoryOnly),
        ("list.delete", EffectClass::ProviderIdempotentNaturalMethod),
        ("mail.send", EffectClass::AtMostOnce),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: SendGrid publishes no idempotency key to bind"
        );
        if class == EffectClass::InventoryOnly {
            assert_eq!(
                connector.admit_operation(id),
                Err(OperationRejection::InventoryOnly),
                "{id} must not be enablable by a deployment"
            );
        } else {
            assert!(connector.admit_operation(id).is_ok(), "{id}");
        }
    }

    assert_eq!(
        connector.admit_operation("mail.batch"),
        Err(OperationRejection::Undeclared)
    );
}

/// Two identical upserts are one request against one resource identity, which
/// is what SendGrid documents as leaving one contact — the `NaturalMethod`
/// evidence spec 010 §7 asks for.
#[tokio::test]
async fn sendgrid_two_identical_upserts_are_one_resource() {
    let stub = ProviderStub::start([
        Expectation::new("PUT", "/v3/marketing/contacts")
            .json_body(json!({ "contacts": [{ "email": "buyer@example.test" }] }))
            .respond_json(202, json!({ "job_id": "job_1" })),
        Expectation::new("PUT", "/v3/marketing/contacts")
            .json_body(json!({ "contacts": [{ "email": "buyer@example.test" }] }))
            .respond_json(202, json!({ "job_id": "job_2" })),
    ])
    .await;

    let first = render(&stub, "contact.upsert", contacts());
    let second = render(&stub, "contact.upsert", contacts());
    assert_eq!(first.method(), second.method());
    assert_eq!(first.url().as_str(), second.url().as_str());
    assert_eq!(first.body(), second.body());
    assert_eq!(
        first.url().path(),
        "/v3/marketing/contacts",
        "the resource identity is the contact collection, keyed by email"
    );

    stub.send(first).await.expect("the stub answers");
    stub.send(second).await.expect("the stub answers");
    stub.assert_satisfied();

    // The same holds for the two deletes, whose identity is in the request.
    let stub = ProviderStub::start([
        Expectation::new("DELETE", "/v3/marketing/contacts")
            .query(&format!("ids={CONTACT_ID}"))
            .respond_json(202, json!({ "job_id": "job_3" })),
        Expectation::new("DELETE", "/v3/marketing/contacts")
            .query(&format!("ids={CONTACT_ID}"))
            .respond_json(202, json!({ "job_id": "job_4" })),
    ])
    .await;
    for _ in 0..2 {
        stub.send(render(
            &stub,
            "contact.delete",
            json!({ "ids": CONTACT_ID }),
        ))
        .await
        .expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `sendgrid_output_contract`: the declared pointers are complete and typed,
/// and a missing required pointer is a validation failure rather than a null.
#[test]
fn sendgrid_output_contract() {
    let contact = operation("contact.get");
    assert_eq!(
        contact
            .decode_response(
                200,
                br#"{"id":"c1","email":"buyer@example.test","created_at":"2026-08-10T00:00:00Z","updated_at":"2026-08-10T00:00:00Z","list_ids":["l1"],"_metadata":{"self":"x"}}"#,
            )
            .expect("the declared contract is satisfied"),
        json!({
            "id": "c1",
            "email": "buyer@example.test",
            "created_at": "2026-08-10T00:00:00Z",
            "updated_at": "2026-08-10T00:00:00Z",
            "list_ids": ["l1"],
        })
    );
    for body in [
        br#"{"email":"buyer@example.test","created_at":"a","updated_at":"b","list_ids":[]}"#
            .as_slice(),
        br#"{"id":null,"email":"buyer@example.test","created_at":"a","updated_at":"b","list_ids":[]}"#
            .as_slice(),
        br#"{"id":7,"email":"buyer@example.test","created_at":"a","updated_at":"b","list_ids":[]}"#
            .as_slice(),
    ] {
        assert_eq!(
            contact
                .decode_response(200, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    assert_eq!(
        operation("list.create")
            .decode_response(201, br#"{"id":"l1","name":"Buyers","contact_count":0}"#)
            .expect("the declared create contract is satisfied"),
        json!({ "id": "l1", "name": "Buyers", "contact_count": 0 })
    );
    assert_eq!(
        operation("contact.upsert")
            .decode_response(202, br#"{"job_id":"job_1"}"#)
            .expect("the declared upsert contract is satisfied"),
        json!({ "job_id": "job_1" })
    );
    assert_eq!(
        operation("list.list")
            .decode_response(200, br#"{"result":[]}"#)
            .expect("a last page carries no next"),
        json!({ "result": [], "next_page": null })
    );

    // The documented success statuses, exactly as SendGrid publishes them.
    for (id, statuses) in [
        ("contact.list", vec![200]),
        ("contact.get", vec![200]),
        ("contact.upsert", vec![202]),
        ("contact.delete", vec![202]),
        ("list.create", vec![201]),
        ("list.get", vec![200]),
        ("list.list", vec![200]),
        ("list.update", vec![200]),
        ("list.delete", vec![200, 204]),
        ("mail.send", vec![202]),
    ] {
        let operation = operation(id);
        for status in 200..=299u16 {
            assert_eq!(
                operation.is_success(status),
                statuses.contains(&status),
                "{id} status {status}"
            );
        }
    }

    // A 202 SendGrid answers with no body at all is the documented success, and
    // the declaration says so: `mail.send` declares its contract as the
    // accepted send and carries no pointer to satisfy.
    let send = operation("mail.send");
    assert!(send.is_no_content_success(202));
    assert_eq!(
        send.decode_response(202, b"")
            .expect("the documented empty 202 is the documented success"),
        json!({})
    );

    // The list delete documents both spellings, and each decodes as its own.
    let delete = operation("list.delete");
    assert!(delete.is_no_content_success(204) && !delete.is_no_content_success(200));
    assert_eq!(
        delete
            .decode_response(204, b"")
            .expect("\"204 The delete has been processed\" carries no body"),
        json!({ "job_id": null })
    );
    assert_eq!(
        delete
            .decode_response(200, br#"{"job_id":"job_3"}"#)
            .expect("\"200 The delete has been accepted\" carries the job"),
        json!({ "job_id": "job_3" })
    );
    // An operation whose contract is a body still fails without one.
    assert!(
        operation("list.get")
            .decode_response(200, b"")
            .is_err_and(|failure| failure.class() == ConnectorErrorClass::Validation),
        "a declared required pointer is not satisfied by an absent body"
    );
}
