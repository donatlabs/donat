//! Cloudflare connector proofs (spec 027 §3, which adopts spec 023 §4), against
//! the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::cloudflare;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const ZONE_ID: &str = "023e105f4ecef8ad9ca31a8372d0c353";
const RECORD_ID: &str = "023e105f4ecef8ad9ca31a8372d0c354";
const ACCOUNT_ID: &str = "023e105f4ecef8ad9ca31a8372d0c355";

fn operation(id: &str) -> &'static Operation {
    cloudflare::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the cloudflare declaration publishes {id}"))
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

/// Every response is the same envelope.
fn envelope(result: JsonValue) -> JsonValue {
    json!({ "errors": [], "messages": [], "success": true, "result": result })
}

fn zone() -> JsonValue {
    json!({ "id": ZONE_ID, "name": "example.test", "status": "active",
            "type": "full", "paused": false, "created_on": "2026-08-01T09:00:00.000000Z" })
}

fn record() -> JsonValue {
    json!({ "id": RECORD_ID, "name": "www.example.test", "type": "A",
            "content": "198.51.100.4", "ttl": 3600, "proxied": true,
            "modified_on": "2026-08-10T09:00:00.000000Z" })
}

fn record_input() -> JsonValue {
    json!({ "zone_id": ZONE_ID, "dns_record_id": RECORD_ID, "type": "A",
            "name": "www.example.test", "content": "198.51.100.4", "ttl": 3600,
            "proxied": true, "comment": null })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("zone.get", json!({ "zone_id": ZONE_ID })),
        ("zone.list", json!({})),
        (
            "zone.create",
            json!({ "name": "example.test", "account": ACCOUNT_ID, "type": "full" }),
        ),
        (
            "zone.update",
            json!({ "zone_id": ZONE_ID, "paused": true, "type": null }),
        ),
        (
            "dns_record.get",
            json!({ "zone_id": ZONE_ID, "dns_record_id": RECORD_ID }),
        ),
        ("dns_record.list", json!({ "zone_id": ZONE_ID })),
        (
            "dns_record.create",
            json!({ "zone_id": ZONE_ID, "type": "A", "name": "www.example.test",
                    "content": "198.51.100.4", "ttl": 3600, "proxied": true,
                    "comment": null }),
        ),
        ("dns_record.update", record_input()),
    ]
}

/// `cloudflare_request_shape`: exact method, path, query, headers and body for
/// every operation, all under the published `/client/v4` server path.
#[tokio::test]
async fn cloudflare_request_shape() {
    let zones = "/client/v4/zones";
    let records = format!("/client/v4/zones/{ZONE_ID}/dns_records");
    let body = json!({ "type": "A", "name": "www.example.test", "content": "198.51.100.4",
                       "ttl": 3600, "proxied": true, "comment": null });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("{zones}/{ZONE_ID}"))
            .query("")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, envelope(zone())),
        Expectation::new("GET", zones)
            .query("")
            .respond_json(200, envelope(json!([zone()]))),
        Expectation::new("POST", zones)
            .json_body(
                json!({ "name": "example.test", "account": { "id": ACCOUNT_ID },
                               "type": "full" }),
            )
            .respond_json(200, envelope(zone())),
        Expectation::new("PATCH", &format!("{zones}/{ZONE_ID}"))
            .json_body(json!({ "paused": true, "type": null }))
            .respond_json(200, envelope(zone())),
        Expectation::new("GET", &format!("{records}/{RECORD_ID}"))
            .query("")
            .respond_json(200, envelope(record())),
        Expectation::new("GET", &records)
            .query("")
            .respond_json(200, envelope(json!([record()]))),
        Expectation::new("POST", &records)
            .json_body(body.clone())
            .respond_json(200, envelope(record())),
        Expectation::new("PUT", &format!("{records}/{RECORD_ID}"))
            .json_body(body)
            .respond_json(200, envelope(record())),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/client/v4/"),
            "{id} renders the published server path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `cloudflare_auth_is_applied`: the API token reaches the wire as the `Bearer`
/// header Cloudflare's `api_token` scheme declares.
#[tokio::test]
async fn cloudflare_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &format!("/client/v4/zones/{ZONE_ID}"))
                .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
                .respond_json(200, envelope(zone())),
        ])
        .await;

    let request = render(&stub, "zone.get", json!({ "zone_id": ZONE_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(
        applied.to_str().expect("an ASCII header"),
        format!("Bearer {SECRET_SENTINEL}")
    );
    // Cloudflare also publishes `X-Auth-Email`/`X-Auth-Key` for its legacy
    // global key; this connector declares the token scheme and only that one.
    assert!(request.headers().get("x-auth-key").is_none());
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        cloudflare::connector().credential(),
        cloudflare::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `cloudflare_error_map`: every documented status reaches exactly one closed
/// class, and none of Cloudflare's prose crosses the boundary.
#[tokio::test]
async fn cloudflare_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/client/v4/zones/{ZONE_ID}"),
        )
        .respond_json(
            status,
            json!({
                "success": false,
                "errors": [{ "code": 1061,
                             "message": format!("api.cloudflare.com rejected {SECRET_SENTINEL}") }],
                "messages": [],
                "result": null,
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "zone.get", json!({ "zone_id": ZONE_ID })))
            .await
            .expect("the stub answers");

        let failure = cloudflare::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "api.cloudflare.com"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// The Slack precedent (spec 023 §4 proof 4): Cloudflare's success schema
/// constrains `success` to `true`, so a `200` whose envelope says otherwise is a
/// failure — and it must never read as success, on a single request or on a page
/// of a walk.
#[tokio::test]
async fn cloudflare_a_success_envelope_can_carry_a_failure() {
    let failure_body = json!({
        "success": false,
        "errors": [{ "code": 81044, "message": "Record does not exist." }],
        "messages": [],
        "result": null,
    });

    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/client/v4/zones/{ZONE_ID}"))
            .respond_json(200, failure_body.clone()),
        Expectation::new("GET", &format!("/client/v4/zones/{ZONE_ID}"))
            .respond_bytes(200, b"<html>not cloudflare</html>".to_vec()),
    ])
    .await;

    let response = stub
        .send(render(&stub, "zone.get", json!({ "zone_id": ZONE_ID })))
        .await
        .expect("the stub answers");
    let failure = cloudflare::decode(
        operation("zone.get"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect_err("a 200 carrying success:false is not a success");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    assert!(!failure.diagnostic().contains("Record does not exist"));

    // A body that is not an envelope at all is outside the declared contract
    // rather than a classified provider failure.
    let response = stub
        .send(render(&stub, "zone.get", json!({ "zone_id": ZONE_ID })))
        .await
        .expect("the stub answers");
    let failure = cloudflare::decode(
        operation("zone.get"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect_err("a body with no envelope is not a success either");
    assert_eq!(failure.class(), ConnectorErrorClass::Invariant);
    stub.assert_satisfied();

    // And the same gate is the page gate: a failing page ends the walk instead
    // of contributing an empty item list.
    let walk_stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/client/v4/zones/{ZONE_ID}/dns_records"),
    )
    .respond_json(200, failure_body)])
    .await;
    let failure = cloudflare::pagination("dns_record.list")
        .expect("the list declares a plan")
        .collect(
            render(&walk_stub, "dns_record.list", json!({ "zone_id": ZONE_ID })),
            &walk_stub.origin(),
            &PaginationBudget::new(4, 4, 100, 1 << 20, Duration::from_secs(5)),
            |status, headers, body| {
                cloudflare::decode(operation("dns_record.list"), status, headers, body).map(|_| ())
            },
            |request| walk_stub.send(request),
        )
        .await
        .expect_err("a failing page fails the walk");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    walk_stub.assert_satisfied();
}

/// `cloudflare_rate_limit_is_classified`: Cloudflare's documented `429` is
/// retryable and its hint is clamped.
#[tokio::test]
async fn cloudflare_rate_limit_is_classified() {
    let path = format!("/client/v4/zones/{ZONE_ID}");
    let stub = ProviderStub::start([
        Expectation::new("GET", &path).respond_json(429, json!({ "success": false })),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(429, json!({ "success": false })),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "zone.get", json!({ "zone_id": ZONE_ID })))
            .await
            .expect("the stub answers");
        failures.push(cloudflare::error_map().classify_response(&response));
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

/// `cloudflare_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the page
/// number is derived from the walk rather than from `result_info`, and the walk
/// makes exactly the number of requests the plan declares (ADR 058).
#[tokio::test]
async fn cloudflare_cursor_is_opaque_and_bounded() {
    let plan = cloudflare::pagination("dns_record.list").expect("the list declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    let path = format!("/client/v4/zones/{ZONE_ID}/dns_records");
    let full: Vec<JsonValue> = (0..100).map(|_| record()).collect();

    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .query("page=1&per_page=100")
            .respond_json(
                200,
                json!({ "success": true, "errors": [], "messages": [], "result": full,
                        // The provider's own page count is deliberately wrong
                        // here: no plan reads it, so the walk cannot be
                        // restarted or rewound by one.
                        "result_info": { "page": 1, "per_page": 100, "total_pages": 99 } }),
            ),
        Expectation::new("GET", &path)
            .query("page=2&per_page=100")
            .respond_json(
                200,
                json!({ "success": true, "errors": [], "messages": [],
                        "result": [record()],
                        "result_info": { "page": 2, "per_page": 100, "total_pages": 99 } }),
            ),
    ])
    .await;

    let records = plan
        .collect(
            render(&stub, "dns_record.list", json!({ "zone_id": ZONE_ID })),
            &stub.origin(),
            &budget,
            |status, headers, body| {
                cloudflare::decode(operation("dns_record.list"), status, headers, body).map(|_| ())
            },
            |request| stub.send(request),
        )
        .await
        .expect("the walk advances the page and stops on a short one");
    assert_eq!(records.len(), 101);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `cloudflare_pagination_is_bounded`: the declared plans terminate and respect
/// the call, page, item and byte budgets, and only the two collections declare
/// one.
#[tokio::test]
async fn cloudflare_pagination_is_bounded() {
    let plan = cloudflare::pagination("dns_record.list").expect("the list declares a plan");
    let path = format!("/client/v4/zones/{ZONE_ID}/dns_records");
    let full: Vec<JsonValue> = (0..100).map(|_| record()).collect();
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &path).respond_json(
                200,
                json!({ "success": true, "errors": [], "messages": [], "result": full }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "dns_record.list", json!({ "zone_id": ZONE_ID })),
                &stub.origin(),
                &budget,
                |status, headers, body| {
                    cloudflare::decode(operation("dns_record.list"), status, headers, body)
                        .map(|_| ())
                },
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    for id in ["zone.list", "dns_record.list"] {
        assert_eq!(
            cloudflare::pagination(id)
                .expect("the collection declares a plan")
                .items_pointer(),
            "/result",
            "{id} collects the envelope's result"
        );
    }
    for id in [
        "zone.get",
        "zone.create",
        "zone.update",
        "dns_record.get",
        "dns_record.create",
        "dns_record.update",
    ] {
        assert!(
            cloudflare::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `cloudflare_effects_are_classified`: every operation carries a class, the
/// `PUT` Cloudflare publishes as "Overwrite" is the batch's one `NaturalMethod`,
/// and two identical sends leave one record (spec 010 §7).
#[tokio::test]
async fn cloudflare_effects_are_classified() {
    let expected = [
        ("zone.get", EffectClass::ReadOnly),
        ("zone.list", EffectClass::ReadOnly),
        ("zone.create", EffectClass::AtMostOnce),
        ("zone.update", EffectClass::InventoryOnly),
        ("dns_record.get", EffectClass::ReadOnly),
        ("dns_record.list", EffectClass::ReadOnly),
        ("dns_record.create", EffectClass::AtMostOnce),
        (
            "dns_record.update",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
    ];
    assert_eq!(cloudflare::connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            cloudflare::connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        // No operation binds a key: `NaturalMethod` needs none and `AtMostOnce`
        // may not carry one.
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        cloudflare::connector().admit_operation("zone.update"),
        Err(OperationRejection::InventoryOnly)
    );
    // The `NaturalMethod` never needs the at-most-once opt-in, and the two
    // at-most-once creates always do.
    assert!(!EffectClass::ProviderIdempotentNaturalMethod.requires_at_most_once_opt_in());
    for id in ["zone.create", "dns_record.create"] {
        assert!(
            operation(id)
                .effect_class()
                .expect("a class")
                .requires_at_most_once_opt_in(),
            "{id}"
        );
    }

    // Spec 010 §7: a `NaturalMethod` needs a test proving two identical sends
    // leave one resource. Cloudflare answers the same record id both times, and
    // the declared contract reads the same identity out of both.
    let stub = ProviderStub::start((0..2).map(|_| {
        Expectation::new(
            "PUT",
            &format!("/client/v4/zones/{ZONE_ID}/dns_records/{RECORD_ID}"),
        )
        .json_body(json!({ "type": "A", "name": "www.example.test",
                           "content": "198.51.100.4", "ttl": 3600, "proxied": true,
                           "comment": null }))
        .respond_json(200, envelope(record()))
    }))
    .await;
    let mut outcomes = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "dns_record.update", record_input()))
            .await
            .expect("the stub answers");
        outcomes.push(
            cloudflare::decode(
                operation("dns_record.update"),
                response.status.as_u16(),
                response.headers(),
                response.body(),
            )
            .expect("the declared contract is satisfied"),
        );
    }
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0].get("id"), Some(&json!(RECORD_ID)));
    assert_eq!(stub.received(), 2, "both sends went to the provider");
    stub.assert_satisfied();
}

/// `cloudflare_triggering_is_not_a_read` (spec 027 §3): creating and overwriting
/// a DNS record changes where a name's traffic goes, and creating a zone changes
/// an account. None of them is a read.
#[test]
fn cloudflare_triggering_is_not_a_read() {
    for id in [
        "zone.create",
        "zone.update",
        "dns_record.create",
        "dns_record.update",
    ] {
        let class = operation(id)
            .effect_class()
            .expect("every operation has one");
        assert_ne!(class, EffectClass::ReadOnly, "{id} changes DNS");
    }
    // And the one that *is* executable without the at-most-once opt-in earned it
    // with the provider's own replacement statement rather than with its method.
    let citation = format!("{:?}", operation("dns_record.update").effect());
    assert!(
        citation.contains("Overwrite an existing DNS record"),
        "{citation}"
    );
    assert!(citation.contains("Update DNS Record"), "{citation}");
}

/// `cloudflare_output_contract`: the declared pointers read Cloudflare's own
/// envelope, and a mistyped field is a validation failure rather than a
/// coercion.
#[test]
fn cloudflare_output_contract() {
    let get = operation("dns_record.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&envelope(record())).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": RECORD_ID, "name": "www.example.test", "type": "A",
            "content": "198.51.100.4", "ttl": 3600, "proxied": true,
            "modified_on": "2026-08-10T09:00:00.000000Z",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"success":true,"result":{"id":42}}"#)
            .expect_err("an id that is not a string is not a record")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"success":true,"result":null}"#)
            .expect_err("an envelope with no result does not satisfy the contract")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(
            200,
            format!(r#"{{"success":true,"result":{{"id":"{RECORD_ID}"}}}}"#).as_bytes()
        )
        .expect("only the identity is required")
        .get("ttl"),
        Some(&json!(null))
    );
}
