//! Cron (scheduled) triggers: a deploy-time-configured webhook fired on a
//! cron schedule with a static payload.
//!
//! There is no runtime admin surface; cron triggers come from YAML metadata
//! (`cron_triggers`). The catalog tables in `dist_api` are created by
//! `migrate` (the serving binary never runs DDL); this module only reads and
//! writes rows.
//!
//! Lifecycle (see [`spawn`]): a single background task periodically
//! *materializes* the next occurrence of each trigger into
//! `dist_api.cron_events`, then *delivers* due events to their webhook with
//! at-least-once semantics — claim with `FOR UPDATE SKIP LOCKED`, deliver
//! while holding the row lock (a crash rolls the claim back, so the event is
//! re-delivered), retry per `retry_conf`, and record an invocation log per
//! attempt.

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use croner::Cron;
use serde_json::{Value as Json, json};

use dist_metadata::{ActionHeader, CronTrigger};

use crate::remote::resolve_url_template;
use crate::state::SharedState;

/// The next scheduled occurrence strictly after `after`, evaluated in UTC.
/// Returns `None` if the schedule is not a valid cron expression or has no
/// future occurrence.
pub fn next_after(schedule: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let cron = Cron::from_str(schedule).ok()?;
    cron.find_next_occurrence(&after, false).ok()
}

/// Start the cron delivery loop as a background task. No-op (the task exits
/// immediately) when the metadata declares no cron triggers, so a plain
/// deployment without cron never touches the `dist_api` catalog.
pub fn spawn(state: SharedState) {
    tokio::spawn(async move { run(state).await });
}

async fn run(state: SharedState) {
    let has_triggers = !state.engine.read().await.metadata.cron_triggers.is_empty();
    if !has_triggers {
        return;
    }
    let interval = std::env::var("DIST_API_CRON_POLL_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .max(1);
    let interval = Duration::from_secs(interval);
    tracing::info!(poll_seconds = interval.as_secs(), "cron delivery loop started");
    loop {
        if let Err(e) = tick(&state).await {
            tracing::warn!(error = %e, "cron tick failed");
        }
        tokio::time::sleep(interval).await;
    }
}

/// One materialize-then-deliver pass.
async fn tick(state: &SharedState) -> anyhow::Result<()> {
    let triggers = { state.engine.read().await.metadata.cron_triggers.clone() };
    if triggers.is_empty() {
        return Ok(());
    }
    let pool = state
        .default_pool()
        .await
        .ok_or_else(|| anyhow::anyhow!("no default source"))?;
    let mut client = pool.get().await?;

    // Materialize the next upcoming occurrence per trigger. ON CONFLICT makes
    // this idempotent: the same occurrence is enqueued at most once.
    let now = Utc::now();
    for t in &triggers {
        match next_after(&t.schedule, now) {
            Some(next) => {
                client
                    .execute(
                        "INSERT INTO dist_api.cron_events (trigger_name, scheduled_time) \
                         VALUES ($1, $2) ON CONFLICT (trigger_name, scheduled_time) DO NOTHING",
                        &[&t.name, &next],
                    )
                    .await?;
            }
            None => {
                tracing::warn!(trigger = %t.name, schedule = %t.schedule,
                    "invalid cron schedule; skipping materialization");
            }
        }
    }

    // Claim due events and deliver them while holding the row lock.
    let tx = client.transaction().await?;
    let rows = tx
        .query(
            "SELECT id, trigger_name, scheduled_time, tries \
             FROM dist_api.cron_events \
             WHERE status = 'scheduled' AND scheduled_time <= now() \
               AND (next_retry_at IS NULL OR next_retry_at <= now()) \
             ORDER BY scheduled_time \
             FOR UPDATE SKIP LOCKED \
             LIMIT 50",
            &[],
        )
        .await?;

    for row in rows {
        let id: uuid::Uuid = row.get("id");
        let trigger_name: String = row.get("trigger_name");
        let scheduled_time: DateTime<Utc> = row.get("scheduled_time");
        let tries: i32 = row.get("tries");

        let Some(trigger) = triggers.iter().find(|t| t.name == trigger_name) else {
            // Trigger was removed from metadata: drop the orphaned event.
            tx.execute(
                "UPDATE dist_api.cron_events SET status = 'dead' WHERE id = $1",
                &[&id],
            )
            .await?;
            continue;
        };
        let retry = trigger.retry_conf.clone().unwrap_or_default();

        // Tolerance: an occurrence delivered too long after its scheduled
        // time is dropped (only on the first attempt, never mid-retry).
        let lateness = (Utc::now() - scheduled_time).num_seconds();
        if tries == 0 && lateness > retry.tolerance_seconds as i64 {
            tx.execute(
                "UPDATE dist_api.cron_events SET status = 'dead' WHERE id = $1",
                &[&id],
            )
            .await?;
            tracing::warn!(trigger = %trigger_name, %id, lateness,
                "cron event past tolerance; dropped");
            continue;
        }

        let envelope = json!({
            "id": id.to_string(),
            "name": trigger_name,
            "scheduled_time": scheduled_time.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "payload": trigger.payload.clone(),
        });

        let (http_status, response_body) = deliver(state, trigger, &envelope).await;
        let success = http_status.map(|s| (200..300).contains(&s)).unwrap_or(false);

        tx.execute(
            "INSERT INTO dist_api.cron_event_invocation_logs (event_id, status, request, response) \
             VALUES ($1, $2, $3, $4)",
            &[&id, &http_status, &envelope, &response_body],
        )
        .await?;

        if success {
            tx.execute(
                "UPDATE dist_api.cron_events SET status = 'delivered', tries = tries + 1 \
                 WHERE id = $1",
                &[&id],
            )
            .await?;
        } else {
            let new_tries = tries + 1;
            if new_tries > retry.num_retries as i32 {
                tx.execute(
                    "UPDATE dist_api.cron_events SET status = 'error', tries = $2 \
                     WHERE id = $1",
                    &[&id, &new_tries],
                )
                .await?;
            } else {
                let next_retry =
                    Utc::now() + chrono::Duration::seconds(retry.retry_interval_seconds as i64);
                tx.execute(
                    "UPDATE dist_api.cron_events SET tries = $2, next_retry_at = $3 \
                     WHERE id = $1",
                    &[&id, &new_tries, &next_retry],
                )
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// POST the envelope to the trigger's webhook. Returns the HTTP status (None
/// on a transport error) and the response body captured for the invocation
/// log.
async fn deliver(state: &SharedState, trigger: &CronTrigger, envelope: &Json) -> (Option<i32>, Json) {
    let url = resolve_url_template(&trigger.webhook);
    let timeout = trigger
        .retry_conf
        .as_ref()
        .map(|r| r.timeout_seconds)
        .unwrap_or(60);
    let mut req = state
        .http
        .post(&url)
        .timeout(Duration::from_secs(timeout))
        .json(envelope);
    for (name, value) in resolve_headers(&trigger.headers) {
        req = req.header(name, value);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16() as i32;
            let body = resp.json::<Json>().await.unwrap_or(Json::Null);
            (Some(status), body)
        }
        Err(e) => (None, json!({ "error": e.to_string() })),
    }
}

/// Resolve header values: literal `value`, or `value_from_env` looked up at
/// delivery time. Headers whose env var is unset are skipped.
fn resolve_headers(headers: &[ActionHeader]) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|h| {
            let value = match (&h.value, &h.value_from_env) {
                (Some(v), _) => Some(v.clone()),
                (None, Some(env)) => std::env::var(env).ok(),
                (None, None) => None,
            };
            value.map(|v| (h.name.clone(), v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use dist_metadata::ActionHeader;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    #[test]
    fn every_minute_rounds_up_to_the_next_minute_boundary() {
        let after = utc(2030, 1, 1, 0, 0, 30);
        let next = next_after("* * * * *", after).unwrap();
        assert_eq!(next, utc(2030, 1, 1, 0, 1, 0));
    }

    #[test]
    fn daily_midnight_rolls_to_the_next_day() {
        let after = utc(2030, 1, 1, 12, 0, 0);
        let next = next_after("0 0 * * *", after).unwrap();
        assert_eq!(next, utc(2030, 1, 2, 0, 0, 0));
    }

    #[test]
    fn step_expression_is_supported() {
        let after = utc(2030, 1, 1, 0, 2, 0);
        let next = next_after("*/5 * * * *", after).unwrap();
        assert_eq!(next, utc(2030, 1, 1, 0, 5, 0));
    }

    #[test]
    fn invalid_schedule_returns_none() {
        assert!(next_after("not a cron", utc(2030, 1, 1, 0, 0, 0)).is_none());
        assert!(next_after("", utc(2030, 1, 1, 0, 0, 0)).is_none());
    }

    #[test]
    fn header_resolution_prefers_literal_and_skips_unset_env() {
        let headers = vec![
            ActionHeader { name: "X-Lit".into(), value: Some("v".into()), value_from_env: None },
            ActionHeader {
                name: "X-Env".into(),
                value: None,
                value_from_env: Some("DIST_API_TEST_UNSET_HEADER_VAR".into()),
            },
        ];
        let resolved = resolve_headers(&headers);
        assert_eq!(resolved, vec![("X-Lit".to_string(), "v".to_string())]);
    }
}
