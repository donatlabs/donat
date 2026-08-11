//! Intercom connector proofs (spec 016 §3), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::intercom;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const CONTACT_ID: &str = "63a07ddf05a32042dffac965";
const COMPANY_ID: &str = "6762f0761bb69f9f2193bae2";
const CONVERSATION_ID: &str = "123";

fn operation(id: &str) -> &'static Operation {
    intercom::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the intercom declaration publishes {id}"))
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

fn contact() -> JsonValue {
    json!({
        "type": "contact",
        "id": CONTACT_ID,
        "role": "user",
        "email": "joe@example.com",
        "name": "Joe",
        "external_id": "user-1",
        "created_at": 1_571_672_154_i64,
        "updated_at": 1_571_672_154_i64,
    })
}

fn company() -> JsonValue {
    json!({
        "type": "company",
        "id": COMPANY_ID,
        "company_id": "company_remote_id",
        "name": "my company",
        "created_at": 1_734_537_334_i64,
        "updated_at": 1_734_537_334_i64,
    })
}

fn conversation() -> JsonValue {
    json!({
        "type": "conversation",
        "id": CONVERSATION_ID,
        "state": "open",
        "open": true,
        "read": true,
        "created_at": 1_734_537_334_i64,
        "updated_at": 1_734_537_334_i64,
    })
}

fn cursor_list(items_key: &str) -> JsonValue {
    json!({
        "type": "list",
        items_key: [],
        "total_count": 0,
        "pages": { "type": "pages", "page": 1, "per_page": 150, "total_pages": 1 },
    })
}

/// Every operation, with an input that satisfies it and a documented success.
fn cases() -> Vec<(&'static str, JsonValue, JsonValue)> {
    vec![
        (
            "contact.get",
            json!({ "contact_id": CONTACT_ID }),
            contact(),
        ),
        ("contact.list", json!({}), cursor_list("data")),
        (
            "contact.search",
            json!({
                "query": { "field": "role", "operator": "=", "value": "user" },
                "starting_after": null,
            }),
            cursor_list("data"),
        ),
        (
            "contact.create",
            json!({ "role": "user", "external_id": "user-1",
                    "email": "joe@example.com", "name": "Joe" }),
            contact(),
        ),
        (
            "contact.update",
            json!({ "contact_id": CONTACT_ID, "email": "joe@example.com", "name": "Joe" }),
            contact(),
        ),
        (
            "company.get",
            json!({ "company_id": COMPANY_ID }),
            company(),
        ),
        ("company.list", json!({}), cursor_list("data")),
        (
            "company.create_or_update",
            json!({ "company_id": "company_remote_id", "name": "my company" }),
            company(),
        ),
        (
            "conversation.get",
            json!({ "conversation_id": CONVERSATION_ID }),
            conversation(),
        ),
        ("conversation.list", json!({}), cursor_list("conversations")),
        (
            "conversation.reply",
            json!({ "conversation_id": CONVERSATION_ID, "message_type": "comment",
                    "type": "admin", "body": "hello", "admin_id": "3156780" }),
            conversation(),
        ),
    ]
}

/// `intercom_request_shape`: exact method, path, query, headers, and body for
/// every operation, including the pinned `Intercom-Version` on all of them.
#[tokio::test]
async fn intercom_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/contacts/{CONTACT_ID}"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .header("intercom-version", intercom::API_VERSION)
            .no_body()
            .respond_json(200, contact()),
        Expectation::new("GET", "/contacts")
            .query("per_page=150")
            .respond_json(200, cursor_list("data")),
        Expectation::new("POST", "/contacts/search")
            .json_body(json!({
                "query": { "field": "role", "operator": "=", "value": "user" },
                "pagination": { "per_page": 150, "starting_after": null },
            }))
            .respond_json(200, cursor_list("data")),
        Expectation::new("POST", "/contacts")
            .json_body(json!({ "role": "user", "external_id": "user-1",
                               "email": "joe@example.com", "name": "Joe" }))
            .respond_json(200, contact()),
        Expectation::new("PUT", &format!("/contacts/{CONTACT_ID}"))
            .json_body(json!({ "email": "joe@example.com", "name": "Joe" }))
            .respond_json(200, contact()),
        Expectation::new("GET", &format!("/companies/{COMPANY_ID}"))
            .query("")
            .respond_json(200, company()),
        Expectation::new("GET", "/companies")
            .query("per_page=150")
            .respond_json(200, cursor_list("data")),
        Expectation::new("POST", "/companies")
            .json_body(json!({ "company_id": "company_remote_id", "name": "my company" }))
            .respond_json(200, company()),
        Expectation::new("GET", &format!("/conversations/{CONVERSATION_ID}"))
            .query("")
            .respond_json(200, conversation()),
        Expectation::new("GET", "/conversations")
            .query("per_page=150")
            .respond_json(200, cursor_list("conversations")),
        Expectation::new("POST", &format!("/conversations/{CONVERSATION_ID}/reply"))
            .json_body(json!({ "message_type": "comment", "type": "admin",
                               "body": "hello", "admin_id": "3156780" }))
            .respond_json(200, conversation()),
    ])
    .await;

    for (id, input, _) in cases() {
        let request = render(&stub, id, input);
        assert_eq!(
            request
                .headers()
                .get("intercom-version")
                .and_then(|value| value.to_str().ok()),
            Some(intercom::API_VERSION),
            "{id} pins the contract version"
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `intercom_auth_is_applied`: the access token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else.
#[tokio::test]
async fn intercom_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/contacts/{CONTACT_ID}"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, contact())])
    .await;

    let request = render(&stub, "contact.get", json!({ "contact_id": CONTACT_ID }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        intercom::connector().credential(),
        intercom::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `intercom_error_map`: every documented status and code reaches exactly one
/// closed class, and none of Intercom's prose crosses the boundary.
#[tokio::test]
async fn intercom_error_map() {
    let documented = [
        (400, "parameter_invalid", ConnectorErrorClass::Validation),
        (400, "parameter_not_found", ConnectorErrorClass::Validation),
        (400, "type_mismatch", ConnectorErrorClass::Validation),
        (401, "token_not_found", ConnectorErrorClass::Authentication),
        (401, "token_revoked", ConnectorErrorClass::Authentication),
        (
            401,
            "missing_authorization",
            ConnectorErrorClass::Authentication,
        ),
        (402, "api_plan_restricted", ConnectorErrorClass::Permanent),
        (403, "action_forbidden", ConnectorErrorClass::Authentication),
        (409, "conflict", ConnectorErrorClass::Permanent),
        (429, "rate_limit_exceeded", ConnectorErrorClass::Http429),
        (500, "server_error", ConnectorErrorClass::Http5xx),
        // A status with an envelope carrying no code rule: the status answers.
        (405, "team_unavailable", ConnectorErrorClass::Permanent),
        (408, "not_a_published_code", ConnectorErrorClass::Timeout),
        (418, "not_a_published_code", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub =
            ProviderStub::start([Expectation::new("GET", &format!("/contacts/{CONTACT_ID}"))
                .respond_header("retry-after", "5")
                .respond_json(
                    status,
                    json!({
                        "type": "error.list",
                        "request_id": "f5b3b3a0",
                        "errors": [{
                            "code": code,
                            "message": format!("workspace acme shard db-7 token {SECRET_SENTINEL}"),
                            "field": "email",
                        }],
                    }),
                )])
            .await;
        let response = stub
            .send(render(
                &stub,
                "contact.get",
                json!({ "contact_id": CONTACT_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = intercom::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme", "db-7", "email"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `intercom_rate_limit_is_classified` (spec 016 §3 proof 5): the documented
/// rate-limit response reaches `http_429` with its retry hint clamped.
#[tokio::test]
async fn intercom_rate_limit_is_classified() {
    // "429 Too Many Requests -- The client has reached or exceeded a rate limit,
    // or the server is overloaded."
    let limited = json!({ "type": "error.list",
                          "errors": [{ "code": "rate_limit_exceeded", "message": "…" }] });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/contacts/{CONTACT_ID}"))
            .respond_header("retry-after", "10")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/contacts/{CONTACT_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "contact.get",
                json!({ "contact_id": CONTACT_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(intercom::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(10)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `intercom_cursor_is_opaque_and_bounded` (spec 016 §3 proof 4): the cursor is
/// echoed verbatim, is never parsed or constructed here, and the loop stops at
/// every budget — including the page-numbered collection, whose walk is derived
/// from the walk rather than from a provider value.
#[tokio::test]
async fn intercom_cursor_is_opaque_and_bounded() {
    let plan = intercom::pagination("contact.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const CURSOR: &str = "WzE2NDA5OTUyMDAsIjYzYSJd&per_page=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", "/contacts")
            .query("per_page=150")
            .respond_json(
                200,
                json!({ "type": "list", "data": [{ "id": "c1" }],
                        "pages": { "next": { "per_page": 150, "starting_after": CURSOR } } }),
            ),
        Expectation::new("GET", "/contacts")
            .query(
                "per_page=150&starting_after=WzE2NDA5OTUyMDAsIjYzYSJd%26per%5Fpage%3D9999%23%2F%2E%2E%2F",
            )
            .respond_json(
                200,
                // `pages.next` is documented nullable: an absent cursor is the
                // end of the collection.
                json!({ "type": "list", "data": [{ "id": "c2" }], "pages": { "next": null } }),
            ),
    ])
    .await;
    let contacts = plan
        .collect(
            render(&stub, "contact.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one cursor and stops on the null one");
    assert_eq!(contacts.len(), 2);
    stub.assert_satisfied();
}

/// `intercom_pagination_is_bounded`: the declared plans terminate and
/// respect the call, page, item, and byte budgets.
#[tokio::test]
async fn intercom_pagination_is_bounded() {
    let plan = intercom::pagination("contact.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const CURSOR: &str = "WzE2NDA5OTUyMDAsIjYzYSJd&per_page=9999#/../";

    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/contacts").respond_json(
                200,
                json!({ "data": [{ "id": "c1" }, { "id": "c2" }],
                        "pages": { "next": { "starting_after": "more" } } }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "contact.list", json!({})),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // The page-numbered collection: the number comes from the walk, never from
    // the provider, and a short page ends it.
    let companies = intercom::pagination("company.list").expect("the list declares a plan");
    let stub = ProviderStub::start([Expectation::new("GET", "/companies")
        .query("per_page=150&page=1")
        .respond_json(
            200,
            // Fewer than one page of results: the walk is over.
            json!({ "type": "list", "data": [{ "id": "co1" }],
                    "pages": { "page": 7, "total_pages": 99 } }),
        )])
    .await;
    let page = companies
        .collect(
            render(&stub, "company.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("a short page ends the walk");
    assert_eq!(
        page.len(),
        1,
        "a provider `page` of 7 cannot restart the walk"
    );
    stub.assert_satisfied();

    // The body-carried search cursor: echoed verbatim, never a query value.
    let stub = ProviderStub::start([Expectation::new("POST", "/contacts/search")
        .query("")
        .json_body(json!({
            "query": {},
            "pagination": { "per_page": 150, "starting_after": CURSOR },
        }))
        .respond_json(200, json!({ "data": [], "pages": { "next": null } }))])
    .await;
    let request = render(
        &stub,
        "contact.search",
        json!({ "query": {}, "starting_after": CURSOR }),
    );
    assert_eq!(request.url().query(), None);
    stub.send(request).await.expect("the stub answers");
    stub.assert_satisfied();

    for id in [
        "contact.get",
        "contact.search",
        "contact.create",
        "contact.update",
        "company.get",
        "company.create_or_update",
        "conversation.get",
        "conversation.reply",
    ] {
        assert!(
            intercom::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `intercom_effects_are_classified`: every operation carries a class, and the
/// documented upsert is refused by the method the provider chose for it.
#[test]
fn intercom_effects_are_classified() {
    let connector = intercom::connector();
    let expected = [
        ("contact.get", EffectClass::ReadOnly),
        ("contact.list", EffectClass::ReadOnly),
        ("contact.search", EffectClass::ReadOnly),
        ("contact.create", EffectClass::AtMostOnce),
        ("contact.update", EffectClass::InventoryOnly),
        ("company.get", EffectClass::ReadOnly),
        ("company.list", EffectClass::ReadOnly),
        ("company.create_or_update", EffectClass::InventoryOnly),
        ("conversation.get", EffectClass::ReadOnly),
        ("conversation.list", EffectClass::ReadOnly),
        ("conversation.reply", EffectClass::AtMostOnce),
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
    assert_eq!(
        connector.admit_operation("company.create_or_update"),
        Err(OperationRejection::InventoryOnly)
    );
    // Spec 016 §2 proposes this one as `NaturalMethod`. The upsert is real and
    // is recorded; the class is refused because the provider chose a `POST`.
    assert!(
        operation("company.create_or_update")
            .effect()
            .and_then(donat_connectors::sdk::Effect::inventory_reason)
            .is_some_and(|reason| reason
                .contains("looked up via `company_id` in a `POST` request")
                && reason.contains("PUT and DELETE only")),
        "the upsert records both the documented semantics and the reason the gate refuses them"
    );
}

/// `intercom_output_contract`: the declared pointers read Intercom's own
/// objects, and a timestamp is the integer Intercom publishes rather than a
/// string.
#[test]
fn intercom_output_contract() {
    let get = operation("contact.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&contact()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": CONTACT_ID, "type": "contact", "email": "joe@example.com",
            "name": "Joe", "role": "user", "external_id": "user-1",
            "created_at": 1_571_672_154_i64, "updated_at": 1_571_672_154_i64,
        })
    );
    // "created_at (integer)" — a string there is a contract violation, not a
    // coercion.
    assert_eq!(
        get.decode_response(
            200,
            br#"{"type":"contact","id":"c1","created_at":"2019-10-21T16:55:54Z"}"#,
        )
        .expect_err("a mistyped pointer is a validation failure")
        .class(),
        ConnectorErrorClass::Validation
    );
    // Intercom's description marks no property required, so only the identity is
    // demanded; everything else may be absent.
    assert_eq!(
        get.decode_response(200, br#"{"type":"contact","id":"c1"}"#)
            .expect("only the identity is required"),
        json!({
            "id": "c1", "type": "contact", "email": null, "name": null,
            "role": null, "external_id": null, "created_at": null, "updated_at": null,
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"type":"contact"}"#)
            .expect_err("an object with no id is not a contact")
            .class(),
        ConnectorErrorClass::Validation
    );
}
