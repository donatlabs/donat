//! Native conformance for table event triggers, ported from tests-py
//! `test_events.py` (`TestCreateEventQuery::test_basic` and
//! `TestEventRetryConf`).
//!
//! tests-py drives delivery to a recording webhook server and asserts the
//! event envelope; we do the same with a native recording stub. Hasura's
//! `create_event_trigger` is a runtime metadata call this engine does not
//! have — here the trigger is declared in YAML (under the table) and its
//! per-table Postgres triggers are created by `migrate --metadata-dir`
//! (reconcile), exactly as a real deploy would.
//!
//! Difference from Hasura, by design: this engine has no admin role, so
//! mutations run as an explicit `tester` role (via
//! HASURA_GRAPHQL_UNAUTHORIZED_ROLE). Session variables are not yet captured
//! into the event payload (the engine does not set the `hasura.user` GUC), so
//! `event.session_variables` is currently null — asserted as such here.

use std::time::{Duration, Instant};

use donat_conformance::Suite;
use donat_conformance::cron_webhook::{CronWebhook, Received};
use donat_metadata::{EventTrigger, QualifiedTable};
use serde_json::{Value as Json, json};

const TABLE: &str = "test_t1";

fn table_ref() -> QualifiedTable {
    serde_json::from_value(json!({ "schema": "hge_tests", "name": TABLE })).unwrap()
}

fn event_trigger(name: &str, webhook_suffix: &str, retry: Json) -> EventTrigger {
    serde_json::from_value(json!({
        "name": name,
        "definition": {
            "enable_manual": false,
            "insert": { "columns": "*" },
            "update": { "columns": "*" },
            "delete": { "columns": "*" },
        },
        "webhook": format!("{{{{EVENT_WEBHOOK_HANDLER}}}}{webhook_suffix}"),
        "retry_conf": retry,
    }))
    .expect("valid event trigger")
}

/// Bring up a suite with: the hge_tests.test_t1 table, a `tester` role with
/// full DML permissions, and the given event trigger — then force the engine
/// (migrate + reconcile + serve) to start.
fn setup(name: &str, trigger: EventTrigger) -> donat_conformance::Running {
    let r = Suite::new(name)
        .env("HASURA_GRAPHQL_UNAUTHORIZED_ROLE", "tester")
        .with_event_webhook()
        .start();

    // Create the table (applied in-harness, before the engine/reconcile).
    r.post(
        "/v2/query",
        &json!({
            "type": "run_sql",
            "args": { "sql":
                "create schema if not exists hge_tests; \
                 create table hge_tests.test_t1 (c1 int, c2 text);" }
        }),
        &[],
    );

    // Track the table and grant the tester role full DML.
    let table = json!({ "schema": "hge_tests", "name": TABLE });
    r.post(
        "/v1/metadata",
        &json!({
            "type": "bulk",
            "args": [
                { "type": "track_table", "args": { "table": table } },
                { "type": "create_insert_permission", "args": {
                    "table": table, "role": "tester",
                    "permission": { "columns": ["c1", "c2"], "check": {} } } },
                { "type": "create_select_permission", "args": {
                    "table": table, "role": "tester",
                    "permission": { "columns": ["c1", "c2"], "filter": {} } } },
                { "type": "create_update_permission", "args": {
                    "table": table, "role": "tester",
                    "permission": { "columns": ["c1", "c2"], "filter": {}, "check": {} } } },
                { "type": "create_delete_permission", "args": {
                    "table": table, "role": "tester", "permission": { "filter": {} } } },
            ]
        }),
        &[],
    );

    r.add_event_trigger(&table_ref(), trigger);
    // Force engine start (migrate + event-trigger reconcile + serve).
    let _ = r.base_url();
    r
}

fn gql(r: &donat_conformance::Running, query: &str) {
    let (status, body) = r.post("/v1/graphql", &json!({ "query": query }), &[]);
    assert_eq!(status, 200, "graphql HTTP status; body: {body}");
    assert!(body.get("errors").is_none(), "graphql errors: {body}");
}

fn wait_events(stub: &CronWebhook, n: usize, timeout: Duration) -> Vec<Received> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if stub.received().len() >= n {
            return stub.received();
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    panic!(
        "timed out waiting for {n} events; got {}",
        stub.received().len()
    );
}

/// Find the (single) received event with the given op and assert its envelope.
fn assert_event(received: &[Received], op: &str, exp_data: Json) {
    let ev = received
        .iter()
        .find(|r| r.body["event"]["op"] == json!(op))
        .unwrap_or_else(|| panic!("no {op} event among {} received", received.len()));
    let body = &ev.body;
    assert_eq!(ev.path, "/", "webhook path");
    assert_eq!(body["table"], json!({ "schema": "hge_tests", "name": TABLE }));
    assert_eq!(body["trigger"]["name"], json!("t1_all"));
    assert_eq!(body["event"]["op"], json!(op));
    assert_eq!(body["event"]["data"], exp_data, "event data for {op}");
    assert!(body["id"].is_string(), "envelope has id");
    assert!(body["created_at"].is_string(), "envelope has created_at");
    // Not yet captured by this engine (no admin role / no hasura.user GUC).
    assert_eq!(body["event"]["session_variables"], Json::Null);
    assert_eq!(body["delivery_info"]["current_retry"], json!(0));
}

#[test]
fn event_trigger_fires_on_insert_update_delete() {
    let r = setup(
        "events_basic",
        event_trigger("t1_all", "", json!({ "num_retries": 0 })),
    );
    let stub = r.event_webhook().clone();

    // INSERT
    gql(&r, r#"mutation { insert_hge_tests_test_t1(objects: [{c1: 1, c2: "hello"}]) { affected_rows } }"#);
    let got = wait_events(&stub, 1, Duration::from_secs(15));
    assert_event(&got, "INSERT", json!({ "old": null, "new": { "c1": 1, "c2": "hello" } }));

    // UPDATE
    gql(&r, r#"mutation { update_hge_tests_test_t1(where: {c1: {_eq: 1}}, _set: {c2: "world"}) { affected_rows } }"#);
    let got = wait_events(&stub, 2, Duration::from_secs(15));
    assert_event(
        &got,
        "UPDATE",
        json!({ "old": { "c1": 1, "c2": "hello" }, "new": { "c1": 1, "c2": "world" } }),
    );

    // DELETE
    gql(&r, r#"mutation { delete_hge_tests_test_t1(where: {c1: {_eq: 1}}) { affected_rows } }"#);
    let got = wait_events(&stub, 3, Duration::from_secs(15));
    assert_event(&got, "DELETE", json!({ "old": { "c1": 1, "c2": "world" }, "new": null }));
}

#[test]
fn event_trigger_retries_then_errors_on_failing_webhook() {
    // num_retries=2, interval 1s, webhook always 500 → 3 attempts total
    // (current_retry 0,1,2), then the event is marked 'error'.
    let r = setup(
        "events_retry",
        event_trigger(
            "t1_all",
            "/fail",
            json!({ "num_retries": 2, "interval_sec": 1, "timeout_sec": 5 }),
        ),
    );
    let stub = r.event_webhook().clone();

    gql(&r, r#"mutation { insert_hge_tests_test_t1(objects: [{c1: 1, c2: "hello"}]) { affected_rows } }"#);

    // 3 delivery attempts to /fail.
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && stub.count_for("/fail") < 3 {
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        stub.count_for("/fail") >= 3,
        "expected >=3 attempts, got {}",
        stub.count_for("/fail")
    );

    // The event ends up in 'error' with one invocation log per attempt.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut errored = 0;
    while Instant::now() < deadline {
        errored = status_count(r.db_url(), "t1_all", "error");
        if errored == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert_eq!(errored, 1, "event marked error after retries");
    assert_eq!(
        invocation_count(r.db_url(), "t1_all"),
        3,
        "one invocation log per attempt (3)"
    );
}

fn status_count(db_url: &str, trigger: &str, status: &str) -> i64 {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.query_one(
        "SELECT count(*) FROM donat.event_log WHERE trigger_name = $1 AND status = $2",
        &[&trigger, &status],
    )
    .expect("status count")
    .get(0)
}

fn invocation_count(db_url: &str, trigger: &str) -> i64 {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.query_one(
        "SELECT count(*) FROM donat.event_invocation_logs l \
         JOIN donat.event_log e ON e.id = l.event_id \
         WHERE e.trigger_name = $1",
        &[&trigger],
    )
    .expect("invocation count")
    .get(0)
}
