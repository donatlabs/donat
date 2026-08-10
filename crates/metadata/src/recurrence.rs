//! Recurrence policies as deployment metadata (spec 021 §3).
//!
//! `local.recurrence` expands RFC 5545 rules. A rule is data — a booking, a
//! shift, a lesson, written by whoever set the schedule up — but the three
//! things that decide what a rule *means* and what it may cost are not:
//!
//! - the zone its wall-clock times are read in, and what it does at the local
//!   time a DST transition skips and at the one it repeats;
//! - the most occurrences one expansion may produce;
//! - the furthest an expansion may reach from a rule's start.
//!
//! All three are declared here, in `recurrence.yaml`, and a running process
//! chooses only *which* policy applies and what rule goes in it. That is the
//! separation spec 019 drew for document templates and spec 022 for media
//! declarations: the part of the path the party supplying the value does not
//! control is the part that has to be a declaration.
//!
//! The DST half is deliberately not a second vocabulary. A recurrence policy
//! declares [`CronDstPolicy`] — the same type, the same two spellings, and the
//! same meanings a zoned cron trigger declares
//! (`knowledgebase/declarative-saas/decisions/039-*`), because "what does
//! 02:30 mean on the night it does not exist" is one question and a deployment
//! should answer it once.
//!
//! The bound half is why an unbounded rule is refused where it is declared
//! rather than where it runs. Whether `FREQ=SECONDLY` with neither `UNTIL` nor
//! `COUNT` fits is a question about the rule *and* the ceilings above; this
//! file is where the ceilings come from, and the engine answers the question
//! from arithmetic before it generates an occurrence.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::types::{
    CronDstPolicy, Metadata, Process, ProcessForEachState, ProcessStateOperation, ProcessValue,
};

/// The connector name every recurrence operation is reached through.
pub const RECURRENCE_CAPABILITY: &str = "local.recurrence";

/// The operations the capability advertises. Declared here so `validate` can
/// refuse a policy pointed at an operation that would never read it, without
/// the metadata crate depending on the connector crate.
pub const RECURRENCE_OPERATIONS: &[&str] = &["rule.validate", "rule.expand", "rule.next"];

/// The largest `max_occurrences` this engine will hold for one expansion. It is
/// the compiled capability's `max_units`; a policy over it would declare an
/// expansion the bound layer refuses anyway.
pub const MAX_DECLARABLE_OCCURRENCES: u64 = 10_000;

/// The furthest an expansion may reach from a rule's start. Ten years of wall
/// clock; anything past it is a report, not a schedule.
pub const MAX_DECLARABLE_WINDOW_SECONDS: u64 = 10 * 366 * 86_400;

/// Everything `recurrence.yaml` declares.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecurrenceMetadata {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<RecurrencePolicy>,
}

impl RecurrenceMetadata {
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    pub fn policy(&self, name: &str) -> Option<&RecurrencePolicy> {
        self.policies.iter().find(|policy| policy.name == name)
    }
}

/// One declared recurrence policy.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecurrencePolicy {
    pub name: String,
    /// IANA zone the rules expanded under this policy are read in. Absent means
    /// UTC — where no local time is ever missing or repeated, which is why it
    /// is also the only case that needs no `dst`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// What this policy does at the wall-clock times a DST transition breaks.
    /// Required with `timezone`, refused without it — the same rule, and the
    /// same type, a zoned cron trigger carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dst: Option<CronDstPolicy>,
    /// The most occurrences one expansion under this policy may produce, and
    /// the number a rule's worst case is admitted against.
    pub max_occurrences: u64,
    /// The furthest an expansion may reach from a rule's start, as a duration
    /// (`30d`, `52w`, `8760h`).
    pub max_window: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// One refusal, naming the exact metadata path that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceDeclarationError {
    pub path: String,
    pub message: String,
}

impl RecurrenceDeclarationError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RecurrenceDeclarationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.path, self.message)
    }
}

/// Parse a declared window duration into seconds.
///
/// The grammar is the durable-process one plus `w`, because a recurrence window
/// is naturally spelled in weeks. It is deliberately small: a window is a
/// number and a unit, not an expression.
pub fn parse_window_seconds(source: &str) -> Option<u64> {
    let (digits, multiplier) = [
        ("s", 1_u64),
        ("m", 60),
        ("h", 3_600),
        ("d", 86_400),
        ("w", 604_800),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| Some((source.strip_suffix(suffix)?, multiplier)))?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

/// Every recurrence rule, applied to one deployment's metadata.
pub fn validate_recurrence_declarations(metadata: &Metadata) -> Vec<RecurrenceDeclarationError> {
    let mut errors = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for (index, policy) in metadata.recurrence.policies.iter().enumerate() {
        let path = format!("recurrence.policies[{index}]");
        if !names.insert(policy.name.as_str()) {
            errors.push(RecurrenceDeclarationError::new(
                format!("{path}.name"),
                format!("recurrence policy `{}` is declared twice", policy.name),
            ));
        }
        validate_policy(policy, &path, &mut errors);
    }
    for process in &metadata.processes {
        validate_process(process, metadata, &mut errors);
    }
    errors
}

fn validate_policy(
    policy: &RecurrencePolicy,
    path: &str,
    errors: &mut Vec<RecurrenceDeclarationError>,
) {
    if policy.name.is_empty()
        || !policy
            .name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        errors.push(RecurrenceDeclarationError::new(
            format!("{path}.name"),
            "a recurrence policy name is alphanumeric with `_` or `-`",
        ));
    }

    // The pairing rule of ADR 039, on a second declaration that asks the same
    // question. A zone with no answer would fire at a time nobody chose; an
    // answer with no zone is a declaration nothing reads (ADR 034).
    match (&policy.timezone, &policy.dst) {
        (Some(zone), Some(_)) => {
            if zone.parse::<chrono_tz::Tz>().is_err() {
                errors.push(RecurrenceDeclarationError::new(
                    format!("{path}.timezone"),
                    format!(
                        "`{zone}` is not an IANA timezone name (for example Europe/Berlin, UTC)"
                    ),
                ));
            }
        }
        (Some(zone), None) => errors.push(RecurrenceDeclarationError::new(
            format!("{path}.dst"),
            format!(
                "recurrence policy `{}` is declared in timezone `{zone}` but has no `dst` \
                 policies; a wall-clock recurrence must say what it does at the local time a DST \
                 transition skips and at the one it repeats",
                policy.name
            ),
        )),
        (None, Some(_)) => errors.push(RecurrenceDeclarationError::new(
            format!("{path}.dst"),
            format!(
                "recurrence policy `{}` declares `dst` policies but no `timezone`; a UTC \
                 recurrence has no DST transitions, so the policies would never be read",
                policy.name
            ),
        )),
        (None, None) => {}
    }

    if policy.max_occurrences == 0 || policy.max_occurrences > MAX_DECLARABLE_OCCURRENCES {
        errors.push(RecurrenceDeclarationError::new(
            format!("{path}.max_occurrences"),
            format!(
                "a recurrence policy admits between 1 and {MAX_DECLARABLE_OCCURRENCES} \
                 occurrences per expansion"
            ),
        ));
    }
    match parse_window_seconds(&policy.max_window) {
        Some(seconds) if seconds > 0 && seconds <= MAX_DECLARABLE_WINDOW_SECONDS => {}
        Some(_) => errors.push(RecurrenceDeclarationError::new(
            format!("{path}.max_window"),
            format!(
                "a recurrence window is between one second and {MAX_DECLARABLE_WINDOW_SECONDS} \
                 seconds"
            ),
        )),
        None => errors.push(RecurrenceDeclarationError::new(
            format!("{path}.max_window"),
            format!(
                "`{}` is not a duration; a recurrence window is a positive integer and one of \
                 `s`, `m`, `h`, `d`, `w`",
                policy.max_window
            ),
        )),
    }
}

fn validate_process(
    process: &Process,
    metadata: &Metadata,
    errors: &mut Vec<RecurrenceDeclarationError>,
) {
    for (index, state) in process.states.iter().enumerate() {
        let path = format!("processes.{}.states[{index}]", process.name);
        match &state.operation {
            ProcessStateOperation::Request { request } => validate_request(
                &request.connector,
                &request.operation,
                &request.input,
                &format!("{path}.request"),
                metadata,
                errors,
            ),
            ProcessStateOperation::ForEach { for_each } => {
                if let ProcessForEachState::Request { request, .. } = for_each.as_ref() {
                    validate_request(
                        &request.connector,
                        &request.operation,
                        &request.input,
                        &format!("{path}.for_each.request"),
                        metadata,
                        errors,
                    );
                }
            }
            _ => {}
        }
    }
}

fn validate_request(
    connector: &str,
    operation: &str,
    input: &std::collections::BTreeMap<String, ProcessValue>,
    path: &str,
    metadata: &Metadata,
    errors: &mut Vec<RecurrenceDeclarationError>,
) {
    if connector != RECURRENCE_CAPABILITY {
        return;
    }
    if !RECURRENCE_OPERATIONS.contains(&operation) {
        errors.push(RecurrenceDeclarationError::new(
            format!("{path}.operation"),
            format!("`{RECURRENCE_CAPABILITY}` has no operation `{operation}`"),
        ));
    }
    // The policy is selected by a literal name from this deployment's
    // declarations. A run that could compute the name would be choosing its own
    // DST answer and its own ceiling, which is the whole thing the declaration
    // exists to take away from it.
    match input.get("policy") {
        Some(ProcessValue::Literal {
            literal: JsonValue::String(name),
        }) => {
            if metadata.recurrence.policy(name).is_none() {
                errors.push(RecurrenceDeclarationError::new(
                    format!("{path}.input.policy"),
                    format!("recurrence policy `{name}` is not declared by this deployment"),
                ));
            }
        }
        Some(_) => errors.push(RecurrenceDeclarationError::new(
            format!("{path}.input.policy"),
            "a recurrence policy is selected by a literal name from this deployment's \
             declarations, not by a computed value",
        )),
        None => errors.push(RecurrenceDeclarationError::new(
            format!("{path}.input.policy"),
            "this activity selects its recurrence policy with `policy`",
        )),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn metadata(value: JsonValue) -> Metadata {
        serde_json::from_value(value).expect("test metadata deserializes")
    }

    fn messages(value: JsonValue) -> String {
        validate_recurrence_declarations(&metadata(value))
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn policies(policies: JsonValue) -> JsonValue {
        json!({ "version": 3, "recurrence": { "policies": policies } })
    }

    /// A policy states its zone and its two DST answers together, or it does
    /// not state a zone at all — the same pairing, with the same spellings, a
    /// zoned cron trigger has (ADR 039). And it states both ceilings, because
    /// "every expansion is bounded by a window and a count" is only true while
    /// somebody wrote both numbers down.
    #[test]
    fn a_recurrence_policy_declares_its_zone_and_its_bounds() {
        assert_eq!(
            messages(policies(json!([{
                "name": "booking",
                "timezone": "Europe/Berlin",
                "dst": { "skipped_time": "fire_after_gap", "repeated_time": "fire_at_first" },
                "max_occurrences": 500,
                "max_window": "52w"
            }]))),
            "",
            "a zoned policy with both answers and both ceilings is complete"
        );
        assert_eq!(
            messages(policies(json!([{
                "name": "utc_only",
                "max_occurrences": 10,
                "max_window": "30d"
            }]))),
            "",
            "UTC has no transitions, so it needs no answer for them"
        );

        let refused = messages(policies(json!([{
            "name": "zoned",
            "timezone": "Europe/Berlin",
            "max_occurrences": 10,
            "max_window": "30d"
        }])));
        assert!(
            refused.contains("recurrence.policies[0].dst:") && refused.contains("skips"),
            "a zone with no DST answer is refused on its own path: {refused}"
        );

        let refused = messages(policies(json!([{
            "name": "unzoned",
            "dst": { "skipped_time": "skip", "repeated_time": "fire_at_second" },
            "max_occurrences": 10,
            "max_window": "30d"
        }])));
        assert!(
            refused.contains("would never be read"),
            "an answer nothing reads is a declaration the runtime ignores: {refused}"
        );

        assert!(
            messages(policies(json!([{
                "name": "mars",
                "timezone": "Mars/Olympus",
                "dst": { "skipped_time": "skip", "repeated_time": "fire_at_first" },
                "max_occurrences": 10,
                "max_window": "30d"
            }])))
            .contains("is not an IANA timezone name")
        );

        for (max_occurrences, max_window, expected) in [
            (0, "30d", "occurrences per expansion"),
            (
                MAX_DECLARABLE_OCCURRENCES + 1,
                "30d",
                "occurrences per expansion",
            ),
            (10, "0d", "between one second"),
            (10, "600w", "between one second"),
            (10, "soon", "is not a duration"),
            (10, "30", "is not a duration"),
        ] {
            let refused = messages(policies(json!([{
                "name": "bounded",
                "max_occurrences": max_occurrences,
                "max_window": max_window
            }])));
            assert!(
                refused.contains(expected),
                "{max_occurrences}/{max_window} must be refused with `{expected}`: {refused}"
            );
        }

        assert!(
            messages(policies(json!([
                { "name": "one", "max_occurrences": 10, "max_window": "30d" },
                { "name": "one", "max_occurrences": 20, "max_window": "60d" }
            ])))
            .contains("is declared twice"),
            "two policies of one name are two answers to one question"
        );
        assert!(
            messages(policies(json!([{
                "name": "not a name",
                "max_occurrences": 10,
                "max_window": "30d"
            }])))
            .contains("alphanumeric")
        );
    }

    /// An activity expands under a policy this deployment declared, named
    /// literally. A computed name would let a run choose its own DST answer and
    /// its own ceiling one expansion at a time.
    #[test]
    fn an_activity_expands_under_a_declared_policy() {
        assert_eq!(messages(process_with(json!({ "literal": "booking" }))), "");

        assert!(
            messages(process_with(json!({ "literal": "absent" })))
                .contains("recurrence policy `absent` is not declared by this deployment")
        );
        assert!(
            messages(process_with(
                json!({ "state": "chosen", "field": "policy" })
            ))
            .contains("not by a computed value")
        );

        let mut without = process_with(json!({ "literal": "booking" }));
        without["processes"][0]["states"][0]["request"]["input"]
            .as_object_mut()
            .expect("the input is an object")
            .remove("policy");
        assert!(messages(without).contains("selects its recurrence policy with `policy`"));

        // An operation the capability does not have is refused here too, so a
        // deployment learns it from `validate` rather than from the first run.
        let mut wrong = process_with(json!({ "literal": "booking" }));
        wrong["processes"][0]["states"][0]["request"]["operation"] = json!("rule.explode");
        assert!(messages(wrong).contains("has no operation `rule.explode`"));
    }

    /// The window grammar, which is the only place a number in this file is
    /// spelled as text.
    #[test]
    fn a_window_is_an_integer_and_a_unit() {
        assert_eq!(parse_window_seconds("90s"), Some(90));
        assert_eq!(parse_window_seconds("30m"), Some(1_800));
        assert_eq!(parse_window_seconds("12h"), Some(43_200));
        assert_eq!(parse_window_seconds("366d"), Some(31_622_400));
        assert_eq!(parse_window_seconds("52w"), Some(31_449_600));
        for refused in ["", "d", "-1d", "1.5d", "1 d", "30", "30x", "30D"] {
            assert_eq!(parse_window_seconds(refused), None, "{refused}");
        }
    }

    /// One process, with the capability enabled and one recurrence activity.
    fn process_with(policy: JsonValue) -> JsonValue {
        json!({
            "version": 3,
            "recurrence": { "policies": [{
                "name": "booking",
                "timezone": "Europe/Berlin",
                "dst": { "skipped_time": "fire_after_gap", "repeated_time": "fire_at_first" },
                "max_occurrences": 500,
                "max_window": "52w"
            }]},
            "connectors": [{
                "name": RECURRENCE_CAPABILITY,
                "module": RECURRENCE_CAPABILITY,
                "operations": [{ "name": "rule.expand" }]
            }],
            "processes": [{
                "name": "schedule",
                "kind": "process",
                "version": 1,
                "source": "default",
                "start_at": "expand",
                "states": [{
                    "id": "expand",
                    "request": {
                        "connector": RECURRENCE_CAPABILITY,
                        "operation": "rule.expand",
                        "input": {
                            "policy": policy,
                            "rule": { "literal": "FREQ=WEEKLY;BYDAY=MO" },
                            "start": { "literal": "2026-01-05T09:00:00" }
                        },
                        "timeout": { "schedule_to_start": "10s", "start_to_close": "20s" },
                        "retry": {
                            "retry_on": ["timeout"], "max_attempts": 1,
                            "initial_interval": "1s", "max_interval": "5s", "jitter": "1s"
                        },
                        "next": "done"
                    }
                }]
            }]
        })
    }
}
