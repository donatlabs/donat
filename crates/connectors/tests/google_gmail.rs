//! Gmail connector proofs (spec 012 §3 plus spec 014 §3), against the SDK's
//! local provider stub.
//!
//! The two proofs that need a database — `google_gmail_scope_shortfall_fails_closed`
//! and `google_gmail_refresh_happens_once_under_concurrency` — live in
//! `crates/server/tests/google_workspace.rs`.

mod google_workspace_support;

use std::time::Duration;

use donat_connectors::providers::{google, google_gmail};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    ConnectorErrorClass, EffectClass, Operation, OperationRejection, PaginationBudget,
};
use google_workspace_support::{assert_effects, documented_failures, google_error, render};
use serde_json::{Value as JsonValue, json};

const MESSAGE: &str = "18f0a1b2c3d4e5f6";

fn operation(id: &str) -> &'static Operation {
    google_gmail::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the google_gmail declaration publishes {id}"))
}

fn message_body() -> JsonValue {
    json!({
        "id": MESSAGE,
        "threadId": "18f0a1b2c3d4e5f0",
        "labelIds": ["INBOX", "UNREAD"],
        "snippet": "a fixture message",
        "internalDate": "1786000000000",
        "sizeEstimate": 4096,
        "payload": { "mimeType": "text/plain" },
    })
}

/// `google_gmail_request_shape`.
#[tokio::test]
async fn google_gmail_request_shape() {
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("/gmail/v1/users/me/messages/{MESSAGE}"))
            .query("format=metadata")
            .no_body()
            .respond_json(200, message_body()),
        Expectation::new("GET", "/gmail/v1/users/me/messages")
            .query("q=is%3Aunread&maxResults=500")
            .respond_json(200, json!({ "messages": [], "resultSizeEstimate": 0 })),
        Expectation::new("POST", "/gmail/v1/users/me/messages/send")
            .header("content-type", "application/json")
            .json_body(json!({ "raw": "UmVjZWl2ZWQ6IGZpeHR1cmU" }))
            .respond_json(200, message_body()),
        Expectation::new(
            "POST",
            &format!("/gmail/v1/users/me/messages/{MESSAGE}/modify"),
        )
        .json_body(json!({ "addLabelIds": ["STARRED"], "removeLabelIds": [] }))
        .respond_json(200, message_body()),
        Expectation::new(
            "POST",
            &format!("/gmail/v1/users/me/messages/{MESSAGE}/trash"),
        )
        .respond_json(200, message_body()),
        Expectation::new("GET", "/gmail/v1/users/me/drafts/r%2D1")
            .query("format=full")
            .respond_json(200, json!({ "id": "r-1", "message": message_body() })),
        Expectation::new("GET", "/gmail/v1/users/me/drafts")
            .query("q=&maxResults=500")
            .respond_json(200, json!({ "drafts": [] })),
        Expectation::new("GET", "/gmail/v1/users/me/labels")
            .query("")
            .respond_json(200, json!({ "labels": [{ "id": "INBOX" }] })),
        Expectation::new("POST", "/gmail/v1/users/me/labels")
            .json_body(json!({ "name": "Donat" }))
            .respond_json(200, json!({ "id": "Label_1", "name": "Donat" })),
        Expectation::new("DELETE", "/gmail/v1/users/me/labels/Label%5F1")
            .no_body()
            .respond_bytes(204, Vec::new()),
        Expectation::new("GET", "/gmail/v1/users/me/threads/18f0a1b2c3d4e5f0")
            .query("format=minimal")
            .respond_json(200, json!({ "id": "18f0a1b2c3d4e5f0" })),
        Expectation::new("GET", "/gmail/v1/users/me/threads")
            .query("q=&maxResults=500")
            .respond_json(200, json!({ "threads": [] })),
        // A hostile message identifier stays one percent-encoded segment, and
        // in particular cannot climb out of the `me` mailbox.
        Expectation::new(
            "GET",
            "/gmail/v1/users/me/messages/%2E%2E%2F%2E%2E%2Fnobody%2Fmessages",
        )
        .query("format=minimal")
        .respond_json(200, message_body()),
    ])
    .await;

    for (id, input) in [
        (
            "message.get",
            json!({ "id": MESSAGE, "format": "metadata" }),
        ),
        ("message.list", json!({ "q": "is:unread" })),
        ("message.send", json!({ "raw": "UmVjZWl2ZWQ6IGZpeHR1cmU" })),
        (
            "message.modify_labels",
            json!({ "id": MESSAGE, "add_label_ids": ["STARRED"], "remove_label_ids": [] }),
        ),
        ("message.trash", json!({ "id": MESSAGE })),
        ("draft.get", json!({ "id": "r-1", "format": "full" })),
        ("draft.list", json!({ "q": "" })),
        ("label.list", json!({})),
        ("label.create", json!({ "name": "Donat" })),
        ("label.delete", json!({ "id": "Label_1" })),
        (
            "thread.get",
            json!({ "id": "18f0a1b2c3d4e5f0", "format": "minimal" }),
        ),
        ("thread.list", json!({ "q": "" })),
    ] {
        stub.send(render(&stub, operation(id), input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(
        &stub,
        operation("message.get"),
        json!({ "id": "../../nobody/messages", "format": "minimal" }),
    );
    assert_eq!(hostile.url().host_str(), stub.origin().as_url().host_str());
    assert!(
        hostile
            .url()
            .path()
            .starts_with("/gmail/v1/users/me/messages/"),
        "the mailbox is fixed at `me` and input cannot leave it: {}",
        hostile.url().path()
    );
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `google_gmail_auth_is_applied`.
#[tokio::test]
async fn google_gmail_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new("GET", "/gmail/v1/users/me/labels")
        .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
        .without_header("x-goog-api-key")
        .respond_json(200, json!({ "labels": [] }))])
    .await;

    let request = render(&stub, operation("label.list"), json!({}));
    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .expect("the credential was applied")
            .is_sensitive()
    );
    assert!(!format!("{:?}", request.headers()).contains(SECRET_SENTINEL));
    assert_eq!(request.url().query(), None);

    let response = stub.send(request).await.expect("the stub answers");
    let failure = google_gmail::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        google_gmail::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    assert!(google_gmail::connector().credential().fields().is_empty());
    stub.assert_satisfied();
}

/// `google_gmail_error_map`.
#[test]
fn google_gmail_error_map() {
    for (status, reason, expected) in documented_failures() {
        let body = serde_json::to_vec(&google_error(status, reason)).expect("a fixture serializes");
        let failure =
            google_gmail::error_map().classify(status, &reqwest::header::HeaderMap::new(), &body);
        assert_eq!(failure.class(), expected, "status {status} reason {reason}");
        let surface = format!(
            "{} {} {} {failure:?}",
            failure.code(),
            failure.safe_message(),
            failure.diagnostic()
        );
        for leaked in [SECRET_SENTINEL, "db-7.internal", reason] {
            assert!(!surface.contains(leaked), "status {status}: {surface}");
        }
    }
}

/// `google_gmail_quota_errors_are_classified`: Gmail's own 403 reasons and its
/// 429 — "Covers mail sending limits, bandwidth limits, and concurrent request
/// limits" — all reach `http_429`.
#[test]
fn google_gmail_quota_errors_are_classified() {
    for reason in [
        "dailyLimitExceeded",
        "rateLimitExceeded",
        "userRateLimitExceeded",
    ] {
        let body = serde_json::to_vec(&google_error(403, reason)).expect("a fixture serializes");
        assert_eq!(
            google_gmail::error_map()
                .classify(403, &reqwest::header::HeaderMap::new(), &body)
                .class(),
            ConnectorErrorClass::Http429,
            "403 {reason}"
        );
    }
    // "domainPolicy: The domain administrators have disabled Gmail apps" is a
    // 403 that retrying will never fix.
    let policy = serde_json::to_vec(&google_error(403, "domainPolicy")).expect("serializes");
    assert_eq!(
        google_gmail::error_map()
            .classify(403, &reqwest::header::HeaderMap::new(), &policy)
            .class(),
        ConnectorErrorClass::Authentication
    );

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "120".parse().unwrap());
    assert_eq!(
        google_gmail::error_map()
            .classify(429, &headers, b"{}")
            .retry_after(),
        Some(Duration::from_secs(120))
    );
    headers.insert(reqwest::header::RETRY_AFTER, "99999999".parse().unwrap());
    assert_eq!(
        google_gmail::error_map()
            .classify(429, &headers, b"{}")
            .retry_after(),
        Some(Duration::from_secs(86_400))
    );
}

/// `google_gmail_pagination_is_bounded`.
#[tokio::test]
async fn google_gmail_pagination_is_bounded() {
    let plan = google_gmail::pagination("message.list").expect("message.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));

    let stub = ProviderStub::start([
        Expectation::new("GET", "/gmail/v1/users/me/messages")
            .query("q=&maxResults=500")
            .respond_json(
                200,
                json!({ "messages": [{ "id": "m1" }], "nextPageToken": "tok~1" }),
            ),
        Expectation::new("GET", "/gmail/v1/users/me/messages")
            .query("q=&maxResults=500&pageToken=tok%7E1")
            .respond_json(200, json!({ "messages": [{ "id": "m2" }] })),
    ])
    .await;
    assert_eq!(
        plan.collect(
            render(&stub, operation("message.list"), json!({ "q": "" })),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the declared plan walks both pages and stops"),
        vec![json!({ "id": "m1" }), json!({ "id": "m2" })]
    );
    stub.assert_satisfied();

    for budget in [
        PaginationBudget::new(2, 8, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 2, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 64, 100, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|index| {
            Expectation::new("GET", "/gmail/v1/users/me/messages").respond_json(
                200,
                json!({ "messages": [{ "id": index }], "nextPageToken": "tok~1" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, operation("message.list"), json!({ "q": "" })),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    // `labels.list` publishes no continuation, so it declares no plan.
    for id in ["label.list", "message.get", "message.send"] {
        assert!(google_gmail::pagination(id).is_none(), "{id}");
    }
}

/// `google_gmail_page_token_cannot_leave_origin`.
#[tokio::test]
async fn google_gmail_page_token_cannot_leave_origin() {
    let plan = google_gmail::pagination("thread.list").expect("thread.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));

    let elsewhere = ProviderStub::start([Expectation::new("GET", "/gmail/v1/users/me/threads")
        .respond_json(200, json!({ "threads": [] }))])
    .await;
    let hostile = format!("{}/gmail/v1/users/me/threads", elsewhere.base_url());

    let stub = ProviderStub::start([
        Expectation::new("GET", "/gmail/v1/users/me/threads")
            .respond_json(200, json!({ "threads": [], "nextPageToken": hostile })),
        Expectation::new("GET", "/gmail/v1/users/me/threads")
            .respond_json(200, json!({ "threads": [] })),
    ])
    .await;
    plan.collect(
        render(&stub, operation("thread.list"), json!({ "q": "" })),
        &stub.origin(),
        &budget,
        undeclared_status_gate,
        |request| {
            assert_eq!(request.url().host_str(), stub.origin().as_url().host_str());
            stub.send(request)
        },
    )
    .await
    .expect("the hostile token is a query value, not a destination");
    assert_eq!(elsewhere.mismatches().len(), 1);
    stub.assert_satisfied();
}

/// `google_gmail_partial_failure_is_typed`: Gmail publishes no per-item failure
/// shape for these operations, so the shape that must be refused is Google's
/// canonical error object arriving under a success status.
#[test]
fn google_gmail_partial_failure_is_typed() {
    let failure = google_gmail::decode(
        operation("message.list"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"messages":[],"error":{"code":403,"errors":[{"reason":"domainPolicy"}]}}"#,
    )
    .expect_err("an error envelope under a 200 is not a success");
    assert_eq!(failure.class(), ConnectorErrorClass::Permanent);
    assert_eq!(failure.code(), google::SUCCESS_CARRIES_ERROR.code());

    google_gmail::decode(
        operation("message.list"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"messages":[{"id":"m1"}],"resultSizeEstimate":1}"#,
    )
    .expect("a complete success decodes");
}

/// `google_gmail_effects_are_classified`.
#[test]
fn google_gmail_effects_are_classified() {
    assert_effects(
        google_gmail::connector(),
        &[
            ("message.get", EffectClass::ReadOnly),
            ("message.list", EffectClass::ReadOnly),
            ("message.send", EffectClass::AtMostOnce),
            ("message.modify_labels", EffectClass::InventoryOnly),
            ("message.trash", EffectClass::InventoryOnly),
            ("draft.get", EffectClass::ReadOnly),
            ("draft.list", EffectClass::ReadOnly),
            ("label.list", EffectClass::ReadOnly),
            ("label.create", EffectClass::AtMostOnce),
            ("label.delete", EffectClass::ProviderIdempotentNaturalMethod),
            ("thread.get", EffectClass::ReadOnly),
            ("thread.list", EffectClass::ReadOnly),
        ],
    );
    assert_eq!(
        google_gmail::connector().admit_operation("message.batch_modify"),
        Err(OperationRejection::Undeclared)
    );
}

/// The scope table is complete and per operation group: a mailbox reader is
/// never asked for `gmail.send`.
#[test]
fn google_gmail_scopes_are_declared_per_operation_group() {
    let least =
        google::declared_scopes(google_gmail::connector().operations(), google_gmail::scopes)
            .expect("every operation declares the scopes Google documents for it");
    assert_eq!(
        least,
        vec![
            "https://www.googleapis.com/auth/gmail.readonly",
            "https://www.googleapis.com/auth/gmail.send",
            "https://www.googleapis.com/auth/gmail.modify",
            "https://www.googleapis.com/auth/gmail.labels",
        ]
    );

    let readonly = vec!["https://www.googleapis.com/auth/gmail.readonly".to_owned()];
    let reads = [
        "message.get".to_owned(),
        "message.list".to_owned(),
        "thread.list".to_owned(),
        "label.list".to_owned(),
    ];
    assert!(
        google::scope_report(google_gmail::scopes, &reads, &readonly).is_empty(),
        "a mailbox reader grants gmail.readonly and nothing else"
    );

    let with_send = [reads.as_slice(), &["message.send".to_owned()]].concat();
    assert_eq!(
        google::scope_report(google_gmail::scopes, &with_send, &readonly).missing,
        vec![(
            "message.send".to_owned(),
            "https://www.googleapis.com/auth/gmail.send"
        )]
    );

    // A deployment that holds `gmail.send` but enables no send is told so.
    let surplus = google::scope_report(
        google_gmail::scopes,
        &reads,
        &[
            "https://www.googleapis.com/auth/gmail.readonly".to_owned(),
            "https://www.googleapis.com/auth/gmail.send".to_owned(),
        ],
    );
    assert_eq!(
        surplus.surplus,
        vec!["https://www.googleapis.com/auth/gmail.send".to_owned()]
    );
}

/// `google_gmail_output_contract`.
#[test]
fn google_gmail_output_contract() {
    assert_eq!(
        google_gmail::decode(
            operation("message.get"),
            200,
            &reqwest::header::HeaderMap::new(),
            serde_json::to_vec(&message_body())
                .expect("a fixture serializes")
                .as_slice(),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": MESSAGE,
            "thread_id": "18f0a1b2c3d4e5f0",
            "label_ids": ["INBOX", "UNREAD"],
            "snippet": "a fixture message",
            "internal_date": "1786000000000",
            "size_estimate": 4096,
            "payload": { "mimeType": "text/plain" },
        })
    );

    for body in [
        br#"{"threadId":"t"}"#.as_slice(),
        br#"{"id":"m","threadId":null}"#.as_slice(),
        br#"{"id":"m","threadId":7}"#.as_slice(),
        br#"nope"#.as_slice(),
    ] {
        assert_eq!(
            google_gmail::decode(
                operation("message.get"),
                200,
                &reqwest::header::HeaderMap::new(),
                body
            )
            .expect_err("a missing, mistyped, or unparseable body is a validation failure")
            .class(),
            ConnectorErrorClass::Validation
        );
    }

    // Gmail omits `messages` entirely when a query matches nothing; the
    // declaration says so, so a Process binds one shape either way.
    assert_eq!(
        google_gmail::decode(
            operation("message.list"),
            200,
            &reqwest::header::HeaderMap::new(),
            br#"{"resultSizeEstimate":0}"#,
        )
        .expect("an empty mailbox listing is a success"),
        json!({ "messages": null, "next_page_token": null, "result_size_estimate": 0 })
    );

    assert_eq!(
        google_gmail::decode(
            operation("label.delete"),
            204,
            &reqwest::header::HeaderMap::new(),
            b"",
        )
        .expect("an empty success is a success"),
        json!({})
    );
}
