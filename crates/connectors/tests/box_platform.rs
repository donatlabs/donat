//! Box connector proofs (spec 025 §4), against the SDK's local provider stub.

use std::time::Duration;

use donat_connectors::providers::box_platform;
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AccessToken, AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation,
    OperationRejection, PaginationBudget, RequestPlan, undeclared_status_gate,
};
use serde_json::{Value as JsonValue, json};

const FILE_ID: &str = "12345";
const FOLDER_ID: &str = "0";
const FILE_FIELDS: &str = "id,type,etag,name,size,created_at,modified_at,parent,shared_link";
const FOLDER_FIELDS: &str = "id,type,etag,name,created_at,modified_at,parent,item_status";
const ITEM_FIELDS: &str = "id,type,etag,name,size,modified_at";

fn operation(id: &str) -> &'static Operation {
    box_platform::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the box declaration publishes {id}"))
}

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

fn file() -> JsonValue {
    json!({
        "id": FILE_ID,
        "type": "file",
        "etag": "1",
        "name": "Contract.pdf",
        "size": 629_644,
        "created_at": "2026-08-01T09:00:00-07:00",
        "modified_at": "2026-08-02T09:00:00-07:00",
        "parent": { "id": FOLDER_ID, "type": "folder", "name": "All Files" },
        "shared_link": null,
    })
}

fn folder() -> JsonValue {
    json!({
        "id": "77777",
        "type": "folder",
        "etag": "1",
        "name": "Contracts",
        "created_at": "2026-08-01T09:00:00-07:00",
        "modified_at": "2026-08-02T09:00:00-07:00",
        "parent": { "id": FOLDER_ID, "type": "folder" },
        "item_status": "active",
    })
}

/// Every operation, with an input that satisfies it and the documented success.
fn cases() -> Vec<(&'static str, JsonValue)> {
    vec![
        ("file.get", json!({ "file_id": FILE_ID })),
        ("folder.get", json!({ "folder_id": FOLDER_ID })),
        ("folder.items", json!({ "folder_id": FOLDER_ID })),
        ("search", json!({ "query": "contract" })),
        (
            "folder.create",
            json!({ "name": "Contracts", "parent_id": FOLDER_ID }),
        ),
        ("file.delete", json!({ "file_id": FILE_ID })),
        ("folder.delete", json!({ "folder_id": "77777" })),
        ("file.share_link_create", json!({ "file_id": FILE_ID })),
    ]
}

/// `box_request_shape`: exact method, path, query, headers, and body for every
/// operation, including the field mask each one pins.
#[tokio::test]
async fn box_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/2.0/files/{FILE_ID}"))
            .query(&format!("fields={}", encoded(FILE_FIELDS)))
            .no_body()
            .respond_json(200, file()),
        Expectation::new("GET", &format!("/2.0/folders/{FOLDER_ID}"))
            .query(&format!("fields={}", encoded(FOLDER_FIELDS)))
            .respond_json(200, folder()),
        Expectation::new("GET", &format!("/2.0/folders/{FOLDER_ID}/items"))
            .query(&format!("usemarker=true&fields={}", encoded(ITEM_FIELDS)))
            .respond_json(200, json!({ "entries": [], "limit": 200 })),
        Expectation::new("GET", "/2.0/search")
            .query(&format!("query=contract&fields={}", encoded(ITEM_FIELDS)))
            .respond_json(200, json!({ "entries": [], "total_count": 0 })),
        Expectation::new("POST", "/2.0/folders")
            .query(&format!("fields={}", encoded(FOLDER_FIELDS)))
            .json_body(json!({ "name": "Contracts", "parent": { "id": FOLDER_ID } }))
            .respond_json(201, folder()),
        Expectation::new("DELETE", &format!("/2.0/files/{FILE_ID}"))
            .query("")
            .respond_json(204, json!(null)),
        Expectation::new("DELETE", "/2.0/folders/77777")
            .query("recursive=false")
            .respond_json(204, json!(null)),
        Expectation::new("PUT", &format!("/2.0/files/{FILE_ID}"))
            .query(&format!("fields={}", encoded(FILE_FIELDS)))
            .json_body(json!({ "shared_link": {} }))
            .respond_json(200, file()),
    ])
    .await;

    for (id, input) in cases() {
        let request = render(&stub, id, input);
        assert!(
            request.url().path().starts_with("/2.0/"),
            "{id} renders a published Box path: {}",
            request.url().path()
        );
        stub.send(request).await.expect("the stub answers");
    }
    stub.assert_satisfied();
}

/// The percent encoding the SDK applies to a static query value.
fn encoded(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_string()
            } else {
                format!("%{:02X}", character as u32)
            }
        })
        .collect()
}

/// `box_auth_is_applied`: the stored OAuth2 token reaches the wire as
/// `Authorization: Bearer …` and appears nowhere else.
#[tokio::test]
async fn box_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/2.0/files/{FILE_ID}"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, file())])
    .await;

    let request = render(&stub, "file.get", json!({ "file_id": FILE_ID }));
    assert!(
        request
            .headers()
            .get("authorization")
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!request.url().as_str().contains(SECRET_SENTINEL));
    assert!(!request.url_carries_credential());

    let response = stub.send(request).await.expect("the stub answers");
    let surface = format!(
        "{:?} {:?}",
        box_platform::connector().credential(),
        box_platform::error_map().classify_response(&response),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    stub.assert_satisfied();
}

/// `box_error_map`: every documented status reaches exactly one closed class,
/// and none of Box's prose crosses the boundary.
#[tokio::test]
async fn box_error_map() {
    let documented = [
        (400, ConnectorErrorClass::Validation),
        (401, ConnectorErrorClass::Authentication),
        (403, ConnectorErrorClass::Permanent),
        (404, ConnectorErrorClass::Permanent),
        (405, ConnectorErrorClass::Permanent),
        (409, ConnectorErrorClass::Permanent),
        (410, ConnectorErrorClass::Permanent),
        (411, ConnectorErrorClass::Validation),
        (412, ConnectorErrorClass::Permanent),
        (413, ConnectorErrorClass::Validation),
        (415, ConnectorErrorClass::Validation),
        (429, ConnectorErrorClass::Http429),
        (500, ConnectorErrorClass::Http5xx),
        (502, ConnectorErrorClass::Http5xx),
        (503, ConnectorErrorClass::Http5xx),
        // An undocumented status still lands in the closed set.
        (418, ConnectorErrorClass::Permanent),
    ];

    for (status, expected) in documented {
        let stub = ProviderStub::start([Expectation::new("GET", &format!("/2.0/files/{FILE_ID}"))
            .respond_header("box-request-id", "abcdef123456")
            .respond_json(
                status,
                json!({
                    "type": "error",
                    "status": status,
                    "code": "item_name_invalid",
                    "message": format!("acme-sandbox rejected token {SECRET_SENTINEL}"),
                    "request_id": "abcdef123456",
                }),
            )])
        .await;
        let response = stub
            .send(render(&stub, "file.get", json!({ "file_id": FILE_ID })))
            .await
            .expect("the stub answers");

        let failure = box_platform::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status}");
        assert_eq!(failure.provider_status(), Some(status));
        assert_eq!(
            failure
                .correlation_ids()
                .get("request_id")
                .map(String::as_str),
            Some("abcdef123456")
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "acme-sandbox", "item_name_invalid"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `box_rate_limit_is_classified`: the documented `429` is retryable, its
/// published `retry-after` becomes the hint, and a hostile one is clamped.
#[tokio::test]
async fn box_rate_limit_is_classified() {
    let limited = json!({
        "type": "error", "status": 429, "code": "rate_limit_exceeded",
        "message": "Request rate limit exceeded, try again later",
    });
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/2.0/files/{FILE_ID}"))
            .respond_header("retry-after", "100")
            .respond_json(429, limited.clone()),
        Expectation::new("GET", &format!("/2.0/files/{FILE_ID}"))
            .respond_header("retry-after", "604800")
            .respond_json(429, limited),
    ])
    .await;

    let mut failures = Vec::new();
    for _ in 0..2 {
        let response = stub
            .send(render(&stub, "file.get", json!({ "file_id": FILE_ID })))
            .await
            .expect("the stub answers");
        failures.push(box_platform::error_map().classify_response(&response));
    }
    assert_eq!(failures[0].class(), ConnectorErrorClass::Http429);
    assert_eq!(
        failures[0].retry_after(),
        Some(Duration::from_secs(100)),
        "Box's own published example is `retry-after: 100`"
    );
    assert_eq!(
        failures[1].retry_after(),
        Some(Duration::from_secs(86_400)),
        "a week is clamped to the SDK ceiling"
    );
    stub.assert_satisfied();
}

/// `box_cursor_is_opaque_and_bounded` (spec 023 §4 proof 3): the marker is
/// echoed back verbatim, is never parsed or constructed here, and the walk makes
/// exactly the number of requests the plan declares.
#[tokio::test]
async fn box_cursor_is_opaque_and_bounded() {
    let plan = box_platform::pagination("folder.items").expect("the listing declares a plan");
    let budget = PaginationBudget::new(8, 8, 1_000, 256 * 1024, Duration::from_secs(5));
    const MARKER: &str = "JV9IRGZmieiBasejOG9yDCRNgd2&limit=9999#/../";

    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/2.0/folders/{FOLDER_ID}/items"))
            .query(&format!(
                "usemarker=true&fields={}&limit=200",
                encoded(ITEM_FIELDS)
            ))
            .respond_json(
                200,
                json!({ "entries": [{ "id": "1" }], "limit": 200, "next_marker": MARKER }),
            ),
        Expectation::new("GET", &format!("/2.0/folders/{FOLDER_ID}/items"))
            .query(&format!(
                "usemarker=true&fields={}&limit=200&marker={}",
                encoded(ITEM_FIELDS),
                encoded(MARKER)
            ))
            // "The marker for the start of the next page of results", absent
            // when there is none.
            .respond_json(200, json!({ "entries": [{ "id": "2" }], "limit": 200 })),
    ])
    .await;

    let items = plan
        .collect(
            render(&stub, "folder.items", json!({ "folder_id": FOLDER_ID })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the walk follows one marker and stops on the absent one");
    assert_eq!(items, vec![json!({ "id": "1" }), json!({ "id": "2" })]);
    assert_eq!(
        stub.received(),
        2,
        "a declared walk is the executor's walk: two pages are two requests"
    );
    stub.assert_satisfied();
}

/// `box_pagination_is_bounded`: every declared plan terminates and respects the
/// call, page, item, and byte budgets, and the operations that declare none send
/// one request.
#[tokio::test]
async fn box_pagination_is_bounded() {
    let plan = box_platform::pagination("folder.items").expect("the listing declares a plan");
    for budget in [
        PaginationBudget::new(2, 8, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 1_000, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1, 1 << 20, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 1_000, 1, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|_| {
            Expectation::new("GET", &format!("/2.0/folders/{FOLDER_ID}/items")).respond_json(
                200,
                json!({ "entries": [{ "id": "1" }, { "id": "2" }], "next_marker": "more" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, "folder.items", json!({ "folder_id": FOLDER_ID })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // The search walks Box's own offset regime and stops on a short page.
    let search = box_platform::pagination("search").expect("the search declares a plan");
    assert_eq!(search.items_pointer(), "/entries");

    for id in [
        "file.get",
        "folder.get",
        "folder.create",
        "file.delete",
        "folder.delete",
        "file.share_link_create",
    ] {
        assert!(
            box_platform::pagination(id).is_none(),
            "{id} declares no continuation plan"
        );
    }
}

/// `box_effects_are_classified`: every operation carries a class, the one
/// executable write is the delete whose repeat Box publishes, and the folder
/// delete beside it is refused for the silence in the same table.
#[test]
fn box_effects_are_classified() {
    let connector = box_platform::connector();
    let expected = [
        ("file.get", EffectClass::ReadOnly),
        ("folder.get", EffectClass::ReadOnly),
        ("folder.items", EffectClass::ReadOnly),
        ("search", EffectClass::ReadOnly),
        ("folder.create", EffectClass::InventoryOnly),
        ("file.delete", EffectClass::ProviderIdempotentNaturalMethod),
        ("folder.delete", EffectClass::InventoryOnly),
        ("file.share_link_create", EffectClass::InventoryOnly),
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
        connector.admit_operation("folder.delete"),
        Err(OperationRejection::InventoryOnly)
    );

    // The file delete carries the provider's own repeat statement, and the
    // folder delete records the sentence Box did *not* publish for it.
    let citation = format!("{:?}", operation("file.delete").effect());
    assert!(
        citation.contains("or has already been deleted"),
        "{citation}"
    );
    let folder_reason = operation("folder.delete")
        .effect()
        .and_then(donat_connectors::sdk::Effect::inventory_reason)
        .expect("an inventory-only operation records why");
    assert!(folder_reason.contains("says nothing about a second send"));
    assert!(folder_reason.contains("600 seconds"));
}

/// `box_download_is_a_redirect_to_a_third_origin` (spec 025 §2): the byte
/// surface is not declared here, and the reason is recorded rather than
/// implied — Box's bytes live on a host no deployment named.
#[test]
fn box_download_is_a_redirect_to_a_third_origin() {
    assert!(
        box_platform::connector()
            .operation("file.download")
            .is_none(),
        "Box publishes `302 … a Location header for the file on dl.boxcloud.com`, an origin \
         nothing in this workspace compiled and which Box says is not persistent"
    );

    // Every request this connector *can* render stays on the compiled origin,
    // so there is no path by which a download could leave it either.
    let origin = donat_connectors::sdk::Origin::parse("https://api.box.com")
        .expect("the published origin is valid");
    for (id, input) in cases() {
        let request = operation(id)
            .plan_request(&origin, &input)
            .expect("the declared request renders");
        assert_eq!(
            request.url().host_str(),
            Some("api.box.com"),
            "{id} renders on the compiled origin"
        );
    }
}

/// `box_output_contract`: the declared pointers read the fields the pinned mask
/// asked for, and a mistyped or absent required field is a validation failure.
#[test]
fn box_output_contract() {
    let get = operation("file.get");
    assert_eq!(
        get.decode_response(
            200,
            &serde_json::to_vec(&file()).expect("a fixture serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": FILE_ID,
            "type": "file",
            "etag": "1",
            "name": "Contract.pdf",
            "size": 629_644,
            "created_at": "2026-08-01T09:00:00-07:00",
            "modified_at": "2026-08-02T09:00:00-07:00",
            "parent": { "id": FOLDER_ID, "type": "folder", "name": "All Files" },
            "shared_link": JsonValue::Null,
        })
    );
    // Box publishes every identifier as a string; a number there is a contract
    // violation rather than a coercion.
    assert_eq!(
        get.decode_response(200, br#"{"id":12345}"#)
            .expect_err("a mistyped pointer is a validation failure")
            .class(),
        ConnectorErrorClass::Validation
    );
    assert_eq!(
        get.decode_response(200, br#"{"type":"file"}"#)
            .expect_err("an answer with no id is not a file")
            .class(),
        ConnectorErrorClass::Validation
    );

    // A delete answers `204` with no body, and the declaration says so.
    let delete = operation("file.delete");
    assert!(delete.is_no_content_success(204));
    assert_eq!(
        delete
            .decode_response(204, b"")
            .expect("an empty success is the provider's own answer"),
        json!({}),
        "a documented empty success is an empty output, not a failure"
    );
}
