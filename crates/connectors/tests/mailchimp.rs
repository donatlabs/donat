//! Mailchimp connector proofs (spec 025 §4), against the SDK's local provider
//! stub.

use std::time::Duration;

use donat_connectors::providers::mailchimp;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorConfiguration, ConnectorErrorClass, Credential, EffectClass, Operation,
    PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const LIST_ID: &str = "abc123def4";
/// "The MD5 hash of the lowercase version of the list member's email address."
const SUBSCRIBER_HASH: &str = "62eeb731c4f9f52b1d5c62f9b0e6b6b0";

fn operation(id: &str) -> &'static Operation {
    mailchimp::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the mailchimp declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::basic(mailchimp::BASIC_USERNAME)
        .expect("the declared username is valid")
        .apply(&Credential::secret(SECRET_SENTINEL), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn list() -> JsonValue {
    json!({
        "id": LIST_ID,
        "name": "Donat customers",
        "date_created": "2026-08-01T09:00:00+00:00",
        "stats": { "member_count": 42, "unsubscribe_count": 1 },
    })
}

fn member() -> JsonValue {
    json!({
        "id": SUBSCRIBER_HASH,
        "email_address": "customer@example.test",
        "unique_email_id": "b1c2d3e4f5",
        "status": "subscribed",
        "merge_fields": { "FNAME": "Ada", "LNAME": "Lovelace" },
        "list_id": LIST_ID,
        "last_changed": "2026-08-02T09:00:00+00:00",
    })
}

/// Every operation, with an input that satisfies it and the documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("list.list", json!({})),
        ("list.get", json!({ "list_id": LIST_ID })),
        (
            "member.list",
            json!({ "list_id": LIST_ID, "status": "subscribed" }),
        ),
        (
            "member.get",
            json!({ "list_id": LIST_ID, "subscriber_hash": SUBSCRIBER_HASH }),
        ),
        (
            "member.upsert",
            json!({
                "list_id": LIST_ID, "subscriber_hash": SUBSCRIBER_HASH,
                "email_address": "customer@example.test", "status_if_new": "subscribed",
                "status": "subscribed",
                "merge_fields": { "FNAME": "Ada", "LNAME": "Lovelace" },
            }),
        ),
    ]
}

/// `mailchimp_request_shape`: exact method, path, query, headers, and body for
/// every operation, each under the published `/3.0` base path.
#[tokio::test]
async fn mailchimp_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", "/3.0/lists")
            .query("")
            .no_body()
            .respond_json(200, json!({ "lists": [list()], "total_items": 1 })),
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}"))
            .query("")
            .respond_json(200, list()),
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}/members"))
            .query("status=subscribed")
            .respond_json(200, json!({ "members": [member()], "total_items": 1 })),
        Expectation::new(
            "GET",
            &format!("/3.0/lists/{LIST_ID}/members/{SUBSCRIBER_HASH}"),
        )
        .query("")
        .respond_json(200, member()),
        Expectation::new(
            "PUT",
            &format!("/3.0/lists/{LIST_ID}/members/{SUBSCRIBER_HASH}"),
        )
        .json_body(json!({
            "email_address": "customer@example.test",
            "status_if_new": "subscribed",
            "status": "subscribed",
            "merge_fields": { "FNAME": "Ada", "LNAME": "Lovelace" },
        }))
        .respond_json(200, member()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/3.0/"),
            "{id} renders a published Mailchimp path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `mailchimp_auth_is_applied`: the API key reaches the wire as the *password*
/// half of HTTP Basic, under the username Mailchimp's own example publishes, and
/// appears nowhere else.
#[tokio::test]
async fn mailchimp_auth_is_applied() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}")).respond_json(200, list())
    ])
    .await;

    let request = render(&stub, "list.get", json!({ "list_id": LIST_ID }));
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    let value = applied
        .to_str()
        .expect("a basic credential is visible ASCII");
    assert!(value.starts_with("Basic "), "{value}");
    assert!(
        !value.contains(SECRET_SENTINEL),
        "the key is base64, not plain"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    // The declaration's username is Mailchimp's published constant, and it is
    // not a secret: it may appear in a `Debug` print.
    assert!(!format!("{:?}", mailchimp::connector().credential()).contains(SECRET_SENTINEL));
    assert_eq!(mailchimp::BASIC_USERNAME, "anystring");

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!("{:?}", mailchimp::error_map().classify_response(&response));
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `mailchimp_host_comes_only_from_deploy_time_configuration`: the data centre
/// is a host label from configuration, and input, a provider body, and a
/// continuation each fail to move it.
#[test]
fn mailchimp_host_comes_only_from_deploy_time_configuration() {
    let connector = mailchimp::connector();
    assert_eq!(
        connector.origin().host_variable(),
        Some(mailchimp::SERVER),
        "the templated host names the one configuration key it reads"
    );

    let origin = connector
        .resolve_origin(&ConnectorConfiguration::from_deployment([(
            mailchimp::SERVER,
            "us19",
        )]))
        .expect("a configured data centre resolves");
    assert_eq!(origin.as_url().host_str(), Some("us19.api.mailchimp.com"));

    // Input cannot move it: a hostile path value stays inside its own segment.
    let request = operation("list.get")
        .plan_request(
            &origin,
            &json!({ "list_id": "../../evil.example.test/3.0/lists/x" }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("us19.api.mailchimp.com"));
    assert!(!request.url().path().contains("evil.example.test/3.0"));

    // A configured value that is not one lowercase label is refused, so a
    // deployment cannot name a different authority.
    for hostile in [
        "us19.evil.example.test",
        "us19/../evil",
        "us19:8443",
        "user@evil",
        "",
    ] {
        assert!(
            connector
                .resolve_origin(&ConnectorConfiguration::from_deployment([(
                    mailchimp::SERVER,
                    hostile,
                )]))
                .is_err(),
            "`{hostile}` is not a data centre label"
        );
    }
    // And an unconfigured instance has no origin at all.
    assert!(
        connector
            .resolve_origin(&ConnectorConfiguration::default())
            .is_err()
    );
}

/// `mailchimp_error_map`: every documented status reaches exactly one closed
/// class, and none of Mailchimp's prose crosses the boundary.
#[tokio::test]
async fn mailchimp_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Permanent),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (414, ConnectorErrorClass::Validation),
        (422, ConnectorErrorClass::Validation),
        (426, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}"))
            .respond_header("x-request-id", "a1efb240-f8d8-40fe-a680-c3a5619a42e9")
            .respond_json(
                status,
                json!({
                    "type": "https://mailchimp.com/developer/marketing/docs/errors/",
                    "title": "Method Not Allowed",
                    "status": status,
                    "detail": format!("acme-audience rejected key {SECRET_SENTINEL}"),
                    "instance": "3b4dcb40-0b6b-4820-bfaa-41267b3826ea",
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "list.get", json!({ "list_id": LIST_ID })))
            .await
            .expect("the stub answers");

        let failure = mailchimp::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("request_id")
                .map(String::as_str),
            Some("a1efb240-f8d8-40fe-a680-c3a5619a42e9")
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-audience", "Method Not Allowed"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `mailchimp_rate_limit_is_classified`: the documented `429` is retryable, and
/// Mailchimp publishes no `Retry-After` — so the hint is absent unless the
/// response carried one, and a hostile one is clamped.
#[tokio::test]
async fn mailchimp_rate_limit_is_classified() {
    let limited = json!({
        "type": "https://mailchimp.com/developer/marketing/docs/errors/",
        "title": "TooManyRequests",
        "status": 429,
        "detail": "You have exceeded the limit of 10 simultaneous connections.",
        "instance": "3b4dcb40",
    });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}"))
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "list.get", json!({ "list_id": LIST_ID })))
            .await
            .expect("the stub answers");
        failures.push(mailchimp::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Mailchimp publishes a simultaneous-connection limit and no Retry-After, so the connector \
         invents none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `mailchimp_pagination_is_bounded`: the two collections walk the published
/// `count`/`offset` regime, stop on a short page, and respect every ceiling.
#[tokio::test]
async fn mailchimp_pagination_is_bounded() {
    let plan = mailchimp::pagination("member.list").expect("the member listing declares a plan");
    let budget = PaginationBudget::new(8, 8, 10_000, 512 * 1024, Duration::from_secs(5));

    let full_page: Vec<JsonValue> = (0..500).map(|index| json!({ "id": index })).collect();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}/members"))
            .query("status=subscribed&offset=0&count=500")
            .respond_json(200, json!({ "members": full_page, "total_items": 501 })),
        Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}/members"))
            .query("status=subscribed&offset=500&count=500")
            .respond_json(
                200,
                json!({ "members": [{ "id": 500 }], "total_items": 501 }),
            ),
    ])
    .await;

    let members = plan
        .collect(
            render(
                &stub,
                "member.list",
                json!({ "list_id": LIST_ID, "status": "subscribed" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk advances one page and stops on the short one");
    assert_eq!(members.len(), 501);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();

    for budget in [
        PaginationBudget::new(2, 8, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 100_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 100_000, 1, Duration::from_secs(5)),
    ] {
        let page: Vec<JsonValue> = (0..500).map(|index| json!({ "id": index })).collect();
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &format!("/3.0/lists/{LIST_ID}/members"))
                .respond_json(200, json!({ "members": page.clone() }))
        }))
        .await;
        let failure = plan
            .collect(
                render(
                    &stub,
                    "member.list",
                    json!({ "list_id": LIST_ID, "status": "subscribed" }),
                ),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    assert_eq!(
        mailchimp::pagination("list.list")
            .expect("the audience listing declares a plan")
            .items_pointer(),
        "/lists"
    );
    for id in ["list.get", "member.get", "member.upsert"] {
        assert!(
            mailchimp::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `mailchimp_effects_are_classified`: every operation carries a class, and the
/// upsert is the batch's clearest `NaturalMethod` — a `PUT` against a fixed
/// identity whose repeat semantics Mailchimp publishes twice.
#[test]
fn mailchimp_effects_are_classified() {
    let connector = mailchimp::connector();
    let expected = [
        ("list.list", EffectClass::ReadOnly),
        ("list.get", EffectClass::ReadOnly),
        ("member.list", EffectClass::ReadOnly),
        ("member.get", EffectClass::ReadOnly),
        (
            "member.upsert",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
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

    let upsert = operation("member.upsert");
    assert_eq!(upsert.method().as_str(), "PUT");
    let citation = format!("{:?}", upsert.effect());
    assert!(
        citation.contains("Add or update a list member"),
        "{citation}"
    );
    assert!(
        citation.contains("required only if the email address is not already present on the list"),
        "{citation}"
    );

    // The `POST` create is deliberately not declared: it is the same effect
    // with a worse contract.
    assert!(connector.operation("member.create").is_none());
}

/// `mailchimp_upsert_repeats_onto_one_member`: two identical sends address the
/// same fixed identity, so the provider is asked to update one member rather
/// than to add a second (spec 010 §7's `NaturalMethod` proof).
#[tokio::test]
async fn mailchimp_upsert_repeats_onto_one_member() {
    let stub = ProviderStub::start([
        Expectation::new(
            "PUT",
            &format!("/3.0/lists/{LIST_ID}/members/{SUBSCRIBER_HASH}"),
        )
        .respond_json(200, member()),
        Expectation::new(
            "PUT",
            &format!("/3.0/lists/{LIST_ID}/members/{SUBSCRIBER_HASH}"),
        )
        .respond_json(200, member()),
    ])
    .await;

    let input = json!({
        "list_id": LIST_ID, "subscriber_hash": SUBSCRIBER_HASH,
        "email_address": "customer@example.test", "status_if_new": "subscribed",
        "status": "subscribed", "merge_fields": {},
    });
    let mut identities = Vec::new();
    for _ in 0..2 {
        let request = render(&stub, "member.upsert", input.clone());
        identities.push(request.url().path().to_owned());
        let response = stub.send(request).await.expect("the stub answers");
        let decoded = operation("member.upsert")
            .decode_response(response.status.as_u16(), response.body())
            .expect("the declared contract is satisfied");
        assert_eq!(decoded.get("id"), Some(&json!(SUBSCRIBER_HASH)));
    }
    assert_eq!(
        identities[0], identities[1],
        "both sends name the same resource identity, so one member exists after both"
    );
    stub.assert_satisfied();
}

/// `mailchimp_output_contract`: the declared pointers read Mailchimp's own
/// objects, including the nested `stats` an audience publishes.
#[test]
fn mailchimp_output_contract() {
    let get = operation("list.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&list()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": LIST_ID,
            "name": "Donat customers",
            "date_created": "2026-08-01T09:00:00+00:00",
            "member_count": 42,
            "unsubscribe_count": 1,
        })
    );
    assert_eq!(
        get.decode_response(200, br#"{"name":"Donat customers"}"#)
            .expect_err("an answer with no id is not an audience")
            .class(),
        ConnectorErrorClass::Validation
    );
    // Mailchimp publishes its counts as numbers; a string there is a contract
    // violation rather than a coercion.
    assert_eq!(
        get.decode_response(200, br#"{"id":"a","stats":{"member_count":"42"}}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );

    let member_get = operation("member.get");
    assert_eq!(
        member_get
            .decode_response(
                200,
                &serde_json::to_vec(&member()).expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied")
            .get("unique_email_id"),
        Some(&json!("b1c2d3e4f5"))
    );
}
