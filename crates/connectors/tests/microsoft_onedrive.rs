//! Microsoft OneDrive connector proofs (spec 012 §3 plus spec 015 §3), against
//! the SDK's local provider stub.
//!
//! No test here reaches Microsoft, and no test carries a real credential. The
//! deployment-level proofs — `microsoft_onedrive_rotation_survives_crash` and
//! the startup half of the permission check — live in
//! `crates/server/tests/microsoft_365.rs`.

mod microsoft_graph_support;

use donat_connectors::providers::{microsoft_graph, microsoft_onedrive};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use microsoft_graph_support::{
    assert_effects, assert_headers_are_static, assert_next_link_stays_on_origin,
    assert_odata_error_is_typed_by_code, assert_throttling_is_classified, documented_failures,
    graph_error,
};
use serde_json::{Value as JsonValue, json};

const ITEM: &str = "01NKDM7HMOJTVYMDOSXFDK2QJDXCDI3WUK";
const ITEM_PATH: &str = "/v1.0/me/drive/items/01NKDM7HMOJTVYMDOSXFDK2QJDXCDI3WUK";
const ITEM_SELECT: &str = "$select=id%2Cname%2Csize%2CwebUrl%2CeTag%2CcTag%2C\
                           lastModifiedDateTime%2Cfile%2Cfolder%2CparentReference";

/// Microsoft's own sample pre-authenticated download URL: a `*.files.1drv.com`
/// host, not `graph.microsoft.com`.
const DOWNLOAD_URL: &str = "https://b0mpua-by3301.files.1drv.com/y23vmagahszhxzlcvhasdhasghasodfi";

fn operation(id: &str) -> &'static Operation {
    microsoft_onedrive::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the microsoft_onedrive declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    microsoft_graph_support::render(stub, operation(id), input)
}

fn item_body() -> JsonValue {
    json!({
        "id": ITEM,
        "name": "report.xlsx",
        "size": 197,
        "webUrl": "https://contoso-my.sharepoint.com/personal/rgregg_contoso_com/Documents",
        "eTag": "\"{86EB4C8E-98BA-4201-9F7D-1FF5B8D2C4A1},1\"",
        "cTag": "\"c:{86EB4C8E-98BA-4201-9F7D-1FF5B8D2C4A1},0\"",
        "lastModifiedDateTime": "2026-08-10T12:00:00Z",
        "file": { "mimeType": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" },
        "parentReference": { "driveId": "11231001", "id": "1231203102!1011" },
    })
}

/// `microsoft_onedrive_request_shape`.
#[tokio::test]
async fn microsoft_onedrive_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", ITEM_PATH)
            .query(ITEM_SELECT)
            .no_body()
            .respond_json(200, item_body()),
        Expectation::new("GET", &format!("{ITEM_PATH}/children"))
            .query(&format!("{ITEM_SELECT}&$top=200"))
            .respond_json(200, json!({ "value": [item_body()] })),
        Expectation::new("GET", ITEM_PATH)
            .query("$select=id%2Cname%2Csize%2Cfile%2C%40microsoft%2Egraph%2EdownloadUrl")
            .respond_json(
                200,
                json!({ "id": ITEM, "name": "report.xlsx", "@microsoft.graph.downloadUrl": DOWNLOAD_URL }),
            ),
        Expectation::new("POST", &format!("{ITEM_PATH}/copy"))
            .header("content-type", "application/json")
            .json_body(json!({ "name": "copy.xlsx", "parentReference": { "id": "folder-1" } }))
            .respond_bytes(202, ""),
        Expectation::new("PATCH", ITEM_PATH)
            .query(ITEM_SELECT)
            .json_body(json!({ "parentReference": { "id": "folder-2" } }))
            .respond_json(200, item_body()),
        Expectation::new("PATCH", ITEM_PATH)
            .query(ITEM_SELECT)
            .json_body(json!({ "name": "renamed.xlsx" }))
            .respond_json(200, item_body()),
        Expectation::new("DELETE", ITEM_PATH)
            .without_header("prefer")
            .without_header("if-match")
            .respond_bytes(204, ""),
        Expectation::new("POST", &format!("{ITEM_PATH}/children"))
            .query(ITEM_SELECT)
            .json_body(json!({
                "name": "New Folder",
                "folder": {},
                "@microsoft.graph.conflictBehavior": "fail",
            }))
            .respond_json(201, item_body()),
        Expectation::new("GET", "/v1.0/me/drive/root/search(q='Contoso%20Project')")
            .query(&format!("{ITEM_SELECT}&$top=200"))
            .respond_json(200, json!({ "value": [] })),
        // A hostile search term stays inside the OData function call's own
        // quoted argument: the quote is doubled, which is OData's own escape,
        // and then every non-alphanumeric byte is percent-encoded.
        Expectation::new(
            "GET",
            "/v1.0/me/drive/root/search(q='%27%27%29%2F%2E%2E%2Fdrive%2Fitems%2Fother')",
        )
        .respond_json(200, json!({ "value": [] })),
        // ...and a legitimate search term with an apostrophe still searches for
        // what it says.
        Expectation::new("GET", "/v1.0/me/drive/root/search(q='O%27%27Brien')")
            .respond_json(200, json!({ "value": [] })),
    ])
    .await;

    for (id, input) in [
        ("file.get", json!({ "item_id": ITEM })),
        ("file.list_children", json!({ "item_id": ITEM })),
        ("file.download", json!({ "item_id": ITEM })),
        (
            "file.copy",
            json!({ "item_id": ITEM, "name": "copy.xlsx", "parent_id": "folder-1" }),
        ),
        (
            "file.move",
            json!({ "item_id": ITEM, "parent_id": "folder-2" }),
        ),
        (
            "file.rename",
            json!({ "item_id": ITEM, "name": "renamed.xlsx" }),
        ),
        ("file.delete", json!({ "item_id": ITEM })),
        (
            "folder.create",
            json!({ "item_id": ITEM, "name": "New Folder" }),
        ),
        ("item.search", json!({ "query": "Contoso Project" })),
    ] {
        let request = render(&stub, id, input);
        stub.send(request).await.expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "item.search",
        json!({ "query": "')/../drive/items/other" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert!(
        hostile.url().path().ends_with("')"),
        "the closing quote and parenthesis are the declaration's: {}",
        hostile.url().path()
    );
    assert!(
        hostile
            .url()
            .path()
            .matches("%27")
            .count()
            .is_multiple_of(2),
        "an odd number of quotes would end the OData literal once the receiver decodes them; \
         the declaration doubles every quote the value carries: {}",
        hostile.url().path()
    );
    stub.send(hostile).await.expect("the stub answers");

    let apostrophe = render(&stub, "item.search", json!({ "query": "O'Brien" }));
    stub.send(apostrophe)
        .await
        .expect("a legitimate apostrophe is one literal");
    stub.assert_satisfied();
}

/// `microsoft_onedrive_auth_is_applied`.
#[tokio::test]
async fn microsoft_onedrive_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", ITEM_PATH)
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, item_body())])
    .await;

    let request = render(&stub, "file.get", json!({ "item_id": ITEM }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let failure = microsoft_onedrive::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        microsoft_onedrive::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    assert!(
        microsoft_onedrive::connector()
            .credential()
            .fields()
            .is_empty()
    );
    stub.assert_satisfied();

    let mut bare = operation("file.get")
        .plan_request(&stub.origin(), &json!({ "item_id": ITEM }))
        .expect("the declared request renders");
    assert_eq!(
        AuthPlan::oauth2_authorization_code()
            .apply(&Credential::from_fields([]), &mut bare, None)
            .expect_err("an unauthorized Graph request is never sent")
            .class(),
        ConnectorErrorClass::Invariant
    );
}

/// `microsoft_onedrive_error_map`.
#[tokio::test]
async fn microsoft_onedrive_error_map() {
    for (status, code, expected) in documented_failures() {
        let stub = ProviderStub::start([
            Expectation::new("GET", ITEM_PATH).respond_json(status, graph_error(code))
        ])
        .await;
        let response = stub
            .send(render(&stub, "file.get", json!({ "item_id": ITEM })))
            .await
            .expect("the stub answers");

        let failure = microsoft_onedrive::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert!(
            microsoft_onedrive::decode(
                operation("file.get"),
                status,
                response.headers(),
                response.body(),
            )
            .is_err(),
            "status {status} is not a declared success"
        );
        let surface = format!(
            "{} {} {}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal"] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        stub.assert_satisfied();
    }
}

/// `microsoft_onedrive_odata_error_is_typed`.
#[test]
fn microsoft_onedrive_odata_error_is_typed() {
    assert_odata_error_is_typed_by_code();

    let failure = microsoft_onedrive::decode(
        operation("file.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"error":{"code":"itemNotFound","message":"The resource could not be found."}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(
        failure.code(),
        microsoft_graph::SUCCESS_CARRIES_ERROR.code()
    );
}

/// `microsoft_onedrive_throttling_is_classified`.
#[tokio::test]
async fn microsoft_onedrive_throttling_is_classified() {
    assert_throttling_is_classified(
        "GET",
        ITEM_PATH,
        operation("file.get"),
        json!({ "item_id": ITEM }),
    )
    .await;
}

/// `microsoft_onedrive_next_link_stays_on_origin`, and its opposite: the
/// pre-authenticated download URL is a provider-chosen absolute URL on a host
/// Microsoft does not publish, and it is decoded as **data** rather than
/// followed.
#[tokio::test]
async fn microsoft_onedrive_next_link_stays_on_origin() {
    for (id, path, input) in [
        (
            "file.list_children",
            format!("{ITEM_PATH}/children"),
            json!({ "item_id": ITEM }),
        ),
        (
            "item.search",
            "/v1.0/me/drive/root/search(q='q')".to_owned(),
            json!({ "query": "q" }),
        ),
    ] {
        let plan = microsoft_onedrive::pagination(id)
            .unwrap_or_else(|| panic!("{id} declares a continuation plan"));
        assert_next_link_stays_on_origin(plan, operation(id), input, "GET", &path, "/value").await;
    }

    for id in ["file.get", "file.download", "file.delete", "folder.create"] {
        assert!(microsoft_onedrive::pagination(id).is_none(), "{id}");
    }

    // The download URL is on a foreign host and is still a perfectly good
    // output: it is data a Process may read, and this connector has no plan
    // that could turn it into a request.
    let decoded = microsoft_onedrive::decode(
        operation("file.download"),
        200,
        &reqwest::header::HeaderMap::new(),
        &serde_json::to_vec(&json!({ "id": ITEM, "@microsoft.graph.downloadUrl": DOWNLOAD_URL }))
            .expect("a fixture body serializes"),
    )
    .expect("the declared contract is satisfied");
    assert_eq!(
        decoded.get("download_url").and_then(JsonValue::as_str),
        Some(DOWNLOAD_URL)
    );
}

/// `microsoft_onedrive_consistency_headers_are_declared`.
///
/// This connector declares no request header at all, and the two Microsoft
/// documents for these operations are deliberately absent: `if-match`, which
/// would be a precondition value from input, and `prefer: bypass-shared-lock`,
/// which would let a Process override somebody else's coauthoring lock per
/// call.
#[test]
fn microsoft_onedrive_consistency_headers_are_declared() {
    assert_headers_are_static(microsoft_onedrive::connector(), &[]);

    for operation in microsoft_onedrive::connector().operations() {
        let projection = operation.project();
        for header in projection.headers() {
            assert!(
                !matches!(header.name(), "if-match" | "prefer"),
                "`{}` must not send `{}`",
                operation.id(),
                header.name()
            );
        }
        assert!(
            projection
                .inputs()
                .iter()
                .all(|input| !matches!(input.name(), "if_match" | "etag" | "prefer")),
            "`{}` publishes no precondition or lock-override slot",
            operation.id()
        );
    }
}

/// `microsoft_onedrive_effects_are_classified`.
#[test]
fn microsoft_onedrive_effects_are_classified() {
    assert_effects(
        microsoft_onedrive::connector(),
        &[
            ("file.get", EffectClass::ReadOnly),
            ("file.list_children", EffectClass::ReadOnly),
            ("file.download", EffectClass::ReadOnly),
            ("file.copy", EffectClass::AtMostOnce),
            ("file.move", EffectClass::InventoryOnly),
            ("file.rename", EffectClass::InventoryOnly),
            ("file.delete", EffectClass::ProviderIdempotentNaturalMethod),
            ("folder.create", EffectClass::AtMostOnce),
            ("item.search", EffectClass::ReadOnly),
        ],
    );

    assert_eq!(
        microsoft_onedrive::connector().admit_operation("file.upload"),
        Err(OperationRejection::Undeclared),
        "upload sessions are out of scope for this batch, so no operation publishes one"
    );
}

/// The permission table is complete and per operation group.
#[test]
fn microsoft_onedrive_permissions_are_declared_per_operation_group() {
    let connector = microsoft_onedrive::connector();
    let least = microsoft_graph::declared_permissions(
        connector.operations(),
        microsoft_onedrive::permissions,
    )
    .expect("every operation declares the permissions Microsoft documents for it");
    assert_eq!(least, vec!["Files.Read", "Files.ReadWrite"]);

    let reads = ["file.get".to_owned(), "file.list_children".to_owned()];
    assert!(
        microsoft_graph::permission_report(
            microsoft_onedrive::permissions,
            &reads,
            &["Files.Read".to_owned()],
        )
        .is_empty(),
        "a read-only deployment is never forced to grant a write permission"
    );
    let with_delete = [reads.as_slice(), &["file.delete".to_owned()]].concat();
    assert_eq!(
        microsoft_graph::permission_report(
            microsoft_onedrive::permissions,
            &with_delete,
            &["Files.Read".to_owned()],
        )
        .missing,
        vec![("file.delete".to_owned(), "Files.ReadWrite")]
    );
    // `Files.ReadWrite` authorizes the reads too, and is then not surplus.
    assert!(
        microsoft_graph::permission_report(
            microsoft_onedrive::permissions,
            &with_delete,
            &["Files.ReadWrite".to_owned()],
        )
        .is_empty()
    );
}

/// `microsoft_onedrive_output_contract`.
#[test]
fn microsoft_onedrive_output_contract() {
    let get = operation("file.get");
    assert_eq!(
        microsoft_onedrive::decode(
            get,
            200,
            &reqwest::header::HeaderMap::new(),
            &serde_json::to_vec(&item_body()).expect("a fixture body serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": ITEM,
            "name": "report.xlsx",
            "size": 197,
            "web_url": "https://contoso-my.sharepoint.com/personal/rgregg_contoso_com/Documents",
            "etag": "\"{86EB4C8E-98BA-4201-9F7D-1FF5B8D2C4A1},1\"",
            "ctag": "\"c:{86EB4C8E-98BA-4201-9F7D-1FF5B8D2C4A1},0\"",
            "last_modified_at": "2026-08-10T12:00:00Z",
            "mime_type": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "child_count": null,
            "parent_id": "1231203102!1011",
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );

    for body in [
        br#"{"name":"no id"}"#.as_slice(),
        br#"{"id":null}"#.as_slice(),
        br#"{"id":7}"#.as_slice(),
        br#"not json at all"#.as_slice(),
    ] {
        assert_eq!(
            microsoft_onedrive::decode(get, 200, &reqwest::header::HeaderMap::new(), body)
                .expect_err("a missing, mistyped, or unparseable body is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    // The two documented empty successes decode as the empty answer.
    for (id, status) in [("file.delete", 204u16), ("file.copy", 202)] {
        assert_eq!(
            microsoft_onedrive::decode(
                operation(id),
                status,
                &reqwest::header::HeaderMap::new(),
                b"",
            )
            .expect("a documented empty success is a success"),
            json!({}),
            "{id}"
        );
    }
}
