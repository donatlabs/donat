//! Google Calendar connector proofs (spec 012 §3 plus spec 014 §3), against the
//! SDK's local provider stub.
//!
//! The two proofs that need a database — `google_calendar_scope_shortfall_fails_closed`
//! and `google_calendar_refresh_happens_once_under_concurrency` — live in
//! `crates/server/tests/google_workspace.rs`.

mod google_workspace_support;

use std::time::Duration;

use donat_connectors::providers::{google, google_calendar};
use donat_connectors::sdk::testing::{Expectation, ProviderStub, SECRET_SENTINEL};
use donat_connectors::sdk::undeclared_status_gate;
use donat_connectors::sdk::{
    ConnectorErrorClass, EffectClass, Operation, OperationRejection, PaginationBudget,
};
use google_workspace_support::{assert_effects, documented_failures, google_error, render};
use serde_json::{Value as JsonValue, json};

/// A calendar identifier is an address, so it percent-encodes to this.
const CALENDAR: &str = "team@example.test";
const CALENDAR_ENCODED: &str = "team%40example%2Etest";
const EVENT: &str = "evt0000000000001";

fn operation(id: &str) -> &'static Operation {
    google_calendar::connector()
        .operation(id)
        .unwrap_or_else(|| panic!("the google_calendar declaration publishes {id}"))
}

fn event_body() -> JsonValue {
    json!({
        "id": EVENT,
        "status": "confirmed",
        "summary": "Standup",
        "start": { "dateTime": "2026-08-10T09:00:00Z" },
        "end": { "dateTime": "2026-08-10T09:15:00Z" },
        "attendees": [{ "email": "someone@example.test" }],
        "htmlLink": "https://calendar.google.com/event?eid=abc",
        "updated": "2026-08-09T12:00:00.000Z",
        "etag": "\"3491\"",
    })
}

fn window() -> JsonValue {
    json!({
        "calendar_id": CALENDAR,
        "time_min": "2026-08-10T00:00:00Z",
        "time_max": "2026-08-11T00:00:00Z",
    })
}

/// `google_calendar_request_shape`.
#[tokio::test]
async fn google_calendar_request_shape() {
    let events = format!("/calendar/v3/calendars/{CALENDAR_ENCODED}/events");
    let stub = ProviderStub::start([
        Expectation::new("GET", &format!("{events}/{EVENT}"))
            .query("")
            .no_body()
            .respond_json(200, event_body()),
        Expectation::new("GET", &events)
            .query(
                "timeMin=2026%2D08%2D10T00%3A00%3A00Z&timeMax=2026%2D08%2D11T00%3A00%3A00Z&maxResults=250",
            )
            .respond_json(200, json!({ "items": [] })),
        Expectation::new("POST", &events)
            .header("content-type", "application/json")
            .json_body(json!({
                "summary": "Standup",
                "start": { "dateTime": "2026-08-10T09:00:00Z" },
                "end": { "dateTime": "2026-08-10T09:15:00Z" },
            }))
            .respond_json(200, event_body()),
        Expectation::new("PUT", &format!("{events}/{EVENT}"))
            .json_body(json!({
                "summary": "Standup",
                "start": { "dateTime": "2026-08-10T09:00:00Z" },
                "end": { "dateTime": "2026-08-10T09:15:00Z" },
            }))
            .respond_json(200, event_body()),
        Expectation::new("DELETE", &format!("{events}/{EVENT}"))
            .no_body()
            .respond_bytes(204, Vec::new()),
        Expectation::new("POST", "/calendar/v3/freeBusy")
            .json_body(json!({
                "timeMin": "2026-08-10T00:00:00Z",
                "timeMax": "2026-08-11T00:00:00Z",
                "items": [{ "id": CALENDAR }],
            }))
            .respond_json(
                200,
                json!({ "calendars": { CALENDAR: { "busy": [] } }, "timeMin": "2026-08-10T00:00:00Z" }),
            ),
        // A hostile calendar identifier stays one percent-encoded segment.
        Expectation::new(
            "GET",
            &format!(
                "/calendar/v3/calendars/%2E%2E%2F%2E%2E%2FfreeBusy%3Fx%3D1%23y/events/{EVENT}"
            ),
        )
        .respond_json(200, event_body()),
    ])
    .await;

    let event_input = json!({
        "summary": "Standup",
        "start": { "dateTime": "2026-08-10T09:00:00Z" },
        "end": { "dateTime": "2026-08-10T09:15:00Z" },
        "calendar_id": CALENDAR,
        "event_id": EVENT,
    });
    for (id, input) in [
        (
            "event.get",
            json!({ "calendar_id": CALENDAR, "event_id": EVENT }),
        ),
        ("event.list", window()),
        ("event.insert", event_input.clone()),
        ("event.update", event_input),
        (
            "event.delete",
            json!({ "calendar_id": CALENDAR, "event_id": EVENT }),
        ),
        (
            "freebusy.query",
            json!({
                "time_min": "2026-08-10T00:00:00Z",
                "time_max": "2026-08-11T00:00:00Z",
                "items": [{ "id": CALENDAR }],
            }),
        ),
    ] {
        stub.send(render(&stub, operation(id), input))
            .await
            .expect("the stub answers");
    }

    let hostile = render(
        &stub,
        operation("event.get"),
        json!({ "calendar_id": "../../freeBusy?x=1#y", "event_id": EVENT }),
    );
    assert_eq!(hostile.url().host_str(), stub.origin().as_url().host_str());
    assert_eq!(hostile.url().query(), None);
    stub.send(hostile).await.expect("the stub answers");
    stub.assert_satisfied();
}

/// `google_calendar_auth_is_applied`.
#[tokio::test]
async fn google_calendar_auth_is_applied() {
    let stub = ProviderStub::start([Expectation::new(
        "GET",
        &format!("/calendar/v3/calendars/{CALENDAR_ENCODED}/events/{EVENT}"),
    )
    .header("authorization", &format!("Bearer {SECRET_SENTINEL}"))
    .without_header("x-goog-api-key")
    .respond_json(200, event_body())])
    .await;

    let request = render(
        &stub,
        operation("event.get"),
        json!({ "calendar_id": CALENDAR, "event_id": EVENT }),
    );
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
    let failure = google_calendar::error_map().classify_response(&response);
    let surface = format!(
        "{} {} {} {failure:?} {:?}",
        failure.code(),
        failure.safe_message(),
        failure.diagnostic(),
        google_calendar::connector().credential(),
    );
    assert!(!surface.contains(SECRET_SENTINEL), "{surface}");
    assert!(
        google_calendar::connector()
            .credential()
            .fields()
            .is_empty()
    );
    stub.assert_satisfied();
}

/// `google_calendar_error_map`, including the two statuses only Calendar
/// documents: `410 Gone` and `412 Precondition Failed`.
#[test]
fn google_calendar_error_map() {
    for (status, reason, expected) in documented_failures() {
        let body = serde_json::to_vec(&google_error(status, reason)).expect("a fixture serializes");
        let failure = google_calendar::error_map().classify(
            status,
            &reqwest::header::HeaderMap::new(),
            &body,
        );
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

    // "410 Gone … can also occur if a request attempts to delete an event that
    // has already been deleted", which is the evidence `event.delete` stands
    // on: the repeat is answered, and the class says asking again will answer
    // the same way.
    let gone = serde_json::to_vec(&google_error(410, "deleted")).expect("a fixture serializes");
    assert_eq!(
        google_calendar::error_map()
            .classify(410, &reqwest::header::HeaderMap::new(), &gone)
            .class(),
        ConnectorErrorClass::Permanent
    );
}

/// `google_calendar_quota_errors_are_classified`.
#[test]
fn google_calendar_quota_errors_are_classified() {
    // "The 403 status code encompasses multiple error reasons:
    // userRateLimitExceeded, rateLimitExceeded, quotaExceeded,
    // forbiddenForNonOrganizer." Three of the four are quota; the fourth is a
    // permission decision and must not be retried as one.
    for (reason, expected) in [
        ("userRateLimitExceeded", ConnectorErrorClass::Http429),
        ("rateLimitExceeded", ConnectorErrorClass::Http429),
        ("quotaExceeded", ConnectorErrorClass::Http429),
        (
            "forbiddenForNonOrganizer",
            ConnectorErrorClass::Authentication,
        ),
    ] {
        let body = serde_json::to_vec(&google_error(403, reason)).expect("a fixture serializes");
        assert_eq!(
            google_calendar::error_map()
                .classify(403, &reqwest::header::HeaderMap::new(), &body)
                .class(),
            expected,
            "403 {reason}"
        );
    }

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::RETRY_AFTER, "45".parse().unwrap());
    assert_eq!(
        google_calendar::error_map()
            .classify(429, &headers, b"{}")
            .retry_after(),
        Some(Duration::from_secs(45))
    );
    headers.insert(reqwest::header::RETRY_AFTER, "31536000".parse().unwrap());
    assert_eq!(
        google_calendar::error_map()
            .classify(429, &headers, b"{}")
            .retry_after(),
        Some(Duration::from_secs(86_400)),
        "a year-long hint is clamped to the SDK's ceiling"
    );
}

/// `google_calendar_pagination_is_bounded`.
#[tokio::test]
async fn google_calendar_pagination_is_bounded() {
    let plan = google_calendar::pagination("event.list").expect("event.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));
    let events = format!("/calendar/v3/calendars/{CALENDAR_ENCODED}/events");

    let stub = ProviderStub::start([
        Expectation::new("GET", &events).respond_json(
            200,
            json!({ "items": [{ "id": "e1" }], "nextPageToken": "tok~1" }),
        ),
        // The last page carries `nextSyncToken` instead, which Google documents
        // as "Omitted if further results are available" — it is not a
        // continuation and the walk stops.
        Expectation::new("GET", &events).respond_json(
            200,
            json!({ "items": [{ "id": "e2" }], "nextSyncToken": "sync~1" }),
        ),
    ])
    .await;
    assert_eq!(
        plan.collect(
            render(&stub, operation("event.list"), window()),
            &stub.origin(),
            &budget,
            undeclared_status_gate,
            |request| stub.send(request),
        )
        .await
        .expect("the declared plan walks both pages and stops"),
        vec![json!({ "id": "e1" }), json!({ "id": "e2" })]
    );
    stub.assert_satisfied();

    for budget in [
        PaginationBudget::new(2, 8, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 2, 64, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 2, 64 * 1024, Duration::from_secs(5)),
        PaginationBudget::new(8, 8, 64, 100, Duration::from_secs(5)),
    ] {
        let stub = ProviderStub::start((0..12).map(|index| {
            Expectation::new("GET", &events).respond_json(
                200,
                json!({ "items": [{ "id": index }], "nextPageToken": "tok~1" }),
            )
        }))
        .await;
        let failure = plan
            .collect(
                render(&stub, operation("event.list"), window()),
                &stub.origin(),
                &budget,
                undeclared_status_gate,
                |request| stub.send(request),
            )
            .await
            .expect_err("an endless provider exhausts the budget");
        assert_eq!(failure.code(), "connector_pagination_budget");
    }

    for id in ["event.get", "event.insert", "freebusy.query"] {
        assert!(google_calendar::pagination(id).is_none(), "{id}");
    }
}

/// `google_calendar_page_token_cannot_leave_origin`.
#[tokio::test]
async fn google_calendar_page_token_cannot_leave_origin() {
    let plan = google_calendar::pagination("event.list").expect("event.list declares a plan");
    let budget = PaginationBudget::new(8, 8, 64, 64 * 1024, Duration::from_secs(5));
    let events = format!("/calendar/v3/calendars/{CALENDAR_ENCODED}/events");

    let elsewhere = ProviderStub::start([
        Expectation::new("GET", &events).respond_json(200, json!({ "items": [] }))
    ])
    .await;
    let hostile = format!("{}{events}", elsewhere.base_url());

    let stub = ProviderStub::start([
        Expectation::new("GET", &events)
            .respond_json(200, json!({ "items": [], "nextPageToken": hostile })),
        Expectation::new("GET", &events).respond_json(200, json!({ "items": [] })),
    ])
    .await;
    plan.collect(
        render(&stub, operation("event.list"), window()),
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

/// `google_calendar_partial_failure_is_typed`: the one documented per-item
/// failure inside a `200` in this batch.
#[test]
fn google_calendar_partial_failure_is_typed() {
    // Google's four documented `freeBusy` reasons, each landing on exactly one
    // class, and a reason Google has not published yet taking the fallback it
    // tells clients to expect.
    for (reason, expected) in [
        ("notFound", ConnectorErrorClass::Permanent),
        ("groupTooBig", ConnectorErrorClass::Validation),
        ("tooManyCalendarsRequested", ConnectorErrorClass::Validation),
        ("internalError", ConnectorErrorClass::Http5xx),
        ("somethingGoogleAddsLater", ConnectorErrorClass::Permanent),
    ] {
        let body = format!(
            r#"{{"calendars":{{"team@example.test":{{"busy":[],"errors":[{{"domain":"calendar","reason":"{reason}"}}]}}}}}}"#
        );
        let failure = google_calendar::decode(
            operation("freebusy.query"),
            200,
            &reqwest::header::HeaderMap::new(),
            body.as_bytes(),
        )
        .expect_err("a per-calendar failure inside a 200 is never a success");
        assert_eq!(failure.class(), expected, "{reason}");
        assert_eq!(failure.code(), "google_partial_failure", "{reason}");
    }

    // A group expansion reports its failures in the same shape.
    assert!(
        google_calendar::decode(
            operation("freebusy.query"),
            200,
            &reqwest::header::HeaderMap::new(),
            br#"{"calendars":{},"groups":{"g":{"errors":[{"reason":"groupTooBig"}]}}}"#,
        )
        .is_err(),
        "a per-group failure is refused exactly as a per-calendar one is"
    );

    // A complete answer, and an empty `errors` list, both decode.
    google_calendar::decode(
        operation("freebusy.query"),
        200,
        &reqwest::header::HeaderMap::new(),
        br#"{"calendars":{"team@example.test":{"busy":[{"start":"2026-08-10T09:00:00Z","end":"2026-08-10T09:15:00Z"}],"errors":[]}}}"#,
    )
    .expect("a complete free/busy answer decodes");

    // ...and the shared fail-closed rule still applies to every operation.
    assert_eq!(
        google_calendar::decode(
            operation("event.get"),
            200,
            &reqwest::header::HeaderMap::new(),
            br#"{"error":{"code":404,"errors":[{"reason":"notFound"}]}}"#,
        )
        .expect_err("an error envelope under a 200 is not a success")
        .code(),
        google::SUCCESS_CARRIES_ERROR.code()
    );
}

/// `google_calendar_effects_are_classified`.
#[test]
fn google_calendar_effects_are_classified() {
    assert_effects(
        google_calendar::connector(),
        &[
            ("event.get", EffectClass::ReadOnly),
            ("event.list", EffectClass::ReadOnly),
            ("event.insert", EffectClass::InventoryOnly),
            ("event.update", EffectClass::ProviderIdempotentNaturalMethod),
            ("event.delete", EffectClass::ProviderIdempotentNaturalMethod),
            // A `POST` that Google documents as returning information and
            // changing nothing: the mutation-shaped read ADR 042 admits
            // evidence for.
            ("freebusy.query", EffectClass::ReadOnly),
        ],
    );
    assert_eq!(
        google_calendar::connector().admit_operation("event.patch"),
        Err(OperationRejection::Undeclared)
    );
    assert_eq!(
        operation("freebusy.query").method(),
        donat_connectors::sdk::HttpMethod::Post,
        "the read-only class here rests on Google's statement, not on the method"
    );
}

/// The scope table is complete and per operation group.
#[test]
fn google_calendar_scopes_are_declared_per_operation_group() {
    let least = google::declared_scopes(
        google_calendar::connector().operations(),
        google_calendar::scopes,
    )
    .expect("every operation declares the scopes Google documents for it");
    assert_eq!(
        least,
        vec![
            "https://www.googleapis.com/auth/calendar.events.readonly",
            "https://www.googleapis.com/auth/calendar.events",
            "https://www.googleapis.com/auth/calendar.freebusy",
        ]
    );

    let readonly = vec!["https://www.googleapis.com/auth/calendar.events.readonly".to_owned()];
    let reads = ["event.get".to_owned(), "event.list".to_owned()];
    assert!(
        google::scope_report(google_calendar::scopes, &reads, &readonly).is_empty(),
        "a calendar reader is never forced to grant calendar.events"
    );

    let with_write = [reads.as_slice(), &["event.insert".to_owned()]].concat();
    assert_eq!(
        google::scope_report(google_calendar::scopes, &with_write, &readonly).missing,
        vec![(
            "event.insert".to_owned(),
            "https://www.googleapis.com/auth/calendar.events"
        )]
    );
}

/// `google_calendar_output_contract`.
#[test]
fn google_calendar_output_contract() {
    assert_eq!(
        google_calendar::decode(
            operation("event.get"),
            200,
            &reqwest::header::HeaderMap::new(),
            serde_json::to_vec(&event_body())
                .expect("a fixture serializes")
                .as_slice(),
        )
        .expect("the declared contract is satisfied"),
        json!({
            "id": EVENT,
            "status": "confirmed",
            "summary": "Standup",
            "start": { "dateTime": "2026-08-10T09:00:00Z" },
            "end": { "dateTime": "2026-08-10T09:15:00Z" },
            "attendees": [{ "email": "someone@example.test" }],
            "html_link": "https://calendar.google.com/event?eid=abc",
            "updated": "2026-08-09T12:00:00.000Z",
            "etag": "\"3491\"",
        })
    );

    for body in [
        br#"{"summary":"Standup"}"#.as_slice(),
        br#"{"id":null}"#.as_slice(),
        br#"{"id":7}"#.as_slice(),
        br#"not json"#.as_slice(),
    ] {
        assert_eq!(
            google_calendar::decode(
                operation("event.get"),
                200,
                &reqwest::header::HeaderMap::new(),
                body
            )
            .expect_err("a missing, mistyped, or unparseable body is a validation failure")
            .class(),
            ConnectorErrorClass::Validation
        );
    }

    assert_eq!(
        google_calendar::decode(
            operation("event.delete"),
            204,
            &reqwest::header::HeaderMap::new(),
            b"",
        )
        .expect("an empty success is a success"),
        json!({})
    );
}
