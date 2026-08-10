//! Google Sheets connector proofs (spec 012 §3 plus spec 014 §3), against the
//! SDK's local provider stub.
//!
//! No test here reaches Google, and no test carries a real credential. The two
//! spec 014 proofs that need a database — `google_sheets_scope_shortfall_fails_closed`
//! and `google_sheets_refresh_happens_once_under_concurrency` — are startup and
//! credential-lifecycle properties rather than request-shape ones, and live in
//! `crates/server/tests/google_workspace.rs`.

mod google_workspace_support;

use std::time::Duration;

use donat_connectors::providers::{google, google_sheets};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::{
    AuthPlan, ConnectorErrorClass, Credential, EffectClass, Operation, OperationRejection,
    RequestPlan,
};
use google_workspace_support::{documented_failures, google_error};
use serde_json::{Value as JsonValue, json};

const SHEET: &str = "1DonatFixtureSpreadsheetId";

fn operation(id: &str) -> &'static Operation {
    google_sheets::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the google_sheets declaration publishes {id}"))
}

/// Render one operation the way a deployment would: the request from the
/// declaration, the credential from the source-local store.
fn render(stub: &ProviderStub, id: &str, input: JsonValue) -> RequestPlan {
    google_workspace_support::render(stub, operation(id), input)
}

fn values(range: &str) -> JsonValue {
    json!({ "spreadsheet_id": SHEET, "range": range })
}

/// `google_sheets_request_shape`: exact method, path, query, headers, and body
/// for every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn google_sheets_request_shape() {
    let stub = ProviderStub::start([
        // A1 notation is a path *value*: `!` and `:` are encoded.
        Expectation::new(
            "GET",
            &format!("/v4/spreadsheets/{SHEET}/values/Sheet1%21A1%3AB2"),
        )
        .query("")
        .no_body()
        .respond_json(200, json!({ "range": "Sheet1!A1:B2", "values": [["a"]] })),
        Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}/values:batchGet"))
            .query("ranges=Sheet1%21A1%3AB2")
            .no_body()
            .respond_json(
                200,
                json!({ "spreadsheetId": SHEET, "valueRanges": [{ "range": "Sheet1!A1:B2" }] }),
            ),
        Expectation::new(
            "PUT",
            &format!("/v4/spreadsheets/{SHEET}/values/Sheet1%21A1%3AB2"),
        )
        .query("valueInputOption=USER%5FENTERED")
        .header("content-type", "application/json")
        .json_body(json!({ "range": "Sheet1!A1:B2", "values": [["a", "b"]] }))
        .respond_json(
            200,
            json!({ "spreadsheetId": SHEET, "updatedRange": "Sheet1!A1:B2", "updatedCells": 2 }),
        ),
        Expectation::new(
            "POST",
            &format!("/v4/spreadsheets/{SHEET}/values/Sheet1%21A1%3AB2:append"),
        )
        .query("valueInputOption=RAW")
        .json_body(json!({ "values": [["a", "b"]] }))
        .respond_json(
            200,
            json!({ "spreadsheetId": SHEET, "tableRange": "Sheet1!A1:B2" }),
        ),
        Expectation::new(
            "POST",
            &format!("/v4/spreadsheets/{SHEET}/values/Sheet1%21A1%3AB2:clear"),
        )
        .json_body(json!({}))
        .respond_json(
            200,
            json!({ "spreadsheetId": SHEET, "clearedRange": "Sheet1!A1:B2" }),
        ),
        Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}"))
            .query("")
            .respond_json(
                200,
                json!({
                    "spreadsheetId": SHEET,
                    "properties": { "title": "Donat fixture" },
                    "spreadsheetUrl": format!("https://docs.google.com/spreadsheets/d/{SHEET}/edit"),
                }),
            ),
        Expectation::new("POST", "/v4/spreadsheets")
            .json_body(json!({ "properties": { "title": "Donat fixture" } }))
            .respond_json(200, json!({ "spreadsheetId": SHEET })),
        // A hostile spreadsheet identifier stays one percent-encoded segment.
        Expectation::new(
            "GET",
            "/v4/spreadsheets/%2E%2E%2F%2E%2E%2Fv4%2Fspreadsheets%3Fx%3D1%23y",
        )
        .respond_json(200, json!({ "spreadsheetId": "recovered" })),
    ])
    .await;

    let rows = json!([["a", "b"]]);
    for (id, input) in [
        ("values.get", values("Sheet1!A1:B2")),
        (
            "values.batch_get",
            json!({ "spreadsheet_id": SHEET, "ranges": "Sheet1!A1:B2" }),
        ),
        (
            "values.update",
            json!({
                "spreadsheet_id": SHEET,
                "range": "Sheet1!A1:B2",
                "values": rows,
                "value_input_option": "USER_ENTERED",
            }),
        ),
        (
            "values.append",
            json!({
                "spreadsheet_id": SHEET,
                "range": "Sheet1!A1:B2",
                "values": rows,
                "value_input_option": "RAW",
            }),
        ),
        ("values.clear", values("Sheet1!A1:B2")),
        ("spreadsheet.get", json!({ "spreadsheet_id": SHEET })),
        ("spreadsheet.create", json!({ "title": "Donat fixture" })),
    ] {
        let request = render(&stub, id, input);
        stub.send(request).await.expect("the stub answers");
    }

    let hostile = render(
        &stub,
        "spreadsheet.get",
        json!({ "spreadsheet_id": "../../v4/spreadsheets?x=1#y" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();

    // A required declared input with no value is a failure, not an omitted
    // parameter: `valueInputOption` is one "the caller must specify".
    assert!(
        operation("values.update")
            .plan_request(
                &stub.origin(),
                &json!({ "spreadsheet_id": SHEET, "range": "A1", "values": [] }),
            )
            .is_err(),
        "a missing required input is refused before the request leaves"
    );
}

/// `google_sheets_auth_is_applied`: the stored access token reaches the wire as
/// `Authorization: Bearer <token>`, and appears in no error, log, or diagnostic.
#[tokio::test]
async fn google_sheets_auth_is_applied() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}"))
            .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
            .without_header("x-goog-api-key")
            .respond_json(200, json!({ "spreadsheetId": SHEET })),
    ])
    .await;

    let request = render(&stub, "spreadsheet.get", json!({ "spreadsheet_id": SHEET }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive(),
        "an applied credential is marked sensitive so a header dump redacts it"
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert_eq!(
        request.url().query(),
        None,
        "Google's `access_token` query parameter is never used: the token is a header"
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = google_sheets::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        google_sheets::connector().credential(),
    );
    assert!(
        !surface.contains(SECRET_SENTINEL),
        "a stored credential never reaches a log line, an error, or a diagnostic: {surface}"
    );
    // The declaration configures no credential field at all: the token is the
    // credential store's, per attempt.
    assert!(google_sheets::connector().credential().fields().is_empty());
    stub.assert_satisfied();

    // ...and with no stored credential the request is refused rather than sent
    // without one.
    let mut bare = operation("spreadsheet.get")
        .plan_request(&stub.origin(), &json!({ "spreadsheet_id": SHEET }))
        .expect("the declared request renders");
    let refused = AuthPlan::oauth2_authorization_code()
        .apply(&Credential::from_fields([]), &mut bare, None)
        .expect_err("an unauthorized Google request is never sent");
    assert_eq!(refused.class(), ConnectorErrorClass::Invariant);
}

/// `google_sheets_error_map`: every documented failure reaches exactly one of
/// the eight classes, with a Donat-owned message and no provider text.
#[tokio::test]
async fn google_sheets_error_map() {
    for (status, reason, expected) in documented_failures() {
        let stub =
            ProviderStub::start([
                Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}"))
                    .respond_json(status, google_error(status, reason)),
            ])
            .await;
        let response = stub
            .send(render(
                &stub,
                "spreadsheet.get",
                json!({ "spreadsheet_id": SHEET }),
            ))
            .await
            .expect("the stub answers");

        let failure = google_sheets::error_map().classify_response(&response);
        assert_eq!(failure.class(), expected, "status {status} reason {reason}");
        assert_eq!(failure.provider_status(), Some(status));
        assert!(
            google_sheets::decode(
                operation("spreadsheet.get"),
                status,
                response.headers(),
                response.body(),
            )
            .is_err(),
            "status {status} is not a declared success"
        );

        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal", reason] {
            assert!(
                !surface.contains(leaked),
                "status {status} leaked {leaked} in {surface}"
            );
        }
        stub.assert_satisfied();
    }
}

/// `google_sheets_quota_errors_are_classified`: Google's documented quota
/// refusals reach `http_429` whichever status carries them, and a retry hint is
/// clamped.
#[tokio::test]
async fn google_sheets_quota_errors_are_classified() {
    // *Usage limits*: "a 429: Too many requests HTTP status code response".
    // The 403 reasons are the ones Drive, Gmail, and Calendar all document,
    // and they mean the same thing on Sheets.
    for reason in [
        "rateLimitExceeded",
        "userRateLimitExceeded",
        "dailyLimitExceeded",
        "quotaExceeded",
    ] {
        for status in [403, 429] {
            let failure = google_sheets::error_map().classify(
                status,
                &reqwest::header::HeaderMap::new(),
                serde_json::to_vec(&google_error(status, reason))
                    .expect("a fixture body serializes")
                    .as_slice(),
            );
            assert_eq!(
                failure.class(),
                ConnectorErrorClass::Http429,
                "{status} {reason} is a quota refusal, whatever status carries it"
            );
        }
    }

    // The same 403 with a permission reason is *not* a quota refusal: it is an
    // authentication failure, and retrying it forever would be wrong.
    assert_eq!(
        google_sheets::error_map()
            .classify(
                403,
                &reqwest::header::HeaderMap::new(),
                br#"{"error":{"code":403,"errors":[{"domain":"global","reason":"forbidden"}]}}"#,
            )
            .class(),
        ConnectorErrorClass::Authentication
    );

    // Google publishes no `Retry-After` for these APIs. When a response carries
    // one it is honoured, and an absurd one is clamped rather than obeyed.
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}"))
            .respond_header("retry-after", "30")
            .respond_json(429, google_error(429, "rateLimitExceeded")),
        Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}"))
            .respond_header("retry-after", "999999999")
            .respond_json(429, google_error(429, "rateLimitExceeded")),
        Expectation::new("GET", &format!("/v4/spreadsheets/{SHEET}"))
            .respond_json(429, google_error(429, "rateLimitExceeded")),
    ])
    .await;
    let mut hints = Vec::new();
    for _ in 0..3 {
        let response = stub
            .send(render(
                &stub,
                "spreadsheet.get",
                json!({ "spreadsheet_id": SHEET }),
            ))
            .await
            .expect("the stub answers");
        hints.push(
            google_sheets::error_map()
                .classify_response(&response)
                .retry_after(),
        );
    }
    assert_eq!(
        hints,
        vec![
            Some(Duration::from_secs(30)),
            Some(Duration::from_secs(86_400)),
            None,
        ]
    );
    stub.assert_satisfied();
}

/// `google_sheets_partial_failure_is_typed`: a failure inside a success
/// envelope is a typed failure, never an output.
#[test]
fn google_sheets_partial_failure_is_typed() {
    // Sheets publishes no per-item failure shape for these operations, so the
    // shape that has to be refused is Google's canonical error object arriving
    // under a success status.
    let failure = google_sheets::decode(
        operation("values.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"error":{"code":429,"message":"quota","errors":[{"reason":"rateLimitExceeded"}]}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    assert_eq!(failure.code(), google::SUCCESS_CARRIES_ERROR.code());

    // The same body under the status Google would really send is classified by
    // the error map, so one shape has exactly one class either way.
    assert_eq!(
        google_sheets::decode(
            operation("values.get"),
            429,
            &reqwest::header::HeaderMap::new(),
            br#"{"error":{"code":429,"errors":[{"reason":"rateLimitExceeded"}]}}"#,
        )
        .expect_err("a 429 is a failure")
        .class(),
        ConnectorErrorClass::Http429
    );

    // An ordinary body still decodes.
    google_sheets::decode(
        operation("values.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"range":"Sheet1!A1:B2","values":[["a"]]}"#,
    )
    .expect("a complete success decodes");
}

/// `google_sheets_page_token_cannot_leave_origin`: Sheets publishes no
/// continuation, so no provider-chosen value can become a destination.
#[test]
fn google_sheets_page_token_cannot_leave_origin() {
    for operation in google_sheets::connector().operations() {
        assert!(
            google_sheets::pagination(operation.id()).is_none(),
            "{}: Sheets publishes no continuation for any operation",
            operation.id()
        );
    }

    // The one provider-chosen URL Sheets does hand back is `spreadsheetUrl`,
    // on `docs.google.com`. It is decoded as data and nothing in this connector
    // can spend it as a destination.
    let decoded = google_sheets::decode(
        operation("spreadsheet.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"spreadsheetId":"1DonatFixtureSpreadsheetId","spreadsheetUrl":"https://attacker.invalid/steal","nextPageToken":"ignored"}"#,
    )
    .expect("the declared contract is satisfied");
    assert_eq!(
        decoded.get("spreadsheet_url").and_then(JsonValue::as_str),
        Some("https://attacker.invalid/steal"),
        "a provider URL is data in the output contract"
    );
    assert!(
        decoded.get("nextPageToken").is_none(),
        "the declaration is the output schema: an undeclared field is not carried"
    );
}

/// `google_sheets_effects_are_classified`: every operation carries a class, and
/// an inventory-only one cannot be enabled by a deployment.
#[test]
fn google_sheets_effects_are_classified() {
    let connector = google_sheets::connector();
    let expected = [
        ("values.get", EffectClass::ReadOnly),
        ("values.batch_get", EffectClass::ReadOnly),
        (
            "values.update",
            EffectClass::ProviderIdempotentNaturalMethod,
        ),
        ("values.append", EffectClass::AtMostOnce),
        ("values.clear", EffectClass::InventoryOnly),
        ("spreadsheet.get", EffectClass::ReadOnly),
        ("spreadsheet.create", EffectClass::AtMostOnce),
    ];
    assert_eq!(
        connector.operations().len(),
        expected.len(),
        "every declared operation is classified here"
    );

    for (id, class) in expected {
        let operation = operation(id);
        assert_eq!(operation.effect_class(), Some(class), "{id}");
        match class {
            EffectClass::InventoryOnly => {
                assert_eq!(
                    connector.admit_operation(id),
                    Err(OperationRejection::InventoryOnly),
                    "{id} must not be enablable by a deployment"
                );
                assert!(
                    operation
                        .effect()
                        .and_then(donat_connectors::sdk::Effect::inventory_reason)
                        .is_some_and(|reason| !reason.is_empty()),
                    "{id} records why it is not executable"
                );
            }
            _ => {
                assert!(connector.admit_operation(id).is_ok(), "{id}");
                assert!(operation.is_executable(), "{id}");
            }
        }
        assert!(
            operation.idempotency_binding().is_none(),
            "{id}: Google publishes no idempotency key to bind"
        );
    }

    assert_eq!(
        connector.admit_operation("values.batch_update"),
        Err(OperationRejection::Undeclared),
        "an operation this binary does not compile cannot be enabled"
    );
}

/// The scope table is complete, is per operation group, and never asks a
/// read-only deployment for a write scope. The *startup* half of
/// `google_sheets_scope_shortfall_fails_closed` is in
/// `crates/server/tests/google_workspace.rs`.
#[test]
fn google_sheets_scopes_are_declared_per_operation_group() {
    let connector = google_sheets::connector();
    let least = google::declared_scopes(connector.operations(), google_sheets::scopes)
        .expect("every operation declares the scopes Google documents for it");
    assert_eq!(
        least,
        vec![
            "https://www.googleapis.com/auth/spreadsheets.readonly",
            "https://www.googleapis.com/auth/spreadsheets",
        ],
        "two groups: the reads and the writes"
    );

    let readonly = vec!["https://www.googleapis.com/auth/spreadsheets.readonly".to_owned()];
    let reads = ["values.get".to_owned(), "spreadsheet.get".to_owned()];
    assert!(
        google::scope_report(google_sheets::scopes, &reads, &readonly).is_empty(),
        "a read-only deployment is never forced to grant a write scope"
    );

    let with_write = [reads.as_slice(), &["values.update".to_owned()]].concat();
    let report = google::scope_report(google_sheets::scopes, &with_write, &readonly);
    assert_eq!(
        report.missing,
        vec![(
            "values.update".to_owned(),
            "https://www.googleapis.com/auth/spreadsheets"
        )]
    );
}

/// `google_sheets_output_contract`: the declared pointers are complete and
/// typed, and a missing required pointer is a validation failure, not a null.
#[test]
fn google_sheets_output_contract() {
    let update = operation("values.update");
    assert_eq!(
        google_sheets::decode(
            update,
            200,
            &reqwest::header::HeaderMap::new(),
            br#"{"spreadsheetId":"1DonatFixtureSpreadsheetId","updatedRange":"Sheet1!A1:B2","updatedCells":2,"updatedRows":1,"updatedData":{"range":"Sheet1!A1:B2"}}"#,
        )
        .expect("the declared contract is satisfied"),
        json!({
            "spreadsheet_id": SHEET,
            "updated_range": "Sheet1!A1:B2",
            "updated_cells": 2,
            "updated_rows": 1,
        }),
        "the declaration is the output schema, not a filter over the provider body"
    );

    for body in [
        br#"{"updatedRange":"Sheet1!A1:B2"}"#.as_slice(),
        br#"{"spreadsheetId":null,"updatedRange":"Sheet1!A1:B2"}"#.as_slice(),
        br#"{"spreadsheetId":7,"updatedRange":"Sheet1!A1:B2"}"#.as_slice(),
        br#"not json at all"#.as_slice(),
    ] {
        assert_eq!(
            google_sheets::decode(update, 200, &reqwest::header::HeaderMap::new(), body)
                .expect_err("a missing, mistyped, or unparseable body is a validation failure")
                .class(),
            ConnectorErrorClass::Validation
        );
    }

    // An optional pointer that is absent is a declared null rather than a
    // missing key, so a Process binds one shape.
    assert_eq!(
        google_sheets::decode(
            update,
            200,
            &reqwest::header::HeaderMap::new(),
            br#"{"spreadsheetId":"1DonatFixtureSpreadsheetId","updatedRange":"Sheet1!A1:B2"}"#,
        )
        .expect("the required pointers are satisfied"),
        json!({
            "spreadsheet_id": SHEET,
            "updated_range": "Sheet1!A1:B2",
            "updated_cells": null,
            "updated_rows": null,
        })
    );
}
