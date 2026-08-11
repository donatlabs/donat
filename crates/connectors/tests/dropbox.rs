//! Dropbox connector proofs (spec 025 §4, which is spec 023 §4 unchanged),
//! against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::dropbox;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, RequestPlan,
};
use serde_json::{Value as JsonValue, json};

const PATH: &str = "/Homework/math/Prime_Numbers.txt";
const FOLDER: &str = "/Homework/math";
const FILE_ID: &str = "id:a4ayc_80_OEAAAAAAAAAYa";

fn operation(id: &str) -> &'static Operation {
    dropbox::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the dropbox declaration publishes {id}"))
}

/// The credential lifecycle's applied header for one attempt (spec 011).
fn applied_token() -> AccessToken {
    AccessToken::new(format!("Bearer {SECRET_SENTINEL}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    let mut request = operation(id)
        .plan_request(&stub.origin(), &input)
        .expect("the declared request renders");
    AuthPlan::oauth2_authorization_code()
        .apply(
            &Credential::from_fields([]),
            &mut request,
            Some(&applied_token()),
        )
        .expect("the declared plan applies the stored credential");
    request
}

fn file_metadata() -> JsonValue {
    json!({
        ".tag": "file",
        "id": FILE_ID,
        "name": "Prime_Numbers.txt",
        "path_lower": "/homework/math/prime_numbers.txt",
        "path_display": PATH,
        "client_modified": "2026-08-01T09:00:00Z",
        "server_modified": "2026-08-02T09:00:00Z",
        "rev": "a1c10ce0dd78",
        "size": 7212,
    })
}

fn folder_metadata() -> JsonValue {
    json!({
        ".tag": "folder",
        "id": "id:a4ayc_80_OEAAAAAAAAAXz",
        "name": "math",
        "path_lower": "/homework/math",
        "path_display": FOLDER,
    })
}

fn shared_link() -> JsonValue {
    json!({
        ".tag": "file",
        "url": "https://www.dropbox.com/s/2sn712vy1ovegw8/Prime_Numbers.txt?dl=0",
        "id": FILE_ID,
        "name": "Prime_Numbers.txt",
        "path_lower": "/homework/math/prime_numbers.txt",
        "link_permissions": { "resolved_visibility": { ".tag": "public" } },
    })
}

/// Every operation, with an input that satisfies it and the documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("file.get_metadata", json!({ "path": PATH })),
        ("folder.list", json!({ "path": FOLDER })),
        ("folder.list_continue", json!({ "cursor": "ZtkX9_EHj3x7" })),
        ("file.search", json!({ "query": "cat" })),
        ("file.search_continue", json!({ "cursor": "ZtkX9_EHj3x7" })),
        ("folder.create", json!({ "path": "/Homework/physics" })),
        ("file.delete", json!({ "path": PATH })),
        ("share_link.create", json!({ "path": PATH })),
    ]
}

/// `dropbox_request_shape`: exact method, path, query, headers, and body for
/// every operation — every one of them a `POST` with a JSON body, which is what
/// Dropbox's RPC style publishes.
#[tokio::test]
async fn dropbox_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("POST", "/2/files/get_metadata")
            .query("")
            .header("content-type", "application/json")
            .json_body(json!({ "path": PATH }))
            .respond_json(200, file_metadata()),
        Expectation::new("POST", "/2/files/list_folder")
            .json_body(json!({ "path": FOLDER, "recursive": false, "limit": 200 }))
            .respond_json(
                200,
                json!({ "entries": [], "cursor": "ZtkX9_EHj3x7", "has_more": false }),
            ),
        Expectation::new("POST", "/2/files/list_folder/continue")
            .json_body(json!({ "cursor": "ZtkX9_EHj3x7" }))
            .respond_json(
                200,
                json!({ "entries": [], "cursor": "ZtkX9_EHj3x7", "has_more": false }),
            ),
        Expectation::new("POST", "/2/files/search_v2")
            .json_body(json!({ "query": "cat" }))
            .respond_json(
                200,
                json!({ "matches": [], "has_more": false, "cursor": null }),
            ),
        Expectation::new("POST", "/2/files/search/continue_v2")
            .json_body(json!({ "cursor": "ZtkX9_EHj3x7" }))
            .respond_json(200, json!({ "matches": [], "has_more": false })),
        Expectation::new("POST", "/2/files/create_folder_v2")
            .json_body(json!({ "path": "/Homework/physics", "autorename": false }))
            .respond_json(200, json!({ "metadata": folder_metadata() })),
        Expectation::new("POST", "/2/files/delete_v2")
            .json_body(json!({ "path": PATH }))
            .respond_json(200, json!({ "metadata": file_metadata() })),
        Expectation::new("POST", "/2/sharing/create_shared_link_with_settings")
            .json_body(json!({ "path": PATH }))
            .respond_json(200, shared_link()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert_eq!(request.method(), "POST", "{id} is a Dropbox RPC call");
        assert_eq!(
            request.url().host_str(),
            Some(
                stub.origin()
                    .as_url()
                    .host_str()
                    .expect("a stub has a host")
            ),
            "{id} renders on the compiled origin"
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// `dropbox_auth_is_applied`: the stored OAuth2 token reaches the wire as
/// `Authorization: Bearer …`, is marked sensitive, and appears nowhere else.
#[tokio::test]
async fn dropbox_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("POST", "/2/files/get_metadata")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, file_metadata())])
    .await;

    let request = render(&stub, "file.get_metadata", json!({ "path": PATH }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        dropbox::connector().credential(),
        dropbox::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");

    // A module whose credential is the stored token cannot render an
    // authenticated request without one.
    let mut unauthorized = operation("file.get_metadata")
        .plan_request(&stub.origin(), &json!({ "path": PATH }))
        .expect("the declared request renders");
    assert!(
        AuthPlan::oauth2_authorization_code()
            .apply(&Credential::from_fields([]), &mut unauthorized, None)
            .is_err(),
        "a stored-credential plan never sends an unauthenticated request"
    );
    stub.assert_satisfied();
}

/// `dropbox_error_map`: every documented status reaches exactly one closed
/// class, and none of Dropbox's prose crosses the boundary.
#[tokio::test]
async fn dropbox_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("POST", "/2/files/get_metadata")
            .respond_header("x-dropbox-request-id", "req-42")
            .respond_json(
                status,
                json!({
                    "error_summary": format!("path/not_found/..{SECRET_SENTINEL}"),
                    "error": { ".tag": "path", "path": { ".tag": "not_found" } },
                    "user_message": { "text": "acme-sandbox could not find that file" },
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "file.get_metadata", json!({ "path": PATH })))
            .await
            .expect("the stub answers");

        let failure = dropbox::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("request_id")
                .map(String::as_str),
            Some("req-42"),
            "the published request id is a safe correlation handle"
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-sandbox", "not_found"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `dropbox_rate_limit_is_classified`: the documented `429` is retryable, its
/// published `Retry-After` becomes the hint, and a hostile one is clamped.
#[tokio::test]
async fn dropbox_rate_limit_is_classified() {
    let limited = json!({ "error_summary": "too_many_requests/..", "error": { ".tag": "too_many_requests" } });
    let stub = ProviderStub::start([
        Expectation::new("POST", "/2/files/get_metadata")
            .respond_header("retry-after", "30")
            .respond_json(429, limited.clone()),
        Expectation::new("POST", "/2/files/get_metadata")
            .respond_header("retry-after", "604800")
            .respond_json(429, limited.clone()),
        // "If a `Retry-After` header is not returned, your app should pause
        // briefly before retrying" — so the connector invents no hint.
        Expectation::new("POST", "/2/files/get_metadata").respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(&stub, "file.get_metadata", json!({ "path": PATH })))
            .await
            .expect("the stub answers");
        failures.push(dropbox::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(failures[0].retry_after(), Some(Duration::from_secs(30)));
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    assert_eq!(failures[2].retry_after(), None);
    stub.assert_satisfied();
}

/// `dropbox_pagination_is_bounded`: this connector declares no continuation
/// plan at all, so every attempt is exactly one request — and the cursor it
/// publishes is an operation input a Process spends, not a plan.
#[tokio::test]
async fn dropbox_pagination_is_bounded() {
    for operation in dropbox::connector().operations() {
        assert!(
            dropbox::pagination(operation.id()).is_none(),
            "`{}` declares no plan: Dropbox's cursor is spent on a different route",
            operation.id()
        );
    }

    // The walk a Process makes instead: the listing's cursor is echoed into the
    // continuation verbatim, and the connector never parses or constructs one.
    const CURSOR: &str = "ZtkX9_EHj3x7PMkVuFIhwKYXEpwpLwyxp9vMKomUhllil9q7eWiAu&limit=9999#/../";
    let stub = ProviderStub::start([
        Expectation::new("POST", "/2/files/list_folder")
            .json_body(json!({ "path": FOLDER, "recursive": false, "limit": 200 }))
            .respond_json(
                200,
                json!({ "entries": [file_metadata()], "cursor": CURSOR, "has_more": true }),
            ),
        Expectation::new("POST", "/2/files/list_folder/continue")
            .json_body(json!({ "cursor": CURSOR }))
            .respond_json(
                200,
                json!({ "entries": [], "cursor": CURSOR, "has_more": false }),
            ),
    ])
    .await;

    let first = stub
        .send(render(&stub, "folder.list", json!({ "path": FOLDER })))
        .await
        .expect("the stub answers");
    let page = operation("folder.list")
        .decode_response(first.status.as_u16(), first.body())
        .expect("the declared contract is satisfied");
    assert_eq!(page.get("has_more"), Some(&json!(true)));
    let cursor = page
        .get("cursor")
        .and_then(JsonValue::as_str)
        .expect("the listing publishes its cursor");

    let second = stub
        .send(render(
            &stub,
            "folder.list_continue",
            json!({ "cursor": cursor }),
        ))
        .await
        .expect("the stub answers");
    assert_eq!(second.status.as_u16(), 200);
    assert_eq!(
        stub.received(),
        2,
        "a Process spends the cursor: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `dropbox_effects_are_classified`: every operation carries a class, the reads
/// are documented reads over `POST`, and the three writes are refused by the
/// method Dropbox serves everything over.
#[test]
fn dropbox_effects_are_classified() {
    let connector = dropbox::connector();
    let expected = [
        ("file.get_metadata", EffectClass::ReadOnly),
        ("folder.list", EffectClass::ReadOnly),
        ("folder.list_continue", EffectClass::ReadOnly),
        ("file.search", EffectClass::ReadOnly),
        ("file.search_continue", EffectClass::ReadOnly),
        ("folder.create", EffectClass::InventoryOnly),
        ("file.delete", EffectClass::InventoryOnly),
        ("share_link.create", EffectClass::InventoryOnly),
    ];
    assert_eq!(connector.operations().len(), expected.len());

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        assert_eq!(
            connector.admit_operation(id).is_ok(),
            class.is_executable(),
            "{id}"
        );
        assert!(operation.idempotency_binding().is_none(), "{id}");
        assert_eq!(operation.method().as_str(), "POST", "{id}");
    }
    assert_eq!(
        connector.admit_operation("file.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    // A read served over `POST` carries the provider statement the gate admits
    // it on, rather than the method's word.
    let assertion = operation("file.get_metadata")
        .effect()
        .and_then(donat_connectors::sdk::Effect::read_only_assertion)
        .expect("a POST read carries its citation");
    let assertion = format!("{assertion:?}");
    assert!(
        assertion.starts_with("ProviderDocumentation"),
        "a read over POST is admitted on the provider's own statement: {assertion}"
    );
    assert!(assertion.contains("RPC endpoints"), "{assertion}");

    // The three writes say the same thing for the same reason, and it is the
    // one the batch found: the method, not a missing consequence.
    for id in ["folder.create", "file.delete", "share_link.create"] {
        let reason = operation(id)
            .effect()
            .and_then(donat_connectors::sdk::Effect::inventory_reason)
            .expect("an inventory-only operation records why");
        assert!(reason.contains("`POST`"), "{id}: {reason}");
        assert!(reason.contains("idempotent in effect"), "{id}: {reason}");
    }
}

/// `dropbox_output_contract`: the declared pointers read Dropbox's own
/// metadata union, and a mistyped or absent required field is a validation
/// failure rather than a null.
#[test]
fn dropbox_output_contract() {
    let get = operation("file.get_metadata");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&file_metadata()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "kind": "file",
            "id": FILE_ID,
            "name": "Prime_Numbers.txt",
            "path_lower": "/homework/math/prime_numbers.txt",
            "path_display": PATH,
            "server_modified": "2026-08-02T09:00:00Z",
            "rev": "a1c10ce0dd78",
            "size": 7212,
        })
    );

    // A folder carries no `rev` and no `size`, and Dropbox's union says so: the
    // optional pointers are null rather than a failure.
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&folder_metadata()).expect("a fixture serializes"),
        )
        .expect("a folder satisfies the same contract")
        .get("size"),
        Some(&JsonValue::Null)
    );

    // "The last component of the path … This never contains a slash" is
    // published as required, so an answer without it is not metadata.
    assert_eq!(
        get.decode_response(200, br#"{".tag":"file","id":"id:1"}"#)
            .expect_err("a metadata answer with no name is not one")
            .class(),
        ConnectorErrorClass::Validation
    );
    // "The file size in bytes" is a number; a string there is a contract
    // violation rather than a coercion.
    assert_eq!(
        get.decode_response(200, br#"{"name":"a.txt","size":"7212"}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    // A non-success status is never a success, whatever the body says.
    assert_eq!(
        get.decode_response(409, br#"{"name":"a.txt"}"#)
            .expect_err("an undeclared status is never a success")
            .class(),
        ConnectorErrorClass::Permanent
    );

    let create = operation("folder.create");
    assert_eq!(
        create
            .decode_response(
                200,
                &serde_json::to_vec(&json!({ "metadata": folder_metadata() }))
                    .expect("a fixture serializes"),
            )
            .expect("the declared contract is satisfied")
            .get("name"),
        Some(&json!("math")),
        "a create reads the nested metadata Dropbox publishes for it"
    );
}
