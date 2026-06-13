//! Native conformance for cron (scheduled) triggers.
//!
//! tests-py has no fixtures that exercise scheduled delivery (it depends on
//! wall-clock timing and an external receiver), so these are native tests, in
//! the spirit of `remote_schemas.rs` / `actions.rs`: a recording webhook stub
//! plus a suite database with the `donat` catalog migrated in.
//!
//! Timing is made deterministic by *seeding a past-due occurrence directly*
//! into `donat.cron_events` and using a schedule far in the future
//! (`0 0 1 1 *`), so the engine's own materialization never fires inside the
//! test window — only the seeded row does.

use std::time::{Duration, Instant};

use donat_conformance::Suite;
use donat_conformance::cron_webhook::{CronWebhook, Received};
use donat_metadata::CronTrigger;
use serde_json::{Value as Json, json};

/// A cron trigger whose webhook resolves to the suite's stub at `path`. The
/// schedule is yearly (Jan 1) so materialization stays out of the test window.
fn cron_trigger(name: &str, path: &str, payload: Json, retry: Json, headers: Json) -> CronTrigger {
    serde_json::from_value(json!({
        "name": name,
        "webhook": format!("{{{{CRON_WEBHOOK_BASE}}}}{path}"),
        "schedule": "0 0 1 1 *",
        "payload": payload,
        "retry_conf": retry,
        "headers": headers,
    }))
    .expect("valid cron trigger")
}

/// Seed a past-due occurrence directly (bypassing the schedule), so delivery
/// is observable immediately instead of waiting for a real cron boundary.
fn seed_past_due(db_url: &str, trigger: &str, seconds_ago: i64) {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.execute(
        &format!(
            "INSERT INTO donat.cron_events (trigger_name, scheduled_time) \
             VALUES ($1, now() - interval '{seconds_ago} seconds')"
        ),
        &[&trigger],
    )
    .expect("seed cron event");
}

fn count(db_url: &str, sql: &str, trigger: &str) -> i64 {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.query_one(sql, &[&trigger]).expect("count query").get(0)
}

fn status_count(db_url: &str, trigger: &str, status: &str) -> i64 {
    let mut c = postgres::Client::connect(db_url, postgres::NoTls).expect("connect suite db");
    c.query_one(
        "SELECT count(*) FROM donat.cron_events WHERE trigger_name = $1 AND status = $2",
        &[&trigger, &status],
    )
    .expect("status count")
    .get(0)
}

fn invocation_count(db_url: &str, trigger: &str) -> i64 {
    count(
        db_url,
        "SELECT count(*) FROM donat.cron_event_invocation_logs l \
         JOIN donat.cron_events e ON e.id = l.event_id \
         WHERE e.trigger_name = $1",
        trigger,
    )
}

fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for: {what}");
}

fn received_for(cw: &CronWebhook, path: &str) -> Vec<Received> {
    cw.received().into_iter().filter(|r| r.path == path).collect()
}

#[test]
fn cron_event_fires_and_delivers_the_hasura_envelope() {
    let s = Suite::new("cron_deliver").with_cron_webhook().start();
    s.add_cron_trigger(cron_trigger(
        "reminders",
        "/ok",
        json!({ "kind": "reminder" }),
        json!({ "num_retries": 0 }),
        json!([{ "name": "X-Cron-Test", "value": "yes" }]),
    ));
    // Force the engine (and its cron loop) to start, then seed a due event.
    let _ = s.base_url();
    seed_past_due(s.db_url(), "reminders", 30);

    let cw = s.cron_webhook().clone();
    wait_until(
        || cw.count_for("/ok") >= 1,
        Duration::from_secs(15),
        "delivery to /ok",
    );

    let got = received_for(&cw, "/ok");
    assert_eq!(got.len(), 1, "exactly one delivery expected");
    let body = &got[0].body;
    assert_eq!(body["name"], json!("reminders"));
    assert_eq!(body["payload"], json!({ "kind": "reminder" }));
    assert!(body["id"].is_string(), "envelope carries an event id");
    assert!(
        body["scheduled_time"].is_string(),
        "envelope carries scheduled_time"
    );
    // Custom header reached the webhook.
    assert!(
        got[0]
            .headers
            .iter()
            .any(|(k, v)| k == "x-cron-test" && v == "yes"),
        "custom header forwarded; got {:?}",
        got[0].headers
    );

    wait_until(
        || status_count(s.db_url(), "reminders", "delivered") == 1,
        Duration::from_secs(5),
        "event marked delivered",
    );
    assert_eq!(
        invocation_count(s.db_url(), "reminders"),
        1,
        "one invocation log row"
    );
}

#[test]
fn cron_event_retries_until_success() {
    let s = Suite::new("cron_retry").with_cron_webhook().start();
    s.add_cron_trigger(cron_trigger(
        "retry_t",
        "/fail-then-ok",
        json!({}),
        json!({ "num_retries": 3, "retry_interval_seconds": 1 }),
        json!([]),
    ));
    let _ = s.base_url();
    seed_past_due(s.db_url(), "retry_t", 5);

    let cw = s.cron_webhook().clone();
    // First attempt 500, second attempt 200.
    wait_until(
        || cw.count_for("/fail-then-ok") >= 2,
        Duration::from_secs(20),
        "retry then success",
    );

    wait_until(
        || status_count(s.db_url(), "retry_t", "delivered") == 1,
        Duration::from_secs(5),
        "event marked delivered after retry",
    );
    assert_eq!(
        invocation_count(s.db_url(), "retry_t"),
        2,
        "two invocation log rows (failed + succeeded)"
    );
}

#[test]
fn cron_event_past_tolerance_is_dropped() {
    let s = Suite::new("cron_tolerance").with_cron_webhook().start();
    s.add_cron_trigger(cron_trigger(
        "late_t",
        "/ok",
        json!({}),
        json!({ "tolerance_seconds": 10 }),
        json!([]),
    ));
    let _ = s.base_url();
    // One hour late, tolerance is 10s → dropped without delivery.
    seed_past_due(s.db_url(), "late_t", 3600);

    wait_until(
        || status_count(s.db_url(), "late_t", "dead") == 1,
        Duration::from_secs(10),
        "late event dropped as dead",
    );
    let cw = s.cron_webhook().clone();
    assert_eq!(cw.count_for("/ok"), 0, "no delivery for a dropped event");
    assert_eq!(
        invocation_count(s.db_url(), "late_t"),
        0,
        "no invocation log for a dropped event"
    );
}
