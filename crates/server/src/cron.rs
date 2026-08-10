//! Cron (scheduled) triggers: a deploy-time-configured webhook fired on a
//! cron schedule with a static payload.
//!
//! There is no runtime admin surface; cron triggers come from YAML metadata
//! (`cron_triggers`). The catalog tables in `donat` are created by
//! `migrate` (the serving binary never runs DDL); this module only reads and
//! writes rows.
//!
//! Lifecycle (see [`spawn`]): a single background task periodically
//! *materializes* the next occurrence of each trigger into
//! `donat.cron_events`, then *delivers* due events to their webhook with
//! at-least-once semantics — claim with `FOR UPDATE SKIP LOCKED`, deliver
//! while holding the row lock (a crash rolls the claim back, so the event is
//! re-delivered), retry per `retry_conf`, and record an invocation log per
//! attempt.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use chrono::{DateTime, LocalResult, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use serde_json::{Value as Json, json};

use donat_metadata::{ActionHeader, CronDstPolicy, CronTrigger, DstRepeatedTime, DstSkippedTime};

use crate::remote::resolve_url_template;
use crate::state::SharedState;

/// The next scheduled occurrence strictly after `after`, evaluated in UTC.
/// Returns `None` if the schedule is not a valid cron expression or has no
/// future occurrence.
pub fn next_after(schedule: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let cron = Cron::from_str(schedule).ok()?;
    cron.find_next_occurrence(&after, false).ok()
}

/// The next occurrence of a trigger, plus the wall-clock occurrences the
/// declared DST policy dropped on the way to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextOccurrence {
    /// The instant the run happens.
    pub at: DateTime<Utc>,
    /// Local times that matched the schedule but do not exist, and which
    /// [`DstSkippedTime::Skip`] therefore declined to run. Reported so a
    /// dropped run is visible in the log rather than silent.
    pub skipped: Vec<NaiveDateTime>,
}

/// The next occurrence of `trigger` strictly after `after`.
///
/// With no declared timezone this is [`next_after`] verbatim — the UTC path is
/// untouched, because every deployment that exists is that path. With one, the
/// cron fields are wall-clock times in that zone and the two DST policies
/// decide the instants.
///
/// `None` means the schedule cannot be evaluated: an unparseable cron
/// expression, an unknown zone, or a zone declared without its DST policies.
/// The last one is deliberate — there is no default policy anywhere in this
/// module to fall back on. Metadata loading refuses all three, so reaching
/// `None` means the trigger did not come through the loader.
pub fn next_occurrence(trigger: &CronTrigger, after: DateTime<Utc>) -> Option<NextOccurrence> {
    let Some(zone) = &trigger.timezone else {
        return next_after(&trigger.schedule, after).map(|at| NextOccurrence {
            at,
            skipped: Vec::new(),
        });
    };
    let tz: Tz = zone.parse().ok()?;
    let dst = trigger.dst.as_ref()?;
    let cron = Cron::from_str(&trigger.schedule).ok()?;
    next_in_zone(&cron, tz, dst, after)
}

/// Wall-clock times are searched with `croner` over naive time (a naive value
/// read as UTC, where every local time exists exactly once), and the zone is
/// applied afterwards by this function. `croner` can take a zoned datetime
/// directly, but then it picks the DST behaviour itself and picks a *different*
/// one per schedule shape — a fixed-time job snaps out of a gap and fires once
/// in an overlap, an interval job silently skips the gap and fires twice in the
/// overlap. Which of those a deployment gets should not depend on how its
/// schedule happens to be spelled, so the policy is metadata and this is where
/// it is applied.
fn next_in_zone(
    cron: &Cron,
    tz: Tz,
    dst: &CronDstPolicy,
    after: DateTime<Utc>,
) -> Option<NextOccurrence> {
    // The search starts before the local time of `after`, because an
    // occurrence whose wall-clock time has already passed can still be in the
    // future: during a repeated span the same local time comes round again,
    // one span-width later. Two hours covers every transition in the database
    // — the widest repeated span is `Antarctica/Troll`'s two hours, and a
    // candidate exactly one span back is still enumerated because the search
    // from the cursor is strict.
    let mut cursor = after.with_timezone(&tz).naive_local() - TimeDelta::hours(2);
    let mut skipped = Vec::new();
    // Candidates are walked in *wall-clock* order, and inside a repeated span
    // that is not instant order — the second pass of 02:00 comes after the
    // first pass of 02:01. So the search keeps the earliest run it has found
    // rather than returning the first one it sees.
    let mut best: Option<DateTime<Utc>> = None;

    // Bounded so a pathological schedule cannot spin: the back-off costs at
    // most one candidate per minute for a per-minute schedule, and a gap adds
    // at most two hours more.
    for _ in 0..4096 {
        // `and_utc` here is not a claim about the zone: it borrows UTC's
        // "every local time exists once" property so the cron matcher only
        // ever sees wall-clock fields.
        let candidate = cron.find_next_occurrence(&cursor.and_utc(), false).ok()?;
        let candidate = candidate.naive_utc();
        cursor = candidate;

        let local = tz.from_local_datetime(&candidate);
        // The earliest instant this candidate can produce — and, because local
        // time and the instants carrying it advance together across a
        // transition in either direction, a lower bound for every later
        // candidate too. Once it has reached the best run found so far,
        // nothing still to come can beat it.
        let earliest = match local {
            LocalResult::Single(at) => at.with_timezone(&Utc),
            LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
            LocalResult::None => gap_end(tz, candidate)?,
        };
        if best.is_some_and(|best| earliest >= best) {
            break;
        }

        let resolved = match local {
            LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
            LocalResult::Ambiguous(first, second) => {
                let first = first.with_timezone(&Utc);
                let second = second.with_timezone(&Utc);
                if names_the_repeated_span_once(cron, tz, first, second) {
                    Some(match dst.repeated_time {
                        DstRepeatedTime::FireAtFirst => first,
                        DstRepeatedTime::FireAtSecond => second,
                    })
                } else {
                    // The schedule matches more than one local time inside the
                    // repeated span, so it is running on a cadence and names no
                    // wall-clock time the span made ambiguous. There is nothing
                    // for the policy to choose between: both passes are runs,
                    // and this candidate's next one is whichever of its two
                    // instants is still ahead.
                    [first, second].into_iter().find(|at| *at > after)
                }
            }
            LocalResult::None => match dst.skipped_time {
                // `earliest` is the gap's end here, and it is the same instant
                // for every local time the gap swallowed — which is what makes
                // this one run rather than one per swallowed minute.
                DstSkippedTime::FireAfterGap => Some(earliest),
                DstSkippedTime::Skip => {
                    // Only an occurrence that was still ahead of us is
                    // worth reporting; the back-off window may reach back
                    // over one that has already been accounted for.
                    if earliest > after {
                        skipped.push(candidate);
                    }
                    None
                }
            },
        };

        if let Some(at) = resolved.filter(|at| *at > after) {
            best = Some(best.map_or(at, |best| best.min(at)));
        }
    }
    best.map(|at| NextOccurrence { at, skipped })
}

/// The instant a DST gap ends, for a `local` time that falls inside it.
///
/// Local time is strictly increasing across a forward transition, so the gap
/// end is the first instant whose local time has reached `local` — found by
/// bisection over a day either side, a window that contains the one transition
/// that made the gap and no other.
///
/// The answer is the transition instant itself, exactly: every local time the
/// gap swallowed resolves to the same value, which is what lets the
/// materializer's `(trigger_name, scheduled_time)` key collapse them into the
/// one run ADR 039 promises. An answer that is merely within a second of the
/// transition would be a *different* instant per candidate, and a per-minute
/// schedule would deliver the same logical occurrence once per swallowed
/// minute.
fn gap_end(tz: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    let lo = local.checked_sub_signed(TimeDelta::hours(24))?.and_utc();
    let hi = local.checked_add_signed(TimeDelta::hours(24))?.and_utc();
    first_instant_where(lo, hi, |at| {
        tz.from_utc_datetime(&at.naive_utc()).naive_local() >= local
    })
}

/// The first instant in `(lo, hi]` at which `reached` becomes true, exactly.
///
/// `reached` must be false at `lo`, true at `hi`, and switch once in between —
/// which is what a transition is, inside a window that holds one. The bisection
/// runs to a one-nanosecond width, `DateTime`'s own resolution, so the answer is
/// the boundary rather than a value near it: about fifty offset lookups for a
/// two-day window, none of which touch the network or the database.
///
/// `None` means the window is not the shape the caller assumed (a zone whose
/// rules changed twice inside it, or a transition wider than the window).
/// Refusing is better than returning an instant chosen by arithmetic alone.
fn first_instant_where(
    mut lo: DateTime<Utc>,
    mut hi: DateTime<Utc>,
    reached: impl Fn(DateTime<Utc>) -> bool,
) -> Option<DateTime<Utc>> {
    if reached(lo) || !reached(hi) {
        return None;
    }
    while hi - lo > TimeDelta::nanoseconds(1) {
        let mid = lo + (hi - lo) / 2;
        if reached(mid) { hi = mid } else { lo = mid }
    }
    Some(hi)
}

/// Whether the schedule names exactly one wall-clock time inside the span a
/// backward transition repeated — the case ADR 039's `repeated_time` policy is
/// about, and the only case in which it is applied.
///
/// `first` and `second` are the two instants of one ambiguous local time. A
/// schedule that matches two or more local times in that span is declaring a
/// cadence, not naming a time the span made ambiguous: applying "take the
/// earlier instant" to every one of its matches would stop it for the width of
/// the span (an hour of a per-minute job in Berlin, two in Troll), which is a
/// blackout rather than the single run the policy promises.
///
/// Anything unexpected answers `true`, so the declared policy is what applies
/// when this cannot tell.
fn names_the_repeated_span_once(
    cron: &Cron,
    tz: Tz,
    first: DateTime<Utc>,
    second: DateTime<Utc>,
) -> bool {
    let Some((start, end)) = repeated_span(tz, first, second) else {
        return true;
    };
    !matches_at_least_twice(cron, start, end)
}

/// The local times a backward transition repeated, as `[start, end)`.
///
/// The transition lies in `(first, second]` by construction — `first` carries
/// the offset before it and `second` the offset after — so it is found by the
/// same exact bisection the gap end uses. Its local time is where the repeated
/// span starts, and the span is as wide as the two instants are apart.
fn repeated_span(
    tz: Tz,
    first: DateTime<Utc>,
    second: DateTime<Utc>,
) -> Option<(NaiveDateTime, NaiveDateTime)> {
    use chrono::Offset;

    let after = tz.offset_from_utc_datetime(&second.naive_utc()).fix();
    let transition = first_instant_where(first, second, |at| {
        tz.offset_from_utc_datetime(&at.naive_utc()).fix() == after
    })?;
    let start = tz.from_utc_datetime(&transition.naive_utc()).naive_local();
    let end = start.checked_add_signed(second - first)?;
    Some((start, end))
}

/// Whether the schedule matches two or more wall-clock times in `[start, end)`.
///
/// The matcher runs over naive time read as UTC, the same borrowed "every local
/// time exists once" property [`next_in_zone`] uses, so no zone resolution
/// happens here.
fn matches_at_least_twice(cron: &Cron, start: NaiveDateTime, end: NaiveDateTime) -> bool {
    let Some(before) = start.checked_sub_signed(TimeDelta::seconds(1)) else {
        return false;
    };
    let mut cursor = before.and_utc();
    for _ in 0..2 {
        let Ok(candidate) = cron.find_next_occurrence(&cursor, false) else {
            return false;
        };
        if candidate.naive_utc() >= end {
            return false;
        }
        cursor = candidate;
    }
    true
}

/// The skipped occurrences of one trigger that have not been announced yet.
///
/// The materializer recomputes the same answer on every tick (ten seconds by
/// default), so announcing the set as it is found says one line thousands of
/// times in the day before a transition — a daily zoned trigger with `Skip` has
/// exactly one dropped run to report and would report it 8640 times. Comparing
/// against the set the previous tick reported makes it one line per skipped
/// occurrence, which is what the log is for.
///
/// Nothing accumulates: the memory holds one entry per trigger that currently
/// skips something, and an occurrence that has fallen out — the transition
/// passed, or the schedule changed — is forgotten with it.
fn newly_skipped(trigger: &str, skipped: &[NaiveDateTime]) -> Vec<NaiveDateTime> {
    /// What the last tick reported, per trigger. A trigger with nothing to
    /// skip holds no entry at all, so the map is bounded by the number of
    /// triggers currently sitting in front of a transition.
    static ANNOUNCED: LazyLock<Mutex<HashMap<String, Vec<NaiveDateTime>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    // A poisoned lock would mean a previous caller panicked between the two
    // lines below; the memory is a log filter, so recovering it and carrying
    // on is better than taking the delivery loop down with it.
    let mut announced = ANNOUNCED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if skipped.is_empty() {
        // Forgotten rather than kept as an empty set: the trigger stops
        // occupying the map until it next has something to skip.
        announced.remove(trigger);
        return Vec::new();
    }

    let previous = announced.get(trigger);
    let fresh: Vec<NaiveDateTime> = skipped
        .iter()
        .filter(|at| previous.is_none_or(|seen| !seen.contains(at)))
        .copied()
        .collect();
    // The whole set, not the union: an occurrence the schedule no longer
    // skips drops out here, and is announced again if it ever comes back.
    announced.insert(trigger.to_owned(), skipped.to_vec());
    fresh
}

/// Start the cron delivery loop as a background task. No-op (the task exits
/// immediately) when the metadata declares no cron triggers, so a plain
/// deployment without cron never touches the `donat` catalog.
pub fn spawn(
    state: SharedState,
    shutdown: tokio_util::sync::CancellationToken,
    tasks: &tokio_util::task::TaskTracker,
) {
    tasks.spawn(async move { run(state, shutdown).await });
}

async fn run(state: SharedState, shutdown: tokio_util::sync::CancellationToken) {
    let has_triggers = !state.engine.read().await.metadata.cron_triggers.is_empty();
    if !has_triggers {
        return;
    }
    let interval = std::env::var("DONAT_CRON_POLL_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .max(1);
    let interval = Duration::from_secs(interval);
    tracing::info!(
        poll_seconds = interval.as_secs(),
        "cron delivery loop started"
    );
    loop {
        if let Err(e) = tick(&state).await {
            tracing::warn!(error = %e, "cron tick failed");
        }
        // A delivery already committed is not repeated; stopping between ticks
        // is the natural seam, and cron delivery is at-least-once anyway.
        if !crate::shutdown::idle(interval, &shutdown).await {
            tracing::info!("cron delivery loop stopped");
            return;
        }
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
        match next_occurrence(t, now) {
            Some(next) => {
                // A run the declared gap policy drops is announced, so an
                // operator can see that a nightly job has no occurrence on a
                // transition day instead of discovering it from its absence.
                for local in newly_skipped(&t.name, &next.skipped) {
                    tracing::warn!(trigger = %t.name, schedule = %t.schedule,
                        timezone = t.timezone.as_deref().unwrap_or("UTC"),
                        local_time = %local,
                        "local time does not exist on this date (DST gap); \
                         skipped by the declared policy");
                }
                client
                    .execute(
                        "INSERT INTO donat.cron_events (trigger_name, scheduled_time) \
                         VALUES ($1, $2) ON CONFLICT (trigger_name, scheduled_time) DO NOTHING",
                        &[&t.name, &next.at],
                    )
                    .await?;
            }
            None => {
                tracing::warn!(trigger = %t.name, schedule = %t.schedule,
                    timezone = t.timezone.as_deref().unwrap_or("UTC"),
                    "cron schedule cannot be evaluated (invalid expression, unknown \
                     timezone, or a timezone without declared dst policies); \
                     skipping materialization");
            }
        }
    }

    // Claim due events and deliver them while holding the row lock.
    let tx = client.transaction().await?;
    let rows = tx
        .query(
            "SELECT id, trigger_name, scheduled_time, tries \
             FROM donat.cron_events \
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
                "UPDATE donat.cron_events SET status = 'dead' WHERE id = $1",
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
                "UPDATE donat.cron_events SET status = 'dead' WHERE id = $1",
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
        let success = http_status
            .map(|s| (200..300).contains(&s))
            .unwrap_or(false);

        tx.execute(
            "INSERT INTO donat.cron_event_invocation_logs (event_id, status, request, response) \
             VALUES ($1, $2, $3, $4)",
            &[&id, &http_status, &envelope, &response_body],
        )
        .await?;

        if success {
            tx.execute(
                "UPDATE donat.cron_events SET status = 'delivered', tries = tries + 1 \
                 WHERE id = $1",
                &[&id],
            )
            .await?;
        } else {
            let new_tries = tries + 1;
            if new_tries > retry.num_retries as i32 {
                tx.execute(
                    "UPDATE donat.cron_events SET status = 'error', tries = $2 \
                     WHERE id = $1",
                    &[&id, &new_tries],
                )
                .await?;
            } else {
                let next_retry =
                    Utc::now() + chrono::Duration::seconds(retry.retry_interval_seconds as i64);
                tx.execute(
                    "UPDATE donat.cron_events SET tries = $2, next_retry_at = $3 \
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
async fn deliver(
    state: &SharedState,
    trigger: &CronTrigger,
    envelope: &Json,
) -> (Option<i32>, Json) {
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
/// delivery time. Headers whose env var is unset are skipped. Shared with
/// table event-trigger delivery.
pub(crate) fn resolve_headers(headers: &[ActionHeader]) -> Vec<(String, String)> {
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
    use chrono::{NaiveDate, NaiveDateTime, TimeZone};
    use chrono_tz::Tz;
    use donat_metadata::{ActionHeader, CronDstPolicy, DstRepeatedTime, DstSkippedTime};

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn naive(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    /// The wall-clock time an instant carries in Berlin, which is what a
    /// schedule declared in that zone is about.
    fn berlin_local(at: DateTime<Utc>) -> NaiveDateTime {
        at.with_timezone(&Tz::Europe__Berlin).naive_local()
    }

    fn trigger(schedule: &str, timezone: Option<&str>, dst: Option<CronDstPolicy>) -> CronTrigger {
        CronTrigger {
            name: "t".into(),
            webhook: "http://localhost/hook".into(),
            schedule: schedule.into(),
            payload: Json::Null,
            include_in_metadata: true,
            retry_conf: None,
            headers: vec![],
            comment: None,
            timezone: timezone.map(str::to_string),
            dst,
        }
    }

    /// A Berlin trigger with both DST policies stated, because a schedule in a
    /// zone cannot be evaluated without them.
    fn berlin(schedule: &str, skipped: DstSkippedTime, repeated: DstRepeatedTime) -> CronTrigger {
        zoned("Europe/Berlin", schedule, skipped, repeated)
    }

    /// `Antarctica/Troll` shifts by two hours, which is the widest transition
    /// in the database: its gap swallows two hours of local time and its
    /// overlap repeats two. Every bound in this module has to hold there and
    /// not merely in the one-hour zones.
    fn troll(schedule: &str, skipped: DstSkippedTime, repeated: DstRepeatedTime) -> CronTrigger {
        zoned("Antarctica/Troll", schedule, skipped, repeated)
    }

    fn zoned(
        timezone: &str,
        schedule: &str,
        skipped: DstSkippedTime,
        repeated: DstRepeatedTime,
    ) -> CronTrigger {
        trigger(
            schedule,
            Some(timezone),
            Some(CronDstPolicy {
                skipped_time: skipped,
                repeated_time: repeated,
            }),
        )
    }

    /// The instants a trigger fires at, walked the way the materializer walks
    /// them: each occurrence becomes the `after` of the next call.
    fn occurrences(
        trigger: &CronTrigger,
        mut after: DateTime<Utc>,
        count: usize,
    ) -> Vec<DateTime<Utc>> {
        let mut fired = Vec::with_capacity(count);
        for _ in 0..count {
            let next = next_occurrence(trigger, after).expect("the schedule has a next occurrence");
            fired.push(next.at);
            after = next.at;
        }
        fired
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
            ActionHeader {
                name: "X-Lit".into(),
                value: Some("v".into()),
                value_from_env: None,
            },
            ActionHeader {
                name: "X-Env".into(),
                value: None,
                value_from_env: Some("DONAT_TEST_UNSET_HEADER_VAR".into()),
            },
        ];
        let resolved = resolve_headers(&headers);
        assert_eq!(resolved, vec![("X-Lit".to_string(), "v".to_string())]);
    }

    /// A trigger with no declared timezone is evaluated exactly as it was
    /// before timezones existed: the cron fields are UTC wall-clock, and
    /// nothing about a zone's DST rules reaches it. Every deployment that
    /// exists today is this case, so it is pinned against
    /// [`next_after`] — the untouched UTC path — rather than against
    /// hand-written expectations.
    #[test]
    fn absent_timezone_keeps_the_utc_schedule_unchanged() {
        // Instants chosen around the European DST transitions: if any local
        // resolution leaked into the default path, these are where it would
        // show.
        let instants = [
            utc(2030, 1, 1, 0, 0, 30),
            utc(2026, 3, 29, 0, 30, 0),
            utc(2026, 3, 29, 1, 30, 0),
            utc(2026, 10, 25, 0, 30, 0),
            utc(2026, 10, 25, 1, 30, 0),
        ];
        for schedule in ["* * * * *", "0 9 * * 1-5", "30 2 * * *", "*/5 * * * *"] {
            for after in instants {
                let occurrence = next_occurrence(&trigger(schedule, None, None), after)
                    .expect("a UTC schedule always has a next occurrence");
                assert_eq!(
                    occurrence.at,
                    next_after(schedule, after).unwrap(),
                    "schedule {schedule} after {after} must keep its UTC meaning"
                );
                assert!(
                    occurrence.skipped.is_empty(),
                    "a UTC schedule has no DST gap to skip"
                );
            }
        }
    }

    /// A weekday-morning schedule declared in `Europe/Berlin` fires at local
    /// 09:00 on both sides of a DST transition — 08:00Z in winter, 07:00Z in
    /// summer. The UTC instant moves precisely so the wall clock does not.
    #[test]
    fn cron_fires_in_declared_timezone() {
        let t = berlin(
            "0 9 * * 1-5",
            DstSkippedTime::FireAfterGap,
            DstRepeatedTime::FireAtFirst,
        );

        // Thursday before the spring transition (2026-03-29): the next run is
        // Friday 09:00 CET = 08:00Z.
        let before = next_occurrence(&t, utc(2026, 3, 26, 12, 0, 0)).unwrap();
        assert_eq!(before.at, utc(2026, 3, 27, 8, 0, 0));
        assert_eq!(berlin_local(before.at), naive(2026, 3, 27, 9, 0));

        // Sunday after it: the next run is Monday 09:00 CEST = 07:00Z. Same
        // wall clock, an hour earlier in UTC.
        let after = next_occurrence(&t, utc(2026, 3, 29, 12, 0, 0)).unwrap();
        assert_eq!(after.at, utc(2026, 3, 30, 7, 0, 0));
        assert_eq!(berlin_local(after.at), naive(2026, 3, 30, 9, 0));

        // The same across the autumn transition (2026-10-25), in the other
        // direction.
        let summer = next_occurrence(&t, utc(2026, 10, 22, 12, 0, 0)).unwrap();
        assert_eq!(summer.at, utc(2026, 10, 23, 7, 0, 0));
        assert_eq!(berlin_local(summer.at), naive(2026, 10, 23, 9, 0));
        let winter = next_occurrence(&t, utc(2026, 10, 25, 12, 0, 0)).unwrap();
        assert_eq!(winter.at, utc(2026, 10, 26, 8, 0, 0));
        assert_eq!(berlin_local(winter.at), naive(2026, 10, 26, 9, 0));

        // A weekend is still skipped: the zone changes when 09:00 is, not
        // which days match.
        assert_eq!(berlin_local(before.at).format("%a").to_string(), "Fri");
    }

    /// The two wall-clock times a DST transition breaks. `02:30` daily in
    /// Berlin does not exist on 2026-03-29 and happens twice on 2026-10-25.
    /// Neither case has a default: the trigger states what to do, the engine
    /// does exactly that, and a run is never dropped without saying so.
    #[test]
    fn cron_dst_gap_and_overlap_policies_are_explicit() {
        // No policy at all is not a schedule the engine will guess at.
        assert!(
            next_occurrence(
                &trigger("30 2 * * *", Some("Europe/Berlin"), None),
                utc(2026, 3, 28, 12, 0, 0)
            )
            .is_none(),
            "a zoned schedule without declared DST policies is refused, not defaulted"
        );

        // --- Spring: local 02:30 does not exist on 2026-03-29. ---
        let gap_after = utc(2026, 3, 28, 12, 0, 0);

        // fire_after_gap: the run happens at the instant the gap ends, 03:00
        // local = 01:00Z. It is late, not lost.
        let fired = next_occurrence(
            &berlin(
                "30 2 * * *",
                DstSkippedTime::FireAfterGap,
                DstRepeatedTime::FireAtFirst,
            ),
            gap_after,
        )
        .unwrap();
        assert_eq!(fired.at, utc(2026, 3, 29, 1, 0, 0));
        assert_eq!(berlin_local(fired.at), naive(2026, 3, 29, 3, 0));
        assert!(fired.skipped.is_empty(), "nothing was dropped");

        // skip: the run does not happen, and the dropped wall-clock time is
        // reported so the drop is auditable rather than silent.
        let skipped = next_occurrence(
            &berlin(
                "30 2 * * *",
                DstSkippedTime::Skip,
                DstRepeatedTime::FireAtFirst,
            ),
            gap_after,
        )
        .unwrap();
        assert_eq!(skipped.skipped, vec![naive(2026, 3, 29, 2, 30)]);
        assert_eq!(skipped.at, utc(2026, 3, 30, 0, 30, 0));
        assert_eq!(berlin_local(skipped.at), naive(2026, 3, 30, 2, 30));

        // --- Autumn: local 02:30 happens twice on 2026-10-25, at 00:30Z
        // (CEST) and 01:30Z (CET). Exactly one of them is a run. ---
        let overlap_after = utc(2026, 10, 24, 12, 0, 0);

        let first = berlin(
            "30 2 * * *",
            DstSkippedTime::FireAfterGap,
            DstRepeatedTime::FireAtFirst,
        );
        let first_run = next_occurrence(&first, overlap_after).unwrap();
        assert_eq!(first_run.at, utc(2026, 10, 25, 0, 30, 0));
        assert_eq!(berlin_local(first_run.at), naive(2026, 10, 25, 2, 30));
        // The repeated hour does not fire again: the next run is the next day.
        let after_first = next_occurrence(&first, first_run.at).unwrap();
        assert_eq!(after_first.at, utc(2026, 10, 26, 1, 30, 0));

        let second = berlin(
            "30 2 * * *",
            DstSkippedTime::FireAfterGap,
            DstRepeatedTime::FireAtSecond,
        );
        let second_run = next_occurrence(&second, overlap_after).unwrap();
        assert_eq!(second_run.at, utc(2026, 10, 25, 1, 30, 0));
        assert_eq!(berlin_local(second_run.at), naive(2026, 10, 25, 2, 30));
        // And the first pass of the hour is not a run under this policy: from
        // an instant just before it, the next run is still the second pass.
        assert_eq!(
            next_occurrence(&second, utc(2026, 10, 25, 0, 0, 0))
                .unwrap()
                .at,
            utc(2026, 10, 25, 1, 30, 0)
        );
        assert_eq!(
            next_occurrence(&second, second_run.at).unwrap().at,
            utc(2026, 10, 26, 1, 30, 0)
        );
    }

    /// A gap has one end, and every local time it swallowed resolves to that
    /// one instant — exactly, to the nanosecond.
    ///
    /// This is what makes `fire_after_gap` "one run" rather than "a run per
    /// swallowed minute": the materializer's `(trigger_name, scheduled_time)`
    /// key can only collapse two candidates that produce the *same* instant, so
    /// an answer that is merely close is an answer that delivers twice.
    #[test]
    fn a_gap_ends_at_one_exact_instant_whatever_local_time_asks() {
        // Berlin, 2026-03-29: local 02:00–02:59 does not exist; the gap ends at
        // 01:00Z (03:00 local).
        for minute in [0, 1, 17, 30, 45, 59] {
            assert_eq!(
                gap_end(Tz::Europe__Berlin, naive(2026, 3, 29, 2, minute)),
                Some(utc(2026, 3, 29, 1, 0, 0)),
                "every local time in one gap must end at the same instant"
            );
        }

        // Troll, same date: a two-hour gap, local 01:00–02:59, ending at 01:00Z
        // (03:00 local).
        for (hour, minute) in [(1, 0), (1, 30), (2, 0), (2, 59)] {
            assert_eq!(
                gap_end(Tz::Antarctica__Troll, naive(2026, 3, 29, hour, minute)),
                Some(utc(2026, 3, 29, 1, 0, 0)),
                "a two-hour gap ends at one instant too"
            );
        }
    }

    /// ADR 039 promises `fire_after_gap` "one run, at the instant the gap
    /// ends". A per-minute schedule offers the gap 120 local times in Troll;
    /// the run is still one, and the schedule picks its cadence up again on the
    /// far side.
    #[test]
    fn fire_after_gap_delivers_one_occurrence_for_a_whole_gap() {
        let t = troll(
            "* * * * *",
            DstSkippedTime::FireAfterGap,
            DstRepeatedTime::FireAtFirst,
        );
        assert_eq!(
            occurrences(&t, utc(2026, 3, 29, 0, 58, 0), 4),
            vec![
                utc(2026, 3, 29, 0, 59, 0),
                // Local 01:00 through 02:59 are one occurrence, at the gap's end.
                utc(2026, 3, 29, 1, 0, 0),
                utc(2026, 3, 29, 1, 1, 0),
                utc(2026, 3, 29, 1, 2, 0),
            ]
        );
    }

    /// The repeated-time policy answers "which of the two instants carries this
    /// *named* local time". A schedule that fires every minute names none: it
    /// declares a cadence, and a cadence that stops for the width of the
    /// repeated span is a blackout, not a policy.
    #[test]
    fn a_per_minute_schedule_keeps_its_cadence_through_a_repeated_span() {
        for repeated in [DstRepeatedTime::FireAtFirst, DstRepeatedTime::FireAtSecond] {
            // Berlin repeats 02:00–02:59 local on 2026-10-25: 00:00Z–00:59Z and
            // then 01:00Z–01:59Z carry the same wall clock.
            let fired = occurrences(
                &berlin("* * * * *", DstSkippedTime::FireAfterGap, repeated),
                utc(2026, 10, 24, 23, 58, 0),
                180,
            );
            let mut expected = utc(2026, 10, 24, 23, 59, 0);
            for at in &fired {
                assert_eq!(
                    *at, expected,
                    "a per-minute schedule fires every minute, {repeated:?} included"
                );
                expected += TimeDelta::minutes(1);
            }

            // And in the two-hour zone, where the span is twice as long.
            let fired = occurrences(
                &troll("* * * * *", DstSkippedTime::FireAfterGap, repeated),
                utc(2026, 10, 25, 0, 58, 0),
                180,
            );
            let mut expected = utc(2026, 10, 25, 0, 59, 0);
            for at in &fired {
                assert_eq!(*at, expected, "{repeated:?} in a two-hour overlap");
                expected += TimeDelta::minutes(1);
            }
        }
    }

    /// A dropped run is announced once per occurrence, not once per tick.
    ///
    /// `next_occurrence` recomputes the same skipped set on every pass of the
    /// delivery loop, so a daily Berlin trigger with `Skip` — one dropped run —
    /// reported the same line every ten seconds for the whole day before the
    /// transition. An operator reading thousands of copies of one warning is
    /// not better informed than one reading it once.
    #[test]
    fn a_skipped_occurrence_is_announced_once_and_not_once_per_tick() {
        let gap = naive(2026, 3, 29, 2, 30);
        let next_year = naive(2027, 3, 28, 2, 30);

        assert_eq!(newly_skipped("nightly", &[gap]), vec![gap]);
        assert!(
            newly_skipped("nightly", &[gap]).is_empty(),
            "the next tick has nothing new to say"
        );

        // Each trigger answers for itself.
        assert_eq!(newly_skipped("hourly", &[gap]), vec![gap]);

        // A further occurrence is announced; the one already said is not.
        assert_eq!(newly_skipped("nightly", &[gap, next_year]), vec![next_year]);
        assert!(newly_skipped("nightly", &[gap, next_year]).is_empty());

        // Once the transition has passed there is nothing to skip, and nothing
        // is remembered: a later recurrence is announced again rather than
        // being suppressed by a set that grew forever.
        assert!(newly_skipped("nightly", &[]).is_empty());
        assert_eq!(newly_skipped("nightly", &[gap]), vec![gap]);
    }

    /// The other side of the same rule: a schedule the repeated span names once
    /// is exactly what the two policies are for, and it still runs once.
    #[test]
    fn a_named_local_time_in_a_repeated_span_still_runs_once() {
        // Hourly: the span 02:00–03:00 local holds one match, so the policy
        // picks which pass carries it and the other pass is not a run.
        let first = berlin(
            "0 * * * *",
            DstSkippedTime::FireAfterGap,
            DstRepeatedTime::FireAtFirst,
        );
        assert_eq!(
            occurrences(&first, utc(2026, 10, 24, 23, 30, 0), 3),
            vec![
                utc(2026, 10, 24, 23, 0, 0) + TimeDelta::hours(1), // 00:00Z, local 02:00 (first pass)
                utc(2026, 10, 25, 2, 0, 0),                        // local 03:00
                utc(2026, 10, 25, 3, 0, 0),
            ]
        );
        let second = berlin(
            "0 * * * *",
            DstSkippedTime::FireAfterGap,
            DstRepeatedTime::FireAtSecond,
        );
        assert_eq!(
            occurrences(&second, utc(2026, 10, 24, 23, 30, 0), 3),
            vec![
                utc(2026, 10, 25, 1, 0, 0), // local 02:00 (second pass)
                utc(2026, 10, 25, 2, 0, 0),
                utc(2026, 10, 25, 3, 0, 0),
            ]
        );
    }

    /// The IANA database decides when every zoned schedule fires: a tzdata
    /// release that changes a rule changes the instants this engine wakes up
    /// at. It is therefore pinned twice over — the crate version exactly
    /// (`=0.10.4`, so the resolver cannot move it) and the tzdb release the
    /// crate embeds, which is the version that actually decides the answer.
    ///
    /// What this proves: the embedded database cannot change without a
    /// deliberate commit that also edits this test. What it does **not** do is
    /// put that version in a deployment fingerprint — the engine has no
    /// engine-wide fingerprint to put it in (the ones that exist are per
    /// connector operation and per process revision), which is the same gap
    /// the phone metadata pin records.
    #[test]
    fn tzdata_version_is_pinned() {
        const TZDB: &str = "2025b";
        const CRATE: &str = "0.10.4";

        assert_eq!(
            chrono_tz::IANA_TZDB_VERSION,
            TZDB,
            "the embedded IANA database changed; that changes when zoned \
             schedules fire, so it is a deliberate decision and not a bump"
        );

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            manifest.contains("chrono-tz = \"=0.10.4\""),
            "the workspace must pin chrono-tz exactly, or a `cargo update` \
             moves the tz database under the deployment"
        );

        let lock = std::fs::read_to_string(root.join("Cargo.lock")).unwrap();
        let resolved = lock
            .split("[[package]]")
            .find(|package| package.contains("\nname = \"chrono-tz\"\n"))
            .and_then(|package| {
                package
                    .lines()
                    .find_map(|line| line.strip_prefix("version = \""))
            })
            .map(|version| version.trim_end_matches('"'))
            .expect("chrono-tz is a resolved dependency");
        assert_eq!(resolved, CRATE);
    }
}
