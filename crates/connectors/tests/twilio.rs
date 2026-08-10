//! Twilio connector proofs (spec 012 §3), against the SDK's local provider
//! stub.  No test reaches Twilio, and no test carries a real credential.

use std::sync::LazyLock;
use std::time::Duration;

use donat_connectors::providers::twilio;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, Connector, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass,
    Operation, OperationRejection, PaginationBudget, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const ACCOUNT_SID: &str = "AC0000000000000000000000000000fixture";
const TEST_ACCOUNT_SID: &str = "AC00000000000000000000000000000001";
const MESSAGE_SID: &str = "SM00000000000000000000000000000002";
const CALL_SID: &str = "CA00000000000000000000000000000003";

/// One deployment's declaration: Twilio's Basic username is the Account SID, so
/// the declaration is completed where the deployment is read.
fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        twilio::connector(TEST_ACCOUNT_SID).expect("a well-formed account SID declares a connector")
    });
    &CONNECTOR
}

fn configuration() -> ConnectorConfiguration {
    ConnectorConfiguration::from_deployment([(twilio::ACCOUNT_SID, TEST_ACCOUNT_SID)])
}

fn credential() -> Credential {
    Credential::secret(SECRET_SENTINEL)
}

fn operation(id: &str) -> &'static Operation {
    connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the twilio declaration publishes {id}"))
}

fn basic_header() -> String {
    format!(
        "Basic {}",
        base64_standard(&format!("{TEST_ACCOUNT_SID}:{SECRET_SENTINEL}"))
    )
}

/// The one place this test spells base64, so the expected `Authorization` value
/// is computed rather than copied.
fn base64_standard(value: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = value.as_bytes();
    let mut encoded = String::new();
    for chunk in bytes.chunks(3) {
        let mut buffer = [0u8; 3];
        buffer[..chunk.len()].copy_from_slice(chunk);
        let packed =
            (u32::from(buffer[0]) << 16) | (u32::from(buffer[1]) << 8) | u32::from(buffer[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                let position = ((packed >> (18 - index * 6)) & 0b11_1111) as usize;
                encoded.push(char::from(ALPHABET[position]));
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let input = twilio::account_scoped_input(&configuration(), &input)
        .expect("an account-scoped input is complete");
    let operation = operation(id);
    let mut request = match id {
        "message.send" => operation.plan_processor_request(
            &stub.origin(),
            &input,
            &reqwest::header::HeaderMap::new(),
            twilio::message_send_body(&input).expect("the documented form body renders"),
        ),
        "call.create" => operation.plan_processor_request(
            &stub.origin(),
            &input,
            &reqwest::header::HeaderMap::new(),
            twilio::call_create_body(&input).expect("the documented form body renders"),
        ),
        _ => operation.plan_request(&stub.origin(), &input),
    }
    .expect("the declared request renders");
    AuthPlan::basic(TEST_ACCOUNT_SID)
        .expect("a static username is valid")
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn message_input() -> JsonValue {
    json!({ "to": "+15005550006", "from": "+15005550001", "body": "Order 1001 shipped" })
}

fn call_input() -> JsonValue {
    json!({
        "to": "+15005550006",
        "from": "+15005550001",
        "url": "https://voice.example.test/twiml",
    })
}

/// `twilio_request_shape`: exact method, path, query, headers, and body for
/// every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn twilio_request_shape() {
    let account = TEST_ACCOUNT_SID;
    let message = json!({
        "sid": MESSAGE_SID,
        "status": "queued",
        "to": "+15005550006",
        "from": "+15005550001",
        "body": "Order 1001 shipped",
        "date_created": "Mon, 10 Aug 2026 00:00:00 +0000",
        "error_code": null,
    });
    let call = json!({
        "sid": CALL_SID,
        "status": "queued",
        "to": "+15005550006",
        "from": "+15005550001",
        "direction": "outbound-api",
        "date_created": "Mon, 10 Aug 2026 00:00:00 +0000",
    });
    let stub = ProviderStub::start([
        Expectation::new(
            "POST",
            &format!("/2010-04-01/Accounts/{account}/Messages.json"),
        )
        .query("")
        .header("content-type", "application/x-www-form-urlencoded")
        .respond_json(201, message.clone()),
        Expectation::new(
            "GET",
            &format!("/2010-04-01/Accounts/{account}/Messages/{MESSAGE_SID}.json"),
        )
        .query("")
        .no_body()
        .respond_json(200, message),
        Expectation::new(
            "GET",
            &format!("/2010-04-01/Accounts/{account}/Messages.json"),
        )
        .query("PageSize=50")
        .no_body()
        .respond_json(200, json!({ "messages": [], "page": 0, "page_size": 50 })),
        Expectation::new(
            "POST",
            &format!("/2010-04-01/Accounts/{account}/Calls.json"),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .respond_json(201, call.clone()),
        Expectation::new(
            "GET",
            &format!("/2010-04-01/Accounts/{account}/Calls/{CALL_SID}.json"),
        )
        .respond_json(200, call),
        Expectation::new("GET", &format!("/2010-04-01/Accounts/{account}/Calls.json"))
            .query("PageSize=50")
            .respond_json(200, json!({ "calls": [], "page": 0, "page_size": 50 })),
        // A hostile message SID stays one percent-encoded path segment.
        Expectation::new(
            "GET",
            &format!("/2010-04-01/Accounts/{account}/Messages/%2E%2E%2FCalls%2Ejson%3Fx%3D1.json"),
        )
        .respond_json(
            200,
            json!({ "sid": MESSAGE_SID, "status": "sent", "to": "+15005550006" }),
        ),
    ])
    .await;

    // The two form bodies are the documented parameter names, percent-encoded
    // so that no value can add a parameter of its own.
    let send = render(&stub, "message.send", message_input());
    assert_eq!(
        std::str::from_utf8(send.body()).expect("a form body is ASCII"),
        "To=%2B15005550006&From=%2B15005550001&Body=Order%201001%20shipped"
    );
    stub.send(send).await.expect("the stub answers");

    for (id, input) in [
        ("message.get", json!({ "sid": MESSAGE_SID })),
        ("message.list", json!({})),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let create = render(&stub, "call.create", call_input());
    assert_eq!(
        std::str::from_utf8(create.body()).expect("a form body is ASCII"),
        "To=%2B15005550006&From=%2B15005550001&\
         Url=https%3A%2F%2Fvoice%2Eexample%2Etest%2Ftwiml"
    );
    stub.send(create).await.expect("the stub answers");

    for (id, input) in [
        ("call.get", json!({ "sid": CALL_SID })),
        ("call.list", json!({})),
    ] {
        stub.send(render(&stub, id, input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(&stub, "message.get", json!({ "sid": "../Calls.json?x=1" }));
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();

    // A hostile form value stays inside its own parameter.
    assert_eq!(
        std::str::from_utf8(
            &twilio::message_send_body(&json!({
                "to": "+15005550006",
                "from": "+15005550001",
                "body": "hi&StatusCallback=https://attacker.invalid&x=",
            }))
            .expect("a hostile body renders")
        )
        .expect("a form body is ASCII"),
        "To=%2B15005550006&From=%2B15005550001&\
         Body=hi%26StatusCallback%3Dhttps%3A%2F%2Fattacker%2Einvalid%26x%3D"
    );

    // The account is deploy-time material: an input that tries to choose one is
    // refused rather than honoured.
    assert!(
        twilio::account_scoped_input(
            &configuration(),
            &json!({ "account_sid": "AC00000000000000000000000000000009" }),
        )
        .is_err()
    );
    assert!(
        twilio::account_scoped_input(&ConnectorConfiguration::default(), &json!({})).is_err(),
        "an unconfigured account is a failure, not a guess"
    );
    assert!(
        twilio::account_scoped_input(
            &ConnectorConfiguration::from_deployment([(twilio::ACCOUNT_SID, "not-a-sid")]),
            &json!({}),
        )
        .is_err()
    );
    assert!(
        twilio::connector(ACCOUNT_SID).is_err(),
        "a declaration is refused unless its account SID is well formed"
    );
}

/// `twilio_auth_is_applied`: the auth token reaches the wire only inside the
/// documented HTTP Basic credential.
#[tokio::test]
async fn twilio_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/2010-04-01/Accounts/{TEST_ACCOUNT_SID}/Messages.json"),
    )
    .header("authorization", &basic_header())
    .without_header("x-api-key")
    .respond_json(200, json!({ "messages": [], "page": 0 }))])
    .await;

    let request = render(&stub, "message.list", json!({}));
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
        "the auth token is not a query value"
    );
    assert!(
        !basic_header().contains(SECRET_SENTINEL),
        "the token crosses the wire base64-encoded inside the documented Basic credential"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = twilio::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `twilio_error_map`: Twilio's documented statuses and its numeric `code` each
/// reach exactly one of the eight closed classes.
#[tokio::test]
async fn twilio_error_map() {
    let documented = [
        // "21201: No to number is specified", the documented 400 example.
        (400, 21201, ConnectorErrorClass::Validation),
        (401, 20003, ConnectorErrorClass::Authentication),
        (403, 20005, ConnectorErrorClass::Authentication),
        (404, 20404, ConnectorErrorClass::Permanent),
        (410, 20404, ConnectorErrorClass::Permanent),
        (429, 20429, ConnectorErrorClass::Http429),
        (500, 20500, ConnectorErrorClass::Http5xx),
        (503, 20503, ConnectorErrorClass::Http5xx),
        // The documented codes whose class the status alone would not settle.
        (400, 20003, ConnectorErrorClass::Authentication),
        (400, 20429, ConnectorErrorClass::Http429),
        // Undocumented: the declared fallback answers.
        (418, 99999, ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &format!("/2010-04-01/Accounts/{TEST_ACCOUNT_SID}/Messages.json"),
        )
        .respond_json(
            status,
            json!({
                "status": status,
                "code": code,
                "message": format!("shard db-7.internal rejected token {SECRET_SENTINEL}"),
                "more_info": format!("https://www.twilio.com/docs/errors/{code}"),
            }),
        )])
        .await;
        let response = stub
            .send(render(&stub, "message.list", json!({})))
            .await
            .expect("the stub answers");

        let failure = twilio::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            operation("message.list")
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
        for leaked in [SECRET_SENTINEL, "db-7.internal", "twilio.com/docs/errors"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }

    // A `Retry-After` a provider sends is clamped to the SDK's ceiling.
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/2010-04-01/Accounts/{TEST_ACCOUNT_SID}/Messages.json"),
    )
    .respond_header("retry-after", "604800")
    .respond_json(429, json!({ "status": 429, "code": 20429 }))])
    .await;
    let response = stub
        .send(render(&stub, "message.list", json!({})))
        .await
        .expect("the stub answers");
    assert_eq!(
        twilio::error_map()
            .classify_response(&response)
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
    stub.assert_satisfied();
}

/// `twilio_pagination_is_bounded`: Twilio publishes both of the protocols this
/// connector declares — a body-carried `next_page_uri` and a *zero-indexed*
/// `Page` — and each is bounded. The continuation is resolved against the
/// compiled origin and a `next_page_uri` naming another origin is rejected, not
/// followed; the page walk starts at page 0, which is where Twilio's first page
/// is.
#[tokio::test]
async fn twilio_pagination_is_bounded() {
    for id in ["message.get", "call.get", "message.send"] {
        assert!(
            twilio::pagination(id).is_none() && twilio::page_number_pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    let messages = format!("/2010-04-01/Accounts/{TEST_ACCOUNT_SID}/Messages.json");
    let plan = twilio::pagination("message.list").expect("message.list declares a continuation");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));

    // Twilio's `next_page_uri` is a *relative* URI, which resolves against the
    // compiled origin and keeps the parameters Twilio chose.
    let stub = ProviderStub::start([
        Expectation::new("GET", &messages)
            .query("PageSize=50")
            .respond_json(
                200,
                json!({
                    "messages": [{ "sid": MESSAGE_SID }],
                    "page": 0,
                    "next_page_uri": format!("{messages}?PageSize=50&Page=1&PageToken=PAfixture"),
                }),
            ),
        Expectation::new("GET", &messages)
            .query("PageSize=50&Page=1&PageToken=PAfixture")
            .respond_json(200, json!({ "messages": [{ "sid": CALL_SID }], "page": 1 })),
    ])
    .await;
    let collected = plan
        .collect(
            render(&stub, "message.list", json!({})),
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
    assert_eq!(collected.len(), 2);
    stub.assert_satisfied();

    // A `next_page_uri` naming another origin is refused, and that origin is
    // never contacted.
    let elsewhere = ProviderStub::start([Expectation::new("GET", "/2010-04-01/next")
        .respond_json(200, json!({ "messages": [], "page": 1 }))])
    .await;
    let next = format!("{}/2010-04-01/next", elsewhere.base_url());
    let stub = ProviderStub::start([Expectation::new("GET", &messages)
        .query("PageSize=50")
        .respond_json(
            200,
            json!({ "messages": [{ "sid": MESSAGE_SID }], "page": 0, "next_page_uri": next }),
        )])
    .await;
    let failure = plan
        .collect(
            render(&stub, "message.list", json!({})),
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

    // The same continuation is published as declared output, and reading it
    // there never turns it into a request.
    let stub = ProviderStub::start([Expectation::new("GET", &messages)
        .query("PageSize=50")
        .respond_json(
            200,
            json!({ "messages": [], "page": 0, "next_page_uri": next }),
        )])
    .await;
    let response = stub
        .send(render(&stub, "message.list", json!({})))
        .await
        .expect("the stub answers");
    let page = operation("message.list")
        .decode_response(200, response.body())
        .expect("the declared contract is satisfied");
    assert_eq!(
        page.get("next_page_uri").and_then(JsonValue::as_str),
        Some(next.as_str()),
        "the continuation is output data"
    );
    let again = render(&stub, "message.list", json!({}));
    assert_eq!(again.url().host_str(), stub.origin().as_url().host_str());
    assert_eq!(again.url().query(), Some("PageSize=50"));

    // Twilio's other documented protocol: `page` is zero-indexed, so the walk
    // starts at 0 and the first page is not skipped.
    let calls = format!("/2010-04-01/Accounts/{TEST_ACCOUNT_SID}/Calls.json");
    let by_page =
        twilio::page_number_pagination("call.list").expect("call.list declares a page walk");
    fn call_page(count: usize, first: usize) -> JsonValue {
        json!({
            "calls": (0..count)
                .map(|index| json!({ "sid": format!("CA{}", first + index) }))
                .collect::<Vec<_>>(),
            "page": 0,
        })
    }
    let stub = ProviderStub::start([
        Expectation::new("GET", &calls)
            .query("PageSize=50&Page=0")
            .respond_json(200, call_page(50, 0)),
        Expectation::new("GET", &calls)
            .query("PageSize=50&Page=1")
            .respond_json(200, call_page(1, 50)),
    ])
    .await;
    let walked = by_page
        .collect(
            render(&stub, "call.list", json!({})),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the zero-indexed walk starts at Twilio's first page");
    assert_eq!(walked.len(), 51);
    assert_eq!(
        walked.first().and_then(|call| call.get("sid")),
        Some(&json!("CA0")),
        "a one-based walk would have dropped Twilio's page 0"
    );
    stub.assert_satisfied();

    // The declared page size is the bound, and it is part of the declaration.
    let stub = ProviderStub::start([]).await;
    assert_eq!(
        render(&stub, "call.list", json!({})).url().query(),
        Some("PageSize=50")
    );
    let budget = PaginationBudget::new(1, 1, 8, 8 * 1024, Duration::from_secs(5));
    assert!(budget.admit_call(0).is_ok());
    assert!(budget.admit_call(1).is_err());
}

/// `twilio_effects_are_classified`: every operation carries a class, and an
/// inventory-only operation cannot be enabled by a deployment.
#[test]
fn twilio_effects_are_classified() {
    let connector = connector();
    let expected = [
        ("message.send", EffectClass::AtMostOnce),
        ("message.get", EffectClass::ReadOnly),
        ("message.list", EffectClass::ReadOnly),
        ("call.create", EffectClass::AtMostOnce),
        ("call.get", EffectClass::ReadOnly),
        ("call.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: the Message and Call resources publish no idempotency key to bind"
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
        connector.admit_operation("alarm.create"),
        Err(OperationRejection::Undeclared),
        "the one Twilio API with a documented idempotency token is not compiled into this binary"
    );
}

/// `twilio_output_contract`: the declared pointers are complete and typed, and
/// a missing required pointer is a validation failure rather than a null.
#[test]
fn twilio_output_contract() {
    let send = operation("message.send");
    assert_eq!(
        send.decode_response(
            201,
            br#"{"sid":"SM1","status":"queued","to":"+15005550006","from":"+1","body":"hi","date_created":"Mon, 10 Aug 2026 00:00:00 +0000","error_code":null,"num_segments":"1"}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "sid": "SM1",
            "status": "queued",
            "to": "+15005550006",
            "date_created": "Mon, 10 Aug 2026 00:00:00 +0000",
            "error_code": null,
        })
    );
    for body in [
        br#"{"status":"queued","to":"+1","date_created":"d"}"#.as_slice(),
        br#"{"sid":null,"status":"queued","to":"+1","date_created":"d"}"#.as_slice(),
        br#"{"sid":7,"status":"queued","to":"+1","date_created":"d"}"#.as_slice(),
        br#"{"sid":"SM1","status":"queued","to":"+1","date_created":"d","error_code":"30003"}"#
            .as_slice(),
    ] {
        assert_eq!(
            send.decode_response(201, body)
                .expect_err("a missing or mistyped required pointer is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    assert_eq!(
        operation("message.list")
            .decode_response(200, br#"{"messages":[],"page":0,"page_size":50}"#)
            .expect("a last page carries no next_page_uri"),
        json!({ "messages": [], "page": 0, "next_page_uri": null })
    );
    assert_eq!(
        operation("call.get")
            .decode_response(
                200,
                br#"{"sid":"CA1","status":"completed","to":"+1","direction":"outbound-api"}"#,
            )
            .expect("the declared call contract is satisfied"),
        json!({ "sid": "CA1", "status": "completed", "to": "+1", "direction": "outbound-api" })
    );

    // The documented success statuses: "201 Resource created" for the two
    // creates, "200 Successfully processed" for the reads.
    for (id, status) in [
        ("message.send", 201),
        ("call.create", 201),
        ("message.get", 200),
        ("message.list", 200),
        ("call.get", 200),
        ("call.list", 200),
    ] {
        let operation = operation(id);
        assert!(operation.is_success(status), "{id}");
        assert!(
            !operation.is_success(if status == 200 { 201 } else { 200 }),
            "{id} admits exactly the status Twilio documents"
        );
    }

    // A processor body is not optional: the declaration refuses to render the
    // create without one.
    assert_eq!(
        operation("message.send")
            .plan_request(
                &donat_connectors::sdk::Origin::parse("https://api.twilio.test")
                    .expect("a static origin"),
                &json!({ "account_sid": TEST_ACCOUNT_SID }),
            )
            .expect_err("a form body is assembled by the declared processor")
            .class(),
        ConnectorErrorClass::Invariant
    );
}
