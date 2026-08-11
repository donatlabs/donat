//! `local.recurrence` — RFC 5545 recurrence rules (spec 021 §3).
//!
//! | Operation | Product |
//! |---|---|
//! | `rule.validate` | the parsed, normalized rule and the bound it was admitted on |
//! | `rule.expand` | the occurrences inside a declared window |
//! | `rule.next` | the next N occurrences after a declared instant |
//!
//! Almost nobody implements RFC 5545 correctly by hand, so the expansion is
//! `rrule`'s. What is written here is the two things a wrapper does not do.
//!
//! **Boundedness is a property of the declaration, not a discovery.** An
//! expansion is bounded by a window *and* by an occurrence count, and both are
//! answered before the first occurrence is generated: the worst case a rule can
//! produce between its start and the end of the window is computed from the
//! rule's own parts — its frequency, its interval, its `BY*` expansions, its
//! `COUNT` and `UNTIL` — and that number is the operation's declared unit count
//! (spec 018 §4). A rule that cannot fit is refused by the bound layer before
//! the executor runs, which is what makes `FREQ=SECONDLY` with neither `UNTIL`
//! nor `COUNT` a refusal at declaration rather than an expansion that runs long.
//!
//! **The DST policy is the deployment's, not the library's.** A daily
//! occurrence at a local time that does not exist on the spring transition day,
//! and one that exists twice in autumn, follow the policy the deployment
//! declared — the same two enums, with the same spellings and the same
//! semantics, that a zoned cron trigger declares
//! (`knowledgebase/declarative-saas/decisions/039-*`). `rrule` answers both
//! cases itself, silently and differently (a missing local time becomes
//! midnight plus the wall-clock offset, an ambiguous one becomes the first
//! instant), so the expansion is driven over *naive wall-clock* time — where
//! every local time exists exactly once — and the zone is resolved afterwards
//! by this module. That is the same technique, for the same reason, that
//! `crates/server/src/cron.rs` uses to keep `croner`'s own classification from
//! running.
//!
//! Everything else follows from `Pure`. The current time is never read: a
//! window, an `after` instant, and a start are declared inputs, which is what
//! makes two expansions of one input identical. The rule text may not carry a
//! `DTSTART`, because a floating `DTSTART` (and a floating `UNTIL`) is read by
//! the library in the *machine's* timezone — an ambient environment lookup, and
//! the one thing spec 018 §3 says a pure operation may not contain.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, LocalResult, NaiveDateTime, TimeDelta, TimeZone, Utc};
use chrono_tz::Tz;
use rrule::{Frequency, NWeekday, RRule, RRuleSet, Unvalidated};
use serde_json::{Value as JsonValue, json};

use crate::local::bounds::LocalBounds;
use crate::local::capability::{LocalCapability, LocalInvocation, LocalOperation, LocalProduct};
use crate::sdk::effect::{DeterminismEvidence, Effect};
use crate::sdk::errors::{ConnectorErrorClass, ConnectorFailure};

/// The connector name every recurrence operation is reached through.
pub const RECURRENCE_CAPABILITY: &str = "local.recurrence";

// ---------------------------------------------------------------------------
// The ceilings no policy can widen
// ---------------------------------------------------------------------------

/// The most occurrences one expansion may walk, and therefore the largest
/// `max_occurrences` a policy may declare.
///
/// It is the operation's declared `max_units`, which is why a rule whose worst
/// case exceeds it is refused by [`LocalOperation::execute`] before the
/// executor is entered at all.
pub const MAX_OCCURRENCES: u64 = 10_000;

/// The longest span a policy may declare between a rule's start and the end of
/// a window. Ten years of wall clock; anything past it is a report, not a
/// schedule.
pub const MAX_WINDOW_SECONDS: u64 = 10 * 366 * 86_400;

/// The longest rule text this capability parses. An RFC 5545 `RRULE` property
/// is short; a kilobyte of it is not a rule.
pub const MAX_RULE_BYTES: usize = 1_024;

// ---------------------------------------------------------------------------
// The declaration
// ---------------------------------------------------------------------------

/// The spring transition skips an hour of wall-clock time; a rule naming a
/// local time inside it has no instant to expand to.
///
/// The spelling and the meaning are a zoned cron trigger's
/// (`donat_metadata::DstSkippedTime`). They are declared twice — once in the
/// metadata crate, once here — for the reason every local declaration is:
/// `donat-connectors` does not depend on `donat-metadata`, and the serving
/// binary maps between them at the seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum SkippedTime {
    /// One occurrence, at the instant the gap ends: late by the width of the
    /// gap, never lost.
    #[default]
    FireAfterGap,
    /// No occurrence. The wall-clock time is reported rather than dropped in
    /// silence.
    Skip,
}

/// The autumn transition repeats an hour of wall-clock time; a rule naming a
/// local time inside it has two instants and takes exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum RepeatedTime {
    /// The earlier, pre-transition instant.
    #[default]
    FireAtFirst,
    /// The later, post-transition instant.
    FireAtSecond,
}

/// What a zoned policy does at the two wall-clock times a DST transition
/// breaks. Both are stated or neither is: there is no default here and none in
/// the metadata that feeds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DstPolicy {
    pub skipped_time: SkippedTime,
    pub repeated_time: RepeatedTime,
}

/// Everything one recurrence policy declares, as the serving binary hands it
/// over.
#[derive(Debug, Clone, Default)]
pub struct RecurrencePolicySpec {
    pub name: String,
    /// IANA zone the rule's wall-clock times are read in. Absent is UTC, where
    /// no local time is ever missing or repeated.
    pub timezone: Option<String>,
    /// Required with a `timezone`, refused without one.
    pub dst: Option<DstPolicy>,
    pub max_occurrences: u64,
    pub max_window_seconds: u64,
}

/// One resolved recurrence policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrencePolicy {
    name: String,
    zone: Tz,
    dst: Option<DstPolicy>,
    max_occurrences: u64,
    max_window_seconds: u64,
}

impl RecurrencePolicy {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The zone a rule's wall-clock times are read in.
    pub const fn zone(&self) -> Tz {
        self.zone
    }

    pub const fn dst(&self) -> Option<DstPolicy> {
        self.dst
    }

    pub const fn max_occurrences(&self) -> u64 {
        self.max_occurrences
    }

    pub const fn max_window_seconds(&self) -> u64 {
        self.max_window_seconds
    }
}

/// One refusal from resolving a recurrence declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceRejection {
    pub policy: String,
    pub message: String,
}

impl RecurrenceRejection {
    fn new(policy: &str, message: impl Into<String>) -> Self {
        Self {
            policy: policy.to_owned(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RecurrenceRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "recurrence policy `{}`: {}",
            self.policy, self.message
        )
    }
}

/// The deployment's resolved recurrence policies, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecurrencePolicySet {
    policies: BTreeMap<String, Arc<RecurrencePolicy>>,
}

impl RecurrencePolicySet {
    /// Resolve a whole set. Every rejection is collected, because a deployment
    /// with three broken declarations should learn about three.
    pub fn resolve(
        specs: impl IntoIterator<Item = RecurrencePolicySpec>,
    ) -> Result<Self, Vec<RecurrenceRejection>> {
        let mut set = Self::default();
        let mut rejections = Vec::new();
        for spec in specs {
            let name = spec.name.clone();
            match resolve_policy(spec) {
                Ok(policy) => {
                    if set
                        .policies
                        .insert(name.clone(), Arc::new(policy))
                        .is_some()
                    {
                        rejections.push(RecurrenceRejection::new(&name, "is declared twice"));
                    }
                }
                Err(rejection) => rejections.push(rejection),
            }
        }
        if rejections.is_empty() {
            Ok(set)
        } else {
            Err(rejections)
        }
    }

    pub fn policy(&self, name: &str) -> Option<&Arc<RecurrencePolicy>> {
        self.policies.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.policies.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }
}

fn resolve_policy(spec: RecurrencePolicySpec) -> Result<RecurrencePolicy, RecurrenceRejection> {
    let name = spec.name.clone();
    let reject = |message: &str| RecurrenceRejection::new(&name, message);
    if spec.name.is_empty() {
        return Err(reject("a recurrence policy has a name"));
    }
    let zone = match (&spec.timezone, spec.dst) {
        (Some(zone), Some(_)) => zone.parse::<Tz>().map_err(|_| {
            RecurrenceRejection::new(&name, format!("`{zone}` is not an IANA timezone name"))
        })?,
        (Some(_), None) => {
            return Err(reject(
                "a policy declared in a timezone must say what it does at the local time a DST \
                 transition skips and at the one it repeats",
            ));
        }
        (None, Some(_)) => {
            return Err(reject(
                "a policy declares `dst` but no `timezone`; a UTC recurrence has no DST \
                 transitions, so the policies would never be read",
            ));
        }
        (None, None) => Tz::UTC,
    };
    if spec.max_occurrences == 0 || spec.max_occurrences > MAX_OCCURRENCES {
        return Err(reject(
            "a recurrence policy admits between one occurrence and this binary's ceiling",
        ));
    }
    if spec.max_window_seconds == 0 || spec.max_window_seconds > MAX_WINDOW_SECONDS {
        return Err(reject(
            "a recurrence policy declares a positive window no longer than this binary's ceiling",
        ));
    }
    Ok(RecurrencePolicy {
        name: spec.name,
        zone,
        dst: spec.dst,
        max_occurrences: spec.max_occurrences,
        max_window_seconds: spec.max_window_seconds,
    })
}

/// The policies the determinism probes run against: compiled in, and nothing a
/// deployment declared.
pub fn builtin_policies() -> Vec<RecurrencePolicySpec> {
    vec![
        RecurrencePolicySpec {
            name: "probe_utc".to_owned(),
            timezone: None,
            dst: None,
            max_occurrences: 64,
            max_window_seconds: 366 * 86_400,
        },
        RecurrencePolicySpec {
            name: "probe_zoned".to_owned(),
            timezone: Some("Europe/Berlin".to_owned()),
            dst: Some(DstPolicy {
                skipped_time: SkippedTime::FireAfterGap,
                repeated_time: RepeatedTime::FireAtFirst,
            }),
            max_occurrences: 64,
            max_window_seconds: 366 * 86_400,
        },
    ]
}

// ---------------------------------------------------------------------------
// Boundedness, answered from the rule
// ---------------------------------------------------------------------------

/// The shortest a frequency's period can be, in seconds.
///
/// Shortest, deliberately: it is one half of an upper bound on how many
/// occurrences fit in a span, and the other half ([`days_in_period`]) takes the
/// longest. A month counts as 28 days and a year as 365 here, and as 31 and 366
/// there.
const fn period_seconds(frequency: Frequency) -> u64 {
    match frequency {
        Frequency::Yearly => 365 * 86_400,
        Frequency::Monthly => 28 * 86_400,
        Frequency::Weekly => 7 * 86_400,
        Frequency::Daily => 86_400,
        Frequency::Hourly => 3_600,
        Frequency::Minutely => 60,
        Frequency::Secondly => 1,
    }
}

/// The most days one period of a frequency can contain.
const fn days_in_period(frequency: Frequency) -> u64 {
    match frequency {
        Frequency::Yearly => 366,
        Frequency::Monthly => 31,
        Frequency::Weekly => 7,
        Frequency::Daily | Frequency::Hourly | Frequency::Minutely | Frequency::Secondly => 1,
    }
}

/// The most occurrences one period of the rule's frequency can carry.
///
/// RFC 5545 §3.3.10 splits `BY*` parts into ones that *expand* a period and
/// ones that *limit* it, by whether the part is finer or coarser than `FREQ`. A
/// limiting part can only remove occurrences, so it is counted as 1; an
/// expanding part multiplies. Every choice here rounds up, because the number
/// is a ceiling a rule is admitted against and an under-count would admit a
/// rule this capability then could not bound.
fn per_period_maximum(rule: &RRule<Unvalidated>) -> u64 {
    let frequency = rule.get_freq();
    let expands_hours = matches!(
        frequency,
        Frequency::Yearly | Frequency::Monthly | Frequency::Weekly | Frequency::Daily
    );
    let expands_minutes = expands_hours || matches!(frequency, Frequency::Hourly);
    let expands_seconds = expands_minutes || matches!(frequency, Frequency::Minutely);
    let expands_days = matches!(
        frequency,
        Frequency::Yearly | Frequency::Monthly | Frequency::Weekly
    );

    let cardinality = |values: usize, expands: bool| -> u64 {
        if expands && values > 0 {
            values as u64
        } else {
            1
        }
    };

    let time = cardinality(rule.get_by_hour().len(), expands_hours)
        .saturating_mul(cardinality(rule.get_by_minute().len(), expands_minutes))
        .saturating_mul(cardinality(rule.get_by_second().len(), expands_seconds));

    let days = days_in_period(frequency);
    let day = if !expands_days {
        1
    } else if !rule.get_by_year_day().is_empty() {
        rule.get_by_year_day().len() as u64
    } else if !rule.get_by_month_day().is_empty() {
        // A month day inside a yearly period lands once per admitted month.
        let months = if matches!(frequency, Frequency::Yearly) {
            if rule.get_by_month().is_empty() {
                12
            } else {
                rule.get_by_month().len() as u64
            }
        } else {
            1
        };
        (rule.get_by_month_day().len() as u64).saturating_mul(months)
    } else if !rule.get_by_weekday().is_empty() {
        // An unprefixed weekday means *every* one of them in the period, which
        // is the case a naive count of the list gets wrong.
        rule.get_by_weekday()
            .iter()
            .map(|weekday| match weekday {
                NWeekday::Every(_) => days / 7 + 1,
                NWeekday::Nth(..) => 1,
            })
            .fold(0_u64, u64::saturating_add)
    } else if !rule.get_by_week_no().is_empty() {
        (rule.get_by_week_no().len() as u64).saturating_mul(7)
    } else if matches!(frequency, Frequency::Yearly) && !rule.get_by_month().is_empty() {
        // `BYMONTH` expands a yearly period (RFC 5545 §3.3.10) — it is the one
        // expanding part that carries no day of its own, so with nothing finer
        // beside it the period holds one occurrence per named month rather than
        // one. Under a monthly or weekly `FREQ` the same part *limits*, and is
        // already covered by the 1 below.
        rule.get_by_month().len() as u64
    } else {
        1
    };

    let mut maximum = time.saturating_mul(day.min(days));
    if !rule.get_by_set_pos().is_empty() {
        // `BYSETPOS` selects from the period rather than adding to it.
        maximum = maximum.min(rule.get_by_set_pos().len() as u64);
    }
    // Nothing recurs more than once a second.
    maximum.min(period_seconds(frequency).saturating_mul(days_in_period(frequency)))
}

/// The most occurrences a rule can produce in `span_seconds` from its start.
///
/// This is the whole of the boundedness answer, and it is arithmetic over the
/// rule's own parts: no iteration, no clock, nothing that depends on how long
/// an expansion would take to discover the same thing.
fn worst_case_occurrences(rule: &RRule<Unvalidated>, span_seconds: u64) -> u64 {
    let step = period_seconds(rule.get_freq())
        .saturating_mul(u64::from(rule.get_interval()))
        .max(1);
    // One extra period, because a span rarely starts on a period boundary.
    let periods = (span_seconds / step).saturating_add(1);
    let worst = periods.saturating_mul(per_period_maximum(rule));
    match rule.get_count() {
        // A rule that ends is bounded by where it ends, however dense it is.
        Some(count) => worst.min(u64::from(count)),
        None => worst,
    }
}

/// The span an expansion walks: from the rule's start to the earlier of the end
/// of the window and the rule's own `UNTIL`.
fn walk_span(start: NaiveDateTime, to: DateTime<Utc>, until: Option<DateTime<Utc>>) -> u64 {
    let end = match until {
        Some(until) if until < to => until,
        _ => to,
    };
    span_seconds(start, end)
}

// ---------------------------------------------------------------------------
// The admitted rule
// ---------------------------------------------------------------------------

/// A rule that passed admission: parsed, bounded, and carrying the two things
/// the expansion needs beside it.
struct AdmittedRule {
    rule: RRule<Unvalidated>,
    /// The `UNTIL` instant, taken out of the rule so it can be applied where
    /// RFC 5545 defines it — on the resolved instant — rather than on the
    /// wall-clock value the expansion is driven over.
    until: Option<DateTime<Utc>>,
    /// The worst case the rule was admitted on.
    worst_case: u64,
}

/// Parse one rule's text.
///
/// The refusals here are the ones that keep the operation pure. A `DTSTART`
/// line, or an `UNTIL` that is not UTC, is read by `rrule` in the machine's
/// local timezone — an ambient environment lookup, which is exactly what spec
/// 018 §3 forbids and what RFC 5545 §3.3.10 also refuses once `DTSTART` carries
/// a zone.
fn parse_rule(text: &str) -> Result<(RRule<Unvalidated>, Option<DateTime<Utc>>), ConnectorFailure> {
    if text.len() > MAX_RULE_BYTES {
        return Err(refuse(
            "recurrence_rule_invalid",
            "a recurrence rule is one RFC 5545 RRULE property value",
        ));
    }
    if text.contains(['\n', '\r'])
        || text.to_ascii_uppercase().contains("DTSTART")
        || text.to_ascii_uppercase().starts_with("RRULE:")
    {
        return Err(refuse(
            "recurrence_rule_invalid",
            "a recurrence rule is the RRULE property value alone: no property name, no DTSTART, \
             and no second line — the start is a declared input",
        ));
    }
    let rule = text.parse::<RRule<Unvalidated>>().map_err(|_| {
        refuse(
            "recurrence_rule_invalid",
            "the recurrence rule is not a valid RFC 5545 RRULE property value",
        )
    })?;
    let until = match rule.get_until() {
        None => None,
        Some(until) => {
            if until.timezone().name() != "UTC" {
                return Err(refuse(
                    "recurrence_rule_invalid",
                    "an UNTIL is written in UTC (a trailing `Z`); a floating one would be read in \
                     the machine's own timezone",
                ));
            }
            Some(DateTime::<Utc>::from_naive_utc_and_offset(
                until.naive_utc(),
                Utc,
            ))
        }
    };
    Ok((rule, until))
}

/// Admit a rule against a policy and the span the expansion would walk.
///
/// `span_seconds` is measured from the rule's start to the end of the window,
/// because that is the sequence an expansion has to walk: `rrule` has no seek,
/// so an occurrence before the window still costs a step.
fn admit(
    rule: RRule<Unvalidated>,
    until: Option<DateTime<Utc>>,
    span_seconds: u64,
    policy: &RecurrencePolicy,
) -> Result<AdmittedRule, ConnectorFailure> {
    let worst_case = worst_case_occurrences(&rule, span_seconds);
    if worst_case > policy.max_occurrences {
        return Err(refuse(
            "recurrence_rule_unbounded",
            "the rule can produce more occurrences than its policy admits; a rule with neither \
             UNTIL nor COUNT at this frequency is bounded by nothing",
        )
        .with_correlation_ids([
            ("worst_case_occurrences", worst_case.to_string()),
            ("max_occurrences", policy.max_occurrences.to_string()),
        ]));
    }
    Ok(AdmittedRule {
        rule,
        until,
        worst_case,
    })
}

// ---------------------------------------------------------------------------
// Expansion
// ---------------------------------------------------------------------------

/// The instant range one expansion answers over, and what bounds it.
///
/// `exclusive_from` is the difference between the two questions the capability
/// answers: an expansion over a window includes an occurrence sitting on the
/// window's first instant, while `next` is asked for what comes *after* an
/// instant and must not answer with that instant itself.
#[derive(Debug, Clone, Copy)]
struct Window {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    exclusive_from: bool,
    limit: u64,
}

/// One occurrence: the instant it happens at, and the wall clock it carries in
/// the policy's zone.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    at: DateTime<Utc>,
    local: NaiveDateTime,
}

/// What one expansion produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Expansion {
    occurrences: Vec<Occurrence>,
    /// Wall-clock times the declared DST policy produced no distinct occurrence
    /// for: one the `skip` policy declined, or one a gap collapsed onto an
    /// instant already taken. Reported rather than dropped in silence.
    skipped: Vec<NaiveDateTime>,
}

/// Expand `rule` over the instant window `(from, to]`, bounded by `limit`.
///
/// The expansion is driven over naive wall-clock time — a naive value read as
/// UTC, where every local time exists exactly once — and each candidate is
/// resolved into the policy's zone here, by the declared policy. That is what
/// keeps `rrule`'s own DST answer (midnight plus the wall-clock offset for a
/// missing time, the first instant for a repeated one) from deciding what a
/// deployment declared.
fn expand(
    admitted: &AdmittedRule,
    start: NaiveDateTime,
    policy: &RecurrencePolicy,
    window: &Window,
    invocation: &LocalInvocation<'_>,
) -> Result<Expansion, ConnectorFailure> {
    let Window {
        from,
        to,
        exclusive_from,
        limit,
    } = *window;
    let zone = policy.zone;
    // The instant bound, in the wall-clock space the iteration happens in. The
    // map from instant to local time is weakly monotone, so a wall-clock stop
    // at the local image of `to` can never cut an occurrence whose instant is
    // inside the window.
    let end = zone.from_utc_datetime(&to.naive_utc()).naive_local();
    let dt_start = rrule::Tz::UTC.from_utc_datetime(&start);
    // `UNTIL` is an instant, and the iteration is over wall clock, so the rule
    // carries the local image of it. Without this rewrite the library would cut
    // the sequence at the naive reading of a UTC instant — early by the zone's
    // offset — and the occurrence the RFC includes would be missing.
    let mut rule = admitted.rule.clone();
    if let Some(until) = admitted.until {
        let local_until = zone.from_utc_datetime(&until.naive_utc()).naive_local();
        rule = rule.until(rrule::Tz::UTC.from_utc_datetime(&local_until));
    }
    let validated = rule.validate(dt_start).map_err(|_| {
        refuse(
            "recurrence_rule_invalid",
            "the recurrence rule is not valid for its declared start (an UNTIL before the start \
             is a rule that never happens)",
        )
    })?;
    let set = RRuleSet::new(dt_start).rrule(validated).limit();

    let mut expansion = Expansion::default();
    let mut walked = 0_u64;
    let mut previous: Option<DateTime<Utc>> = None;
    for candidate in &set {
        invocation.checkpoint()?;
        walked += 1;
        if walked > admitted.worst_case.saturating_add(1) {
            // The admitted bound and the expansion disagree. That is a defect
            // in this module, not something a deployment can fix by retrying.
            return Err(ConnectorFailure::new(
                ConnectorErrorClass::Invariant,
                "recurrence_bound_exceeded",
                "the recurrence expansion walked past the bound its rule was admitted on",
            ));
        }
        let local = candidate.naive_utc();
        if local > end {
            break;
        }
        let at = match resolve(zone, local, policy.dst)? {
            Some(at) => at,
            None => {
                // The declared `skip` policy: no occurrence, and the wall-clock
                // time is reported rather than dropped in silence.
                expansion.skipped.push(local);
                continue;
            }
        };
        if matches!(admitted.until, Some(until) if at > until) {
            break;
        }
        if at > to {
            break;
        }
        let before_window = if exclusive_from {
            at <= from
        } else {
            at < from
        };
        if before_window {
            continue;
        }
        if matches!(previous, Some(previous) if at <= previous) {
            // Two wall-clock times inside one gap collapse onto the instant it
            // ends. A declaration that says once does not happen twice, so the
            // second one is reported beside the ones `skip` declined.
            expansion.skipped.push(local);
            continue;
        }
        previous = Some(at);
        invocation.reserve(std::mem::size_of::<Occurrence>())?;
        // The wall clock reported is the one the *resolved instant* carries,
        // not the one the rule asked for: under `fire_after_gap` those differ,
        // and the honest answer is the time the occurrence actually happens at.
        expansion.occurrences.push(Occurrence {
            at,
            local: zone.from_utc_datetime(&at.naive_utc()).naive_local(),
        });
        if expansion.occurrences.len() as u64 >= limit {
            break;
        }
    }
    Ok(expansion)
}

/// Resolve one wall-clock time into an instant, by the declared policy.
///
/// This is the whole of ADR 039 applied to an expansion: `Single` is the 8758
/// hours a year that need no policy, `Ambiguous` is the autumn hour that has
/// two instants and takes exactly one, and `None` is the spring hour that has
/// none. `Ok(None)` means the declared policy produced no occurrence.
///
/// A zone whose transitions are reachable without a declared policy is a defect
/// rather than a default: [`RecurrencePolicySet::resolve`] refuses a zoned
/// policy without one, so this branch means a policy reached the expansion
/// through some other door.
fn resolve(
    zone: Tz,
    local: NaiveDateTime,
    dst: Option<DstPolicy>,
) -> Result<Option<DateTime<Utc>>, ConnectorFailure> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(at) => Ok(Some(at.with_timezone(&Utc))),
        LocalResult::Ambiguous(first, second) => {
            let dst = dst.ok_or_else(undeclared_transition)?;
            Ok(Some(
                match dst.repeated_time {
                    RepeatedTime::FireAtFirst => first,
                    RepeatedTime::FireAtSecond => second,
                }
                .with_timezone(&Utc),
            ))
        }
        LocalResult::None => {
            let dst = dst.ok_or_else(undeclared_transition)?;
            match dst.skipped_time {
                SkippedTime::FireAfterGap => gap_end(zone, local).map(Some).ok_or_else(|| {
                    ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "recurrence_gap_unresolvable",
                        "a local time is missing from its zone in a shape this engine cannot \
                         bisect a day either side of",
                    )
                }),
                SkippedTime::Skip => Ok(None),
            }
        }
    }
}

fn undeclared_transition() -> ConnectorFailure {
    ConnectorFailure::new(
        ConnectorErrorClass::Invariant,
        "recurrence_dst_undeclared",
        "a zoned recurrence reached a DST transition with no declared policy for it",
    )
}

/// The instant a DST gap ends, for a local time that falls inside it.
///
/// Local time is strictly increasing across a forward transition, so the gap
/// end is the first instant whose local time has reached `local` — found by
/// bisection over a day either side, a window that contains the one transition
/// that made the gap and no other. This is the same search, for the same
/// reason, that a zoned cron trigger runs (`crates/server/src/cron.rs`); the
/// two crates do not depend on each other, so the ten lines are written twice.
fn gap_end(zone: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    let reached =
        |at: DateTime<Utc>| zone.from_utc_datetime(&at.naive_utc()).naive_local() >= local;

    let mut lo = local.checked_sub_signed(TimeDelta::hours(24))?.and_utc();
    let mut hi = local.checked_add_signed(TimeDelta::hours(24))?.and_utc();
    if reached(lo) || !reached(hi) {
        return None;
    }
    while hi - lo > TimeDelta::seconds(1) {
        let mid = lo + (hi - lo) / 2;
        if reached(mid) { hi = mid } else { lo = mid }
    }
    Some(hi)
}

// ---------------------------------------------------------------------------
// The capability
// ---------------------------------------------------------------------------

/// The capability's declaration, built once by the table in
/// [`crate::local::capabilities`].
pub fn capability() -> LocalCapability {
    LocalCapability::declare(RECURRENCE_CAPABILITY, "1.0.0")
        .operation(validate_operation())
        .operation(expand_operation())
        .operation(next_operation())
        .build()
        .expect("the recurrence capability declaration is static and complete")
}

/// The bounds every recurrence operation runs inside.
///
/// `max_units` is [`MAX_OCCURRENCES`], and the unit count an input implies is
/// the worst case its rule can produce — so an unbounded rule is refused by
/// [`crate::local::LocalOperation::execute`]'s unit gate, before the executor
/// is entered.
fn bounds(cpu_deadline: Duration) -> LocalBounds {
    LocalBounds::declare(
        cpu_deadline,
        4_096,
        2 * 1_024 * 1_024,
        8 * 1_024 * 1_024,
        "occurrences",
        MAX_OCCURRENCES,
    )
    .expect("the recurrence bounds are static and complete")
}

fn validate_operation() -> LocalOperation {
    LocalOperation::declare("rule.validate", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "policy": "probe_utc",
                    "rule": "FREQ=DAILY;COUNT=3",
                    "start": "2026-01-01T09:00:00"
                }),
                "the output is the parsed rule and the bound it was admitted on; \
                 no clock, no random seed, no environment, no locale",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds(Duration::from_secs(1)))
        // A `validate` that could be refused by the unit gate could never
        // explain *why* a rule is unbounded, which is its whole job.
        .units(|_| 1)
        .run(run_validate)
        .build()
        .expect("rule.validate is deterministic")
}

fn expand_operation() -> LocalOperation {
    LocalOperation::declare("rule.expand", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "policy": "probe_zoned",
                    "rule": "FREQ=DAILY;COUNT=3",
                    "start": "2026-03-27T09:00:00",
                    "window": { "from": "2026-03-27T00:00:00Z", "to": "2026-03-31T00:00:00Z" }
                }),
                "the output is the occurrences of the declared rule inside the declared window, \
                 resolved through the declared policy; the current time is never read",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds(Duration::from_secs(5)))
        .units(expand_units)
        .run(run_expand)
        .build()
        .expect("rule.expand is deterministic")
}

fn next_operation() -> LocalOperation {
    LocalOperation::declare("rule.next", "1.0.0")
        .effect(Effect::pure(
            DeterminismEvidence::double_render(
                json!({
                    "policy": "probe_utc",
                    "rule": "FREQ=WEEKLY;COUNT=10",
                    "start": "2026-01-05T09:00:00",
                    "after": "2026-01-10T00:00:00Z",
                    "count": 3
                }),
                "the output is the occurrences after the declared instant; the instant is an \
                 input, so the wall clock is never read",
            )
            .expect("a probe and a statement are evidence"),
        ))
        .bounds(bounds(Duration::from_secs(5)))
        .units(next_units)
        .run(run_next)
        .build()
        .expect("rule.next is deterministic")
}

/// The occurrences `rule.expand` may have to walk, read from the input alone.
///
/// An input this cannot read counts as zero rather than as the ceiling: it is
/// about to be refused by the executor with a message that says *which* field
/// was wrong, and a unit-ceiling refusal here would replace that with "too
/// large". The gate exists for the rule that is bounded by nothing, not for the
/// input that is malformed.
fn expand_units(input: &JsonValue) -> u64 {
    let Ok((rule, until)) = text(input, "rule").and_then(parse_rule) else {
        return 0;
    };
    let Ok(start) = naive(input, "start") else {
        return 0;
    };
    let Ok(to) = input
        .get("window")
        .ok_or(())
        .and_then(|window| instant(window, "to").map_err(|_| ()))
    else {
        return 0;
    };
    worst_case_occurrences(&rule, walk_span(start, to, until))
}

/// The occurrences `rule.next` may have to walk. The horizon is this binary's
/// window ceiling, because the policy's own — which is smaller — is not
/// readable from the input.
fn next_units(input: &JsonValue) -> u64 {
    let Ok((rule, until)) = text(input, "rule").and_then(parse_rule) else {
        return 0;
    };
    let Ok(start) = naive(input, "start") else {
        return 0;
    };
    let Ok(after) = instant(input, "after") else {
        return 0;
    };
    let horizon = after
        .checked_add_signed(TimeDelta::seconds(MAX_WINDOW_SECONDS as i64))
        .unwrap_or(after);
    worst_case_occurrences(&rule, walk_span(start, horizon, until))
}

/// The span an expansion walks: from the rule's start to the end of its window.
fn span_seconds(start: NaiveDateTime, to: DateTime<Utc>) -> u64 {
    (to.naive_utc() - start).num_seconds().max(0) as u64
}

fn run_validate(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let policy = policy(invocation)?;
    let (rule, until) = text(input, "rule").and_then(parse_rule)?;
    let start = naive(input, "start")?;
    // With no window of its own, `validate` is admitted against the largest one
    // its policy would ever grant: a rule admitted here is a rule every
    // expansion under that policy can also admit. An `UNTIL` still shortens it,
    // which is why the start is required here too — a recurrence rule without
    // one is not a sequence, in this engine or in RFC 5545.
    let horizon = start
        .and_utc()
        .checked_add_signed(TimeDelta::seconds(policy.max_window_seconds as i64))
        .ok_or_else(|| {
            refuse(
                "recurrence_input_contract",
                "the declared start is outside the range this engine expands over",
            )
        })?;
    let admitted = admit(rule, until, walk_span(start, horizon, until), &policy)?;
    Ok(LocalProduct::Value(json!({
        "policy": policy.name(),
        "timezone": policy.zone().name(),
        "rule": admitted.rule.to_string(),
        "frequency": frequency_name(admitted.rule.get_freq()),
        "interval": admitted.rule.get_interval(),
        "bounded_by": bounded_by(&admitted),
        "worst_case_occurrences": admitted.worst_case,
    })))
}

fn run_expand(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let policy = policy(invocation)?;
    let (rule, until) = text(input, "rule").and_then(parse_rule)?;
    let start = naive(input, "start")?;
    let window = input.get("window").ok_or_else(|| {
        refuse(
            "recurrence_input_contract",
            "an expansion declares the `window` it is bounded by",
        )
    })?;
    let from = instant(window, "from")?;
    let to = instant(window, "to")?;
    if to <= from {
        return Err(refuse(
            "recurrence_window_invalid",
            "a recurrence window ends after it starts",
        ));
    }
    let span = span_seconds(start, to);
    if span > policy.max_window_seconds {
        return Err(refuse(
            "recurrence_window_too_long",
            "the window reaches further from the rule's start than its policy admits",
        )
        .with_correlation_ids([
            ("window_seconds", span.to_string()),
            ("max_window_seconds", policy.max_window_seconds.to_string()),
        ]));
    }
    let admitted = admit(rule, until, walk_span(start, to, until), &policy)?;
    let expansion = expand(
        &admitted,
        start,
        &policy,
        &Window {
            from,
            to,
            exclusive_from: false,
            limit: policy.max_occurrences,
        },
        invocation,
    )?;
    Ok(occurrences_value(&policy, &expansion))
}

fn run_next(invocation: &LocalInvocation<'_>) -> Result<LocalProduct, ConnectorFailure> {
    let input = invocation.input();
    let policy = policy(invocation)?;
    let (rule, until) = text(input, "rule").and_then(parse_rule)?;
    let start = naive(input, "start")?;
    let after = instant(input, "after")?;
    let count = input
        .get("count")
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| {
            refuse(
                "recurrence_input_contract",
                "`next` declares how many occurrences it wants",
            )
        })?;
    if count == 0 || count > policy.max_occurrences {
        return Err(refuse(
            "recurrence_count_too_large",
            "the requested occurrence count is outside what the policy admits",
        )
        .with_correlation_ids([
            ("count", count.to_string()),
            ("max_occurrences", policy.max_occurrences.to_string()),
        ]));
    }
    // The horizon is the policy's window, measured from the instant asked
    // about: a `next` with nothing ahead of it stops looking rather than
    // walking to the end of the rule.
    let to = after
        .checked_add_signed(TimeDelta::seconds(policy.max_window_seconds as i64))
        .ok_or_else(|| {
            refuse(
                "recurrence_input_contract",
                "the declared instant is outside the range this engine expands over",
            )
        })?;
    let admitted = admit(rule, until, walk_span(start, to, until), &policy)?;
    let expansion = expand(
        &admitted,
        start,
        &policy,
        &Window {
            from: after,
            to,
            exclusive_from: true,
            limit: count,
        },
        invocation,
    )?;
    Ok(occurrences_value(&policy, &expansion))
}

fn occurrences_value(policy: &RecurrencePolicy, expansion: &Expansion) -> LocalProduct {
    LocalProduct::Value(json!({
        "policy": policy.name(),
        "timezone": policy.zone().name(),
        "count": expansion.occurrences.len(),
        "occurrences": expansion
            .occurrences
            .iter()
            .map(|occurrence| json!({
                "at": occurrence.at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "local": occurrence.local.format("%Y-%m-%dT%H:%M:%S").to_string(),
            }))
            .collect::<Vec<_>>(),
        "skipped": expansion
            .skipped
            .iter()
            .map(|local| local.format("%Y-%m-%dT%H:%M:%S").to_string())
            .collect::<Vec<_>>(),
    }))
}

/// The bound the rule was admitted on, in the vocabulary an operator reads.
fn bounded_by(admitted: &AdmittedRule) -> &'static str {
    if admitted.rule.get_count().is_some() {
        "count"
    } else if admitted.until.is_some() {
        "until"
    } else {
        "window"
    }
}

const fn frequency_name(frequency: Frequency) -> &'static str {
    match frequency {
        Frequency::Yearly => "yearly",
        Frequency::Monthly => "monthly",
        Frequency::Weekly => "weekly",
        Frequency::Daily => "daily",
        Frequency::Hourly => "hourly",
        Frequency::Minutely => "minutely",
        Frequency::Secondly => "secondly",
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// The policy an input selects, from the deployment's declarations.
///
/// It travels in the [`LocalContext`] beside the input and never inside it: a
/// DST policy and an occurrence ceiling are promises the deployment made, and a
/// run that could supply its own would be making them itself.
fn policy(invocation: &LocalInvocation<'_>) -> Result<Arc<RecurrencePolicy>, ConnectorFailure> {
    let name = text(invocation.input(), "policy")?;
    invocation
        .context()
        .recurrence_policies()
        .policy(name)
        .cloned()
        .ok_or_else(|| {
            refuse(
                "recurrence_policy_unknown",
                "the recurrence policy this input names is not declared by this deployment",
            )
        })
}

fn text<'a>(input: &'a JsonValue, field: &str) -> Result<&'a str, ConnectorFailure> {
    input.get(field).and_then(JsonValue::as_str).ok_or_else(|| {
        refuse(
            "recurrence_input_contract",
            "a recurrence input names its policy, its rule, and its start",
        )
    })
}

/// A wall-clock start: `YYYY-MM-DDTHH:MM:SS`, with no offset and no `Z`.
///
/// An offset is refused rather than converted, because a zoned rule is about
/// the wall clock: "09:00 every weekday" is the declaration, and an instant
/// would hide which local time its author meant.
fn naive(input: &JsonValue, field: &str) -> Result<NaiveDateTime, ConnectorFailure> {
    let value = text(input, field)?;
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").map_err(|_| {
        refuse(
            "recurrence_input_contract",
            "a recurrence start is a wall-clock time (`YYYY-MM-DDTHH:MM:SS`) in the policy's \
             zone, with no offset",
        )
    })
}

fn instant(input: &JsonValue, field: &str) -> Result<DateTime<Utc>, ConnectorFailure> {
    let value = text(input, field)?;
    DateTime::parse_from_rfc3339(value)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|_| {
            refuse(
                "recurrence_input_contract",
                "a recurrence instant is an RFC 3339 timestamp",
            )
        })
}

/// An input outside the operation's contract is a `validation` failure, exactly
/// as an over-limit input is: the same input will fail the same way again.
fn refuse(code: &'static str, message: &'static str) -> ConnectorFailure {
    ConnectorFailure::new(ConnectorErrorClass::Validation, code, message)
}

/// Run one operation of this capability against one context. Used by the tests
/// and by nothing else.
#[cfg(test)]
use crate::local::capability::StopSignal;
#[cfg(test)]
use crate::local::context::LocalContext;

#[cfg(test)]
fn run(
    operation: &str,
    input: JsonValue,
    context: &LocalContext,
) -> Result<JsonValue, ConnectorFailure> {
    let capability = capability();
    let declared = capability
        .admit_operation(operation)
        .expect("the operation is declared and executable");
    match declared.execute(&input, context, None, &StopSignal::new())? {
        LocalProduct::Value(value) => Ok(value),
        LocalProduct::Artifact { .. } => unreachable!("recurrence produces values"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> LocalContext {
        LocalContext::default().with_recurrence(
            RecurrencePolicySet::resolve(builtin_policies())
                .expect("the built-in policies resolve"),
        )
    }

    fn policy_context(spec: RecurrencePolicySpec) -> LocalContext {
        LocalContext::default().with_recurrence(
            RecurrencePolicySet::resolve([spec]).expect("the declared policy resolves"),
        )
    }

    fn utc_policy(
        name: &str,
        max_occurrences: u64,
        max_window_seconds: u64,
    ) -> RecurrencePolicySpec {
        RecurrencePolicySpec {
            name: name.to_owned(),
            timezone: None,
            dst: None,
            max_occurrences,
            max_window_seconds,
        }
    }

    fn berlin_policy(
        name: &str,
        skipped_time: SkippedTime,
        repeated_time: RepeatedTime,
    ) -> RecurrencePolicySpec {
        RecurrencePolicySpec {
            name: name.to_owned(),
            timezone: Some("Europe/Berlin".to_owned()),
            dst: Some(DstPolicy {
                skipped_time,
                repeated_time,
            }),
            max_occurrences: 500,
            max_window_seconds: 366 * 86_400,
        }
    }

    fn instants(value: &JsonValue) -> Vec<String> {
        value["occurrences"]
            .as_array()
            .expect("an expansion carries its occurrences")
            .iter()
            .map(|occurrence| occurrence["at"].as_str().expect("an instant").to_owned())
            .collect()
    }

    fn locals(value: &JsonValue) -> Vec<String> {
        value["occurrences"]
            .as_array()
            .expect("an expansion carries its occurrences")
            .iter()
            .map(|occurrence| {
                occurrence["local"]
                    .as_str()
                    .expect("a wall clock")
                    .to_owned()
            })
            .collect()
    }

    /// Spec 021 §4 `recurrence_rejects_unbounded_rules`.
    ///
    /// A rule with neither `UNTIL` nor `COUNT` at a pathological frequency is
    /// refused when it is declared — from arithmetic over the rule's own parts,
    /// with no occurrence generated — and the refusal happens on all three
    /// operations, including the bound layer that runs before the executor is
    /// entered at all.
    #[test]
    fn recurrence_rejects_unbounded_rules() {
        /// A recurrence rule is not a sequence without a start, so every
        /// operation takes one — including the one whose job is to refuse.
        const START: &str = "2026-01-01T09:00:00";

        let context = context();

        // `validate` is the operation whose job is the refusal, so it is the
        // one that must name it.
        let failure = run(
            "rule.validate",
            json!({ "policy": "probe_utc", "rule": "FREQ=SECONDLY", "start": START }),
            &context,
        )
        .expect_err("a secondly rule with no bound is not a schedule");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "recurrence_rule_unbounded");

        // And the same rule never reaches an expansion: the unit ceiling is the
        // worst case its declaration implies, so `execute` refuses it before
        // the executor runs.
        let failure = run(
            "rule.expand",
            json!({
                "policy": "probe_utc",
                "rule": "FREQ=SECONDLY",
                "start": "2026-01-01T00:00:00",
                "window": { "from": "2026-01-01T00:00:00Z", "to": "2026-01-02T00:00:00Z" }
            }),
            &context,
        )
        .expect_err("an unbounded rule is refused before it is expanded");
        assert_eq!(failure.class(), ConnectorErrorClass::Validation);
        assert_eq!(failure.code(), "local_units_exceeded");

        // The same shape, one step coarser, is still refused: a minutely rule
        // over a year is 525 600 occurrences.
        assert_eq!(
            run(
                "rule.validate",
                json!({ "policy": "probe_utc", "rule": "FREQ=MINUTELY", "start": START }),
                &context,
            )
            .expect_err("a minutely rule with no bound is refused too")
            .code(),
            "recurrence_rule_unbounded"
        );

        // Bounding it is what admits it: the same frequency with a COUNT the
        // policy admits is a rule.
        let admitted = run(
            "rule.validate",
            json!({ "policy": "probe_utc", "rule": "FREQ=SECONDLY;COUNT=10", "start": START }),
            &context,
        )
        .expect("a rule that ends is bounded");
        assert_eq!(admitted["bounded_by"], "count");
        assert_eq!(admitted["worst_case_occurrences"], 10);

        // So is an UNTIL close enough to the start.
        let admitted = run(
            "rule.validate",
            json!({
                "policy": "probe_utc",
                "rule": "FREQ=DAILY;UNTIL=20260201T000000Z",
                "start": START
            }),
            &context,
        )
        .expect("a rule that ends is bounded");
        assert_eq!(admitted["bounded_by"], "until");

        // A rule that ends on its own but produces more than the policy admits
        // is refused on the same path, with both numbers in the diagnostic.
        let failure = run(
            "rule.validate",
            json!({ "policy": "probe_utc", "rule": "FREQ=SECONDLY;COUNT=5000", "start": START }),
            &context,
        )
        .expect_err("a bounded rule still has to fit its policy");
        assert_eq!(failure.code(), "recurrence_rule_unbounded");
        assert_eq!(
            failure.correlation_ids().get("max_occurrences"),
            Some(&"64".to_owned())
        );

        // A rule the window itself bounds needs no COUNT and no UNTIL: three
        // days a week for a year is 156 occurrences, and a policy that admits
        // that many admits the rule. This is the case that makes "unbounded" a
        // judgement about the rule *and* its policy rather than about `FREQ`.
        let roomy = policy_context(utc_policy("roomy", 500, 366 * 86_400));
        let admitted = run(
            "rule.validate",
            json!({ "policy": "roomy", "rule": "FREQ=WEEKLY;BYDAY=MO,WE,FR", "start": START }),
            &roomy,
        )
        .expect("a weekly rule inside the policy's window is bounded by the window");
        assert_eq!(admitted["bounded_by"], "window");
        assert_eq!(
            admitted["worst_case_occurrences"], 318,
            "the bound rounds up — 53 weeks, and an unprefixed weekday may land twice in one              of them — because a ceiling that under-counts is not a ceiling"
        );

        // The same rule under a policy that admits fewer is refused, and
        // nothing about the rule changed: boundedness is the pair.
        assert_eq!(
            run(
                "rule.validate",
                json!({ "policy": "probe_utc", "rule": "FREQ=WEEKLY;BYDAY=MO,WE,FR", "start": START }),
                &context,
            )
            .expect_err("a 64-occurrence policy does not admit a year of three days a week")
            .code(),
            "recurrence_rule_unbounded"
        );

        // The `BY*` expansions count: a daily rule that fires every minute of
        // every hour is 1440 a day, not one.
        assert_eq!(
            run(
                "rule.validate",
                json!({
                    "policy": "probe_utc",
                    "rule": "FREQ=DAILY;COUNT=2000;BYHOUR=0,6,12,18;BYMINUTE=0,30",
                    "start": START
                }),
                &context,
            )
            .expect_err("a rule that expands into its own period is counted as it expands")
            .code(),
            "recurrence_rule_unbounded"
        );
    }

    /// Spec 021 §4 `recurrence_expansion_is_bounded_and_deterministic`.
    ///
    /// The window and count bounds hold exactly, two expansions of one input
    /// match byte for byte, and nothing in the path reads a clock: the window,
    /// the start, and the `after` instant are all declared inputs.
    #[test]
    fn recurrence_expansion_is_bounded_and_deterministic() {
        let context = policy_context(utc_policy("bookings", 500, 366 * 86_400));

        // The window is inclusive at both ends, and exact: an occurrence at the
        // window's last instant is inside it, and the next one is not.
        let expansion = run(
            "rule.expand",
            json!({
                "policy": "bookings",
                "rule": "FREQ=DAILY",
                "start": "2026-01-01T09:00:00",
                "window": { "from": "2026-01-02T09:00:00Z", "to": "2026-01-05T09:00:00Z" }
            }),
            &context,
        )
        .expect("a bounded window expands");
        assert_eq!(
            instants(&expansion),
            [
                "2026-01-02T09:00:00Z",
                "2026-01-03T09:00:00Z",
                "2026-01-04T09:00:00Z",
                "2026-01-05T09:00:00Z",
            ]
        );

        // One second either side moves the boundary by exactly one occurrence.
        let expansion = run(
            "rule.expand",
            json!({
                "policy": "bookings",
                "rule": "FREQ=DAILY",
                "start": "2026-01-01T09:00:00",
                "window": { "from": "2026-01-02T09:00:01Z", "to": "2026-01-05T08:59:59Z" }
            }),
            &context,
        )
        .expect("a bounded window expands");
        assert_eq!(
            instants(&expansion),
            ["2026-01-03T09:00:00Z", "2026-01-04T09:00:00Z"]
        );

        // The count bound is the policy's, and it is exact: a policy admitting
        // three occurrences expands three and refuses a rule that would need
        // four.
        let three = policy_context(utc_policy("three", 3, 366 * 86_400));
        let expansion = run(
            "rule.expand",
            json!({
                "policy": "three",
                "rule": "FREQ=DAILY;COUNT=3",
                "start": "2026-01-01T09:00:00",
                "window": { "from": "2026-01-01T00:00:00Z", "to": "2026-01-10T00:00:00Z" }
            }),
            &three,
        )
        .expect("three occurrences fit a three-occurrence policy");
        assert_eq!(expansion["count"], 3);
        assert_eq!(
            run(
                "rule.expand",
                json!({
                    "policy": "three",
                    "rule": "FREQ=DAILY;COUNT=4",
                    "start": "2026-01-01T09:00:00",
                    "window": { "from": "2026-01-01T00:00:00Z", "to": "2026-01-10T00:00:00Z" }
                }),
                &three,
            )
            .expect_err("a fourth occurrence is one over the policy")
            .code(),
            "recurrence_rule_unbounded"
        );

        // The window ceiling is the policy's too, measured from the rule's own
        // start, because that is the sequence an expansion walks.
        let short = policy_context(utc_policy("short", 500, 7 * 86_400));
        let failure = run(
            "rule.expand",
            json!({
                "policy": "short",
                "rule": "FREQ=DAILY;COUNT=3",
                "start": "2026-01-01T09:00:00",
                "window": { "from": "2026-01-01T00:00:00Z", "to": "2026-02-01T00:00:00Z" }
            }),
            &short,
        )
        .expect_err("a month reaches further than a seven-day policy admits");
        assert_eq!(failure.code(), "recurrence_window_too_long");

        // Determinism: the same input twice, compared as canonical bytes. This
        // is the registration condition of ADR 044 restated where the rule is
        // the interesting part.
        let input = json!({
            "policy": "bookings",
            "rule": "FREQ=MONTHLY;BYDAY=2MO;COUNT=6",
            "start": "2026-01-01T09:00:00",
            "window": { "from": "2026-01-01T00:00:00Z", "to": "2026-12-01T00:00:00Z" }
        });
        let first = run("rule.expand", input.clone(), &context).expect("the rule expands");
        let second = run("rule.expand", input, &context).expect("the rule expands again");
        assert_eq!(
            crate::local::canonical_bytes(&first),
            crate::local::canonical_bytes(&second)
        );

        // `next` is bounded by its own count and answers from a declared
        // instant, never from the wall clock.
        let next = run(
            "rule.next",
            json!({
                "policy": "bookings",
                "rule": "FREQ=WEEKLY;BYDAY=MO",
                "start": "2026-01-05T09:00:00",
                "after": "2026-01-05T09:00:00Z",
                "count": 2
            }),
            &context,
        )
        .expect("a bounded next answers");
        assert_eq!(
            instants(&next),
            ["2026-01-12T09:00:00Z", "2026-01-19T09:00:00Z"],
            "`after` is exclusive: the occurrence at the instant asked about is behind it"
        );

        // The count a caller asks for is bounded by the policy, and the
        // refusal names both numbers.
        assert_eq!(
            run(
                "rule.next",
                json!({
                    "policy": "three",
                    "rule": "FREQ=WEEKLY;BYDAY=MO;COUNT=3",
                    "start": "2026-01-05T09:00:00",
                    "after": "2026-01-05T00:00:00Z",
                    "count": 4
                }),
                &three,
            )
            .expect_err("four is one more than the policy admits")
            .code(),
            "recurrence_count_too_large"
        );
    }

    /// Spec 021 §4 `recurrence_respects_timezone`.
    ///
    /// One rule expanded in UTC and in a local zone differs exactly as RFC 5545
    /// requires — the wall clock is held and the instant moves — and the two
    /// wall-clock times a DST transition breaks follow the declared policy,
    /// with the same spellings and the same meanings a zoned cron trigger uses
    /// (ADR 039).
    #[test]
    fn recurrence_respects_timezone() {
        let utc = policy_context(utc_policy("utc", 500, 366 * 86_400));
        let berlin = policy_context(berlin_policy(
            "berlin",
            SkippedTime::FireAfterGap,
            RepeatedTime::FireAtFirst,
        ));

        // The same rule, the same wall-clock start, across the spring
        // transition (2026-03-29 in Europe/Berlin).
        let rule = "FREQ=DAILY;COUNT=4";
        let start = "2026-03-27T09:00:00";
        let window = json!({ "from": "2026-03-01T00:00:00Z", "to": "2026-04-30T00:00:00Z" });

        let in_utc = run(
            "rule.expand",
            json!({ "policy": "utc", "rule": rule, "start": start, "window": window }),
            &utc,
        )
        .expect("the rule expands in UTC");
        assert_eq!(
            instants(&in_utc),
            [
                "2026-03-27T09:00:00Z",
                "2026-03-28T09:00:00Z",
                "2026-03-29T09:00:00Z",
                "2026-03-30T09:00:00Z",
            ],
            "in UTC the wall clock and the instant are the same thing"
        );

        let in_berlin = run(
            "rule.expand",
            json!({ "policy": "berlin", "rule": rule, "start": start, "window": window }),
            &berlin,
        )
        .expect("the rule expands in Berlin");
        assert_eq!(
            instants(&in_berlin),
            [
                "2026-03-27T08:00:00Z",
                "2026-03-28T08:00:00Z",
                "2026-03-29T07:00:00Z",
                "2026-03-30T07:00:00Z",
            ],
            "the instant moves by an hour at the transition so the wall clock does not"
        );
        assert_eq!(
            locals(&in_berlin),
            [
                "2026-03-27T09:00:00",
                "2026-03-28T09:00:00",
                "2026-03-29T09:00:00",
                "2026-03-30T09:00:00",
            ]
        );
        assert_ne!(
            instants(&in_utc),
            instants(&in_berlin),
            "one rule in two zones is two answers, and that is the point"
        );

        // --- The spring gap: local 02:30 does not exist on 2026-03-29. ---
        let gap_window = json!({ "from": "2026-03-28T00:00:00Z", "to": "2026-03-31T00:00:00Z" });
        let after_gap = run(
            "rule.expand",
            json!({
                "policy": "berlin",
                "rule": "FREQ=DAILY;COUNT=3",
                "start": "2026-03-28T02:30:00",
                "window": gap_window
            }),
            &berlin,
        )
        .expect("the rule expands");
        assert_eq!(
            instants(&after_gap),
            [
                "2026-03-28T01:30:00Z",
                "2026-03-29T01:00:00Z",
                "2026-03-30T00:30:00Z",
            ],
            "`fire_after_gap` puts the missing occurrence at the instant the gap ends"
        );
        assert_eq!(
            locals(&after_gap)[1],
            "2026-03-29T03:00:00",
            "late by the width of the gap, never lost"
        );
        assert_eq!(after_gap["skipped"].as_array().map(Vec::len), Some(0));

        // `skip` is the other declared answer: no occurrence, and the dropped
        // wall-clock time is reported rather than dropped in silence.
        let skipping = policy_context(berlin_policy(
            "berlin_skip",
            SkippedTime::Skip,
            RepeatedTime::FireAtFirst,
        ));
        let skipped = run(
            "rule.expand",
            json!({
                "policy": "berlin_skip",
                "rule": "FREQ=DAILY;COUNT=3",
                "start": "2026-03-28T02:30:00",
                "window": gap_window
            }),
            &skipping,
        )
        .expect("the rule expands");
        assert_eq!(
            instants(&skipped),
            ["2026-03-28T01:30:00Z", "2026-03-30T00:30:00Z"]
        );
        assert_eq!(
            skipped["skipped"],
            json!(["2026-03-29T02:30:00"]),
            "a dropped occurrence is auditable, never silent"
        );

        // --- The autumn overlap: local 02:30 happens twice on 2026-10-25. ---
        let overlap_window =
            json!({ "from": "2026-10-24T00:00:00Z", "to": "2026-10-27T00:00:00Z" });
        let first = run(
            "rule.expand",
            json!({
                "policy": "berlin",
                "rule": "FREQ=DAILY;COUNT=3",
                "start": "2026-10-24T02:30:00",
                "window": overlap_window
            }),
            &berlin,
        )
        .expect("the rule expands");
        assert_eq!(
            instants(&first),
            [
                "2026-10-24T00:30:00Z",
                "2026-10-25T00:30:00Z",
                "2026-10-26T01:30:00Z",
            ],
            "`fire_at_first` takes the earlier of the two instants carrying the local time"
        );

        let second_context = policy_context(berlin_policy(
            "berlin_second",
            SkippedTime::FireAfterGap,
            RepeatedTime::FireAtSecond,
        ));
        let second = run(
            "rule.expand",
            json!({
                "policy": "berlin_second",
                "rule": "FREQ=DAILY;COUNT=3",
                "start": "2026-10-24T02:30:00",
                "window": overlap_window
            }),
            &second_context,
        )
        .expect("the rule expands");
        assert_eq!(
            instants(&second),
            [
                "2026-10-24T00:30:00Z",
                "2026-10-25T01:30:00Z",
                "2026-10-26T01:30:00Z",
            ],
            "`fire_at_second` takes the later one — and exactly one of them either way"
        );
        assert_eq!(
            locals(&second)[1],
            "2026-10-25T02:30:00",
            "both policies name the same wall clock; they differ in which instant carries it"
        );
    }
    /// A policy is the deployment's promise about *when*, so the declaration
    /// refuses everything that would leave it half-stated: a zone with no DST
    /// answer, a DST answer with no zone, a zone this binary's database does
    /// not carry, a ceiling outside what the binary can hold, and two policies
    /// answering to one name.
    ///
    /// The two pairing rules are ADR 039's, verbatim: they are the same
    /// refusals `cron_triggers.yaml` earns for the same reason.
    #[test]
    fn a_policy_declaration_is_zoned_and_bounded_or_refused() {
        assert!(RecurrencePolicySet::resolve(builtin_policies()).is_ok());

        let refusal = |spec: RecurrencePolicySpec| {
            RecurrencePolicySet::resolve([spec])
                .expect_err("the declaration is not resolvable")
                .remove(0)
                .message
        };

        let mut zoned_without_dst = berlin_policy(
            "zoned",
            SkippedTime::FireAfterGap,
            RepeatedTime::FireAtFirst,
        );
        zoned_without_dst.dst = None;
        assert!(refusal(zoned_without_dst).contains("DST transition skips"));

        let mut dst_without_zone = berlin_policy(
            "unzoned",
            SkippedTime::FireAfterGap,
            RepeatedTime::FireAtFirst,
        );
        dst_without_zone.timezone = None;
        assert!(refusal(dst_without_zone).contains("would never be read"));

        let mut unknown_zone = berlin_policy(
            "unknown",
            SkippedTime::FireAfterGap,
            RepeatedTime::FireAtFirst,
        );
        unknown_zone.timezone = Some("Mars/Olympus".to_owned());
        assert!(refusal(unknown_zone).contains("not an IANA timezone"));

        assert!(refusal(utc_policy("none", 0, 86_400)).contains("one occurrence"));
        assert!(
            refusal(utc_policy("greedy", MAX_OCCURRENCES + 1, 86_400)).contains("one occurrence")
        );
        assert!(refusal(utc_policy("instant", 10, 0)).contains("positive window"));
        assert!(
            refusal(utc_policy("forever", 10, MAX_WINDOW_SECONDS + 1)).contains("positive window")
        );

        let twice = RecurrencePolicySet::resolve([
            utc_policy("one", 10, 86_400),
            utc_policy("one", 20, 86_400),
        ])
        .expect_err("two policies of one name are two answers to one question");
        assert_eq!(twice[0].message, "is declared twice");

        // A UTC policy needs no DST answer, because UTC has no transitions.
        let utc = RecurrencePolicySet::resolve([utc_policy("plain", 10, 86_400)])
            .expect("a UTC policy is complete without a DST answer");
        assert_eq!(
            utc.policy("plain").map(|policy| policy.zone()),
            Some(Tz::UTC)
        );
        assert!(utc.policy("absent").is_none());
    }

    /// The rule text is the one input a *user* may have written, so it may not
    /// reach anything ambient. `rrule` reads a `DTSTART` without a zone, and an
    /// `UNTIL` without a `Z`, in the machine's own timezone — which would make
    /// the answer depend on the container's locale and `Pure` a lie.
    #[test]
    fn a_rule_may_not_reach_the_machines_clock() {
        let context = context();
        let refusal = |rule: &str| {
            run(
                "rule.validate",
                json!({
                    "policy": "probe_utc",
                    "rule": rule,
                    "start": "2026-01-01T09:00:00"
                }),
                &context,
            )
            .expect_err("the rule is refused")
        };

        for rule in [
            "DTSTART;TZID=Europe/Berlin:20260101T090000\nRRULE:FREQ=DAILY;COUNT=3",
            "RRULE:FREQ=DAILY;COUNT=3",
            "FREQ=DAILY;COUNT=3\nRDATE:20260105T090000Z",
        ] {
            assert_eq!(refusal(rule).code(), "recurrence_rule_invalid", "{rule}");
        }
        // A floating UNTIL is the same hole with a shorter spelling.
        assert_eq!(
            refusal("FREQ=DAILY;UNTIL=20260201T000000").code(),
            "recurrence_rule_invalid"
        );
        // And a start is a wall clock, not an instant: an offset would hide
        // which local time its author meant.
        assert_eq!(
            run(
                "rule.validate",
                json!({
                    "policy": "probe_zoned",
                    "rule": "FREQ=DAILY;COUNT=3",
                    "start": "2026-01-01T09:00:00Z"
                }),
                &context,
            )
            .expect_err("an instant is not a wall clock")
            .code(),
            "recurrence_input_contract"
        );
        // A policy nobody declared is not a policy.
        assert_eq!(
            run(
                "rule.validate",
                json!({
                    "policy": "absent",
                    "rule": "FREQ=DAILY;COUNT=3",
                    "start": "2026-01-01T09:00:00"
                }),
                &context,
            )
            .expect_err("an undeclared policy is refused")
            .code(),
            "recurrence_policy_unknown"
        );
    }

    /// The admitted bound is only worth having while it is an over-count: a
    /// ceiling that an expansion can walk past is not a ceiling.
    ///
    /// So every shape the estimator reasons about — plain frequencies, an
    /// interval, the time expansions, an unprefixed weekday inside a month, a
    /// `BYSETPOS`, a `COUNT`, an `UNTIL` — is expanded for real and compared
    /// against the number it was admitted on.
    #[test]
    fn the_admitted_bound_is_never_an_under_count() {
        let context = policy_context(utc_policy("wide", MAX_OCCURRENCES, 366 * 86_400));
        let start = "2026-01-01T00:00:00";
        let window = json!({ "from": "2026-01-01T00:00:00Z", "to": "2026-12-31T00:00:00Z" });

        for rule in [
            "FREQ=DAILY",
            "FREQ=DAILY;INTERVAL=3",
            "FREQ=WEEKLY;BYDAY=MO,WE,FR",
            "FREQ=MONTHLY;BYDAY=MO",
            "FREQ=MONTHLY;BYDAY=2MO",
            "FREQ=MONTHLY;BYMONTHDAY=1,15",
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1",
            "FREQ=YEARLY;BYMONTH=1,7;BYMONTHDAY=1",
            // `BYMONTH` with nothing finer beside it. It is the one shape where
            // an expanding part is the *only* expanding part, and it is the
            // shape the list above walked around: every other yearly rule here
            // carries a day part, so the day term never had to notice the
            // month term on its own.
            "FREQ=YEARLY;BYMONTH=1,7",
            "FREQ=YEARLY;BYMONTH=3",
            "FREQ=YEARLY;BYMONTH=1,2,3,4,5,6,7,8,9,10,11,12",
            "FREQ=YEARLY;BYMONTH=1,7;BYHOUR=9,17",
            "FREQ=YEARLY;BYDAY=SU",
            "FREQ=DAILY;BYHOUR=8,12,18",
            "FREQ=HOURLY;INTERVAL=6",
            "FREQ=DAILY;COUNT=40",
            "FREQ=DAILY;UNTIL=20260401T000000Z",
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=TU;COUNT=13",
        ] {
            let admitted = run(
                "rule.validate",
                json!({ "policy": "wide", "rule": rule, "start": start }),
                &context,
            )
            .unwrap_or_else(|failure| panic!("{rule} must be admitted: {failure:?}"));
            // The expansion has to *succeed*, and that is half the assertion
            // rather than a precondition for it: `expand` refuses with
            // `recurrence_bound_exceeded` the moment it walks past the number
            // it was admitted on, so an under-counted bound shows up here as a
            // refused expansion of a perfectly bounded rule.
            let expanded = run(
                "rule.expand",
                json!({ "policy": "wide", "rule": rule, "start": start, "window": window }),
                &context,
            )
            .unwrap_or_else(|failure| {
                panic!(
                    "{rule} was admitted at {} and then refused its own expansion, which means \
                     the bound was an under-count: {failure:?}",
                    admitted["worst_case_occurrences"]
                )
            });
            let bound = admitted["worst_case_occurrences"]
                .as_u64()
                .expect("a bound is a number");
            let produced = expanded["count"].as_u64().expect("a count is a number");
            assert!(
                produced <= bound,
                "{rule} produced {produced} occurrences against a bound of {bound}"
            );
            assert!(produced > 0, "{rule} produced nothing");
        }
    }
}
