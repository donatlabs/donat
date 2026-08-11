//! Zoho CRM connector proofs (spec 023 §4), against the SDK's local provider
//! stub.

use std::sync::LazyLock;
use std::time::Duration;

use donat_connectors::providers::zoho_crm;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, Connector, ConnectorConfiguration, ConnectorErrorClass, Credential,
    EffectClass, Operation, OperationRejection, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const MODULE: &str = "Deals";
const RECORD_ID: &str = "3652397000009851001";

fn connector() -> &'static Connector {
    static CONNECTOR: LazyLock<Connector> = LazyLock::new(|| {
        zoho_crm::connector(zoho_crm::Region::parse("eu").expect("a published region"))
            .expect("a published region declares")
    });
    &CONNECTOR
}

fn operation(id: &str) -> &'static Operation {
    connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the zoho_crm declaration publishes {id}"))
}

/// The credential lifecycle's applied header for one attempt, in the scheme this
/// connector declared (spec 011).
fn applied_token() -> AccessToken {
    AccessToken::new(format!(
        "{} {SECRET_SENTINEL}",
        zoho_crm::AUTHORIZATION_SCHEME
    ))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code_scheme(zoho_crm::AUTHORIZATION_SCHEME)
        .expect("the published scheme is a valid token")
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_token()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn path(suffix: &str) -> String {
    format!("/crm/{}{suffix}", zoho_crm::API_VERSION)
}

fn record_page() -> JsonValue {
    json!({
        "data": [{ "id": RECORD_ID, "Deal_Name": "Renewal" }],
        "info": { "per_page": 200, "count": 1, "page": 1, "more_records": false,
                  "next_page_token": null },
    })
}

fn write_result() -> JsonValue {
    json!({ "data": [{
        "code": "SUCCESS",
        "details": { "id": RECORD_ID },
        "message": "record added",
        "status": "success",
    }]})
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        (
            "record.get",
            json!({ "module_api_name": MODULE, "record_id": RECORD_ID }),
        ),
        (
            "record.list",
            json!({ "module_api_name": MODULE, "fields": "Deal_Name", "page": 1 }),
        ),
        (
            "record.search",
            json!({ "module_api_name": MODULE, "criteria": "(Deal_Name:equals:Renewal)",
                    "page": 1 }),
        ),
        (
            "record.create",
            json!({ "module_api_name": MODULE, "data": [{ "Deal_Name": "Renewal" }],
                    "trigger": [] }),
        ),
        (
            "record.update",
            json!({ "module_api_name": MODULE, "record_id": RECORD_ID,
                    "data": [{ "Stage": "Closed Won" }], "trigger": [] }),
        ),
        (
            "record.upsert",
            json!({ "module_api_name": MODULE, "data": [{ "Deal_Name": "Renewal" }],
                    "duplicate_check_fields": ["Deal_Name"] }),
        ),
        (
            "note.create",
            json!({ "data": [{ "Note_Content": "called", "Parent_Id": RECORD_ID }] }),
        ),
        (
            "note.list",
            json!({ "module_api_name": MODULE, "record_id": RECORD_ID, "page": 1 }),
        ),
    ]
}

/// `zoho_crm_request_shape`: exact method, path, query, headers, and body for
/// every operation, all under the pinned version segment.
#[tokio::test]
async fn zoho_crm_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &path(&format!("/{MODULE}/{RECORD_ID}")))
            .query("")
            .header(
                "authorization",
                &format!("{} {SECRET_SENTINEL}", zoho_crm::AUTHORIZATION_SCHEME),
            )
            .no_body()
            .respond_json(200, record_page()),
        Expectation::new("GET", &path(&format!("/{MODULE}")))
            .query("fields=Deal%5FName&page=1&per_page=200")
            .respond_json(200, record_page()),
        Expectation::new("GET", &path(&format!("/{MODULE}/search")))
            .query("criteria=%28Deal%5FName%3Aequals%3ARenewal%29&page=1&per_page=200")
            .respond_json(200, record_page()),
        Expectation::new("POST", &path(&format!("/{MODULE}")))
            .json_body(json!({ "data": [{ "Deal_Name": "Renewal" }], "trigger": [] }))
            .respond_json(201, write_result()),
        Expectation::new("PUT", &path(&format!("/{MODULE}/{RECORD_ID}")))
            .json_body(json!({ "data": [{ "Stage": "Closed Won" }], "trigger": [] }))
            .respond_json(200, write_result()),
        Expectation::new("POST", &path(&format!("/{MODULE}/upsert")))
            .json_body(json!({ "data": [{ "Deal_Name": "Renewal" }],
                               "duplicate_check_fields": ["Deal_Name"] }))
            .respond_json(200, write_result()),
        Expectation::new("POST", &path("/Notes"))
            .json_body(json!({ "data": [{ "Note_Content": "called", "Parent_Id": RECORD_ID }] }))
            .respond_json(201, write_result()),
        Expectation::new("GET", &path(&format!("/{MODULE}/{RECORD_ID}/Notes")))
            .query("page=1&per_page=200")
            .respond_json(200, record_page()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request
                .url()
                .path()
                .starts_with(&format!("/crm/{}/", zoho_crm::API_VERSION)),
            "{id} renders the pinned version segment: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `zoho_crm_auth_is_applied`: the stored token reaches the wire under Zoho's
/// own `Zoho-oauthtoken` scheme, and a header in any other scheme is refused
/// rather than sent.
#[tokio::test]
async fn zoho_crm_auth_is_applied() {
    let stub =
        ProviderStub::start([
            Expectation::new("GET", &path(&format!("/{MODULE}/{RECORD_ID}")))
                .header(
                    "authorization",
                    &format!("{} {SECRET_SENTINEL}", zoho_crm::AUTHORIZATION_SCHEME),
                )
                .respond_json(200, record_page()),
        ])
        .await;

    let request = render(
        &stub,
        "record.get",
        json!({ "module_api_name": MODULE, "record_id": RECORD_ID }),
    );
    let applied = request
        .headers()
        .get("authorization")
        .expect("the credential was applied");
    assert!(applied.is_sensitive());
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));

    let plan = AuthPlan::oauth2_authorization_code_scheme(zoho_crm::AUTHORIZATION_SCHEME)
        .expect("the published scheme is a valid token");
    assert_eq!(
        plan.oauth2_authorization_scheme(),
        Some(zoho_crm::AUTHORIZATION_SCHEME),
        "the lifecycle reads the scheme back off the declaration it formats for"
    );
    assert_eq!(
        connector()
            .credential()
            .plan()
            .and_then(donat_connectors::sdk::AuthPlan::oauth2_authorization_scheme),
        Some(zoho_crm::AUTHORIZATION_SCHEME)
    );

    // RFC 6750's scheme is what every other stored-OAuth2 connector sends, and
    // Zoho CRM publishes none of its endpoints with it: a header in that shape
    // is a seam that changed under this connector, and it fails the attempt.
    let mut wrong = operation("record.get")
        .plan_request(
            &stub.origin(),
            &json!({ "module_api_name": MODULE, "record_id": RECORD_ID }),
        )
        .expect("the declared request renders");
    let refusal = plan
        .apply(
            &Credential::from_fields([]),
            &mut wrong,
            Some(&AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))),
        )
        .expect_err("a Bearer header is not the credential this connector declared");
    assert_eq!(refusal.code(), "connector_credential_not_applicable");
    // And no token at all is a refusal rather than an unauthenticated request.
    assert!(
        plan.apply(&Credential::from_fields([]), &mut wrong, None)
            .is_err()
    );

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        connector().credential(),
        zoho_crm::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `zoho_crm_host_comes_only_from_deploy_time_configuration` (spec 023 §4 proof
/// 1): the data centre is a closed compiled table a deployment selects from, and
/// input, a provider body, and a continuation each fail to move it.
#[test]
fn zoho_crm_host_comes_only_from_deploy_time_configuration() {
    // The origin of a compiled instance is fixed: a deployment names a region,
    // and a region it did not name is not reachable.
    for region in zoho_crm::Region::ALL {
        let compiled = zoho_crm::connector(region).expect("every published region declares");
        let origin = compiled
            .resolve_origin(&ConnectorConfiguration::default())
            .expect("a fixed origin resolves without configuration");
        assert_eq!(
            origin.as_url().as_str(),
            format!("{}/", region.api_origin())
        );
        assert_eq!(compiled.origin().host_variable(), None);
    }
    assert_eq!(
        zoho_crm::Region::parse("eu")
            .expect("a published region")
            .api_origin(),
        "https://www.zohoapis.eu"
    );
    // Canada is the one to read twice: the accounts host and the API host do not
    // share a domain, and neither is derivable from the other.
    let canada = zoho_crm::Region::parse("ca").expect("a published region");
    assert_eq!(canada.api_origin(), "https://www.zohoapis.ca");
    assert_eq!(canada.accounts_origin(), "https://accounts.zohocloud.ca");

    for hostile in [
        "us-east-1",
        "attacker.invalid",
        "EU",
        "",
        "https://www.zohoapis.eu",
    ] {
        assert!(
            zoho_crm::Region::parse(hostile).is_err(),
            "`{hostile}` is not a region Zoho publishes a data centre for"
        );
    }

    // A deployment whose token endpoint belongs to another data centre is
    // refused: Zoho serves one org from one centre, and a token minted elsewhere
    // does not authenticate here.
    let eu = zoho_crm::Region::parse("eu").expect("a published region");
    assert!(eu.admits_token_endpoint("https://accounts.zoho.eu/oauth/v2/token"));
    assert!(eu.admits_token_endpoint("https://accounts.zoho.eu"));
    for foreign in [
        "https://accounts.zoho.com/oauth/v2/token",
        "https://accounts.zoho.eu.attacker.invalid/oauth/v2/token",
        "https://attacker.invalid/oauth/v2/token",
        "",
    ] {
        assert!(!eu.admits_token_endpoint(foreign), "`{foreign}`");
    }

    // 1. Operation input. A module name that spells another authority stays one
    //    percent-encoded segment on the compiled host.
    let origin = connector()
        .resolve_origin(&ConnectorConfiguration::default())
        .expect("a fixed origin resolves");
    let request = operation("record.get")
        .plan_request(
            &origin,
            &json!({ "module_api_name": "..//attacker.invalid", "record_id": RECORD_ID }),
        )
        .expect("the declared request renders");
    assert_eq!(request.url().host_str(), Some("www.zohoapis.eu"));
    assert_eq!(request.url().scheme(), "https");
    assert!(!request.url().path().contains("attacker.invalid/"));

    // 2. A provider body naming another host is data, not a destination — and
    //    there is no continuation plan for it to become one through.
    let output = operation("record.list")
        .extract_output(&json!({
            "data": [{ "id": "1" }],
            "info": { "next_page_token": "https://attacker.invalid/crm/v8/Deals",
                      "more_records": true },
        }))
        .expect("the declared contract is satisfied");
    assert_eq!(
        output.get("next_page_token"),
        Some(&json!("https://attacker.invalid/crm/v8/Deals"))
    );
    assert!(zoho_crm::pagination("record.list").is_none());
}

/// `zoho_crm_error_map`: every documented status and `code` reaches exactly one
/// closed class, and none of Zoho's prose crosses the boundary.
#[tokio::test]
async fn zoho_crm_error_map() {
    let documented = [
        (400, "INVALID_DATA", ConnectorErrorClass::Validation),
        (400, "MANDATORY_NOT_FOUND", ConnectorErrorClass::Validation),
        (400, "INVALID_MODULE", ConnectorErrorClass::Validation),
        (400, "DUPLICATE_DATA", ConnectorErrorClass::Permanent),
        (400, "RECORD_LOCKED", ConnectorErrorClass::Permanent),
        (
            401,
            "OAUTH_SCOPE_MISMATCH",
            ConnectorErrorClass::Authentication,
        ),
        (401, "INVALID_TOKEN", ConnectorErrorClass::Authentication),
        (403, "NO_PERMISSION", ConnectorErrorClass::Authentication),
        (404, "INVALID_URL_PATTERN", ConnectorErrorClass::Permanent),
        (
            405,
            "INVALID_REQUEST_METHOD",
            ConnectorErrorClass::Validation,
        ),
        (413, "TOO_LARGE", ConnectorErrorClass::Validation),
        (429, "TOO_MANY_REQUESTS", ConnectorErrorClass::Http429),
        (500, "INTERNAL_ERROR", ConnectorErrorClass::Http5xx),
        // A partial-status write whose one record failed.
        (207, "not_a_published_code", ConnectorErrorClass::Permanent),
        (418, "not_a_published_code", ConnectorErrorClass::Permanent),
    ];

    for (status, code, expected) in documented {
        let stub = ProviderStub::start([Expectation::new(
            "GET",
            &path(&format!("/{MODULE}/{RECORD_ID}")),
        )
        .respond_json(
            status,
            json!({
                "code": code,
                "details": { "api_name": "Deal_Name", "org": SECRET_SENTINEL },
                "message": format!("acme org rejected {SECRET_SENTINEL}"),
                "status": "error",
            }),
        )])
        .await;
        let response = stub
            .send(render(
                &stub,
                "record.get",
                json!({ "module_api_name": MODULE, "record_id": RECORD_ID }),
            ))
            .await
            .expect("the stub answers");

        let failure = zoho_crm::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme org", "Deal_Name"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `zoho_crm_rate_limit_is_classified`: the documented `429` and its published
/// `TOO_MANY_REQUESTS` code both reach `http_429`, and Zoho publishes no
/// `Retry-After`, so a hint is only ever one the response carried, clamped.
#[tokio::test]
async fn zoho_crm_rate_limit_is_classified() {
    let limited = json!({ "code": "TOO_MANY_REQUESTS", "details": {},
                          "message": "too many requests", "status": "error" });
    let stub = ProviderStub::start([
        Expectation::new("GET", &path(&format!("/{MODULE}/{RECORD_ID}")))
            .respond_header("x-api-credits-remaining", "0")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &path(&format!("/{MODULE}/{RECORD_ID}")))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited.clone()),
        // The concurrency refusal Zoho documents by code rather than by status.
        Expectation::new("GET", &path(&format!("/{MODULE}/{RECORD_ID}")))
            .respond_json(400, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(
                &stub,
                "record.get",
                json!({ "module_api_name": MODULE, "record_id": RECORD_ID }),
            ))
            .await
            .expect("the stub answers");
        failures.push(zoho_crm::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        None,
        "Zoho publishes no Retry-After, so the connector invents none"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    assert_eq!(
        failures[2].class(),
        ConnectorErrorClass::Http429,
        "the published code answers even where the status would not"
    );
    stub.assert_satisfied();
}

/// `zoho_crm_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): Zoho's cursor
/// exists and this connector deliberately does not walk it, so every operation
/// is one request and the token is published as data a Process carries.
#[tokio::test]
async fn zoho_crm_cursor_is_opaque_and_bounded() {
    for operation in connector().operations() {
        assert!(
            zoho_crm::pagination(operation.id()).is_none(),
            "`{}` declares no continuation plan",
            operation.id()
        );
    }

    // One attempt is one request, and the page number comes from the caller
    // rather than from a provider value.
    let stub = ProviderStub::start([Expectation::new("GET", &path(&format!("/{MODULE}")))
        .query("fields=Deal%5FName&page=2&per_page=200")
        .respond_json(
            200,
            json!({
                "data": [{ "id": "1" }],
                "info": { "more_records": true, "next_page_token": "c8582xx9e7c7" },
            }),
        )])
    .await;
    let decoded = operation("record.list")
        .decode_response(
            200,
            stub.send(render(
                &stub,
                "record.list",
                json!({ "module_api_name": MODULE, "fields": "Deal_Name", "page": 2 }),
            ))
            .await
            .expect("the stub answers")
            .body(),
        )
        .expect("the declared contract is satisfied");
    assert_eq!(stub.received(), 1, "one attempt is one request");
    // The token is published verbatim and is never parsed, rebuilt, or spent.
    assert_eq!(decoded.get("next_page_token"), Some(&json!("c8582xx9e7c7")));
    assert_eq!(decoded.get("more_records"), Some(&json!(true)));
    stub.assert_satisfied();
}

/// `zoho_crm_pagination_is_bounded`: with no plan declared, no operation can
/// spend a provider value as a request, and the published page regime is a
/// caller's input inside Zoho's own ceiling.
#[test]
fn zoho_crm_pagination_is_bounded() {
    let projected = operation("record.list").project();
    assert!(
        projected
            .inputs()
            .iter()
            .any(|input| input.name() == "page"),
        "the published page regime is a declared input"
    );
    assert!(
        projected
            .inputs()
            .iter()
            .all(|input| input.name() != "page_token"),
        "the token regime is not bound at all"
    );
    // A walked aggregate would need a plan, and there is none for any operation.
    for operation in connector().operations() {
        assert!(zoho_crm::pagination(operation.id()).is_none());
    }
}

/// `zoho_crm_success_envelope_cannot_carry_a_failure` (spec 023 §4 proof 4): the
/// per-record gate sits between the status check and the output pointers, so a
/// `2xx` whose record failed never reads as an activity success.
#[test]
fn zoho_crm_success_envelope_cannot_carry_a_failure() {
    let create = operation("record.create");
    let headers = reqwest::header::HeaderMap::new();

    let rejected = serde_json::to_vec(&json!({ "data": [{
        "code": "MANDATORY_NOT_FOUND",
        "details": { "api_name": "Deal_Name" },
        "message": "required field not found",
        "status": "error",
    }]}))
    .expect("a fixture serializes");
    let failure = zoho_crm::decode(create, 200, &headers, &rejected)
        .expect_err("a 2xx whose record failed is not a success");
    assert_eq!(failure.class(), ConnectorErrorClass::Validation);
    assert!(
        !failure.diagnostic().contains("Deal_Name"),
        "provider prose never crosses the boundary"
    );
    // The same body under Zoho's own multi-status is refused one layer earlier,
    // by the status the declaration never admitted.
    assert_eq!(
        zoho_crm::decode(create, 207, &headers, &rejected)
            .expect_err("a multi-status is not a declared success")
            .class(),
        ConnectorErrorClass::Permanent
    );

    let accepted = serde_json::to_vec(&write_result()).expect("a fixture serializes");
    assert_eq!(
        zoho_crm::decode(create, 201, &headers, &accepted)
            .expect("the declared contract is satisfied")
            .get("id"),
        Some(&json!(RECORD_ID))
    );
    // A read carries no per-record status, and the gate leaves it alone.
    let page = serde_json::to_vec(&record_page()).expect("a fixture serializes");
    assert!(zoho_crm::decode(operation("record.list"), 200, &headers, &page).is_ok());
}

/// `zoho_crm_effects_are_classified`: every operation carries a class, and the
/// upsert Zoho documents as repeat-safe is refused for that reason rather than
/// admitted for it.
#[test]
fn zoho_crm_effects_are_classified() {
    let expected = [
        ("record.get", EffectClass::ReadOnly),
        ("record.list", EffectClass::ReadOnly),
        ("record.search", EffectClass::ReadOnly),
        ("record.create", EffectClass::AtMostOnce),
        ("record.update", EffectClass::InventoryOnly),
        ("record.upsert", EffectClass::InventoryOnly),
        ("note.create", EffectClass::AtMostOnce),
        ("note.list", EffectClass::ReadOnly),
    ];
    assert_eq!(connector().operations().len(), expected.len());

    for (id, class) in expected {
        assert_eq!(operation(id).effect_class(), Some(class), "{id}");
        assert_eq!(
            connector().admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation(id).idempotency_binding().is_none(), "{id}");
    }
    assert_eq!(
        connector().admit_operation("record.upsert"),
        Err(OperationRejection::InventoryOnly)
    );

    let reason = operation("record.upsert")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(reason.contains("repeat-safe"));
    assert!(reason.contains("PUT and DELETE only"));

    let evidence = operation("record.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("idempot"));
    assert!(evidence.repeat_produces().contains("DUPLICATE_DATA"));
}

/// `zoho_crm_output_contract`: the declared pointers read Zoho's own envelope,
/// and its documented empty-collection status decodes as the silence Zoho
/// publishes rather than as a failure.
#[test]
fn zoho_crm_output_contract() {
    let list = operation("record.list");
    assert_eq!(
        list.decode_response(
            200,
            &serde_json::to_vec(&record_page()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "data": [{ "id": RECORD_ID, "Deal_Name": "Renewal" }],
            "next_page_token": null,
            "more_records": false,
        })
    );
    // "No Content HTTP 204 — There is no content available for the request."
    assert_eq!(
        list.decode_response(204, b"")
            .expect("a documented empty collection is not a failure"),
        json!({ "data": null, "next_page_token": null, "more_records": null })
    );
    // A write's id is the string Zoho publishes, not a number.
    assert_eq!(
        operation("record.create")
            .decode_response(
                201,
                &serde_json::to_vec(&write_result()).expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied"),
        json!({ "id": RECORD_ID, "code": "SUCCESS", "status": "success" })
    );
    assert_eq!(
        operation("record.create")
            .decode_response(201, br#"{"data":[{"code":"SUCCESS","details":{}}]}"#)
            .expect_err("a write with no record id is not a write result")
            .class(),
        ConnectorErrorClass::Validation
    );
}
