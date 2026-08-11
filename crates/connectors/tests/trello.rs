//! Trello connector proofs (spec 024 §3, which adopts spec 023 §4), against the
//! SDK's local provider stub.
//!
//! Trello is the batch's two-secret credential, so the redaction proof here is
//! wider than the others': neither half may appear in a log line, a diagnostic,
//! an error, or a fingerprint.

use donat_connectors::providers::trello;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection, RequestPlan,
    Secret,
};
use serde_json::{Value as JsonValue, json};

const CARD_ID: &str = "5abbe4b7ddc1b351ef961414";
const LIST_ID: &str = "5abbe4b7ddc1b351ef961415";
const BOARD_ID: &str = "5abbe4b7ddc1b351ef961416";

/// The second half of Trello's credential, distinct from the SDK's own sentinel
/// so a leak of either half is attributable.
const KEY_SENTINEL: &str = "donat-trello-key-sentinel-do-not-log";

fn operation(id: &str) -> &'static Operation {
    trello::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the trello declaration publishes {id}"))
}

/// The resolved credential: two secrets, neither of them declaration material.
fn credential() -> Credential {
    Credential::from_fields([
        ("api_key", Secret::new(KEY_SENTINEL)),
        ("secret", Secret::new(SECRET_SENTINEL)),
    ])
}

/// The exact query Trello's own example spells, percent-encoded as the SDK
/// encodes a credential.
fn expected_credential_query() -> String {
    format!(
        "key={}&token={}",
        KEY_SENTINEL.replace('-', "%2D"),
        SECRET_SENTINEL.replace('-', "%2D")
    )
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    trello::auth_plan()
        .apply(&credential(), &mut request, None)
        .expect("the declared plan applies the credential");
    request
}

fn card() -> JsonValue {
    json!({
        "id": CARD_ID,
        "name": "Ship the batch",
        "desc": "with its evidence",
        "closed": false,
        "idList": LIST_ID,
        "idBoard": BOARD_ID,
        "due": "2026-08-20T12:00:00.000Z",
        "dueComplete": false,
        "url": "https://trello.com/c/abcdefgh/1-ship-the-batch",
        "dateLastActivity": "2026-08-02T11:56:51.000Z",
    })
}

fn comment_action() -> JsonValue {
    json!({
        "id": "62aa1b0f",
        "type": "commentCard",
        "date": "2026-08-02T11:56:51.000Z",
        "data": { "text": "Looks right to me." },
    })
}

/// Every operation, with an input that satisfies it.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("card.get", json!({ "id": CARD_ID })),
        ("card.list", json!({ "id": LIST_ID })),
        (
            "card.search",
            json!({ "query": "batch", "id_boards": "mine" }),
        ),
        (
            "card.create",
            json!({
                "id_list": LIST_ID, "name": "Ship the batch", "desc": "with its evidence",
                "pos": "top", "due": "2026-08-20T12:00:00.000Z",
                "id_members": [], "id_labels": [],
            }),
        ),
        (
            "card.update",
            json!({
                "id": CARD_ID, "name": "Ship the batch", "desc": null, "closed": false,
                "id_list": LIST_ID, "due": null, "due_complete": true,
            }),
        ),
        ("card.delete", json!({ "id": CARD_ID })),
        (
            "comment.add",
            json!({ "id": CARD_ID, "text": "Looks right to me." }),
        ),
        ("comment.list", json!({ "id": CARD_ID })),
        ("board.get", json!({ "id": BOARD_ID })),
        ("list.list", json!({ "id": BOARD_ID, "filter": "open" })),
    ]
}

/// `trello_request_shape`: exact method, path, query, headers, and body for
/// every operation, with the credential always last on the query string.
#[tokio::test]
async fn trello_request_shape() {
    let credential_query = expected_credential_query();
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/1/cards/{CARD_ID}"))
            .query(&credential_query)
            .header("accept", "application/json")
            .no_body()
            .respond_json(200, card()),
        Expectation::new("GET", &format!("/1/lists/{LIST_ID}/cards"))
            .query(&credential_query)
            .respond_json(200, json!([card()])),
        Expectation::new("GET", "/1/search")
            .query(&format!(
                "query=batch&idBoards=mine&modelTypes=cards&cards_limit=100&{credential_query}"
            ))
            .respond_json(200, json!({ "cards": [card()] })),
        Expectation::new("POST", "/1/cards")
            .query(&credential_query)
            .json_body(json!({
                "idList": LIST_ID, "name": "Ship the batch", "desc": "with its evidence",
                "pos": "top", "due": "2026-08-20T12:00:00.000Z",
                "idMembers": [], "idLabels": [],
            }))
            .respond_json(200, card()),
        Expectation::new("PUT", &format!("/1/cards/{CARD_ID}"))
            .query(&credential_query)
            .json_body(json!({
                "name": "Ship the batch", "desc": null, "closed": false,
                "idList": LIST_ID, "due": null, "dueComplete": true,
            }))
            .respond_json(200, card()),
        Expectation::new("DELETE", &format!("/1/cards/{CARD_ID}"))
            .query(&credential_query)
            .respond_json(200, json!({ "_value": null })),
        Expectation::new("POST", &format!("/1/cards/{CARD_ID}/actions/comments"))
            .query(&format!(
                "text=Looks%20right%20to%20me%2E&{credential_query}"
            ))
            .respond_json(200, comment_action()),
        Expectation::new("GET", &format!("/1/cards/{CARD_ID}/actions"))
            .query(&format!("filter=commentCard&{credential_query}"))
            .respond_json(200, json!([comment_action()])),
        Expectation::new("GET", &format!("/1/boards/{BOARD_ID}"))
            .query(&credential_query)
            .respond_json(
                200,
                json!({ "id": BOARD_ID, "name": "Connectors", "desc": "",
                        "closed": false, "url": "https://trello.com/b/abc" }),
            ),
        Expectation::new("GET", &format!("/1/boards/{BOARD_ID}/lists"))
            .query(&format!("filter=open&{credential_query}"))
            .respond_json(200, json!([{ "id": LIST_ID, "name": "Doing" }])),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/1/"),
            "{id} renders a published Trello path: {}",
            request.url().path()
        );
        assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `trello_auth_is_applied` (spec 024 §3): both credential halves reach the wire
/// as the query parameters Trello publishes, and **neither** appears in a log
/// line, a diagnostic, an error, or a fingerprint.
#[tokio::test]
async fn trello_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/1/cards/{CARD_ID}"))
        .query(&expected_credential_query())
        .respond_json(500, json!({ "message": "server error" }))])
    .await;

    let request = render(&stub, "card.get", json!({ "id": CARD_ID }));

    // 1. The wire form is Trello's own, and both halves are on it.
    let query = request.url().query().expect("the credential is applied");
    assert_eq!(query, expected_credential_query());
    // No `Authorization` header at all: Trello's other form is OAuth 1.0a
    // signing, which this SDK cannot produce.
    assert!(request.headers().get("authorization").is_none());

    // 2. The URL is marked as carrying a credential, so every printed form of
    //    it is the origin rather than the query.
    assert!(request.url_carries_credential());
    let redacted = request.redacted_url();
    let debug = format!("{request:?}");
    for sentinel in [KEY_SENTINEL, SECRET_SENTINEL] {
        assert!(!redacted.contains(sentinel), "{redacted}");
        assert!(!debug.contains(sentinel), "{debug}");
    }
    assert!(redacted.starts_with(stub.base_url()), "{redacted}");
    assert!(redacted.ends_with("<redacted>"), "{redacted}");

    // 3. The declaration itself carries neither half.
    let declaration = format!("{:?}", trello::connector().credential());
    for sentinel in [KEY_SENTINEL, SECRET_SENTINEL] {
        assert!(!declaration.contains(sentinel), "{declaration}");
    }

    // 4. A classified provider failure carries neither half either.
    let response = stub.send(request).await.expect("the stub answers");
    let failure = trello::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic()
    );
    for sentinel in [KEY_SENTINEL, SECRET_SENTINEL] {
        assert!(!surface.contains(sentinel), "{surface}");
    }
    stub.assert_satisfied();
}

/// A credential missing either half is refused before a byte leaves, because
/// neither authenticates alone.
#[test]
fn trello_refuses_a_half_credential() {
    let plan = trello::auth_plan();
    assert_eq!(plan.required_fields(), ["api_key", "secret"]);

    for partial in [
        Credential::from_fields([("api_key", Secret::new(KEY_SENTINEL))]),
        Credential::from_fields([("secret", Secret::new(SECRET_SENTINEL))]),
        Credential::from_fields([]),
    ] {
        assert!(
            trello::connector().credential().admits(&partial).is_err(),
            "a half credential is not a Trello credential"
        );
    }
    assert!(
        trello::connector()
            .credential()
            .admits(&credential())
            .is_ok(),
        "both halves are the whole credential"
    );
}

/// `trello_error_map`: every documented status reaches exactly one closed class,
/// and Trello's bare-string error body never crosses the boundary.
#[tokio::test]
async fn trello_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Authentication),
        (404, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &format!("/1/cards/{CARD_ID}"))
            .respond_bytes(status, format!("invalid id {KEY_SENTINEL}").into_bytes())])
        .await;
        let response = stub
            .send(render(&stub, "card.get", json!({ "id": CARD_ID })))
            .await
            .expect("the stub answers");

        let failure = trello::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [KEY_SENTINEL, SECRET_SENTINEL, "invalid id"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `trello_rate_limit_is_classified`: Trello's `429` reaches `http_429`, and —
/// because Trello publishes no `Retry-After` — it carries no retry hint the
/// engine could have invented.
#[tokio::test]
async fn trello_rate_limit_is_classified() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/1/cards/{CARD_ID}"))
        .respond_header("x-rate-limit-api-key-max", "300")
        .respond_header("x-rate-limit-api-key-remaining", "0")
        .respond_header("x-rate-limit-api-key-interval-ms", "10000")
        .respond_bytes(429, b"API_TOKEN_LIMIT_EXCEEDED".to_vec())])
    .await;

    let response = stub
        .send(render(&stub, "card.get", json!({ "id": CARD_ID })))
        .await
        .expect("the stub answers");
    let failure = trello::error_map().classify_response(&response);
    assert_eq!(failure.class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failure.retry_after(),
        None,
        "Trello publishes no Retry-After, so the connector invents none"
    );
    stub.assert_satisfied();
}

/// `trello_pagination_is_bounded`: no operation declares a continuation plan,
/// which is the module's own recorded answer rather than an omission.
#[test]
fn trello_pagination_is_bounded() {
    for (id, _) in cases() {
        assert!(
            trello::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
    // The bound is the provider's own instead: the search's card page is a
    // declared literal, so one call cannot ask for the whole board.
    let request = operation("card.search")
        .plan_request(
            &donat_connectors::sdk::Origin::parse("https://api.trello.com")
                .expect("a valid origin"),
            &json!({ "query": "batch", "id_boards": "mine" }),
        )
        .expect("the declared request renders");
    assert!(
        request
            .url()
            .query()
            .expect("the search declares a query")
            .contains("cards_limit=100")
    );
}

/// `trello_effects_are_classified`: every operation carries a class, and the
/// update and the delete are unreachable from a Process.
#[test]
fn trello_effects_are_classified() {
    let connector = trello::connector();
    let expected = [
        ("card.get", EffectClass::ReadOnly),
        ("card.list", EffectClass::ReadOnly),
        ("card.search", EffectClass::ReadOnly),
        ("card.create", EffectClass::AtMostOnce),
        ("card.update", EffectClass::InventoryOnly),
        ("card.delete", EffectClass::InventoryOnly),
        ("comment.add", EffectClass::AtMostOnce),
        ("comment.list", EffectClass::ReadOnly),
        ("board.get", EffectClass::ReadOnly),
        ("list.list", EffectClass::ReadOnly),
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
        connector.admit_operation("card.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    let evidence = operation("card.create")
        .effect()
        .and_then(donat_connectors::sdk::Effect::no_idempotency_evidence)
        .expect("the class carries the evidence it was admitted on");
    assert!(evidence.searched_documentation().contains("oauth_nonce"));
    assert!(evidence.repeat_produces().contains("a second card"));
}

/// `trello_output_contract`: the declared pointers read Trello's own objects,
/// and a bare array stays the whole document.
#[test]
fn trello_output_contract() {
    let get = operation("card.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&card()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": CARD_ID, "name": "Ship the batch", "desc": "with its evidence",
            "closed": false, "id_list": LIST_ID, "id_board": BOARD_ID,
            "due": "2026-08-20T12:00:00.000Z", "due_complete": false,
            "url": "https://trello.com/c/abcdefgh/1-ship-the-batch",
            "date_last_activity": "2026-08-02T11:56:51.000Z",
        })
    );
    // Trello omits `due` as null on a card with no due date; only the identity
    // is demanded.
    assert_eq!(
        get.decode_response(200, br#"{"id":"5abbe4b7ddc1b351ef961414"}"#)
            .expect("only the identity is required")
            .get("id"),
        Some(&json!(CARD_ID))
    );
    assert_eq!(
        get.decode_response(200, br#"{"id":42}"#)
            .expect_err("a Trello id is a string")
            .class(),
        ConnectorErrorClass::Validation
    );
    // A bare array is the whole output of a list.
    assert_eq!(
        operation("card.list")
            .decode_response(200, br#"[{"id":"1"}]"#)
            .expect("a bare array is the whole output"),
        json!([{ "id": "1" }])
    );
    // And the search publishes the envelope Trello answers it with.
    assert_eq!(
        operation("card.search")
            .decode_response(200, br#"{"cards":[{"id":"1"}],"boards":[]}"#)
            .expect("the declared contract is satisfied"),
        json!({ "cards": [{ "id": "1" }] })
    );
}
