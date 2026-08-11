//! Microsoft Excel connector proofs (spec 012 §3 plus spec 015 §3), against the
//! SDK's local provider stub.
//!
//! No test here reaches Microsoft, and no test carries a real credential. The
//! deployment-level proofs — `microsoft_excel_rotation_survives_crash` and the
//! startup half of the permission check — live in
//! `crates/server/tests/microsoft_365.rs`.

mod microsoft_graph_support;

use std::time::Duration;

use donat_connectors::providers::{microsoft_excel, microsoft_graph};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    PaginationBudget, RequestPlan,
};
use microsoft_graph_support::{
    assert_effects, assert_headers_are_static, assert_next_link_stays_on_origin,
    assert_odata_error_is_typed_by_code, assert_throttling_is_classified, documented_failures,
    graph_error,
};
use serde_json::{Value as JsonValue, json};

const ITEM: &str = "01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ";
/// Microsoft's own worksheet id form, with the braces it says "need to be URL
/// encoded for the API to work".
const WORKSHEET: &str = "{FC034FA8-F8CC-4D24-9C0A-02A81B7792A0}";
const WORKBOOK: &str = "/v1.0/me/drive/items/01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ/workbook";
const RANGE_PATH: &str = "/v1.0/me/drive/items/01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ/workbook/worksheets/\
     %7BFC034FA8%2DF8CC%2D4D24%2D9C0A%2D02A81B7792A0%7D/range(address='test%21A1%3AB2')";

fn operation(id: &str) -> &'static Operation {
    microsoft_excel::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the microsoft_excel declaration publishes {id}"))
}

fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    microsoft_graph_support::render(stub, operation(id), input)
}

/// One range as Microsoft's own *Get Range* example prints it.
fn range_body() -> JsonValue {
    json!({
        "address": "test!A1:B2",
        "addressLocal": "test!A1:B2",
        "cellCount": 4,
        "columnCount": 2,
        "rowCount": 2,
        "values": [["Test", "Value"], ["For", "Update"]],
        "formulas": [["Test", "Value"], ["For", "Update"]],
        "text": [["Test", "Value"], ["For", "Update"]],
        "valueTypes": [["String", "String"], ["String", "String"]],
    })
}

/// `microsoft_excel_request_shape`: exact method, path, query, headers, and body
/// for every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn microsoft_excel_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("{WORKBOOK}/worksheets"))
            .query("")
            .no_body()
            .respond_json(
                200,
                json!({ "value": [{ "id": WORKSHEET, "name": "Sheet1" }] }),
            ),
        Expectation::new("GET", RANGE_PATH)
            .query("")
            .no_body()
            .respond_json(200, range_body()),
        Expectation::new("PATCH", RANGE_PATH)
            .header("content-type", "application/json")
            .json_body(json!({ "values": [["Test", "Value"], ["For", "Update"]] }))
            .respond_json(200, range_body()),
        Expectation::new("GET", &format!("{WORKBOOK}/tables"))
            .respond_json(200, json!({ "value": [{ "id": "1", "name": "Table1" }] })),
        Expectation::new("GET", &format!("{WORKBOOK}/tables/Table1/rows")).respond_json(
            200,
            json!({ "value": [{ "index": 0, "values": [[1, 2, 3]] }] }),
        ),
        Expectation::new("POST", &format!("{WORKBOOK}/tables/Table1/rows/add"))
            .json_body(json!({ "values": [[1, 2, 3]] }))
            .respond_json(200, json!({ "index": 1, "values": [[1, 2, 3]] })),
        Expectation::new(
            "GET",
            "/v1.0/me/drive/items/01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ/children",
        )
        .query("$select=id%2Cname%2Csize%2Cfile%2Cfolder%2CwebUrl%2ClastModifiedDateTime&$top=200")
        .respond_json(200, json!({ "value": [] })),
        // A hostile range address stays inside the OData function call's own
        // quoted argument: the quote is doubled, which is OData's own escape,
        // and then every non-alphanumeric byte is percent-encoded.
        Expectation::new(
            "GET",
            "/v1.0/me/drive/items/01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ/workbook/worksheets/Sheet1/\
             range(address='%27%27%29%2F%2E%2E%2Fworkbook%2FcloseSession%3Fx%3D1')",
        )
        .respond_json(200, range_body()),
        // ...and a legitimate address with a quoted sheet name renders as one
        // literal too: `'My Sheet'!A1:B2` is what Microsoft's own A1 notation
        // looks like when a sheet name has a space.
        Expectation::new(
            "GET",
            "/v1.0/me/drive/items/01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ/workbook/worksheets/Sheet1/\
             range(address='%27%27My%20Sheet%27%27%21A1%3AB2')",
        )
        .respond_json(200, range_body()),
    ])
    .await;

    let values = json!([["Test", "Value"], ["For", "Update"]]);
    for (id, input) in [
        ("worksheet.list", json!({ "item_id": ITEM })),
        (
            "worksheet.get_range",
            json!({ "item_id": ITEM, "worksheet": WORKSHEET, "address": "test!A1:B2" }),
        ),
        (
            "worksheet.update_range",
            json!({
                "item_id": ITEM,
                "worksheet": WORKSHEET,
                "address": "test!A1:B2",
                "values": values,
            }),
        ),
        ("table.list", json!({ "item_id": ITEM })),
        (
            "table.get_rows",
            json!({ "item_id": ITEM, "table": "Table1" }),
        ),
        (
            "table.add_row",
            json!({ "item_id": ITEM, "table": "Table1", "values": [[1, 2, 3]] }),
        ),
        ("workbook.list", json!({ "item_id": ITEM })),
    ] {
        let request = render(&stub, id, input);
        stub.send(request).await.expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "worksheet.get_range",
        json!({
            "item_id": ITEM,
            "worksheet": "Sheet1",
            "address": "')/../workbook/closeSession?x=1",
        }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None, "and cannot add a query");
    assert!(
        hostile.url().path().ends_with("')"),
        "the closing quote and parenthesis are the declaration's, not the value's: {}",
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

    let quoted = render(
        &stub,
        "worksheet.get_range",
        json!({
            "item_id": ITEM,
            "worksheet": "Sheet1",
            "address": "'My Sheet'!A1:B2",
        }),
    );
    stub.send(quoted)
        .await
        .expect("a legitimate quoted sheet name is one literal");
    stub.assert_satisfied();
}

/// `microsoft_excel_auth_is_applied`.
#[tokio::test]
async fn microsoft_excel_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("{WORKBOOK}/worksheets"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .respond_json(200, json!({ "value": [] }))])
    .await;

    let request = render(&stub, "worksheet.list", json!({ "item_id": ITEM }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));

    let response = stub.send(request).await.expect("the stub answers");
    let failure = microsoft_excel::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        microsoft_excel::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    assert!(
        microsoft_excel::connector()
            .credential()
            .fields()
            .is_empty()
    );
    stub.assert_satisfied();

    let mut bare = operation("worksheet.list")
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

/// `microsoft_excel_error_map`.
#[tokio::test]
async fn microsoft_excel_error_map() {
    for (status, code, expected) in documented_failures() {
        let stub =
            ProviderStub::start([Expectation::new("GET", &format!("{WORKBOOK}/worksheets"))
                .respond_json(status, graph_error(code))])
            .await;
        let response = stub
            .send(render(&stub, "worksheet.list", json!({ "item_id": ITEM })))
            .await
            .expect("the stub answers");

        let failure = microsoft_excel::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} code {code}");
        assert!(
            microsoft_excel::decode(
                operation("worksheet.list"),
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

/// `microsoft_excel_odata_error_is_typed`.
#[test]
fn microsoft_excel_odata_error_is_typed() {
    assert_odata_error_is_typed_by_code();

    let failure = microsoft_excel::decode(
        operation("worksheet.get_range"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"error":{"code":"invalidRequest","message":"The range is unbounded."}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(
        failure.code(),
        microsoft_graph::SUCCESS_CARRIES_ERROR.code()
    );
}

/// `microsoft_excel_throttling_is_classified`.
#[tokio::test]
async fn microsoft_excel_throttling_is_classified() {
    assert_throttling_is_classified(
        "GET",
        &format!("{WORKBOOK}/worksheets"),
        operation("worksheet.list"),
        json!({ "item_id": ITEM }),
    )
    .await;
}

/// `microsoft_excel_next_link_stays_on_origin` (and
/// `microsoft_excel_pagination_is_bounded`).
///
/// Only the Drive listing walks `@odata.nextLink`. The three workbook
/// collections page by offset, which is the stronger property: their plan
/// spends provider values nowhere at all, so there is no destination to check.
#[tokio::test]
async fn microsoft_excel_next_link_stays_on_origin() {
    let plan = microsoft_excel::pagination("workbook.list").expect("the Drive listing has a plan");
    assert_next_link_stays_on_origin(
        plan,
        operation("workbook.list"),
        json!({ "item_id": ITEM }),
        "GET",
        "/v1.0/me/drive/items/01CCETFLK7GVZTZHSQNRD2AEI5XWTCU6FJ/children",
        "/value",
    )
    .await;

    // The workbook collections walk `$top`/`$skip`, which Microsoft documents
    // for them in place of a continuation, and stop at a short page. A body
    // that spells an absolute URL changes no destination, because this plan has
    // no way to spend one.
    let path = format!("{WORKBOOK}/tables");
    let stub = ProviderStub::start([
        Expectation::new("GET", &path)
            .query("$skip=0&$top=100")
            .respond_json(
                200,
                json!({
                    "value": (0..100).map(|index| json!({ "id": index.to_string() }))
                        .collect::<Vec<_>>(),
                    "@odata.nextLink": "https://attacker.invalid/v1.0/me/drive",
                }),
            ),
        Expectation::new("GET", &path)
            .query("$skip=100&$top=100")
            .respond_json(200, json!({ "value": [{ "id": "100" }] })),
    ])
    .await;
    let plan = microsoft_excel::pagination("table.list").expect("the table listing has a plan");
    let first = render(&stub, "table.list", json!({ "item_id": ITEM }));
    let collected = plan
        .collect(
            first,
            &stub.origin(),
            &PaginationBudget::new(8, 8, 256, 512 * 1024, Duration::from_secs(5)),
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("an offset walk cannot be redirected by a body");
    assert_eq!(collected.len(), 101);
    stub.assert_satisfied();

    for id in [
        "worksheet.get_range",
        "worksheet.update_range",
        "table.add_row",
    ] {
        assert!(microsoft_excel::pagination(id).is_none(), "{id}");
    }
}

/// `microsoft_excel_consistency_headers_are_declared`.
///
/// This connector is sessionless by declaration: no operation sends
/// `workbook-session-id`, and none may take one from input — a session id from
/// operation input could redirect a deployment's writes into a non-persistent
/// session whose "changes made by the API aren't saved to the source location".
#[test]
fn microsoft_excel_consistency_headers_are_declared() {
    assert_headers_are_static(microsoft_excel::connector(), &[]);

    for operation in microsoft_excel::connector().operations() {
        let projection = operation.project();
        assert!(
            projection
                .headers()
                .iter()
                .all(|header| header.name() != "workbook-session-id"),
            "`{}` must stay sessionless",
            operation.id()
        );
        assert!(
            projection
                .inputs()
                .iter()
                .all(|input| !input.name().contains("session")),
            "`{}` publishes no session slot a Process could bind",
            operation.id()
        );
    }
}

/// `microsoft_excel_effects_are_classified`.
///
/// `worksheet.update_range` is the operation spec 015 §2 flagged for
/// verification: Microsoft publishes it as a `PATCH` and publishes no
/// repeat-safety for it, so it lands `InventoryOnly`.
#[test]
fn microsoft_excel_effects_are_classified() {
    assert_effects(
        microsoft_excel::connector(),
        &[
            ("worksheet.list", EffectClass::ReadOnly),
            ("worksheet.get_range", EffectClass::ReadOnly),
            ("worksheet.update_range", EffectClass::InventoryOnly),
            ("table.list", EffectClass::ReadOnly),
            ("table.get_rows", EffectClass::ReadOnly),
            ("table.add_row", EffectClass::AtMostOnce),
            ("workbook.list", EffectClass::ReadOnly),
        ],
    );

    assert_eq!(
        microsoft_excel::connector().admit_operation("workbook.create_session"),
        Err(OperationRejection::Undeclared),
        "this connector opens no workbook session, so it publishes no operation that could"
    );
}

/// The permission table is complete and per operation group.
#[test]
fn microsoft_excel_permissions_are_declared_per_operation_group() {
    let connector = microsoft_excel::connector();
    let least =
        microsoft_graph::declared_permissions(connector.operations(), microsoft_excel::permissions)
            .expect("every operation declares the permissions Microsoft documents for it");
    assert_eq!(
        least,
        vec!["Files.ReadWrite", "Files.Read"],
        "the workbook APIs publish one permission for reads as well as writes; only the Drive \
         listing has a narrower one"
    );

    // A deployment holding `Files.ReadWrite` for the workbook reads is not told
    // to also hold `Files.Read` for the listing: the broader one authorizes it.
    let enabled = [
        "worksheet.list".to_owned(),
        "table.get_rows".to_owned(),
        "workbook.list".to_owned(),
    ];
    assert!(
        microsoft_graph::permission_report(
            microsoft_excel::permissions,
            &enabled,
            &["Files.ReadWrite".to_owned()],
        )
        .is_empty()
    );

    // And a deployment holding only `Files.Read` is refused the workbook reads,
    // which Microsoft does not publish under it.
    assert_eq!(
        microsoft_graph::permission_report(
            microsoft_excel::permissions,
            &enabled,
            &["Files.Read".to_owned()],
        )
        .missing,
        vec![
            ("worksheet.list".to_owned(), "Files.ReadWrite"),
            ("table.get_rows".to_owned(), "Files.ReadWrite"),
        ]
    );
}

/// `microsoft_excel_output_contract`.
#[test]
fn microsoft_excel_output_contract() {
    let get = operation("worksheet.get_range");
    assert_eq!(
        microsoft_excel::decode(
            get,
            200,
            &reqwest::header::HeaderMap::new(),
            &serde_json::to_vec(&range_body()).expect("a fixture body serializes"),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "address": "test!A1:B2",
            "address_local": "test!A1:B2",
            "values": [["Test", "Value"], ["For", "Update"]],
            "formulas": [["Test", "Value"], ["For", "Update"]],
            "text": [["Test", "Value"], ["For", "Update"]],
            "value_types": [["String", "String"], ["String", "String"]],
            "row_count": 2,
            "column_count": 2,
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );

    for body in [
        br#"{"values":[[1]]}"#.as_slice(),
        br#"{"address":null}"#.as_slice(),
        br#"{"address":7}"#.as_slice(),
        br#"not json at all"#.as_slice(),
    ] {
        assert_eq!(
            microsoft_excel::decode(get, 200, &reqwest::header::HeaderMap::new(), body)
                .expect_err("a missing, mistyped, or unparseable body is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }
}
