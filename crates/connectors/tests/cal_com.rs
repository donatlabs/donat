//! Cal.com connector proofs (spec 028 §4, which adopts spec 023 §4), against
//! the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::cal_com;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const EVENT_TYPE_ID: i64 = 50;
const BOOKING_UID: &str = "nCn6QqbAsPqZDzWCVAtsdE";

fn operation(id: &str) -> &'static Operation {
    cal_com::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the cal_com declaration publishes {id}"))
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

/// Cal.com's own published booking sample, trimmed to the fields declared.
fn booking() -> JsonValue {
    json!({ "id": 100, "uid": BOOKING_UID, "title": "Strategy Session",
            "status": "accepted", "start": "2024-08-13T15:30:00Z",
            "end": "2024-08-13T16:30:00Z", "duration": 60,
            "eventType": { "id": EVENT_TYPE_ID, "slug": "some-event" } })
}

fn event_type() -> JsonValue {
    json!({ "id": EVENT_TYPE_ID, "slug": "15min", "title": "15 min meeting",
            "lengthInMinutes": 15 })
}

/// The success envelope every v2 response carries.
fn success(data: JsonValue) -> JsonValue {
    json!({ "status": "success", "data": data })
}

/// The booking collection's envelope, with its published cursor half.
fn bookings_page(data: JsonValue, next: Option<&str>) -> JsonValue {
    json!({ "status": "success", "data": data,
            "pagination": { "nextCursor": next, "hasMore": next.is_some() } })
}

fn create_input() -> JsonValue {
    json!({
        "event_type_id": EVENT_TYPE_ID,
        "start": "2024-08-13T15:30:00Z",
        "attendee_name": "John Doe",
        "attendee_email": "john@example.test",
        "attendee_time_zone": "America/New_York",
        "booking_fields_responses": { "notes": "Discussing the integration" },
    })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("event_type.list", json!({})),
        (
            "event_type.get",
            json!({ "event_type_id": EVENT_TYPE_ID.to_string() }),
        ),
        ("booking.list", json!({})),
        ("booking.get", json!({ "booking_uid": BOOKING_UID })),
        ("booking.create", create_input()),
        (
            "booking.cancel",
            json!({ "booking_uid": BOOKING_UID, "cancellation_reason": "Will travel" }),
        ),
    ]
}

/// `cal_com_request_shape`: exact method, path, query, headers and body for
/// every operation, each under the `cal-api-version` its own reference names.
#[tokio::test]
async fn cal_com_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v2/event-types")
            .query("")
            .header("cal-api-version", "2024-06-14")
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .no_body()
            .respond_json(200, success(json!([event_type()]))),
        Expectation::new("GET", &format!("/v2/event-types/{EVENT_TYPE_ID}"))
            .query("")
            .header("cal-api-version", "2024-06-14")
            .respond_json(200, success(event_type())),
        Expectation::new("GET", "/v2/bookings")
            .query("")
            .header("cal-api-version", "2026-05-01")
            .no_body()
            .respond_json(200, bookings_page(json!([booking()]), None)),
        Expectation::new("GET", &format!("/v2/bookings/{BOOKING_UID}"))
            .query("")
            .header("cal-api-version", "2026-02-25")
            .respond_json(200, success(booking())),
        Expectation::new("POST", "/v2/bookings")
            .header("cal-api-version", "2026-02-25")
            .json_body(json!({
                "eventTypeId": EVENT_TYPE_ID,
                "start": "2024-08-13T15:30:00Z",
                "attendee": { "name": "John Doe", "email": "john@example.test",
                              "timeZone": "America/New_York" },
                "bookingFieldsResponses": { "notes": "Discussing the integration" },
            }))
            .respond_json(201, success(booking())),
        Expectation::new("POST", &format!("/v2/bookings/{BOOKING_UID}/cancel"))
            .header("cal-api-version", "2026-02-25")
            .json_body(json!({ "cancellationReason": "Will travel" }))
            .respond_json(200, success(booking())),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/v2/"),
            "{id} renders the published base path: {}",
            request.url().path()
        );
        assert!(
            request.headers().contains_key("cal-api-version"),
            "{id} pins the version its own reference names — omitting it is a documented 404"
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `cal_com_version_header_is_per_operation`: the three values Cal.com's OpenAPI
/// publishes are three values, and each operation carries its own.
#[test]
fn cal_com_version_header_is_per_operation() {
    let pinned = |id: &str| {
        let projection = format!("{:?}", operation(id).project().headers());
        for candidate in ["2026-05-01", "2026-02-25", "2024-06-14"] {
            if projection.contains(candidate) {
                return candidate;
            }
        }
        panic!("{id} declares no cal-api-version: {projection}");
    };
    assert_eq!(pinned("booking.list"), "2026-05-01");
    for id in ["booking.get", "booking.create", "booking.cancel"] {
        assert_eq!(pinned(id), "2026-02-25", "{id}");
    }
    for id in ["event_type.list", "event_type.get"] {
        assert_eq!(pinned(id), "2024-06-14", "{id}");
    }
    // The version is a static of the declaration, never an input a Process could
    // move to an endpoint version this connector was not written against.
    for operation in cal_com::connector().operations() {
        assert!(
            !operation
                .project()
                .inputs()
                .iter()
                .any(|input| input.name().contains("version")),
            "{} publishes no version input",
            operation.id()
        );
    }
}

/// `cal_com_auth_is_applied`: the API key reaches the wire as the bearer header
/// v2 publishes, and never as the query parameter the decommissioned v1 used.
#[tokio::test]
async fn cal_com_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/v2/bookings")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, bookings_page(json!([]), None))])
    .await;

    let request = render(&stub, "booking.list", json!({}));
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

    // v1 put the key in `?apiKey=`. Nothing in this connector's compiled request
    // contract has a slot it could reach: no operation declares such an input,
    // and none of the declared queries names one.
    for operation in cal_com::connector().operations() {
        let projection = operation.project();
        let surface =
            format!("{:?} {:?}", projection.inputs(), projection.query()).to_ascii_lowercase();
        assert!(
            !surface.contains("apikey"),
            "{} publishes no credential-shaped slot",
            operation.id()
        );
    }

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        cal_com::connector().credential(),
        cal_com::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `cal_com_secret_and_non_secret_are_separated` (spec 028 §3): the API key is
/// this connector's whole secret contract, and the values that are *not* secret
/// — the origin and the three API versions — are compiled constants a
/// deployment cannot get wrong and a diagnostic may print.
#[tokio::test]
async fn cal_com_secret_and_non_secret_are_separated() {
    assert_eq!(
        cal_com::connector()
            .credential()
            .fields()
            .iter()
            .map(|field| (field.name(), field.is_secret()))
            .collect::<Vec<_>>(),
        [("secret", true)]
    );
    assert_eq!(cal_com::connector().origin().host_variable(), None);
    let declaration = format!("{:?}", cal_com::connector());
    assert!(declaration.contains("api.cal.com"));
    assert!(declaration.contains("2026-02-25"));
    assert!(!declaration.contains(SECRET_SENTINEL));

    let stub = ProviderStub::start([Expectation::new("GET", "/v2/bookings").respond_json(
        401,
        json!({ "status": "error",
                "error": { "code": "UNAUTHORIZED",
                           "message": format!("token {SECRET_SENTINEL} is invalid") } }),
    )])
    .await;
    let request = render(&stub, "booking.list", json!({}));
    let redacted = request.redacted_url();
    let debug = format!("{request:?}");
    let response = stub.send(request).await.expect("the stub answers");
    let failure = cal_com::error_map().classify_response(&response);
    let decoded = cal_com::decode(
        operation("booking.list"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect_err("a 401 is not a success");

    for surface in [
        redacted,
        debug,
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

/// `cal_com_error_map`: every documented status reaches exactly one closed
/// class, and none of Cal.com's prose crosses the boundary.
#[tokio::test]
async fn cal_com_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", "/v2/bookings").respond_json(
            status,
            json!({ "status": "error",
                    "error": { "code": "NOT_FOUND",
                               "message": format!("api.cal.com rejected {SECRET_SENTINEL}") } }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "booking.list", json!({})))
            .await
            .expect("the stub answers");

        let failure = cal_com::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "api.cal.com", "NOT_FOUND"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `cal_com_rate_limit_is_classified`: "Exceeding the limit returns a 429
/// response", with the documented "`Retry-After` header carrying the number of
/// seconds to wait before retrying", clamped.
#[tokio::test]
async fn cal_com_rate_limit_is_classified() {
    let limited = json!({ "status": "error",
                          "error": { "code": "TOO_MANY_REQUESTS",
                                     "message": "Rate limit exceeded" } });
    let stub = ProviderStub::start([
        Expectation::new("GET", "/v2/bookings")
            .respond_header("retry-after", "60")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", "/v2/bookings")
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "booking.list", json!({})))
            .await
            .expect("the stub answers");
        failures.push(cal_com::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(60)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `cal_com_failure_inside_a_200_never_reads_as_success` (spec 023 §4 proof 4):
/// the envelope gate sits between the status check and the declared pointers.
#[test]
fn cal_com_failure_inside_a_200_never_reads_as_success() {
    let get = operation("booking.get");
    let headers = reqwest::header::HeaderMap::new();

    let reported =
        br#"{"status":"error","data":{"uid":"x"},"error":{"code":"NOT_FOUND","message":"no"}}"#;
    assert!(
        get.decode_response(200, reported).is_ok(),
        "the declared pointers alone would have read this as a booking"
    );
    assert_eq!(
        cal_com::decode(get, 200, &headers, reported)
            .expect_err("an error envelope is a failure whatever the status was")
            .class(),
        ConnectorErrorClass::Permanent
    );

    assert_eq!(
        cal_com::decode(get, 200, &headers, br#"{"data":{"uid":"x"}}"#)
            .expect_err("an envelope with no status is outside the contract")
            .class(),
        ConnectorErrorClass::Invariant
    );
    assert_eq!(
        cal_com::decode(get, 200, &headers, b"not json")
            .expect_err("a non-JSON body is outside the contract")
            .class(),
        ConnectorErrorClass::Invariant
    );
    assert!(
        cal_com::decode(
            get,
            200,
            &headers,
            &serde_json::to_vec(&success(booking())).expect("a fixture serializes")
        )
        .is_ok()
    );
}

/// `cal_com_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the walk spends
/// the provider's own opaque cursor as a query value, ends when the provider
/// stops publishing one, and makes exactly the number of requests the plan
/// declares (ADR 058).
#[tokio::test]
async fn cal_com_cursor_is_opaque_and_bounded() {
    let plan = cal_com::pagination("booking.list").expect("the collection declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 1 << 20, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/v2/bookings")
            .query("limit=100")
            .respond_json(
                200,
                // A cursor that spells a whole other origin is still only ever a
                // query value: `Cursor` cannot make one a destination.
                bookings_page(json!([booking()]), Some("https://attacker.invalid/x")),
            ),
        Expectation::new("GET", "/v2/bookings")
            .query("limit=100&cursor=https%3A%2F%2Fattacker%2Einvalid%2Fx")
            .respond_json(200, bookings_page(json!([booking(), booking()]), None)),
    ])
    .await;

    let items = plan
        .collect(
            render(&stub, "booking.list", json!({})),
            &stub.origin(),
            &budget,
            |status, headers, body| {
                cal_com::decode(operation("booking.list"), status, headers, body).map(|_| ())
            },
            |request| stub.send(request),
        )
        .await
        .expect("the walk spends the cursor and stops when nextCursor is null");
    assert_eq!(items.len(), 3);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    for recorded in stub.recorded() {
        assert!(
            recorded.path.starts_with("/v2/bookings"),
            "every page stays on the compiled origin and path: {}",
            recorded.path
        );
    }
    stub.assert_satisfied();
}

/// `cal_com_pagination_is_bounded`: the declared plan terminates under every
/// budget, only the one collection that publishes a cursor declares a plan, and
/// no page size binds from input.
#[tokio::test]
async fn cal_com_pagination_is_bounded() {
    for id in [
        "event_type.list",
        "event_type.get",
        "booking.get",
        "booking.create",
        "booking.cancel",
    ] {
        assert!(
            cal_com::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    assert_eq!(
        cal_com::pagination("booking.list")
            .expect("the collection declares a plan")
            .items_pointer(),
        "/data"
    );

    let plan = cal_com::pagination("booking.list").expect("the collection declares a plan");
    let full: Vec<JsonValue> = (0..100).map(|_| booking()).collect();
    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 22, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 22, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 22, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", "/v2/bookings").respond_json(
                200,
                bookings_page(JsonValue::Array(full.clone()), Some("next")),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "booking.list", json!({})),
                &stub.origin(),
                &budget,
                |status, headers, body| {
                    cal_com::decode(operation("booking.list"), status, headers, body).map(|_| ())
                },
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget", "{budget:?}");
    }

    let queries: Vec<(String, String)> = cal_com::connector()
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
        !queries.iter().any(|(_, query)| query.contains("Input")),
        "no page size binds from input: {queries:?}"
    );
}

/// `cal_com_effects_are_classified`: the create is the batch's one at-most-once
/// write and carries the consequence an operator accepts; the cancel stays
/// unreachable because Cal.com publishes no consequence for a repeat.
#[test]
fn cal_com_effects_are_classified() {
    let expected = [
        ("event_type.list", EffectClass::ReadOnly),
        ("event_type.get", EffectClass::ReadOnly),
        ("booking.list", EffectClass::ReadOnly),
        ("booking.get", EffectClass::ReadOnly),
        ("booking.create", EffectClass::AtMostOnce),
        ("booking.cancel", EffectClass::InventoryOnly),
    ];
    assert_eq!(cal_com::connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            cal_com::connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        cal_com::connector().admit_operation("booking.cancel"),
        Err(OperationRejection::InventoryOnly)
    );
    assert!(
        operation("booking.create")
            .effect_class()
            .expect("a class")
            .requires_at_most_once_opt_in()
    );

    let evidence = operation("booking.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(
        evidence
            .searched_documentation()
            .contains("/v2/credits/charge"),
        "the search names the endpoint the mechanism is on: {}",
        evidence.searched_documentation()
    );
    assert!(
        evidence
            .searched_documentation()
            .contains("occurs exactly twice")
    );
    assert!(evidence.repeat_produces().contains("second booking"));

    let reason = operation("booking.cancel")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("NaturalMethod"), "{reason}");
    assert!(reason.contains("AtMostOnce"), "{reason}");
}

/// `cal_com_output_contract`: the declared pointers read Cal.com's own envelope,
/// with its own typing.
#[test]
fn cal_com_output_contract() {
    let get = operation("booking.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&success(booking())).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({ "status": "success", "data": booking() })
    );
    assert_eq!(
        get.decode_response(200, br#"{"status":"success"}"#)
            .expect_err("an envelope with no data is not a booking")
            .class(),
        ConnectorErrorClass::Validation
    );

    let list = operation("booking.list");
    assert_eq!(
        list.decode_response(
            200,
            &serde_json::to_vec(&bookings_page(json!([booking()]), Some("abc")))
                .expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({ "status": "success", "data": [booking()],
                "next_cursor": "abc", "has_more": true })
    );
    assert_eq!(
        list.decode_response(
            200,
            &serde_json::to_vec(&bookings_page(json!([]), None)).expect("a fixture serializes")
        )
        .expect("the last page publishes neither half of the continuation")
        .get("next_cursor"),
        Some(&json!(null))
    );
}
