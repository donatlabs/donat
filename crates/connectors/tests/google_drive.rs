//! Google Drive connector proofs (spec 012 §3 plus spec 014 §3), against the
//! SDK's local provider stub.
//!
//! The two proofs that need a database — `google_drive_scope_shortfall_fails_closed`
//! and `google_drive_refresh_happens_once_under_concurrency` — are startup and
//! credential-lifecycle properties and live in
//! `crates/server/tests/google_workspace.rs`.

mod google_workspace_support;

use std::time::Duration;

use donat_connectors::providers::{google, google_drive};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    ConnectorErrorClass, EffectClass, Operation, OperationRejection, PaginationBudget,
};
use google_workspace_support::{assert_effects, documented_failures, google_error, render};
use serde_json::{Value as JsonValue, json};

const FILE: &str = "1DonatFixtureFileId";

/// The percent-encoded field mask every file-shaped operation sends.
const FILE_FIELDS: &str =
    "id%2Cname%2CmimeType%2CmodifiedTime%2Csize%2Ctrashed%2Cparents%2CwebViewLink%2Cmd5Checksum";

fn operation(id: &str) -> &'static Operation {
    google_drive::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the google_drive declaration publishes {id}"))
}

fn file_body() -> JsonValue {
    json!({
        "id": FILE,
        "name": "report.pdf",
        "mimeType": "application/pdf",
        "modifiedTime": "2026-08-10T00:00:00.000Z",
        "size": "10240",
        "trashed": false,
        "parents": ["root"],
        "webViewLink": "https://drive.google.com/file/d/1DonatFixtureFileId/view",
        "md5Checksum": "0cc175b9c0f1b6a831c399e269772661",
    })
}

/// `google_drive_request_shape`: exact method, path, query, headers, and body
/// for every operation, including percent-encoding of a hostile path value.
#[tokio::test]
async fn google_drive_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/drive/v3/files/{FILE}"))
            .query(&format!("fields={FILE_FIELDS}"))
            .no_body()
            .respond_json(200, file_body()),
        Expectation::new("GET", "/drive/v3/files")
            .query(&format!(
                "q=trashed%20%3D%20false&pageSize=100&fields=nextPageToken%2CincompleteSearch%2Cfiles%28{FILE_FIELDS}%29"
            ))
            .respond_json(200, json!({ "files": [], "incompleteSearch": false })),
        Expectation::new("GET", &format!("/drive/v3/files/{FILE}"))
            .query("alt=media")
            .respond_bytes(200, b"%PDF-1.7 not really a pdf".to_vec()),
        Expectation::new("PATCH", &format!("/drive/v3/files/{FILE}"))
            .query(&format!("fields={FILE_FIELDS}"))
            .json_body(json!({ "name": "renamed.pdf" }))
            .respond_json(200, file_body()),
        Expectation::new("DELETE", &format!("/drive/v3/files/{FILE}"))
            .no_body()
            .respond_bytes(204, Vec::new()),
        Expectation::new("POST", &format!("/drive/v3/files/{FILE}/copy"))
            .query(&format!("fields={FILE_FIELDS}"))
            .json_body(json!({ "name": "copy.pdf" }))
            .respond_json(200, file_body()),
        Expectation::new("POST", "/drive/v3/files")
            .query(&format!("fields={FILE_FIELDS}"))
            .json_body(json!({
                "name": "Reports",
                "mimeType": "application/vnd.google-apps.folder",
                "parents": ["root"],
            }))
            .respond_json(200, file_body()),
        Expectation::new("GET", &format!("/drive/v3/files/{FILE}/permissions"))
            .query("pageSize=100&fields=nextPageToken%2Cpermissions%28id%2Ctype%2Crole%2CemailAddress%2Cdomain%2CdisplayName%29")
            .respond_json(200, json!({ "permissions": [] })),
        Expectation::new("POST", &format!("/drive/v3/files/{FILE}/permissions"))
            .query("fields=id%2Ctype%2Crole%2CemailAddress%2Cdomain%2CdisplayName")
            .json_body(json!({
                "type": "user",
                "role": "reader",
                "emailAddress": "reader@example.test",
            }))
            .respond_json(200, json!({ "id": "perm_1", "type": "user", "role": "reader" })),
        // A hostile file identifier stays one percent-encoded path segment.
        Expectation::new(
            "GET",
            "/drive/v3/files/%2E%2E%2F%2E%2E%2Fdrive%2Fv3%2Ffiles%3Fx%3D1%23y",
        )
        .respond_json(200, file_body()),
    ])
    .await;

    let file = json!({ "file_id": FILE });
    for (id, input) in [
        ("file.get", file.clone()),
        ("file.list", json!({ "q": "trashed = false" })),
        ("file.download", file.clone()),
        (
            "file.update_metadata",
            json!({ "file_id": FILE, "name": "renamed.pdf" }),
        ),
        ("file.delete", file.clone()),
        ("file.copy", json!({ "file_id": FILE, "name": "copy.pdf" })),
        (
            "folder.create",
            json!({ "name": "Reports", "parents": ["root"] }),
        ),
        ("permission.list", file),
        (
            "permission.create",
            json!({
                "file_id": FILE,
                "type": "user",
                "role": "reader",
                "email_address": "reader@example.test",
            }),
        ),
    ] {
        stub.send(render(&stub, operation(id), input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(
        &stub,
        operation("file.get"),
        json!({ "file_id": "../../drive/v3/files?x=1#y" }),
    );
    assert_eq!(
        hostile.url().host_str(),
        stub.origin().as_url().host_str(),
        "a hostile path value cannot move the request"
    );
    assert_eq!(
        hostile.url().query(),
        Some(format!("fields={FILE_FIELDS}").as_str()),
        "the declared field mask is the only query, and input cannot widen it"
    );
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `google_drive_auth_is_applied`: the stored access token reaches the wire as
/// `Authorization: Bearer <token>` and nowhere else.
#[tokio::test]
async fn google_drive_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", &format!("/drive/v3/files/{FILE}"))
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .without_header("x-goog-api-key")
        .respond_json(200, file_body())])
    .await;

    let request = render(&stub, operation("file.get"), json!({ "file_id": FILE }));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert!(
        !request
            .url()
            .query()
            .unwrap_or_default()
            .contains(SECRET_SENTINEL)
    );

    let response = stub.send(request).await.expect("the stub answers");
    let failure = google_drive::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        google_drive::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    assert!(google_drive::connector().credential().fields().is_empty());
    stub.assert_satisfied();
}

/// `google_drive_error_map`: every documented failure reaches exactly one class
/// and carries no provider text.
#[test]
fn google_drive_error_map() {
    for (status, reason, expected) in documented_failures() {
        let body = serde_json::to_vec(&google_error(status, reason)).expect("a fixture serializes");
        let failure =
            google_drive::error_map().classify(status, &reqwest::header::HeaderMap::new(), &body);
        assert_eq!(failure.class(), expected, "status {status} reason {reason}");
        assert_eq!(failure.provider_status(), Some(status));
        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal", reason] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
        assert!(
            google_drive::decode(
                operation("file.get"),
                status,
                &reqwest::header::HeaderMap::new(),
                &body,
            )
            .is_err(),
            "status {status} is not a declared success"
        );
    }
}

/// `google_drive_quota_errors_are_classified`: Drive's four documented 403
/// rate-limit reasons and the 429 all reach `http_429`, and a retry hint is
/// clamped.
#[test]
fn google_drive_quota_errors_are_classified() {
    // "403 responses include specific reasons: rateLimitExceeded,
    // userRateLimitExceeded, dailyLimitExceeded, sharingRateLimitExceeded."
    for reason in [
        "rateLimitExceeded",
        "userRateLimitExceeded",
        "dailyLimitExceeded",
        "sharingRateLimitExceeded",
    ] {
        let body = serde_json::to_vec(&google_error(403, reason)).expect("a fixture serializes");
        assert_eq!(
            google_drive::error_map()
                .classify(403, &reqwest::header::HeaderMap::new(), &body)
                .class(),
            ConnectorErrorClass::Http429,
            "403 {reason}"
        );
    }

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "999999999".parse().unwrap());
    assert_eq!(
        google_drive::error_map()
            .classify(429, &headers, b"{}")
            .retry_after(),
        Some(Duration::from_secs(86_400)),
        "an absurd provider hint is clamped rather than obeyed"
    );
    assert_eq!(
        google_drive::error_map()
            .classify(429, &reqwest::header::HeaderMap::new(), b"{}")
            .retry_after(),
        None,
        "Google publishes no Retry-After for Drive, and none is invented"
    );
}

/// `google_drive_pagination_is_bounded`: the declared plans terminate, respect
/// the budget, and cannot leave the compiled origin.
#[tokio::test]
async fn google_drive_pagination_is_bounded() {
    let plan = google_drive::pagination("file.list").expect("file.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/drive/v3/files").respond_json(
            200,
            json!({ "files": [{ "id": "f1" }], "nextPageToken": "tok~1" }),
        ),
        Expectation::new("GET", "/drive/v3/files")
            .respond_json(200, json!({ "files": [{ "id": "f2" }] })),
    ])
    .await;
    let items = plan
        .collect(
            render(
                &stub,
                operation("file.list"),
                json!({ "q": "trashed = false" }),
            ),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| {
                assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
                stub.send(request)
            },
        )
        .await
        .expect("the declared plan walks both pages and stops");
    assert_eq!(items, vec![json!({ "id": "f1" }), json!({ "id": "f2" })]);
    stub.assert_satisfied();

    // An endless provider exhausts each ceiling instead of looping.
    for budget in [
        PaginationBudget::new(2, 8, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 2, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 64, 100, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|index| {
            Expectation::new("GET", "/drive/v3/files").respond_json(
                200,
                json!({ "files": [{ "id": index }], "nextPageToken": "tok~1" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, operation("file.list"), json!({ "q": "" })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // Only the two listings Google documents as paginated declare a plan.
    for id in ["file.get", "file.download", "folder.create", "file.delete"] {
        assert!(google_drive::pagination(id).is_none(), "{id}");
    }
}

/// `google_drive_page_token_cannot_leave_origin`: a `nextPageToken` that spells
/// another host is spent as a query value on this connector's own origin, and
/// the foreign origin is never contacted.
#[tokio::test]
async fn google_drive_page_token_cannot_leave_origin() {
    let plan = google_drive::pagination("file.list").expect("file.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));

    let elsewhere = ProviderStub::start([
        Expectation::new("GET", "/drive/v3/files").respond_json(200, json!({ "files": [] }))
    ])
    .await;
    let hostile = format!("{}/drive/v3/files", elsewhere.base_url());

    let stub = ProviderStub::start([
        Expectation::new("GET", "/drive/v3/files")
            .respond_json(200, json!({ "files": [], "nextPageToken": hostile })),
        Expectation::new("GET", "/drive/v3/files").respond_json(200, json!({ "files": [] })),
    ])
    .await;
    plan.collect(
        render(&stub, operation("file.list"), json!({ "q": "" })),
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
    .expect("the hostile token is a query value, not a destination");
    assert_eq!(
        elsewhere.mismatches().len(),
        1,
        "the other origin was never contacted"
    );
    stub.assert_satisfied();
}

/// `google_drive_partial_failure_is_typed`: `incompleteSearch` is a typed
/// failure rather than a shorter list, and an error envelope under a `200` is
/// refused.
#[test]
fn google_drive_partial_failure_is_typed() {
    let incomplete = google_drive::decode(
        operation("file.list"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"files":[{"id":"f1"}],"incompleteSearch":true}"#,
    )
    .expect_err("a search Google reports as incomplete is not a success");
    assert_eq!(incomplete.class(), ConnectorErrorClass::Permanent);
    assert_eq!(incomplete.code(), google::INCOMPLETE_RESULT.code());

    // A complete search with the same shape decodes.
    google_drive::decode(
        operation("file.list"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"files":[{"id":"f1"}],"incompleteSearch":false}"#,
    )
    .expect("a complete search decodes");

    let enveloped = google_drive::decode(
        operation("file.get"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"error":{"code":403,"errors":[{"reason":"forbidden"}]}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(enveloped.code(), google::SUCCESS_CARRIES_ERROR.code());
}

/// `google_drive_effects_are_classified`.
#[test]
fn google_drive_effects_are_classified() {
    assert_effects(
        google_drive::connector(),
        &[
            ("file.get", EffectClass::ReadOnly),
            ("file.list", EffectClass::ReadOnly),
            ("file.download", EffectClass::ReadOnly),
            ("file.update_metadata", EffectClass::InventoryOnly),
            ("file.delete", EffectClass::ProviderIdempotentNaturalMethod),
            ("file.copy", EffectClass::AtMostOnce),
            ("folder.create", EffectClass::AtMostOnce),
            ("permission.list", EffectClass::ReadOnly),
            ("permission.create", EffectClass::AtMostOnce),
        ],
    );
    assert_eq!(
        google_drive::connector().admit_operation("file.export"),
        Err(OperationRejection::Undeclared)
    );
}

/// The scope table is complete and per operation group.
#[test]
fn google_drive_scopes_are_declared_per_operation_group() {
    let least =
        google::declared_scopes(google_drive::connector().operations(), google_drive::scopes)
            .expect("every operation declares the scopes Google documents for it");
    assert_eq!(
        least,
        vec![
            "https://www.googleapis.com/auth/drive.metadata.readonly",
            "https://www.googleapis.com/auth/drive.file",
        ]
    );

    let metadata_only = vec!["https://www.googleapis.com/auth/drive.metadata.readonly".to_owned()];
    let reads = ["file.get".to_owned(), "permission.list".to_owned()];
    assert!(
        google::scope_report(google_drive::scopes, &reads, &metadata_only).is_empty(),
        "a read-only deployment is never forced to grant drive.file"
    );

    let with_delete = [reads.as_slice(), &["file.delete".to_owned()]].concat();
    assert_eq!(
        google::scope_report(google_drive::scopes, &with_delete, &metadata_only).missing,
        vec![(
            "file.delete".to_owned(),
            "https://www.googleapis.com/auth/drive.file"
        )]
    );
}

/// `google_drive_output_contract`, including the one operation whose output the
/// module composes rather than reads through a pointer.
#[test]
fn google_drive_output_contract() {
    assert_eq!(
        google_drive::decode(
            operation("file.get"),
            200,
            &reqwest::header::HeaderMap::new(),
            serde_json::to_vec(&file_body())
                .expect("a fixture serializes")
                .as_slice(),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": FILE,
            "name": "report.pdf",
            "mime_type": "application/pdf",
            "modified_time": "2026-08-10T00:00:00.000Z",
            "size": "10240",
            "trashed": false,
            "parents": ["root"],
            "web_view_link": "https://drive.google.com/file/d/1DonatFixtureFileId/view",
            "md5_checksum": "0cc175b9c0f1b6a831c399e269772661",
        })
    );

    for body in [
        br#"{"name":"report.pdf"}"#.as_slice(),
        br#"{"id":null}"#.as_slice(),
        br#"{"id":7}"#.as_slice(),
        br#"not json"#.as_slice(),
    ] {
        assert_eq!(
            google_drive::decode(
                operation("file.get"),
                200,
                &reqwest::header::HeaderMap::new(),
                body
            )
            .expect_err("a missing, mistyped, or unparseable body is a validation failure")
            .class(),
            ConnectorErrorClass::Validation
        );
    }

    // A documented empty success decodes to an empty output rather than a
    // parse failure.
    assert_eq!(
        google_drive::decode(
            operation("file.delete"),
            204,
            &reqwest::header::HeaderMap::new(),
            b"",
        )
        .expect("an empty success is a success"),
        json!({})
    );

    // `alt=media` returns bytes, and the module composes the declared contract
    // from them.
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        "application/pdf".parse().unwrap(),
    );
    assert_eq!(
        google_drive::decode(operation("file.download"), 200, &headers, b"hello")
            .expect("a downloaded body is composed into the declared contract"),
        json!({
            "content_base64": "aGVsbG8=",
            "content_bytes": 5,
            "content_type": "application/pdf",
        })
    );
    // ...and a failure on the same route is still classified rather than
    // base64-encoded into a success.
    assert_eq!(
        google_drive::decode(
            operation("file.download"),
            403,
            &headers,
            br#"{"error":{"code":403,"errors":[{"reason":"forbidden"}]}}"#,
        )
        .expect_err("a 403 is a failure on the download route too")
        .class(),
        ConnectorErrorClass::Authentication
    );
}
