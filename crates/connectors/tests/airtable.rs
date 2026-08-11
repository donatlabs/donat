//! Airtable connector proofs (spec 012 §3), against the SDK's local provider
//! stub.  No test reaches Airtable, and no test carries a real credential.

use std::time::Duration;

use donat_connectors::providers::airtable;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, Pagination, PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const BASE: &str = "appDonatFixture01";

fn configuration() -> ConnectorConfiguration {
    ConnectorConfiguration::from_deployment([(airtable::BASE_ID, BASE)])
}

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    airtable::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the airtable declaration publishes {id}"))
}

/// Render one operation the way a deployment would: the base identifier comes
/// from deploy-time configuration, and the credential is applied by the plan
/// the declaration carries.
fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let input = airtable::base_scoped_input(&configuration(), &input)
        .expect("a base-scoped input is complete");
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::bearer()
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

/// `airtable_request_shape`: exact method, path, query, headers, and body for
/// every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn airtable_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
            .query("")
            .no_body()
            .respond_json(200, json!({ "records": [] })),
        Expectation::new("GET", &format!("/v0/{BASE}/Orders/recDonat0000001"))
            .query("")
            .no_body()
            .respond_json(
                200,
                json!({ "id": "recDonat0000001", "createdTime": "2026-08-10T00:00:00.000Z", "fields": {} }),
            ),
        Expectation::new("POST", &format!("/v0/{BASE}/Orders"))
            .header("content-type", "application/json")
            .json_body(json!({ "fields": { "Name": "widget" } }))
            .respond_json(
                200,
                json!({ "id": "recDonat0000002", "createdTime": "2026-08-10T00:00:00.000Z", "fields": { "Name": "widget" } }),
            ),
        Expectation::new("PATCH", &format!("/v0/{BASE}/Orders/recDonat0000001"))
            .json_body(json!({ "fields": { "Name": "widget" } }))
            .respond_json(
                200,
                json!({ "id": "recDonat0000001", "createdTime": "2026-08-10T00:00:00.000Z", "fields": { "Name": "widget" } }),
            ),
        Expectation::new("PUT", &format!("/v0/{BASE}/Orders/recDonat0000001"))
            .json_body(json!({ "fields": { "Name": "widget" } }))
            .respond_json(
                200,
                json!({ "id": "recDonat0000001", "createdTime": "2026-08-10T00:00:00.000Z", "fields": { "Name": "widget" } }),
            ),
        Expectation::new("DELETE", &format!("/v0/{BASE}/Orders/recDonat0000001"))
            .no_body()
            .respond_json(200, json!({ "id": "recDonat0000001", "deleted": true })),
        Expectation::new("GET", "/v0/meta/bases")
            .query("")
            .respond_json(200, json!({ "bases": [] })),
        Expectation::new("GET", &format!("/v0/meta/bases/{BASE}/tables"))
            .respond_json(200, json!({ "tables": [] })),
        // A hostile record identifier stays one percent-encoded path segment.
        Expectation::new(
            "GET",
            &format!("/v0/{BASE}/Orders/%2E%2E%2F%2E%2E%2Fmeta%2Fbases%3Fx%3D1%23y"),
        )
        .respond_json(
            200,
            json!({ "id": "recDonat0000001", "createdTime": "2026-08-10T00:00:00.000Z", "fields": {} }),
        ),
    ])
    .await;

    let table = json!({ "table": "Orders" });
    let record = json!({ "table": "Orders", "record_id": "recDonat0000001" });
    let fields = json!({ "table": "Orders", "record_id": "recDonat0000001", "fields": { "Name": "widget" } });

    for (id, input) in [
        ("record.list", table.clone()),
        ("record.get", record.clone()),
        (
            "record.create",
            json!({ "table": "Orders", "fields": { "Name": "widget" } }),
        ),
        ("record.update_patch", fields.clone()),
        ("record.replace", fields),
        ("record.delete", record),
        ("base.list", json!({})),
        ("base.schema", json!({})),
    ] {
        let request = render(&stub, id, input);
        stub.send(request).await.expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "record.get",
        json!({ "table": "Orders", "record_id": "../../meta/bases?x=1#y" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");

    stub.assert_satisfied();

    // The base identifier is deploy-time material: an input that tries to
    // choose one is refused rather than honoured.
    assert!(
        airtable::base_scoped_input(
            &configuration(),
            &json!({ "table": "Orders", "base_id": "appAttackerOwned0" }),
        )
        .is_err(),
        "operation input must not choose the Airtable base"
    );
    assert!(
        airtable::base_scoped_input(&ConnectorConfiguration::default(), &json!({})).is_err(),
        "an unconfigured base identifier is a failure, not a guess"
    );
}

/// `airtable_auth_is_applied`: the personal access token reaches the wire as
/// `Authorization: Bearer <token>` and appears nowhere else.
#[tokio::test]
async fn airtable_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .without_header("x-api-key")
        .respond_json(200, json!({ "records": [] }))])
    .await;

    let request = render(&stub, "record.list", json!({ "table": "Orders" }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive(),
        "an applied credential is marked sensitive so a header dump redacts it"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert_eq!(
        request.url().query(),
        None,
        "the token is not a query value"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = airtable::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        airtable::connector().credential(),
    );
    assert!(
        !surface.contains(SECRET_SENTINEL),
        "a secret never reaches a log line, an error, or a diagnostic: {surface}"
    );
    stub.assert_satisfied();
}

/// `airtable_error_map`: every documented failure status reaches exactly one of
/// the eight closed classes, with a Donat-owned message.
#[tokio::test]
async fn airtable_error_map() {
    // Airtable's own documented status codes, with the error types it
    // documents alongside them.
    let documented = [
        (
            400,
            "INVALID_REQUEST_UNKNOWN",
            ConnectorErrorClass::Validation,
        ),
        (
            401,
            "AUTHENTICATION_REQUIRED",
            ConnectorErrorClass::Authentication,
        ),
        (
            403,
            "INVALID_PERMISSIONS_OR_MODEL_NOT_FOUND",
            ConnectorErrorClass::Authentication,
        ),
        (404, "NOT_FOUND", ConnectorErrorClass::Permanent),
        (413, "REQUEST_TOO_LARGE", ConnectorErrorClass::Validation),
        (
            422,
            "LIST_RECORDS_ITERATOR_NOT_AVAILABLE",
            ConnectorErrorClass::Validation,
        ),
        (429, "RATE_LIMIT_REACHED", ConnectorErrorClass::Http429),
        (500, "SERVER_ERROR", ConnectorErrorClass::Http5xx),
        (502, "SERVER_ERROR", ConnectorErrorClass::Http5xx),
        (503, "RETRIABLE_ERROR", ConnectorErrorClass::Http5xx),
        // Nothing Airtable documents: the declared fallback answers.
        (418, "TEAPOT", ConnectorErrorClass::Permanent),
    ];

    for (status, error_type, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
            .respond_json(
                status,
                json!({
                    "error": {
                        "type": error_type,
                        "message": format!("base {BASE} on shard db-7.internal rejected key {SECRET_SENTINEL}"),
                    }
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "record.list", json!({ "table": "Orders" })))
            .await
            .expect("the stub answers");

        let failure = airtable::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            operation("record.list")
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
        for leaked in [SECRET_SENTINEL, "db-7.internal", "shard", error_type] {
            assert!(
                !surface.contains(leaked),
                "status {status} leaked {leaked} in {surface}"
            );
        }
        stub.assert_satisfied();
    }

    // Airtable answers a 429 with `Retry-After`-less prose and a documented
    // 30 second wait; a provider that does send one is clamped by the SDK.
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
        .respond_header("retry-after", "999999")
        .respond_json(429, json!({ "error": { "type": "RATE_LIMIT_REACHED" } }))])
    .await;
    let response = stub
        .send(render(&stub, "record.list", json!({ "table": "Orders" })))
        .await
        .expect("the stub answers");
    assert_eq!(
        airtable::error_map()
            .classify_response(&response)
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
    stub.assert_satisfied();
}

/// `airtable_pagination_is_bounded`: the declared plans terminate, respect the
/// budget, and cannot leave the compiled origin.
#[tokio::test]
async fn airtable_pagination_is_bounded() {
    let plan = airtable::pagination("record.list").expect("record.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));

    // Airtable's documented protocol: a page carries an `offset` while there
    // are more records, and omits it on the last page.
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
            .query("pageSize=100")
            .respond_json(
                200,
                json!({ "records": [{ "id": "rec1" }], "offset": "itrX/recY" }),
            ),
        Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
            .query("pageSize=100&offset=itrX%2FrecY")
            .respond_json(200, json!({ "records": [{ "id": "rec2" }] })),
    ])
    .await;
    let items = plan
        .collect(
            render(&stub, "record.list", json!({ "table": "Orders" })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the declared plan walks both pages and stops");
    assert_eq!(
        items,
        vec![json!({ "id": "rec1" }), json!({ "id": "rec2" })]
    );
    stub.assert_satisfied();

    // An offset that spells another origin stays a percent-encoded query
    // value: a continuation cannot move the request.
    let elsewhere =
        ProviderStub::start([Expectation::new("GET", "/v0/appOther/Orders")
            .respond_json(200, json!({ "records": [] }))])
        .await;
    let hostile = format!("{}/v0/appOther/Orders", elsewhere.base_url());
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
            .respond_json(200, json!({ "records": [], "offset": hostile })),
        Expectation::new("GET", &format!("/v0/{BASE}/Orders"))
            .respond_json(200, json!({ "records": [] })),
    ])
    .await;
    plan.collect(
        render(&stub, "record.list", json!({ "table": "Orders" })),
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
    .expect("the hostile offset is a query value, not a destination");
    assert_eq!(
        elsewhere.mismatches().len(),
        1,
        "the other origin was never contacted"
    );

    // An endless provider exhausts each ceiling instead of looping.
    for budget in [
        PaginationBudget::new(2, 8, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 2, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 64, 100, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|index| {
            Expectation::new("GET", &format!("/v0/{BASE}/Orders")).respond_json(
                200,
                json!({ "records": [{ "id": index }], "offset": "itrX/recY" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "record.list", json!({ "table": "Orders" })),
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

    // The meta endpoint paginates on the same `offset`, with no documented
    // page size to declare.
    let bases = airtable::pagination("base.list").expect("base.list declares a plan");
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v0/meta/bases")
            .query("")
            .respond_json(
                200,
                json!({ "bases": [{ "id": "app1" }], "offset": "itr2" }),
            ),
        Expectation::new("GET", "/v0/meta/bases")
            .query("offset=itr2")
            .respond_json(200, json!({ "bases": [{ "id": "app2" }] })),
    ])
    .await;
    assert_eq!(
        bases
            .collect(
                render(&stub, "base.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect("the meta plan walks both pages")
            .len(),
        2
    );
    stub.assert_satisfied();

    // Only the operations Airtable documents as paginated declare a plan.
    for id in ["record.get", "record.create", "base.schema"] {
        assert!(
            airtable::pagination(id).is_none(),
            "{id} is not a paginated endpoint"
        );
    }
}

/// `airtable_effects_are_classified`: every operation carries a class, and an
/// inventory-only operation cannot be enabled by a deployment.
#[test]
fn airtable_effects_are_classified() {
    let connector = airtable::connector();
    let expected = [
        ("record.list", EffectClass::ReadOnly),
        ("record.get", EffectClass::ReadOnly),
        ("record.create", EffectClass::AtMostOnce),
        ("record.update_patch", EffectClass::InventoryOnly),
        (
            "record.replace",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        (
            "record.delete",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        ("base.list", EffectClass::ReadOnly),
        ("base.schema", EffectClass::ReadOnly),
    ];
    assert_eq!(
        connector.operations().len(),
        expected.len(),
        "every declared operation is classified here"
    );

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        match class {
            EffectClass::InventoryOnly => {
                assert_eq!(
                    connector.admit_operation(id),
                    Err(OperationRejection::InventoryOnly),
                    "{id} must not be enablable by a deployment"
                );
                assert!(
                    operation
                        .effect()
                        .and_then(donat_connectors::sdk::Effect::inventory_reason)
                        .is_some_and(|reason| !reason.is_empty()),
                    "{id} records why it is not executable"
                );
            }
            _ => {
                assert!(connector.admit_operation(id).is_ok(), "{id}");
                assert!(operation.is_executable(), "{id}");
            }
        }
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: Airtable publishes no idempotency key to bind"
        );
    }

    assert_eq!(
        connector.admit_operation("record.upsert"),
        Err(OperationRejection::Undeclared),
        "an operation this binary does not compile cannot be enabled"
    );
}

/// `airtable_output_contract`: the declared pointers are complete and typed,
/// and a missing required pointer is a validation failure rather than a null.
#[test]
fn airtable_output_contract() {
    let record = operation("record.get");
    assert_eq!(
        record
            .decode_response(
                200,
                br#"{"id":"recDonat0000001","createdTime":"2026-08-10T00:00:00.000Z","fields":{"Name":"widget"},"commentCount":2}"#,
            )
            .expect("the declared contract is satisfied"),
        json!({
            "id": "recDonat0000001",
            "created_time": "2026-08-10T00:00:00.000Z",
            "fields": { "Name": "widget" },
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );

    for body in [
        br#"{"createdTime":"2026-08-10T00:00:00.000Z","fields":{}}"#.as_slice(),
        br#"{"id":null,"createdTime":"2026-08-10T00:00:00.000Z","fields":{}}"#.as_slice(),
        br#"{"id":7,"createdTime":"2026-08-10T00:00:00.000Z","fields":{}}"#.as_slice(),
    ] {
        assert_eq!(
            record
                .decode_response(200, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    // A list carries its page and its continuation, and the continuation is
    // optional exactly as Airtable documents it.
    assert_eq!(
        operation("record.list")
            .decode_response(200, br#"{"records":[]}"#)
            .expect("a last page carries no offset"),
        json!({ "records": [], "offset": null })
    );
    assert_eq!(
        operation("record.delete")
            .decode_response(200, br#"{"id":"recDonat0000001","deleted":true}"#)
            .expect("the declared delete contract is satisfied"),
        json!({ "id": "recDonat0000001", "deleted": true })
    );
    assert_eq!(
        operation("base.schema")
            .decode_response(200, br#"{"tables":[{"id":"tbl1","name":"Orders"}]}"#)
            .expect("the declared schema contract is satisfied"),
        json!({ "tables": [{ "id": "tbl1", "name": "Orders" }] })
    );

    // An undeclared status is never a silent success.
    assert_eq!(
        record
            .decode_response(204, b"")
            .expect_err("an undeclared status is a failure")
            .class(),
        ConnectorErrorClass::Permanent
    );

    for operation in airtable::connector().operations() {
        assert!(
            operation.is_success(200),
            "{}: Airtable documents 200 as its one success status",
            operation.id()
        );
    }
}

/// The declaration itself: one fixed origin, one credential contract, and a
/// pagination plan that only names endpoints the provider paginates.
#[test]
fn airtable_declaration_is_static() {
    let connector = airtable::connector();
    assert_eq!(connector.name(), "airtable");
    assert!(
        matches!(
            connector.origin(),
            donat_connectors::sdk::OriginSpec::Fixed(_)
        ),
        "Airtable publishes one fixed API origin"
    );
    assert_eq!(
        connector
            .credential()
            .fields()
            .iter()
            .map(|field| (field.name(), field.is_secret()))
            .collect::<Vec<_>>(),
        [("secret", true), (airtable::BASE_ID, false)]
    );
    let _: &Pagination = airtable::pagination("record.list").expect("record.list paginates");
}
