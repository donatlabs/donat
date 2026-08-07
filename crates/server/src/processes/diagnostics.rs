//! Read-only diagnostics for one durable Process instance.
//!
//! [[002-durable-process-operational-contracts]] is explicit about the shape
//! of this: "There is no generic runtime cancel, retry, replay,
//! definition-mutation API, mutating process-management operator CLI, admin
//! role, or permission bypass. Operators use deployment-owned observability
//! for the internal journal. The only CLI exceptions are `donat process
//! inspect --source <name> --instance <uuid>` and `donat process
//! verify-history --source <name> --instance <uuid>`; both are read-only
//! diagnostics and never mutate history or invoke a command or connector."
//!
//! So this module reads. Every statement here is a `SELECT`, it opens no
//! transaction that could take a lock a worker needs, and it never touches a
//! command, a connector, or the definition. An operator looking at a stuck
//! instance gets the journal as it stands; changing anything remains the job
//! of an explicit declared command, the same as for any other caller.

use anyhow::{Context, Result};
use serde_json::{Value as Json, json};
use uuid::Uuid;

/// Read everything the journal records about one instance.
pub async fn inspect(database_url: &str, source: &str, instance: Uuid) -> Result<Json> {
    let client = connect(database_url).await?;

    let instance_row = client
        .query_opt(
            "SELECT process_name, revision, status, current_state, source_request_id, \
                    start_idempotency_key, version, created_at, updated_at, \
                    input_json, state_json \
             FROM donat.process_instances WHERE source_name = $1 AND id = $2",
            &[&source, &instance],
        )
        .await
        .context("reading the Process instance")?
        .ok_or_else(|| anyhow::anyhow!("no Process instance {instance} in source {source:?}"))?;

    let events = client
        .query(
            "SELECT id, kind, status, attempts, available_at, created_at, consumed_at, \
                    idempotency_key \
             FROM donat.process_events \
             WHERE source_name = $1 AND instance_id = $2 \
             ORDER BY created_at, id",
            &[&source, &instance],
        )
        .await
        .context("reading the Process event journal")?;

    let activities = client
        .query(
            "SELECT id, state_name, logical_activity_id, connector_instance, operation, \
                    status, attempts, lease_generation, available_at, \
                    schedule_to_start_deadline, start_to_close_deadline, lease_expires_at, \
                    last_error_json, created_at, updated_at \
             FROM donat.process_activity_jobs \
             WHERE source_name = $1 AND instance_id = $2 \
             ORDER BY created_at, id",
            &[&source, &instance],
        )
        .await
        .context("reading the Process activity journal")?;

    let transitions = client
        .query(
            "SELECT id, event_id, activity_job_id, activity_attempt, \
                    activity_lease_generation, from_state, to_state, outcome, \
                    definition_revision, created_at \
             FROM donat.process_transition_logs \
             WHERE source_name = $1 AND instance_id = $2 \
             ORDER BY created_at, id",
            &[&source, &instance],
        )
        .await
        .context("reading the Process transition log")?;

    Ok(json!({
        "source": source,
        "instance": instance,
        "process": instance_row.get::<_, String>("process_name"),
        "revision": instance_row.get::<_, String>("revision"),
        "status": instance_row.get::<_, String>("status"),
        "current_state": instance_row.get::<_, String>("current_state"),
        "version": instance_row.get::<_, i64>("version"),
        "source_request_id": instance_row.get::<_, Uuid>("source_request_id"),
        "start_idempotency_key": instance_row.get::<_, String>("start_idempotency_key"),
        "created_at": timestamp(&instance_row, "created_at"),
        "updated_at": timestamp(&instance_row, "updated_at"),
        "input": instance_row.get::<_, Json>("input_json"),
        "state": instance_row.get::<_, Json>("state_json"),
        "events": events.iter().map(event_json).collect::<Vec<_>>(),
        "activities": activities.iter().map(activity_json).collect::<Vec<_>>(),
        "transitions": transitions.iter().map(transition_json).collect::<Vec<_>>(),
    }))
}

/// One finding about a history that does not hold together.
#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub detail: String,
}

/// Check that the recorded history of one instance is internally consistent.
///
/// This does not re-run anything — it reads what the journal says and asks
/// whether it can all be true at once. The checks are deliberately about the
/// journal's own invariants, because those are what a partial write, a
/// restored backup or an out-of-band `UPDATE` breaks:
///
/// * the applied transitions form one connected chain, each starting where
///   the previous one ended;
/// * that chain ends where the instance says it currently is;
/// * an instance that has finished has no work still waiting for it;
/// * every transition names the revision the instance is pinned to.
pub fn verify(history: &Json) -> Vec<Finding> {
    let mut findings = Vec::new();
    let status = history["status"].as_str().unwrap_or_default();
    let current_state = history["current_state"].as_str().unwrap_or_default();
    let revision = history["revision"].as_str().unwrap_or_default();
    let empty = Vec::new();
    let transitions = history["transitions"].as_array().unwrap_or(&empty);

    // Only transitions that moved the instance describe the chain; a refused
    // one records that nothing happened, which is not a gap.
    let applied: Vec<&Json> = transitions
        .iter()
        .filter(|transition| transition["to_state"].is_string())
        .collect();

    let mut previous: Option<&str> = None;
    for transition in &applied {
        let from = transition["from_state"].as_str();
        let to = transition["to_state"].as_str().unwrap_or_default();
        if let (Some(previous), Some(from)) = (previous, from)
            && previous != from
        {
            findings.push(Finding {
                code: "broken-chain",
                detail: format!(
                    "transition {} starts at {from:?} but the previous one ended at {previous:?}",
                    transition["id"]
                ),
            });
        }
        if transition["definition_revision"].as_str() != Some(revision) {
            findings.push(Finding {
                code: "revision-drift",
                detail: format!(
                    "transition {} was applied under revision {} but the instance is pinned to {revision:?}",
                    transition["id"], transition["definition_revision"]
                ),
            });
        }
        previous = Some(to);
    }

    if let Some(last) = previous
        && last != current_state
    {
        findings.push(Finding {
            code: "state-mismatch",
            detail: format!(
                "the last applied transition ended at {last:?} but the instance reports {current_state:?}"
            ),
        });
    }

    if matches!(status, "terminal" | "cancelled") {
        let pending_events = history["events"]
            .as_array()
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event["status"] == "pending")
                    .count()
            })
            .unwrap_or(0);
        if pending_events > 0 {
            findings.push(Finding {
                code: "work-after-end",
                detail: format!(
                    "the instance is {status} but {pending_events} event(s) are still pending"
                ),
            });
        }
        let live_activities = history["activities"]
            .as_array()
            .map(|activities| {
                activities
                    .iter()
                    .filter(|activity| {
                        matches!(activity["status"].as_str(), Some("scheduled" | "running"))
                    })
                    .count()
            })
            .unwrap_or(0);
        if live_activities > 0 {
            findings.push(Finding {
                code: "work-after-end",
                detail: format!(
                    "the instance is {status} but {live_activities} activity job(s) are still live"
                ),
            });
        }
    }

    findings
}

async fn connect(database_url: &str) -> Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(database_url, crate::pgtls::connector())
        .await
        .context("connecting to the source database")?;
    // Nothing here outlives the command, so the driver task is simply left to
    // run until the process exits.
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

fn timestamp(row: &tokio_postgres::Row, column: &str) -> Json {
    row.get::<_, Option<chrono::DateTime<chrono::Utc>>>(column)
        .map(|at| json!(at.to_rfc3339()))
        .unwrap_or(Json::Null)
}

fn event_json(row: &tokio_postgres::Row) -> Json {
    json!({
        "id": row.get::<_, Uuid>("id"),
        "kind": row.get::<_, String>("kind"),
        "status": row.get::<_, String>("status"),
        "attempts": row.get::<_, i32>("attempts"),
        "idempotency_key": row.get::<_, Option<String>>("idempotency_key"),
        "available_at": timestamp(row, "available_at"),
        "created_at": timestamp(row, "created_at"),
        "consumed_at": timestamp(row, "consumed_at"),
    })
}

fn activity_json(row: &tokio_postgres::Row) -> Json {
    json!({
        "id": row.get::<_, Uuid>("id"),
        "state": row.get::<_, String>("state_name"),
        "logical_activity_id": row.get::<_, String>("logical_activity_id"),
        "connector": row.get::<_, String>("connector_instance"),
        "operation": row.get::<_, String>("operation"),
        "status": row.get::<_, String>("status"),
        "attempts": row.get::<_, i32>("attempts"),
        "lease_generation": row.get::<_, i64>("lease_generation"),
        "available_at": timestamp(row, "available_at"),
        "schedule_to_start_deadline": timestamp(row, "schedule_to_start_deadline"),
        "start_to_close_deadline": timestamp(row, "start_to_close_deadline"),
        "lease_expires_at": timestamp(row, "lease_expires_at"),
        "last_error": row.get::<_, Option<Json>>("last_error_json"),
        "created_at": timestamp(row, "created_at"),
        "updated_at": timestamp(row, "updated_at"),
    })
}

fn transition_json(row: &tokio_postgres::Row) -> Json {
    json!({
        "id": row.get::<_, Uuid>("id"),
        "event_id": row.get::<_, Option<Uuid>>("event_id"),
        "activity_job_id": row.get::<_, Option<Uuid>>("activity_job_id"),
        "activity_attempt": row.get::<_, Option<i32>>("activity_attempt"),
        "activity_lease_generation": row.get::<_, Option<i64>>("activity_lease_generation"),
        "from_state": row.get::<_, Option<String>>("from_state"),
        "to_state": row.get::<_, Option<String>>("to_state"),
        "outcome": row.get::<_, String>("outcome"),
        "definition_revision": row.get::<_, String>("definition_revision"),
        "created_at": timestamp(row, "created_at"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(transitions: Json, status: &str, current_state: &str) -> Json {
        json!({
            "status": status,
            "current_state": current_state,
            "revision": "rev-1",
            "events": [],
            "activities": [],
            "transitions": transitions,
        })
    }

    fn applied(id: &str, from: Option<&str>, to: &str) -> Json {
        json!({
            "id": id,
            "from_state": from,
            "to_state": to,
            "outcome": "applied",
            "definition_revision": "rev-1",
        })
    }

    #[test]
    fn a_connected_history_has_no_findings() {
        let history = history(
            json!([
                applied("t1", None, "authorizing"),
                applied("t2", Some("authorizing"), "fulfilling"),
            ]),
            "running",
            "fulfilling",
        );
        assert_eq!(verify(&history), Vec::new());
    }

    /// A gap between two transitions means a state change nobody recorded.
    #[test]
    fn a_gap_in_the_chain_is_reported() {
        let history = history(
            json!([
                applied("t1", None, "authorizing"),
                applied("t2", Some("shipped"), "closed"),
            ]),
            "running",
            "closed",
        );
        let findings = verify(&history);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "broken-chain");
    }

    /// The chain has to end where the instance says it is.
    #[test]
    fn a_current_state_off_the_chain_is_reported() {
        let history = history(
            json!([applied("t1", None, "authorizing")]),
            "running",
            "fulfilling",
        );
        let findings = verify(&history);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "state-mismatch");
    }

    /// An instance pinned to one revision cannot have been advanced under
    /// another: that is the pinning contract, and a rolling deployment that
    /// broke it is exactly what this command is for.
    #[test]
    fn a_transition_under_another_revision_is_reported() {
        let mut transition = applied("t1", None, "authorizing");
        transition["definition_revision"] = json!("rev-2");
        let findings = verify(&history(json!([transition]), "running", "authorizing"));
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, "revision-drift");
    }

    /// A refused transition records that nothing happened; it is not a gap in
    /// the chain and must not be reported as one.
    #[test]
    fn a_refused_transition_does_not_break_the_chain() {
        let history = history(
            json!([
                applied("t1", None, "authorizing"),
                json!({
                    "id": "t2",
                    "from_state": "authorizing",
                    "to_state": Json::Null,
                    "outcome": "guard_false",
                    "definition_revision": "rev-1",
                }),
                applied("t3", Some("authorizing"), "fulfilling"),
            ]),
            "running",
            "fulfilling",
        );
        assert_eq!(verify(&history), Vec::new());
    }

    /// An instance that has finished must not still have work queued for it.
    #[test]
    fn work_left_after_the_end_is_reported() {
        let mut history = history(json!([applied("t1", None, "closed")]), "terminal", "closed");
        history["events"] = json!([{ "status": "pending" }]);
        history["activities"] = json!([{ "status": "running" }]);
        let findings = verify(&history);
        assert_eq!(findings.len(), 2);
        assert!(
            findings
                .iter()
                .all(|finding| finding.code == "work-after-end")
        );
    }
}
