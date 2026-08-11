//! Acuity Scheduling connector proofs (spec 028 §4, which adopts spec 023 §4),
//! against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::acuity;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const APPOINTMENT_ID: i64 = 1;
const APPOINTMENT_TYPE_ID: i64 = 1;

/// The deployment's non-secret half: Acuity's numeric User ID, which is the
/// HTTP Basic *username*. It is deliberately distinguishable from the secret
/// sentinel so a leak test cannot pass by accident.
const USER_ID: &str = "11145481";

fn declaration() -> &'static Connector {
    static DECLARATION: std::sync::LazyLock<Connector> = std::sync::LazyLock::new(|| {
        acuity::connector(USER_ID).expect("a numeric user id declares")
    });
    &DECLARATION
}

fn operation(id: &str) -> &'static Operation {
    declaration()
        .operation(id)
        .unwrap_or_else(|| panic!("the acuity declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::basic(USER_ID)
        .expect("a numeric username is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

/// The `Authorization` value Acuity's own example produces:
/// `curl -u ACUITY_USER_ID:ACUITY_API_KEY`.
fn basic_header() -> String {
    use base64::Engine as _;
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{USER_ID}:{SECRET_SENTINEL}"))
    )
}

/// Acuity's own published appointment example, trimmed to the fields declared.
fn appointment() -> JsonValue {
    json!({
        "id": APPOINTMENT_ID, "firstName": "Bob", "lastName": "McTest",
        "email": "bob.mctest@example.com", "datetime": "2013-06-17T10:15:00-0700",
        "appointmentTypeID": APPOINTMENT_TYPE_ID, "calendarID": 1,
        "canceled": false, "noShow": false,
        "confirmationPage": "https://acuityscheduling.com/schedule.php?owner=11145481",
    })
}

fn appointment_type() -> JsonValue {
    json!({ "id": APPOINTMENT_TYPE_ID, "active": true, "name": "Regular Visit",
            "description": "", "duration": 60 })
}

fn create_input() -> JsonValue {
    json!({
        "datetime": "2016-02-03T14:00:00-0800",
        "appointment_type_id": APPOINTMENT_TYPE_ID,
        "first_name": "Bob",
        "last_name": "McTest",
        "email": "bob.mctest@example.com",
    })
}

fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("appointment_type.list", json!({})),
        (
            "availability.times",
            json!({ "date": "2016-02-04", "appointment_type_id": APPOINTMENT_TYPE_ID }),
        ),
        (
            "appointment.list",
            json!({ "min_date": "2026-08-01", "max_date": "2026-08-31" }),
        ),
        (
            "appointment.get",
            json!({ "appointment_id": APPOINTMENT_ID }),
        ),
        ("appointment.create", create_input()),
        (
            "appointment.cancel",
            json!({ "appointment_id": APPOINTMENT_ID, "cancel_note": "Will travel" }),
        ),
    ]
}

/// `acuity_request_shape`: exact method, path, query, headers and body for every
/// operation, all under the `/api/v1` server Acuity's own OpenAPI declares.
#[tokio::test]
async fn acuity_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/api/v1/appointment-types")
            .query("")
            .header("authorization", &basic_header())
            .no_body()
            .respond_json(200, json!([appointment_type()])),
        Expectation::new("GET", "/api/v1/availability/times")
            .query("date=2016%2D02%2D04&appointmentTypeID=1")
            .no_body()
            .respond_json(200, json!([{ "time": "2016-02-04T13:00:00-0800" }])),
        Expectation::new("GET", "/api/v1/appointments")
            .query("max=100&minDate=2026%2D08%2D01&maxDate=2026%2D08%2D31")
            .no_body()
            .respond_json(200, json!([appointment()])),
        Expectation::new("GET", &format!("/api/v1/appointments/{APPOINTMENT_ID}"))
            .query("")
            .respond_json(200, appointment()),
        Expectation::new("POST", "/api/v1/appointments")
            .json_body(json!({
                "datetime": "2016-02-03T14:00:00-0800",
                "appointmentTypeID": APPOINTMENT_TYPE_ID,
                "firstName": "Bob",
                "lastName": "McTest",
                "email": "bob.mctest@example.com",
            }))
            .respond_json(200, appointment()),
        Expectation::new(
            "PUT",
            &format!("/api/v1/appointments/{APPOINTMENT_ID}/cancel"),
        )
        .json_body(json!({ "cancelNote": "Will travel" }))
        .respond_json(200, appointment()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/api/v1/"),
            "{id} renders the published server path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `acuity_auth_is_applied`: the API key reaches the wire as the HTTP Basic
/// *password* behind the account's numeric User ID, exactly as Acuity's own
/// `curl -u ACUITY_USER_ID:ACUITY_API_KEY` example produces.
#[tokio::test]
async fn acuity_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/v1/appointments/{APPOINTMENT_ID}"),
    )
    .header("authorization", &basic_header())
    .respond_json(200, appointment())])
    .await;

    let request = render(
        &stub,
        "appointment.get",
        json!({ "appointment_id": APPOINTMENT_ID }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert_eq!(applied.to_str().expect("an ASCII header"), basic_header());
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    for operation in declaration().operations() {
        let projection = operation.project();
        let surface =
            format!("{:?} {:?}", projection.inputs(), projection.query()).to_ascii_lowercase();
        for shape in ["api_key", "apikey", "user_id"] {
            assert!(
                !surface.contains(shape),
                "{} publishes no credential-shaped slot ({shape})",
                operation.id()
            );
        }
    }

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        declaration().credential(),
        acuity::error_map().classify_response(&response)
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `acuity_secret_and_non_secret_are_separated` (spec 028 §3): the numeric User
/// ID is non-secret deploy-time configuration that may appear anywhere, and the
/// API key beside it appears in no log line, diagnostic, error or fingerprint.
#[tokio::test]
async fn acuity_secret_and_non_secret_are_separated() {
    // The declared credential contract names both halves and classifies them
    // apart, and it carries neither value.
    assert_eq!(
        declaration()
            .credential()
            .fields()
            .iter()
            .map(|field| (field.name(), field.is_secret()))
            .collect::<Vec<_>>(),
        [("secret", true), (acuity::USER_ID, false)]
    );

    // The non-secret half is compiled into the declaration and prints freely.
    let declaration_debug = format!("{:?}", declaration());
    assert!(declaration_debug.contains(USER_ID));
    assert!(!declaration_debug.contains(SECRET_SENTINEL));

    // A resolved credential missing the non-secret half is refused by name, and
    // the refusal carries no value at all.
    let missing = declaration()
        .credential()
        .admits(&Credential::secret(SECRET_SENTINEL))
        .expect_err("a declared field a deployment did not configure is refused");
    assert_eq!(missing.name(), acuity::USER_ID);
    assert!(!missing.to_string().contains(SECRET_SENTINEL));
    assert_eq!(
        declaration().credential().admits(&Credential::from_fields([
            (
                "secret",
                donat_connectors::sdk::Secret::new(SECRET_SENTINEL)
            ),
            (acuity::USER_ID, donat_connectors::sdk::Secret::new(USER_ID)),
        ])),
        Ok(())
    );

    // The secret half reaches no request debug, no diagnostic, and no error.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/api/v1/appointments/{APPOINTMENT_ID}"),
    )
    .respond_json(
        401,
        json!({ "status_code": 401,
                "message": format!("key {SECRET_SENTINEL} is not valid for user {USER_ID}"),
                "error": "unauthorized" }),
    )])
    .await;
    let request = render(
        &stub,
        "appointment.get",
        json!({ "appointment_id": APPOINTMENT_ID }),
    );
    let redacted = request.redacted_url();
    let debug = format!("{request:?}");
    let response = stub.send(request).await.expect("the stub answers");
    let failure = acuity::error_map().classify_response(&response);
    let decoded = acuity::decode(
        operation("appointment.get"),
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

/// `acuity_user_id_comes_only_from_deploy_time_configuration`: the Basic
/// username is Acuity's own numeric grammar, checked before a listener opens,
/// and no request may choose it.
#[test]
fn acuity_user_id_comes_only_from_deploy_time_configuration() {
    for valid in ["1", USER_ID, "99999999999999999999"] {
        assert!(acuity::validate_user_id(valid).is_ok(), "{valid}");
        assert!(acuity::connector(valid).is_ok(), "{valid}");
    }
    for hostile in [
        "",
        "acme",
        "1114 5481",
        "11145481:extra",
        "-1",
        "111454811114548111145481",
    ] {
        let refusal =
            acuity::validate_user_id(hostile).expect_err("a non-numeric user id is refused");
        assert!(refusal.message().contains("numeric User ID"), "{hostile}");
        assert!(acuity::connector(hostile).is_err(), "{hostile}");
    }

    // No operation publishes the username as an input, and nothing in a request
    // renders it into a path, a query or a body.
    for operation in declaration().operations() {
        assert!(
            !operation
                .project()
                .inputs()
                .iter()
                .any(|input| input.name() == acuity::USER_ID),
            "{} publishes no user id input",
            operation.id()
        );
    }
    let origin = declaration()
        .resolve_origin(&Default::default())
        .expect("a fixed origin resolves");
    let request = operation("appointment.get")
        .plan_request(&origin, &json!({ "appointment_id": APPOINTMENT_ID }))
        .expect("the declared request renders");
    assert!(!request.url().as_str().contains(USER_ID));
}

/// `acuity_error_map`: every documented status and every published `error` key
/// reaches exactly one closed class, and none of Acuity's prose crosses the
/// boundary.
#[tokio::test]
async fn acuity_error_map() {
    let documented = [
        (400, "bad_request", ConnectorErrorClass::Validation),
        (401, "unauthorized", ConnectorErrorClass::Authentication),
        (403, "forbidden", ConnectorErrorClass::Authentication),
        (404, "not_found", ConnectorErrorClass::Permanent),
        (405, "method_not_allowed", ConnectorErrorClass::Permanent),
        (422, "invalid_time", ConnectorErrorClass::Validation),
        (429, "too_many_requests", ConnectorErrorClass::Http429),
        (500, "internal_server_error", ConnectorErrorClass::Http5xx),
        (418, "unknown_teapot", ConnectorErrorClass::Permanent),
    ];

    for (status, error, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/api/v1/appointments/{APPOINTMENT_ID}"),
        )
        .respond_json(
            status,
            json!({ "status_code": status, "error": error,
                    "message": format!("acuityscheduling.com rejected {SECRET_SENTINEL}") }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "appointment.get",
                json!({ "appointment_id": APPOINTMENT_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = acuity::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} error {error}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acuityscheduling.com", error] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // The cancel's own two published errors are validation failures rather than
    // the fallback: "Clients are not allowed to cancel." and "Clients are not
    // allowed to cancel this close to the time of the appointment."
    for error in ["cancel_not_allowed", "cancel_too_close"] {
        let failure = acuity::error_map().classify(
            400,
            &reqwest::header::HeaderMap::new(),
            format!(r#"{{"status_code":400,"error":"{error}"}}"#).as_bytes(),
        );
        assert_eq!(failure.class(), ConnectorErrorClass::Validation, "{error}");
    }
}

/// `acuity_rate_limit_is_classified`: "Our API is currently rate limited to 10
/// requests a second and 20 concurrent connections from an IP", answered with
/// `429`; Acuity publishes no `Retry-After`, so one only ever arrives if it
/// sends the standard header — and it is clamped when it does.
#[tokio::test]
async fn acuity_rate_limit_is_classified() {
    let path = format!("/api/v1/appointments/{APPOINTMENT_ID}");
    let limited = json!({ "status_code": 429, "error": "too_many_requests",
                          "message": "Rate limit reached.  Limit 10req/s and 20 concurrent connections." });
    let stub = ProviderStub::start([
        Expectation::new("GET", &path).respond_json(429, limited.clone()),
        Expectation::new("GET", &path)
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(
                &stub,
                "appointment.get",
                json!({ "appointment_id": APPOINTMENT_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(acuity::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "no delay is invented where the provider publishes none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `acuity_pagination_is_bounded`: Acuity publishes no continuation of any kind,
/// so no operation declares a plan, one attempt is one request, and the one
/// collection that can grow is bounded by the provider's own date window.
#[tokio::test]
async fn acuity_pagination_is_bounded() {
    for operation in declaration().operations() {
        assert!(
            acuity::pagination(operation.id()).is_none(),
            "{} declares no continuation plan",
            operation.id()
        );
    }

    let stub = ProviderStub::start([Expectation::new("GET", "/api/v1/appointments")
        .query("max=100&minDate=2026%2D08%2D01&maxDate=2026%2D08%2D31")
        .respond_json(200, json!([appointment(), appointment()]))])
    .await;
    let response = stub
        .send(render(
            &stub,
            "appointment.list",
            json!({ "min_date": "2026-08-01", "max_date": "2026-08-31" }),
        ))
        .await
        .expect("the stub answers");
    let output = acuity::decode(
        operation("appointment.list"),
        response.status.as_u16(),
        response.headers(),
        response.body(),
    )
    .expect("the declared contract is satisfied");
    assert_eq!(
        output.as_array().map(Vec::len),
        Some(2),
        "the bare array is the whole output"
    );
    assert_eq!(
        stub.received(),
        1,
        "one attempt is one request: this provider publishes nothing to follow"
    );
    stub.assert_satisfied();

    // The window is mandatory, so a Process cannot ask for an unbounded page by
    // omitting it, and the page size is a static rather than an input.
    assert!(
        operation("appointment.list")
            .plan_request(&stub.origin(), &json!({}))
            .is_err(),
        "the date window is part of the declared contract"
    );
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
            .any(|(id, query)| id == "appointment.list" && query.contains("100")),
        "the appointment list asks for the provider's own default maximum: {queries:?}"
    );
    assert!(
        !queries.iter().any(|(id, query)| id == "appointment.list"
            && query.contains("Input")
            && query.contains("max\"")),
        "no page size binds from input: {queries:?}"
    );
}

/// `acuity_effects_are_classified`: the create is the batch's at-most-once
/// write; the cancel is a `PUT` against a fixed identity that stays unreachable
/// because the method is not the evidence.
#[test]
fn acuity_effects_are_classified() {
    let expected = [
        ("appointment_type.list", EffectClass::ReadOnly),
        ("availability.times", EffectClass::ReadOnly),
        ("appointment.list", EffectClass::ReadOnly),
        ("appointment.get", EffectClass::ReadOnly),
        ("appointment.create", EffectClass::AtMostOnce),
        ("appointment.cancel", EffectClass::InventoryOnly),
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
        declaration().admit_operation("appointment.cancel"),
        Err(OperationRejection::InventoryOnly)
    );

    let evidence = operation("appointment.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(
        evidence.searched_documentation().contains("OpenAPI 3.1"),
        "{}",
        evidence.searched_documentation()
    );
    assert!(evidence.repeat_produces().contains("second appointment"));

    // The cancel is method-eligible for `NaturalMethod` and still refused: the
    // reason has to say that the method is not the evidence.
    assert_eq!(
        operation("appointment.cancel").method().as_str(),
        "PUT",
        "the cancel really is the method the gate would admit on evidence"
    );
    let reason = operation("appointment.cancel")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("NaturalMethod"), "{reason}");
    assert!(reason.contains("un-cancel"), "{reason}");
    assert!(reason.contains("AtMostOnce"), "{reason}");
}

/// `acuity_output_contract`: the declared pointers read Acuity's own objects,
/// with its own typing.
#[test]
fn acuity_output_contract() {
    let get = operation("appointment.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&appointment()).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": APPOINTMENT_ID, "datetime": "2013-06-17T10:15:00-0700",
            "appointment_type_id": APPOINTMENT_TYPE_ID, "calendar_id": 1,
            "first_name": "Bob", "last_name": "McTest",
            "email": "bob.mctest@example.com", "canceled": false, "no_show": false,
            "confirmation_page": "https://acuityscheduling.com/schedule.php?owner=11145481",
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":"1","datetime":"x","appointmentTypeID":1}"#)
            .expect_err("an id Acuity publishes as an integer is not a string")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":1,"datetime":"x"}"#)
            .expect_err("an appointment with no type is not the declared contract")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":1,"datetime":"x","appointmentTypeID":1}"#)
            .expect("only the identity, time and type are required")
            .get("no_show"),
        Some(&json!(null))
    );

    // A collection is a bare array at the document root, so the whole document
    // is the output.
    assert_eq!(
        acuity::decode(
            operation("appointment_type.list"),
            200,
            &reqwest::header::HeaderMap::new(),
            &serde_json::to_vec(&json!([appointment_type()])).expect("a fixture serializes")
        )
        .expect("the declared contract is satisfied"),
        json!([appointment_type()])
    );
}
