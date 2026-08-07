//! One-state-at-a-time execution for deterministic Process states.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, NaiveDateTime, Utc};
use donat_ir::CommandMutation;
use donat_metadata::ProcessErrorKind;
use donat_schema::Session;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio_postgres::{Row, Transaction};
use uuid::Uuid;

use crate::connectors::canonical_json_sha256;

use super::start::typed_value;
use super::value::{ProcessValueContext, evaluate_process_value, evaluate_process_values};
use super::{
    CompiledProcessCommandRole, CompiledProcessCommandState, CompiledProcessDefinition,
    CompiledProcessFailState, CompiledProcessForEachActivity, CompiledProcessForEachState,
    CompiledProcessOutputState, CompiledProcessRequestState, CompiledProcessSignalDeadline,
    CompiledProcessStateOperation, CompiledProcessTimestampKind, CompiledProcessWaitState,
    CompiledProcessWhenPredicate, CompiledProcessWhenState, InFlightGuard, ProcessCommandOutcome,
    ProcessRuntime, execute_process_command_in_savepoint,
};
use crate::commands::CommandBusinessRejection;

const PREPARATION_BATCH_SIZE: i64 = 128;

/// How long a transient failure waits before its first retry, and the ceiling
/// that exponential doubling reaches. Engine-level rather than declared: a
/// transition is not an activity, and nothing in the metadata describes how
/// long a deadlock should keep an instance out of the queue.
const TRANSITION_RETRY_INITIAL_MS: u64 = 50;
const TRANSITION_RETRY_MAXIMUM_MS: u64 = 30_000;

/// How many transient failures one event may take before the instance is ended
/// rather than retried without end.
const TRANSITION_RETRY_MAX_ATTEMPTS: i32 = 12;

#[derive(Debug, Clone, PartialEq)]
pub enum TransitionConsumption {
    NoWork,
    /// A transition that failed for a transient reason. The event carries its
    /// attempt count and comes back later; the queue moves on meanwhile.
    Deferred {
        instance_id: Uuid,
        event_id: Uuid,
        attempts: i32,
        delay_milliseconds: u64,
    },
    /// A Command the database refused unrecoverably. The instance is failed;
    /// the queue moves on.
    CommandFailed {
        instance_id: Uuid,
        event_id: Uuid,
        code: &'static str,
    },
    Advanced {
        instance_id: Uuid,
        event_id: Uuid,
        from_state: String,
        to_state: String,
    },
    Completed {
        instance_id: Uuid,
        event_id: Uuid,
        state: String,
    },
    Failed {
        instance_id: Uuid,
        event_id: Uuid,
        state: String,
        code: String,
    },
    CommandRejected {
        instance_id: Uuid,
        event_id: Uuid,
        error: CommandBusinessRejection,
    },
    ActivityScheduled {
        instance_id: Uuid,
        event_id: Uuid,
        activity_job_id: Uuid,
        state: String,
    },
    FanOutExpanded {
        instance_id: Uuid,
        event_id: Uuid,
        state: String,
        item_count: usize,
        scheduled_count: usize,
    },
    FanOutItemCompleted {
        instance_id: Uuid,
        event_id: Uuid,
        state: String,
        ordinal: i32,
    },
    WaitEntered {
        instance_id: Uuid,
        event_id: Uuid,
        timer_event_id: Uuid,
        state: String,
    },
}

struct TransitionSnapshot {
    event_id: Uuid,
    event_kind: String,
    event_payload: Json,
    instance_id: Uuid,
    process_name: String,
    revision: String,
    current_state: String,
    input: Json,
    state: Json,
    version: i64,
    caller_role: Option<String>,
    caller_session: Option<Json>,
    workflow_time: Json,
    /// Transient failures this event has already survived.
    attempts: i32,
}

struct PreparedCommandTransition {
    snapshot: TransitionSnapshot,
    definition: Arc<CompiledProcessDefinition>,
    state: CompiledProcessCommandState,
    command: CommandMutation,
}

struct PreparedWhenTransition {
    snapshot: TransitionSnapshot,
    next: String,
    output: Json,
    redacted_context: Json,
}

struct PreparedRequestTransition {
    snapshot: TransitionSnapshot,
    state: CompiledProcessRequestState,
    input: Json,
    request_fingerprint: String,
    serialization_key_hash: Option<Vec<u8>>,
}

#[derive(Clone)]
struct PreparedActivityInput {
    input: Json,
    request_fingerprint: String,
    serialization_key_hash: Option<Vec<u8>>,
}

struct PreparedFanOutItem {
    ordinal: i32,
    item_key: String,
    item_key_identity: String,
    item: Json,
    request: Option<PreparedActivityInput>,
}

struct PreparedFanOutExpansion {
    snapshot: TransitionSnapshot,
    definition: Arc<CompiledProcessDefinition>,
    state: CompiledProcessForEachState,
    items: Vec<PreparedFanOutItem>,
}

struct PreparedFanOutFailure {
    snapshot: TransitionSnapshot,
    code: &'static str,
    message: &'static str,
}

struct PreparedFanOutCommandItem {
    snapshot: TransitionSnapshot,
    definition: Arc<CompiledProcessDefinition>,
    state: CompiledProcessForEachState,
    ordinal: i32,
    item_key: String,
    item_key_identity: String,
    item: Json,
    command: CommandMutation,
}

struct PreparedFanOutRequestCompletion {
    snapshot: TransitionSnapshot,
    definition: Arc<CompiledProcessDefinition>,
    state: CompiledProcessForEachState,
    ordinal: i32,
    item_key: String,
    item_key_identity: String,
    item: Json,
    activity_job_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    result: Result<Json, Json>,
}

struct FanOutItemFailure {
    output: Json,
    route: String,
    error_kind: String,
}

struct PreparedRequestSuccessTransition {
    snapshot: TransitionSnapshot,
    state: CompiledProcessRequestState,
    activity_job_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    output: Json,
}

struct PreparedRequestFailureTransition {
    snapshot: TransitionSnapshot,
    activity_job_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    next: String,
    error_kind: String,
}

struct PreparedOutputTransition {
    snapshot: TransitionSnapshot,
    output: Json,
}

struct PreparedFailTransition {
    snapshot: TransitionSnapshot,
    state: CompiledProcessFailState,
}

enum PreparedTimerSchedule {
    AfterMilliseconds(i64),
    At(DateTime<Utc>),
}

struct PreparedWaitEntry {
    snapshot: TransitionSnapshot,
    schedule: PreparedTimerSchedule,
    timer_payload: Json,
    /// The wait declared `persist_before_match`, so a signal already recorded
    /// as unmatched is offered to it again the moment it becomes receptive.
    persist_before_match: bool,
}

struct PreparedWaitCompletion {
    snapshot: TransitionSnapshot,
    next: String,
    output: Option<Json>,
    outcome: &'static str,
    redacted_context: Json,
}

enum PreparedTransition {
    Command(PreparedCommandTransition),
    Request(PreparedRequestTransition),
    RequestSuccess(PreparedRequestSuccessTransition),
    RequestFailure(PreparedRequestFailureTransition),
    When(PreparedWhenTransition),
    Output(PreparedOutputTransition),
    Fail(PreparedFailTransition),
    WaitEntry(PreparedWaitEntry),
    WaitCompletion(PreparedWaitCompletion),
    FanOutExpansion(PreparedFanOutExpansion),
    FanOutFailure(PreparedFanOutFailure),
    FanOutCommandItem(Box<PreparedFanOutCommandItem>),
    FanOutRequestCompletion(PreparedFanOutRequestCompletion),
}

impl PreparedTransition {
    fn snapshot(&self) -> &TransitionSnapshot {
        match self {
            Self::Command(prepared) => &prepared.snapshot,
            Self::Request(prepared) => &prepared.snapshot,
            Self::RequestSuccess(prepared) => &prepared.snapshot,
            Self::RequestFailure(prepared) => &prepared.snapshot,
            Self::When(prepared) => &prepared.snapshot,
            Self::Output(prepared) => &prepared.snapshot,
            Self::Fail(prepared) => &prepared.snapshot,
            Self::WaitEntry(prepared) => &prepared.snapshot,
            Self::WaitCompletion(prepared) => &prepared.snapshot,
            Self::FanOutExpansion(prepared) => &prepared.snapshot,
            Self::FanOutFailure(prepared) => &prepared.snapshot,
            Self::FanOutCommandItem(prepared) => &prepared.snapshot,
            Self::FanOutRequestCompletion(prepared) => &prepared.snapshot,
        }
    }
}

impl ProcessRuntime {
    /// Consume one due deterministic state token.
    ///
    /// Pure binding evaluation, rule evaluation, and command planning happen
    /// from an optimistic immutable snapshot before a transaction opens. The
    /// short transaction locks the exact event/instance pair, verifies the
    /// version, and commits one state transition.
    pub async fn consume_one_transition(&self) -> anyhow::Result<TransitionConsumption> {
        self.consume_one_transition_kind(None).await
    }

    /// Step a transiently failing transition aside.
    ///
    /// The event keeps its place in the journal but stops being due: its
    /// attempt count rises and its next attempt moves into the future, so the
    /// worker that was holding it goes back for other instances instead of
    /// re-reading the same failure. This is what keeps one locked table, one
    /// deadlock or one starved pool local to the instance that hit it.
    ///
    /// Past the attempt ceiling the instance ends rather than retrying without
    /// end — a durable retry that never gives up is indistinguishable from a
    /// deployment that has stopped.
    async fn defer_transition(
        &self,
        failing: FailingTransition,
        error: &anyhow::Error,
        held: Option<&mut deadpool_postgres::Client>,
    ) -> anyhow::Result<TransitionConsumption> {
        let attempts = failing.attempts.saturating_add(1);
        let delay_milliseconds = transition_retry_delay_ms(failing.event_id, attempts);
        let delay = i64::try_from(delay_milliseconds).unwrap_or(i64::MAX) as f64;
        let reschedule = "
            UPDATE donat.process_events
            SET attempts = attempts + 1,
                available_at = statement_timestamp()
                    + make_interval(secs => $3::double precision / 1000.0)
            WHERE source_name = $1
              AND id = $2
              AND status = 'pending'
            RETURNING attempts
            ";
        let parameters: [&(dyn tokio_postgres::types::ToSql + Sync); 3] =
            [&self.source_name, &failing.event_id, &delay];

        // Prefer the connection this transition already holds. Its transaction
        // is poisoned and rolled back, but the connection itself is usually
        // fine — and a starved pool is one of the transient failures being
        // deferred, so asking the pool for another one is asking for the
        // condition that caused this.
        let recorded = match &held {
            Some(client) => client.query_opt(reschedule, &parameters).await,
            None => {
                let client = self
                    .pool
                    .get()
                    .await
                    .context("deferring a transient Process transition failure")?;
                client.query_opt(reschedule, &parameters).await
            }
        }
        .context("rescheduling a transient Process transition failure")?;

        // Nothing to reschedule: another worker consumed the event while this
        // one was failing over it. That is progress, but not this worker's.
        let Some(recorded) = recorded else {
            return Ok(TransitionConsumption::NoWork);
        };
        // The count comes back from the row rather than from the snapshot, so
        // two deployments failing the same event cannot lose an increment
        // between them — and the give-up point is decided on what is durable.
        let attempts: i32 = recorded.get("attempts");
        if attempts >= TRANSITION_RETRY_MAX_ATTEMPTS {
            return self
                .fail_instance(failing, "transition_retry_exhausted", error, held)
                .await;
        }
        tracing::warn!(
            source = %self.source_name,
            instance_id = %failing.instance_id,
            event_id = %failing.event_id,
            state = %failing.current_state,
            attempts,
            delay_milliseconds,
            error = format_args!("{error:#}"),
            "Process transition deferred after a transient failure"
        );
        Ok(TransitionConsumption::Deferred {
            instance_id: failing.instance_id,
            event_id: failing.event_id,
            attempts,
            delay_milliseconds,
        })
    }

    /// End an instance whose transition cannot be applied, in a transaction of
    /// its own: the one that failed is already poisoned.
    /// `held` is the connection the caller is already using, when it has one.
    /// Taking a second while the first is still checked out is what
    /// `defer_transition` documents and avoids: with the worker pool at half
    /// the pool size, a burst of simultaneous failures can hold every
    /// connection and then block waiting for one.
    async fn fail_instance(
        &self,
        failing: FailingTransition,
        code: &'static str,
        error: &anyhow::Error,
        held: Option<&mut deadpool_postgres::Client>,
    ) -> anyhow::Result<TransitionConsumption> {
        tracing::error!(
            source = %self.source_name,
            instance_id = %failing.instance_id,
            state = %failing.current_state,
            code,
            error = format_args!("{error:#}"),
            "Process transition failed unrecoverably; failing the instance"
        );
        let mut owned;
        let client = match held {
            Some(client) => client,
            None => {
                owned = self
                    .pool
                    .get()
                    .await
                    .context("failing an unrecoverable Process transition")?;
                &mut owned
            }
        };
        let transaction = client
            .transaction()
            .await
            .context("starting Process failure transaction")?;
        let failure = json!({
            "kind": "transition_failed",
            "code": code,
            "path": failing.current_state,
            "message": "the Process transition could not be applied",
        });
        let version = failing
            .version
            .checked_add(1)
            .ok_or_else(|| anyhow!("Process instance version overflow"))?;
        let updated = transaction
            .execute(
                "
                UPDATE donat.process_instances
                SET status = 'failed',
                    failure_json = $3,
                    version = $4,
                    updated_at = statement_timestamp()
                WHERE source_name = $1
                  AND id = $2
                  AND version = $5
                  AND status = 'running'
                ",
                &[
                    &self.source_name,
                    &failing.instance_id,
                    &failure,
                    &version,
                    &failing.version,
                ],
            )
            .await
            .context("recording an unrecoverable Process transition failure")?;
        // The UPDATE is guarded on the version this worker read. A concurrent
        // writer that moved it matches nothing — and committing anyway would
        // consume the event, record no failure, and leave the instance
        // `running` with nothing left to advance it. `commit_failed_command`
        // has always insisted on this; the two must agree.
        if updated != 1 {
            bail!(
                "Process instance {} changed while its failure was being recorded; \
                 the transition was not marked failed",
                failing.instance_id
            );
        }
        consume_event(&transaction, &self.source_name, failing.event_id).await?;
        transaction
            .commit()
            .await
            .context("committing an unrecoverable Process transition failure")?;
        Ok(TransitionConsumption::CommandFailed {
            instance_id: failing.instance_id,
            event_id: failing.event_id,
            code,
        })
    }

    pub(crate) async fn consume_one_transition_kind(
        &self,
        event_kind: Option<&str>,
    ) -> anyhow::Result<TransitionConsumption> {
        // Held until the transition is applied or abandoned: this worker owns
        // the instance for that whole span.
        let (prepared, _in_flight) = match self.prepare_one_transition(event_kind).await? {
            Preparation::Prepared(prepared, guard) => (*prepared, guard),
            Preparation::Failed(consumption) => return Ok(consumption),
            Preparation::NoWork => return Ok(TransitionConsumption::NoWork),
        };

        let mut client = self.pool.get().await.with_context(|| {
            format!(
                "claiming Process transition for source `{}`",
                self.source_name
            )
        })?;
        let transaction = client
            .transaction()
            .await
            .context("starting Process transition transaction")?;
        if !lock_prepared_snapshot(&transaction, &self.source_name, prepared.snapshot()).await? {
            transaction
                .commit()
                .await
                .context("committing stale Process transition claim")?;
            return Ok(TransitionConsumption::NoWork);
        }
        if self
            .pending_signal_precedes_timer(&transaction, prepared.snapshot())
            .await?
        {
            transaction
                .commit()
                .await
                .context("committing Process timeout deferral")?;
            return Ok(TransitionConsumption::NoWork);
        }

        // One instance's refusal is its own. Anything that is not a known
        // transient condition ends this instance instead of travelling up to
        // the consumer, where it would be retried against the head of the
        // shared queue forever — stopping every other Process in the
        // deployment. See issue #21 for the queue-level half of this.
        let failing = FailingTransition::of(prepared.snapshot());
        let applied: anyhow::Result<TransitionConsumption> = async {
            Ok(match prepared {
                PreparedTransition::Command(prepared) => {
                    self.consume_prepared_command(&transaction, &prepared)
                        .await?
                }
                PreparedTransition::Request(prepared) => {
                    let activity_job_id =
                        commit_request_schedule(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::ActivityScheduled {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        activity_job_id,
                        state: prepared.snapshot.current_state,
                    }
                }
                PreparedTransition::RequestSuccess(prepared) => {
                    commit_request_success(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Advanced {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        from_state: prepared.snapshot.current_state.clone(),
                        to_state: prepared.state.next,
                    }
                }
                PreparedTransition::RequestFailure(prepared) => {
                    commit_request_failure(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Advanced {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        from_state: prepared.snapshot.current_state.clone(),
                        to_state: prepared.next,
                    }
                }
                PreparedTransition::When(prepared) => {
                    commit_when(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Advanced {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        from_state: prepared.snapshot.current_state.clone(),
                        to_state: prepared.next,
                    }
                }
                PreparedTransition::Output(prepared) => {
                    commit_output(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Completed {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        state: prepared.snapshot.current_state,
                    }
                }
                PreparedTransition::Fail(prepared) => {
                    commit_fail(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Failed {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        state: prepared.snapshot.current_state,
                        code: prepared.state.code,
                    }
                }
                PreparedTransition::WaitEntry(prepared) => {
                    let timer_event_id =
                        commit_wait_entry(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::WaitEntered {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        timer_event_id,
                        state: prepared.snapshot.current_state,
                    }
                }
                PreparedTransition::WaitCompletion(prepared) => {
                    commit_wait_completion(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Advanced {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        from_state: prepared.snapshot.current_state,
                        to_state: prepared.next,
                    }
                }
                PreparedTransition::FanOutExpansion(prepared) => {
                    commit_fanout_expansion(&transaction, &self.source_name, &prepared).await?
                }
                PreparedTransition::FanOutFailure(prepared) => {
                    commit_fanout_failure(&transaction, &self.source_name, &prepared).await?;
                    TransitionConsumption::Failed {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        state: prepared.snapshot.current_state,
                        code: prepared.code.to_owned(),
                    }
                }
                PreparedTransition::FanOutCommandItem(prepared) => {
                    self.consume_prepared_fanout_command(&transaction, &prepared)
                        .await?
                }
                PreparedTransition::FanOutRequestCompletion(prepared) => {
                    commit_fanout_request_completion(&transaction, &self.source_name, &prepared)
                        .await?
                }
            })
        }
        .await;
        let result = match applied {
            Ok(result) => result,
            Err(error) if is_transient(&error) => {
                // Rolls back and gives the connection back to this scope, which
                // the deferral then writes on rather than queueing for another.
                drop(transaction);
                return self
                    .defer_transition(failing, &error, Some(&mut client))
                    .await;
            }
            Err(error) => {
                drop(transaction);
                return self
                    .fail_instance(failing, "transition_failed", &error, Some(&mut client))
                    .await;
            }
        };
        transaction
            .commit()
            .await
            .context("committing Process deterministic transition")?;
        Ok(result)
    }

    async fn pending_signal_precedes_timer(
        &self,
        transaction: &Transaction<'_>,
        snapshot: &TransitionSnapshot,
    ) -> anyhow::Result<bool> {
        if snapshot.event_kind != "timer" {
            return Ok(false);
        }
        let definition = self
            .deployed_catalog
            .revision(&snapshot.process_name, &snapshot.revision)
            .ok_or_else(|| {
                anyhow!(
                    "Process timer `{}` references absent revision `{}`",
                    snapshot.event_id,
                    snapshot.revision
                )
            })?;
        let Some(wait) = definition
            .states
            .get(&snapshot.current_state)
            .and_then(|state| match &state.operation {
                CompiledProcessStateOperation::Wait(wait) => Some(wait.as_ref()),
                _ => None,
            })
        else {
            return Ok(false);
        };
        if matches!(wait, CompiledProcessWaitState::Timer(_)) {
            return Ok(false);
        }
        let pending_event_precedes_timer = transaction
            .query_one(
                "
                SELECT EXISTS (
                    SELECT 1
                    FROM donat.process_events signal
                    JOIN donat.process_events timer
                      ON timer.source_name = signal.source_name
                     AND timer.id = $2
                    WHERE signal.source_name = $1
                      AND signal.instance_id = $3
                      AND signal.id <> timer.id
                      AND signal.kind = 'signal'
                      AND signal.status = 'pending'
                      AND signal.available_at <= timer.available_at
                      AND signal.payload_json ->> 'wait_state' = $4
                      AND signal.payload_json ->> 'wait_version' = $5
                      AND timer.instance_id = $3
                      AND timer.kind = 'timer'
                )
                ",
                &[
                    &self.source_name,
                    &snapshot.event_id,
                    &snapshot.instance_id,
                    &snapshot.current_state,
                    &snapshot.version.to_string(),
                ],
            )
            .await
            .context("checking accepted events committed before a Process timeout")?
            .get::<_, bool>(0);
        if pending_event_precedes_timer {
            return Ok(true);
        }
        let CompiledProcessWaitState::Signal(wait) = wait else {
            let CompiledProcessWaitState::Webhook(wait) = wait else {
                unreachable!("decision timers returned before timeout precedence checks");
            };
            if snapshot
                .event_payload
                .get("connector_instance")
                .and_then(Json::as_str)
                != Some(wait.connector.as_str())
                || snapshot.event_payload.get("trigger").and_then(Json::as_str)
                    != Some(wait.trigger.as_str())
                || snapshot.event_payload.get("route").and_then(Json::as_str) != Some("timeout")
            {
                bail!(
                    "Process webhook timeout event `{}` does not match wait state `{}`",
                    snapshot.event_id,
                    snapshot.current_state
                );
            }
            return Ok(false);
        };
        if snapshot
            .event_payload
            .get("signal_name")
            .and_then(Json::as_str)
            != Some(wait.signal.as_str())
            || snapshot.event_payload.get("route").and_then(Json::as_str) != Some("timeout")
        {
            bail!(
                "Process signal timeout event `{}` does not match wait state `{}`",
                snapshot.event_id,
                snapshot.current_state
            );
        }
        let correlation = snapshot.event_payload.get("correlation").ok_or_else(|| {
            anyhow!(
                "Process signal timeout event `{}` has no correlation",
                snapshot.event_id
            )
        })?;
        let wait_signal = definition.signals.get(&wait.signal).ok_or_else(|| {
            anyhow!(
                "Process signal `{}` disappeared from revision `{}`",
                wait.signal,
                snapshot.revision
            )
        })?;
        let rows = transaction
            .query(
                "
                SELECT request.process_revision
                FROM donat.process_signal_requests request
                JOIN donat.process_events timer
                  ON timer.source_name = request.source_name
                 AND timer.id = $2
                WHERE request.source_name = $1
                  AND request.process_name = $3
                  AND request.signal_name = $4
                  AND request.correlation_json = $5
                  AND request.status = 'pending'
                  AND request.created_at >= timer.created_at
                  AND request.created_at <= timer.available_at
                  AND timer.instance_id = $6
                  AND timer.process_name = $3
                  AND timer.revision = $7
                  AND timer.kind = 'timer'
                ORDER BY request.created_at, request.id
                ",
                &[
                    &self.source_name,
                    &snapshot.event_id,
                    &snapshot.process_name,
                    &wait.signal,
                    &correlation,
                    &snapshot.instance_id,
                    &snapshot.revision,
                ],
            )
            .await
            .context("checking signals committed before a Process timeout")?;
        for row in rows {
            let request_revision: String = row.get("process_revision");
            let request_definition = self
                .deployed_catalog
                .revision(&snapshot.process_name, &request_revision)
                .ok_or_else(|| {
                    anyhow!(
                        "pending Process signal references absent revision `{request_revision}`"
                    )
                })?;
            let request_signal = request_definition
                .signals
                .get(&wait.signal)
                .ok_or_else(|| {
                    anyhow!(
                        "pending Process signal `{}` is absent from revision `{request_revision}`",
                        wait.signal
                    )
                })?;
            if request_signal.contract_fingerprint == wait_signal.contract_fingerprint {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn consume_prepared_command(
        &self,
        transaction: &Transaction<'_>,
        prepared: &PreparedCommandTransition,
    ) -> anyhow::Result<TransitionConsumption> {
        let outcome =
            execute_process_command_in_savepoint(transaction, &prepared.command, false).await?;
        match outcome {
            ProcessCommandOutcome::Applied { result } => {
                // A result that does not fit its declared contract is a
                // deployment mistake, not weather: the same command will
                // produce the same shape on every retry. Retrying it forever
                // held the shared transition queue against every other
                // instance, so it fails this one and names the cause in the
                // log.
                if let Err(error) =
                    validate_state_output(&prepared.definition, &prepared.snapshot, &result)
                {
                    tracing::error!(
                        instance_id = %prepared.snapshot.instance_id,
                        state = %prepared.snapshot.current_state,
                        command = %prepared.state.name,
                        error = format_args!("{error:#}"),
                        "Process command result violated its contract; failing the instance"
                    );
                    commit_failed_command(
                        transaction,
                        &self.source_name,
                        prepared,
                        "command_result_contract_violation",
                    )
                    .await?;
                    return Ok(TransitionConsumption::CommandFailed {
                        instance_id: prepared.snapshot.instance_id,
                        event_id: prepared.snapshot.event_id,
                        code: "command_result_contract_violation",
                    });
                }
                commit_applied_command(transaction, &self.source_name, prepared, &result).await?;
                Ok(TransitionConsumption::Advanced {
                    instance_id: prepared.snapshot.instance_id,
                    event_id: prepared.snapshot.event_id,
                    from_state: prepared.snapshot.current_state.clone(),
                    to_state: prepared.state.next.clone(),
                })
            }
            ProcessCommandOutcome::Rejected { error } => {
                commit_rejected_command(transaction, &self.source_name, prepared, &error).await?;
                Ok(TransitionConsumption::CommandRejected {
                    instance_id: prepared.snapshot.instance_id,
                    event_id: prepared.snapshot.event_id,
                    error,
                })
            }
            ProcessCommandOutcome::Unrecoverable { code } => {
                commit_failed_command(transaction, &self.source_name, prepared, code).await?;
                Ok(TransitionConsumption::CommandFailed {
                    instance_id: prepared.snapshot.instance_id,
                    event_id: prepared.snapshot.event_id,
                    code,
                })
            }
        }
    }

    async fn consume_prepared_fanout_command(
        &self,
        transaction: &Transaction<'_>,
        prepared: &PreparedFanOutCommandItem,
    ) -> anyhow::Result<TransitionConsumption> {
        let CompiledProcessForEachActivity::Command(activity) = &prepared.state.activity else {
            bail!("prepared command fan-out item retained a request activity");
        };
        let outcome =
            execute_process_command_in_savepoint(transaction, &prepared.command, false).await?;
        let outcome = match outcome {
            ProcessCommandOutcome::Applied { result } => {
                let dependency = prepared
                    .definition
                    .dependencies
                    .commands
                    .get(&(self.source_name.clone(), activity.name.clone()))
                    .ok_or_else(|| {
                        anyhow!(
                            "Process fan-out command `{}.{}` disappeared from the pinned closure",
                            self.source_name,
                            activity.name
                        )
                    })?;
                dependency
                    .result
                    .validate(
                        &typed_value(&result).context("decoding Process command fan-out result")?,
                    )
                    .map_err(|error| {
                        anyhow!(
                            "Process command fan-out result violated `{}`: {error}",
                            activity.name
                        )
                    })?;
                if merge_fanout_success_item(&prepared.item, &result, prepared.state.preserve_input)
                    .is_err()
                {
                    let output = fanout_failure_output(
                        &prepared.item,
                        &prepared.item_key,
                        "command",
                        "fanout_result_conflict",
                        "the fan-out result conflicted with preserved input",
                        false,
                        &fanout_logical_activity_id(
                            &prepared.snapshot,
                            &prepared.item_key_identity,
                        ),
                    )?;
                    Err(FanOutItemFailure {
                        output,
                        route: prepared.state.next.clone(),
                        error_kind: "invariant".to_owned(),
                    })
                } else {
                    Ok(result)
                }
            }
            ProcessCommandOutcome::Rejected { error } => {
                let output = fanout_failure_output(
                    &prepared.item,
                    &prepared.item_key,
                    "command",
                    &error.code,
                    &error.message,
                    false,
                    &fanout_logical_activity_id(&prepared.snapshot, &prepared.item_key_identity),
                )?;
                Err(FanOutItemFailure {
                    output,
                    route: prepared.state.next.clone(),
                    error_kind: "command_rejected".to_owned(),
                })
            }
            ProcessCommandOutcome::Unrecoverable { code } => {
                let output = fanout_failure_output(
                    &prepared.item,
                    &prepared.item_key,
                    "command",
                    code,
                    "the Process command was refused by the database",
                    false,
                    &fanout_logical_activity_id(&prepared.snapshot, &prepared.item_key_identity),
                )?;
                Err(FanOutItemFailure {
                    output,
                    route: prepared.state.next.clone(),
                    error_kind: "command_failed".to_owned(),
                })
            }
        };
        commit_fanout_item_completion(
            transaction,
            &self.source_name,
            &prepared.snapshot,
            &prepared.definition,
            &prepared.state,
            prepared.ordinal,
            &prepared.item_key,
            &prepared.item_key_identity,
            outcome,
            None,
        )
        .await
    }

    async fn prepare_one_transition(
        &self,
        event_kind: Option<&str>,
    ) -> anyhow::Result<Preparation> {
        let mut client = self
            .pool
            .get()
            .await
            .context("reading due Process transitions")?;
        let event_kind = event_kind.map(str::to_owned);
        let rows = client
            .query(
                "
                SELECT
                    event.id AS event_id,
                    event.kind AS event_kind,
                    event.payload_json AS event_payload,
                    instance.id AS instance_id,
                    instance.process_name,
                    instance.revision,
                    instance.current_state,
                    instance.input_json,
                    instance.state_json,
                    instance.version,
                    instance.caller_role,
                    instance.caller_session_json,
                    to_jsonb(event.available_at) AS workflow_time,
                    event.attempts
                FROM donat.process_events event
                JOIN donat.process_instances instance
                  ON instance.source_name = event.source_name
                 AND instance.id = event.instance_id
                 AND instance.process_name = event.process_name
                 AND instance.revision = event.revision
                WHERE event.source_name = $1
                  AND event.status = 'pending'
                  AND ($2::text IS NULL OR event.kind = $2)
                  AND event.kind IN (
                      'start',
                      'continue',
                      'signal',
                      'timer',
                      'fanout_item',
                      'activity_succeeded',
                      'activity_failed',
                      'retry_exhausted'
                  )
                  AND event.available_at <= statement_timestamp()
                  AND instance.status = 'running'
                  -- Instances another worker of this deployment is already
                  -- inside. Skipping them here is what lets two workers pick
                  -- two different instances instead of racing for one.
                  AND event.instance_id <> ALL($4)
                ORDER BY
                    event.available_at,
                    CASE WHEN event.kind = 'signal' THEN 0 ELSE 1 END,
                    event.id
                LIMIT $3
                ",
                &[
                    &self.source_name,
                    &event_kind,
                    &PREPARATION_BATCH_SIZE,
                    &self.busy_instances(),
                ],
            )
            .await
            .context("reading due Process event snapshots")?;

        for row in rows {
            let snapshot = transition_snapshot(&row);
            // Between reading the batch and reaching this row another worker
            // may have taken the instance; that one is its transition to make.
            let Some(guard) = self.claim_in_flight(snapshot.instance_id) else {
                continue;
            };
            let failing = FailingTransition::of(&snapshot);
            // Preparing a transition is as capable of a deterministic refusal
            // as applying one — an absent compiled dependency, an activity
            // input that does not fit its contract. Those must end their own
            // instance too, or the queue stops for everybody (issue #21).
            let prepared = match self
                .prepare_one_snapshot(&client, snapshot, event_kind.as_deref())
                .await
            {
                Ok(prepared) => prepared,
                Err(error) if is_transient(&error) => {
                    // Transient means "try again", not "try again right now
                    // against the head of the queue": the event steps aside so
                    // the other instances keep moving.
                    return self
                        .defer_transition(failing, &error, Some(&mut client))
                        .await
                        .map(Preparation::Failed);
                }
                Err(error) => {
                    // Report it rather than swallowing it: the consumer logs
                    // the failure and counts the tick as progress, so the loop
                    // moves straight on to the next instance.
                    return self
                        .fail_instance(failing, "transition_preparation_failed", &error, None)
                        .await
                        .map(Preparation::Failed);
                }
            };
            if let Some(prepared) = prepared {
                return Ok(Preparation::Prepared(Box::new(prepared), guard));
            }
        }
        Ok(Preparation::NoWork)
    }

    /// Prepare one due event, or report that this event does not apply to the
    /// state the instance is in.
    async fn prepare_one_snapshot(
        &self,
        client: &deadpool_postgres::Client,
        snapshot: TransitionSnapshot,
        event_kind: Option<&str>,
    ) -> anyhow::Result<Option<PreparedTransition>> {
        let _ = event_kind;
        {
            let definition = self
                .deployed_catalog
                .revision(&snapshot.process_name, &snapshot.revision)
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "Process event `{}` references absent deployed revision `{}.{}` / `{}`",
                        snapshot.event_id,
                        self.source_name,
                        snapshot.process_name,
                        snapshot.revision
                    )
                })?;
            let operation = definition
                .states
                .get(&snapshot.current_state)
                .ok_or_else(|| {
                    anyhow!(
                        "Process instance `{}` references absent compiled state `{}`",
                        snapshot.instance_id,
                        snapshot.current_state
                    )
                })?
                .operation
                .clone();
            let prepared = match operation {
                CompiledProcessStateOperation::Command(state)
                    if deterministic_event_kind(&snapshot.event_kind) =>
                {
                    Some(self.prepare_command_transition(snapshot, definition, state)?)
                }
                CompiledProcessStateOperation::Command(_) => None,
                CompiledProcessStateOperation::Request(state) => {
                    let state = state.as_ref().clone();
                    match snapshot.event_kind.as_str() {
                        "start" | "continue" => {
                            Some(self.prepare_request_transition(snapshot, definition, state)?)
                        }
                        "activity_succeeded" => Some(
                            self.prepare_request_success_transition(
                                client, snapshot, definition, state,
                            )
                            .await?,
                        ),
                        "activity_failed" | "retry_exhausted" => Some(
                            self.prepare_request_failure_transition(client, snapshot, state)
                                .await?,
                        ),
                        _ => None,
                    }
                }
                CompiledProcessStateOperation::When(state)
                    if deterministic_event_kind(&snapshot.event_kind) =>
                {
                    Some(self.prepare_when_transition(snapshot, definition, state)?)
                }
                CompiledProcessStateOperation::When(_) => None,
                CompiledProcessStateOperation::Output(state)
                    if deterministic_event_kind(&snapshot.event_kind) =>
                {
                    Some(self.prepare_output_transition(snapshot, definition, state)?)
                }
                CompiledProcessStateOperation::Output(_) => None,
                CompiledProcessStateOperation::Fail(state)
                    if deterministic_event_kind(&snapshot.event_kind) =>
                {
                    Some(PreparedTransition::Fail(PreparedFailTransition {
                        snapshot,
                        state,
                    }))
                }
                CompiledProcessStateOperation::Fail(_) => None,
                CompiledProcessStateOperation::Wait(state) => {
                    self.prepare_wait_transition(snapshot, definition, state.as_ref().clone())?
                }
                CompiledProcessStateOperation::ForEach(state) => {
                    let state = state.as_ref().clone();
                    match snapshot.event_kind.as_str() {
                        "start" | "continue" => {
                            Some(self.prepare_fanout_expansion(snapshot, definition, state)?)
                        }
                        "fanout_item" => Some(
                            self.prepare_fanout_command_item(client, snapshot, definition, state)
                                .await?,
                        ),
                        "activity_succeeded" | "activity_failed" | "retry_exhausted" => Some(
                            self.prepare_fanout_request_completion(
                                client, snapshot, definition, state,
                            )
                            .await?,
                        ),
                        _ => None,
                    }
                }
            };
            if prepared.is_some() {
                return Ok(prepared);
            }
        }
        Ok(None)
    }

    fn prepare_wait_transition(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessWaitState,
    ) -> anyhow::Result<Option<PreparedTransition>> {
        match snapshot.event_kind.as_str() {
            "start" | "continue" => {
                let prepared = self.prepare_wait_entry(snapshot, &definition, &state)?;
                Ok(Some(PreparedTransition::WaitEntry(prepared)))
            }
            "signal" | "timer" => {
                let prepared = self.prepare_wait_completion(snapshot, &definition, &state)?;
                Ok(prepared.map(PreparedTransition::WaitCompletion))
            }
            _ => Ok(None),
        }
    }

    fn prepare_wait_entry(
        &self,
        snapshot: TransitionSnapshot,
        definition: &CompiledProcessDefinition,
        state: &CompiledProcessWaitState,
    ) -> anyhow::Result<PreparedWaitEntry> {
        let wait_version = next_version(&snapshot)?;
        let context = process_value_context(&self.source_name, &snapshot);
        let mut persist_before_match = false;
        let (schedule, timer_payload) = match state {
            CompiledProcessWaitState::Signal(wait) => {
                let signal = definition.signals.get(&wait.signal).ok_or_else(|| {
                    anyhow!(
                        "compiled Process signal `{}` disappeared from revision `{}`",
                        wait.signal,
                        snapshot.revision
                    )
                })?;
                if signal.role.as_ref().is_some_and(|role| role != &wait.role) {
                    bail!(
                        "compiled Process signal `{}` role differs from wait state `{}`",
                        wait.signal,
                        snapshot.current_state
                    );
                }
                let correlation = Json::Object(
                    evaluate_process_values(&wait.correlate, &context)?
                        .into_iter()
                        .collect(),
                );
                signal
                    .correlation
                    .validate(
                        &typed_value(&correlation).context("decoding Process wait correlation")?,
                    )
                    .map_err(|error| {
                        anyhow!(
                            "Process wait correlation violated signal `{}` contract: {error}",
                            wait.signal
                        )
                    })?;
                let schedule = prepare_signal_deadline(&wait.deadline, &context)?;
                persist_before_match = wait.persist_before_match;
                (
                    schedule,
                    json!({
                        "wait_state": snapshot.current_state,
                        "wait_version": wait_version,
                        "signal_name": wait.signal,
                        "correlation": correlation,
                        "route": "timeout",
                    }),
                )
            }
            CompiledProcessWaitState::Webhook(wait) => {
                let dependency = definition
                    .dependencies
                    .connector_triggers
                    .get(&(
                        self.source_name.clone(),
                        wait.connector.clone(),
                        wait.trigger,
                    ))
                    .ok_or_else(|| {
                        anyhow!(
                            "Process connector trigger `{}.{}` is absent from the pinned dependency closure",
                            wait.connector,
                            wait.trigger.as_str()
                        )
                    })?;
                let live_spec = self
                    .connector_registry
                    .trigger_spec_handle(&self.source_name, &wait.connector, wait.trigger)
                    .ok_or_else(|| {
                        anyhow!(
                            "Process connector trigger `{}.{}` is absent from the immutable registry",
                            wait.connector,
                            wait.trigger.as_str()
                        )
                    })?;
                let live_fingerprint = self
                    .connector_registry
                    .trigger_configuration_fingerprint(&wait.connector, wait.trigger)
                    .ok_or_else(|| {
                        anyhow!(
                            "Process connector trigger `{}.{}` has no immutable deployment fingerprint",
                            wait.connector,
                            wait.trigger.as_str()
                        )
                    })?;
                if dependency.source != self.source_name
                    || dependency.instance != wait.connector
                    || !Arc::ptr_eq(&dependency.spec, &live_spec)
                    || dependency.deployment_fingerprint != live_fingerprint
                {
                    bail!(
                        "Process connector trigger `{}.{}` differs from pinned revision `{}`",
                        wait.connector,
                        wait.trigger.as_str(),
                        snapshot.revision
                    );
                }
                let correlation = Json::Object(
                    evaluate_process_values(&wait.correlate, &context)?
                        .into_iter()
                        .collect(),
                );
                let schedule = prepare_signal_deadline(&wait.deadline, &context)?;
                (
                    schedule,
                    json!({
                        "wait_state": snapshot.current_state,
                        "wait_version": wait_version,
                        "connector_instance": wait.connector,
                        "trigger": wait.trigger.as_str(),
                        "correlation": correlation,
                        "route": "timeout",
                    }),
                )
            }
            CompiledProcessWaitState::Timer(wait) => {
                if !definition
                    .dependencies
                    .decision_tables
                    .contains_key(&wait.decision.name)
                {
                    bail!(
                        "Process decision table `{}` is absent from the pinned dependency closure",
                        wait.decision.name
                    );
                }
                let rules = self.planning_snapshot.rules();
                if rules.decision_table(&wait.decision.name).is_none() {
                    bail!(
                        "Process decision table `{}` is absent from the immutable rule snapshot",
                        wait.decision.name
                    );
                }
                let input = evaluate_process_values(&wait.decision.input, &context)?;
                let result = rules
                    .evaluate_decision(&wait.decision.name, &input)
                    .map_err(|error| {
                        anyhow!(
                            "evaluating Process timer decision table `{}`: {error}",
                            wait.decision.name
                        )
                    })?;
                let delay_seconds = result
                    .output
                    .get(&wait.duration_output)
                    .and_then(Json::as_i64)
                    .filter(|value| *value >= 0)
                    .ok_or_else(|| {
                        anyhow!(
                            "Process timer decision output `{}` must be a non-negative integer",
                            wait.duration_output
                        )
                    })?;
                let delay_milliseconds = delay_seconds
                    .checked_mul(1_000)
                    .ok_or_else(|| anyhow!("Process timer duration overflowed milliseconds"))?;
                validate_state_output(definition, &snapshot, &result.output)
                    .context("validating Process timer decision output")?;
                (
                    PreparedTimerSchedule::AfterMilliseconds(delay_milliseconds),
                    json!({
                        "wait_state": snapshot.current_state,
                        "wait_version": wait_version,
                        "decision_table": wait.decision.name,
                        "matched_row_id": result.matched_row_id,
                        "output": result.output,
                        "route": "timer",
                    }),
                )
            }
        };
        Ok(PreparedWaitEntry {
            snapshot,
            schedule,
            timer_payload,
            persist_before_match,
        })
    }

    fn prepare_wait_completion(
        &self,
        snapshot: TransitionSnapshot,
        definition: &CompiledProcessDefinition,
        state: &CompiledProcessWaitState,
    ) -> anyhow::Result<Option<PreparedWaitCompletion>> {
        if !wait_event_matches_snapshot(&snapshot) {
            return Ok(None);
        }
        match (state, snapshot.event_kind.as_str()) {
            (CompiledProcessWaitState::Signal(wait), "signal") => {
                if snapshot
                    .event_payload
                    .get("signal_name")
                    .and_then(Json::as_str)
                    != Some(wait.signal.as_str())
                {
                    return Ok(None);
                }
                let signal = definition.signals.get(&wait.signal).ok_or_else(|| {
                    anyhow!(
                        "compiled Process signal `{}` disappeared from revision `{}`",
                        wait.signal,
                        snapshot.revision
                    )
                })?;
                let correlation = snapshot
                    .event_payload
                    .get("correlation")
                    .cloned()
                    .ok_or_else(|| anyhow!("Process signal event has no correlation object"))?;
                let payload = snapshot
                    .event_payload
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| anyhow!("Process signal event has no payload object"))?;
                signal
                    .correlation
                    .validate(
                        &typed_value(&correlation)
                            .context("decoding Process signal event correlation")?,
                    )
                    .map_err(|error| {
                        anyhow!(
                            "Process signal event correlation violated `{}`: {error}",
                            wait.signal
                        )
                    })?;
                signal
                    .payload
                    .validate(
                        &typed_value(&payload).context("decoding Process signal event payload")?,
                    )
                    .map_err(|error| {
                        anyhow!(
                            "Process signal event payload violated `{}`: {error}",
                            wait.signal
                        )
                    })?;
                let mut output = correlation
                    .as_object()
                    .cloned()
                    .ok_or_else(|| anyhow!("Process signal correlation is not an object"))?;
                for (name, value) in payload
                    .as_object()
                    .ok_or_else(|| anyhow!("Process signal payload is not an object"))?
                {
                    if output.insert(name.clone(), value.clone()).is_some() {
                        bail!("Process signal field `{name}` exists in correlation and payload");
                    }
                }
                let output = Json::Object(output);
                validate_state_output(definition, &snapshot, &output)
                    .context("validating Process signal wait output")?;
                Ok(Some(PreparedWaitCompletion {
                    snapshot,
                    next: wait.next.clone(),
                    output: Some(output),
                    outcome: "signal_received",
                    redacted_context: json!({ "signal": wait.signal }),
                }))
            }
            (CompiledProcessWaitState::Signal(wait), "timer") => Ok(Some(PreparedWaitCompletion {
                snapshot,
                next: wait.on_timeout.clone(),
                output: None,
                outcome: "timer_fired",
                redacted_context: json!({
                    "kind": "signal_timeout",
                    "signal": wait.signal,
                }),
            })),
            (CompiledProcessWaitState::Webhook(wait), "signal") => {
                if snapshot
                    .event_payload
                    .get("connector_instance")
                    .and_then(Json::as_str)
                    != Some(wait.connector.as_str())
                    || snapshot.event_payload.get("trigger").and_then(Json::as_str)
                        != Some(wait.trigger.as_str())
                {
                    return Ok(None);
                }
                let output = snapshot
                    .event_payload
                    .get("output")
                    .cloned()
                    .ok_or_else(|| anyhow!("Process webhook event has no normalized output"))?;
                let dependency = definition
                    .dependencies
                    .connector_triggers
                    .get(&(
                        self.source_name.clone(),
                        wait.connector.clone(),
                        wait.trigger,
                    ))
                    .ok_or_else(|| {
                        anyhow!(
                            "Process connector trigger `{}.{}` disappeared from the pinned dependency closure",
                            wait.connector,
                            wait.trigger.as_str()
                        )
                    })?;
                let donat_connector_catalog::TriggerSpec::Webhook {
                    output: contract, ..
                } = dependency.spec.as_ref()
                else {
                    bail!("a compiled Process webhook wait retained a poll trigger");
                };
                contract
                    .validate(
                        &typed_value(&output)
                            .context("decoding normalized Process webhook output")?,
                    )
                    .map_err(|error| {
                        anyhow!(
                            "Process webhook output violated `{}.{}`: {error}",
                            wait.connector,
                            wait.trigger.as_str()
                        )
                    })?;
                validate_state_output(definition, &snapshot, &output)
                    .context("validating Process webhook wait output")?;
                Ok(Some(PreparedWaitCompletion {
                    snapshot,
                    next: wait.next.clone(),
                    output: Some(output),
                    outcome: "webhook_received",
                    redacted_context: json!({
                        "connector": wait.connector,
                        "trigger": wait.trigger.as_str(),
                    }),
                }))
            }
            (CompiledProcessWaitState::Webhook(wait), "timer") => {
                Ok(Some(PreparedWaitCompletion {
                    snapshot,
                    next: wait.on_timeout.clone(),
                    output: None,
                    outcome: "timer_fired",
                    redacted_context: json!({
                        "kind": "webhook_timeout",
                        "connector": wait.connector,
                        "trigger": wait.trigger.as_str(),
                    }),
                }))
            }
            (CompiledProcessWaitState::Timer(wait), "timer") => {
                let output = snapshot
                    .event_payload
                    .get("output")
                    .cloned()
                    .ok_or_else(|| anyhow!("Process timer event has no decision output"))?;
                validate_state_output(definition, &snapshot, &output)
                    .context("validating persisted Process timer output")?;
                Ok(Some(PreparedWaitCompletion {
                    snapshot,
                    next: wait.next.clone(),
                    output: Some(output),
                    outcome: "timer_fired",
                    redacted_context: json!({
                        "kind": "decision_timer",
                        "decision_table": wait.decision.name,
                    }),
                }))
            }
            _ => Ok(None),
        }
    }

    fn prepare_command_transition(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessCommandState,
    ) -> anyhow::Result<PreparedTransition> {
        let command = self.prepare_command_mutation(
            &snapshot,
            &definition,
            &state.name,
            &state.role,
            &state.arguments,
            process_value_context(&self.source_name, &snapshot),
        )?;

        Ok(PreparedTransition::Command(PreparedCommandTransition {
            snapshot,
            definition,
            state,
            command,
        }))
    }

    fn prepare_command_mutation(
        &self,
        snapshot: &TransitionSnapshot,
        definition: &CompiledProcessDefinition,
        command_name: &str,
        role: &CompiledProcessCommandRole,
        bindings: &std::collections::BTreeMap<String, donat_metadata::ProcessValue>,
        context: ProcessValueContext<'_>,
    ) -> anyhow::Result<CommandMutation> {
        let finalized = self
            .finalized_command_catalog
            .source(&self.source_name)
            .and_then(|catalog| catalog.command(command_name))
            .ok_or_else(|| {
                anyhow!(
                    "finalized Process command `{}.{}` is absent from the immutable snapshot",
                    self.source_name,
                    command_name
                )
            })?;
        let pre_process = self
            .command_catalog
            .source(&self.source_name)
            .and_then(|catalog| catalog.command(command_name))
            .ok_or_else(|| {
                anyhow!(
                    "compiled Process command `{}.{}` is absent from the immutable snapshot",
                    self.source_name,
                    command_name
                )
            })?;
        let dependency = definition
            .dependencies
            .commands
            .get(&(self.source_name.clone(), command_name.to_owned()))
            .ok_or_else(|| {
                anyhow!(
                    "Process command `{}.{}` is absent from the pinned dependency closure",
                    self.source_name,
                    command_name
                )
            })?;
        let expected = &dependency.definition_fingerprint;
        if expected != &pre_process.descriptor().definition_fingerprint
            || expected != &finalized.command.descriptor().definition_fingerprint
        {
            bail!(
                "Process command `{}.{}` differs from pinned revision `{}`",
                self.source_name,
                command_name,
                snapshot.revision
            );
        }

        let session = process_command_session(definition, role, snapshot)?;
        let arguments = evaluate_process_values(bindings, &context)?;
        let planner = self
            .planning_snapshot
            .planner(&self.source_name)
            .map_err(|error| anyhow!("constructing Process command planner: {error}"))?;
        let command = planner
            .plan_process_command(
                finalized,
                arguments,
                &session,
                &format!(
                    "$.processes.{}.states.{}",
                    snapshot.process_name, snapshot.current_state
                ),
            )
            .map_err(|error| anyhow!("planning Process command `{command_name}`: {error}"))?;

        Ok(command)
    }

    fn prepare_request_transition(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessRequestState,
    ) -> anyhow::Result<PreparedTransition> {
        let prepared = self.prepare_activity_input(
            &snapshot,
            &definition,
            &state,
            process_value_context(&self.source_name, &snapshot),
        )?;
        Ok(PreparedTransition::Request(PreparedRequestTransition {
            snapshot,
            state,
            input: prepared.input,
            request_fingerprint: prepared.request_fingerprint,
            serialization_key_hash: prepared.serialization_key_hash,
        }))
    }

    fn prepare_activity_input(
        &self,
        snapshot: &TransitionSnapshot,
        definition: &CompiledProcessDefinition,
        state: &CompiledProcessRequestState,
        context: ProcessValueContext<'_>,
    ) -> anyhow::Result<PreparedActivityInput> {
        let dependency = definition
            .dependencies
            .connector_operations
            .get(&(
                self.source_name.clone(),
                state.connector.clone(),
                state.operation,
            ))
            .ok_or_else(|| {
                anyhow!(
                    "Process connector operation `{}.{}` is absent from the pinned dependency closure",
                    state.connector,
                    state.operation.as_str()
                )
            })?;
        if dependency.source != self.source_name
            || dependency.instance != state.connector
            || dependency.spec.operation != state.operation
        {
            bail!(
                "pinned Process connector identity differs from request state `{}`",
                snapshot.current_state
            );
        }
        let live_spec = self
            .connector_registry
            .operation_spec_handle(&self.source_name, &state.connector, state.operation)
            .ok_or_else(|| {
                anyhow!(
                    "Process connector operation `{}.{}` is absent from the immutable registry",
                    state.connector,
                    state.operation.as_str()
                )
            })?;
        let live_fingerprint = self
            .connector_registry
            .configuration_fingerprint(&state.connector, state.operation.as_str())
            .ok_or_else(|| {
                anyhow!(
                    "Process connector operation `{}.{}` has no immutable deployment fingerprint",
                    state.connector,
                    state.operation.as_str()
                )
            })?;
        if !Arc::ptr_eq(&dependency.spec, &live_spec)
            || dependency.deployment_fingerprint != live_fingerprint
            || dependency.serialization_key_input.as_deref()
                != self
                    .connector_registry
                    .serialization_key_input(&state.connector, state.operation.as_str())
        {
            bail!(
                "Process connector operation `{}.{}` differs from pinned revision `{}`",
                state.connector,
                state.operation.as_str(),
                snapshot.revision
            );
        }
        let provider_idempotent = matches!(
            dependency.spec.effect,
            donat_connector_catalog::OperationEffect::ProviderIdempotent { .. }
        );
        if provider_idempotent != state.provider_idempotent {
            bail!(
                "Process request state `{}` has a mismatched connector effect",
                snapshot.current_state
            );
        }

        let input = Json::Object(
            evaluate_process_values(&state.input, &context)?
                .into_iter()
                .collect(),
        );
        dependency
            .spec
            .input
            .validate(&typed_value(&input).context("decoding Process activity input")?)
            .map_err(|error| {
                anyhow!(
                    "Process activity input violated the contract of instance `{}` (declared fields: {}): {error}",
                    dependency.instance,
                    dependency
                        .spec
                        .input
                        .roots
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                )
            })?;
        let request_fingerprint = canonical_json_sha256(&input);
        let serialization_key_hash = dependency
            .serialization_key_input
            .as_deref()
            .map(|field| super::activity::process_serialization_key_hash(&input, field))
            .transpose()?;
        Ok(PreparedActivityInput {
            input,
            request_fingerprint,
            serialization_key_hash,
        })
    }

    fn prepare_fanout_expansion(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessForEachState,
    ) -> anyhow::Result<PreparedTransition> {
        let input = match evaluate_process_value(
            &state.input,
            &process_value_context(&self.source_name, &snapshot),
        ) {
            Ok(input) => input,
            Err(_) => {
                return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                    snapshot,
                    code: "fanout_input_invalid",
                    message: "the bounded fan-out input could not be evaluated",
                }));
            }
        };
        let Some(values) = input.as_array() else {
            return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                snapshot,
                code: "fanout_input_not_list",
                message: "the bounded fan-out input is not a list",
            }));
        };
        if values.len() > state.max_items as usize {
            return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                snapshot,
                code: "fanout_max_items_exceeded",
                message: "the bounded fan-out input exceeded max_items",
            }));
        }

        let mut seen = BTreeSet::new();
        let mut items = Vec::with_capacity(values.len());
        for (ordinal, item) in values.iter().enumerate() {
            let Some(object) = item.as_object() else {
                return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                    snapshot,
                    code: "fanout_item_not_object",
                    message: "a bounded fan-out item is not an object",
                }));
            };
            let Some(raw_item_key) = object.get(&state.item_key) else {
                return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                    snapshot,
                    code: "fanout_item_key_missing",
                    message: "a bounded fan-out item has no declared item_key",
                }));
            };
            let Some(item_key) = canonical_fanout_item_key(raw_item_key) else {
                return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                    snapshot,
                    code: "fanout_item_key_invalid",
                    message: "a bounded fan-out item_key is not a scalar",
                }));
            };
            if !seen.insert(item_key.identity.clone()) {
                return Ok(PreparedTransition::FanOutFailure(PreparedFanOutFailure {
                    snapshot,
                    code: "fanout_item_key_duplicate",
                    message: "the bounded fan-out input contains a duplicate item_key",
                }));
            }
            let request = match &state.activity {
                CompiledProcessForEachActivity::Command(_) => None,
                CompiledProcessForEachActivity::Request(request) => {
                    let context = fanout_value_context(
                        &self.source_name,
                        &snapshot,
                        item,
                        &item_key.identity,
                    );
                    Some(self.prepare_activity_input(&snapshot, &definition, request, context)?)
                }
            };
            items.push(PreparedFanOutItem {
                ordinal: i32::try_from(ordinal)
                    .expect("compiled fan-out maximum fits PostgreSQL integer"),
                item_key: item_key.output,
                item_key_identity: item_key.identity,
                item: item.clone(),
                request,
            });
        }

        Ok(PreparedTransition::FanOutExpansion(
            PreparedFanOutExpansion {
                snapshot,
                definition,
                state,
                items,
            },
        ))
    }

    async fn prepare_fanout_command_item(
        &self,
        client: &tokio_postgres::Client,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessForEachState,
    ) -> anyhow::Result<PreparedTransition> {
        let CompiledProcessForEachActivity::Command(activity) = &state.activity else {
            bail!(
                "Process fan-out item event references request state `{}`",
                snapshot.current_state
            );
        };
        let ordinal = fanout_event_ordinal(&snapshot)?;
        let item_key_identity = fanout_event_item_key_identity(&snapshot)?.to_owned();
        let entry_event_id = fanout_event_entry_id(&snapshot)?;
        if !fanout_event_matches_snapshot(&snapshot) {
            bail!(
                "Process fan-out item event `{}` differs from active state `{}`",
                snapshot.event_id,
                snapshot.current_state
            );
        }
        let row = client
            .query_opt(
                "
                SELECT item_key, item_json
                FROM donat.process_fanout_items
                WHERE source_name = $1
                  AND instance_id = $2
                  AND state_name = $3
                  AND entry_event_id = $4
                  AND ordinal = $5
                  AND item_key_identity = $6
                  AND status = 'scheduled'
                  AND activity_job_id IS NULL
                ",
                &[
                    &self.source_name,
                    &snapshot.instance_id,
                    &snapshot.current_state,
                    &entry_event_id,
                    &ordinal,
                    &item_key_identity,
                ],
            )
            .await
            .context("reading scheduled Process command fan-out item")?
            .ok_or_else(|| {
                anyhow!(
                    "Process fan-out item event `{}` has no scheduled item",
                    snapshot.event_id
                )
            })?;
        let item: Json = row.get("item_json");
        let item_key: String = row.get("item_key");
        let command = self.prepare_command_mutation(
            &snapshot,
            &definition,
            &activity.name,
            &activity.role,
            &activity.arguments,
            fanout_value_context(&self.source_name, &snapshot, &item, &item_key_identity),
        )?;
        Ok(PreparedTransition::FanOutCommandItem(Box::new(
            PreparedFanOutCommandItem {
                snapshot,
                definition,
                state,
                ordinal,
                item_key,
                item_key_identity,
                item,
                command,
            },
        )))
    }

    async fn prepare_fanout_request_completion(
        &self,
        client: &tokio_postgres::Client,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessForEachState,
    ) -> anyhow::Result<PreparedTransition> {
        let CompiledProcessForEachActivity::Request(request) = &state.activity else {
            bail!(
                "Process activity completion references command fan-out state `{}`",
                snapshot.current_state
            );
        };
        let activity_job_id = snapshot
            .event_payload
            .get("activity_job_id")
            .and_then(Json::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| anyhow!("Process fan-out activity event has no valid job ID"))?;
        let row = client
            .query_opt(
                "
                SELECT
                    item.ordinal,
                    item.item_key,
                    item.item_key_identity,
                    item.item_json,
                    job.state_name,
                    job.connector_instance,
                    job.operation,
                    job.status AS job_status,
                    job.attempts,
                    job.lease_generation,
                    job.result_json,
                    job.last_error_json
                FROM donat.process_fanout_items item
                JOIN donat.process_activity_jobs job
                  ON job.source_name = item.source_name
                 AND job.id = item.activity_job_id
                 AND job.instance_id = item.instance_id
                 AND job.state_name = item.state_name
                WHERE item.source_name = $1
                  AND item.instance_id = $2
                  AND item.state_name = $3
                  AND item.activity_job_id = $4
                  AND item.status = 'scheduled'
                ",
                &[
                    &self.source_name,
                    &snapshot.instance_id,
                    &snapshot.current_state,
                    &activity_job_id,
                ],
            )
            .await
            .context("reading terminal Process request fan-out item")?
            .ok_or_else(|| {
                anyhow!(
                    "Process fan-out activity event references no scheduled item `{activity_job_id}`"
                )
            })?;
        let state_name: String = row.get("state_name");
        let connector: String = row.get("connector_instance");
        let operation: String = row.get("operation");
        let job_status: String = row.get("job_status");
        let attempt: i32 = row.get("attempts");
        let lease_generation: i64 = row.get("lease_generation");
        if state_name != snapshot.current_state
            || connector != request.connector
            || operation != request.operation.as_str()
        {
            bail!("Process fan-out activity event differs from its terminal job");
        }

        let result = if snapshot.event_kind == "activity_succeeded" {
            let event_attempt = snapshot
                .event_payload
                .get("attempt")
                .and_then(Json::as_i64)
                .and_then(|value| i32::try_from(value).ok());
            let event_generation = snapshot
                .event_payload
                .get("lease_generation")
                .and_then(Json::as_i64);
            if job_status != "succeeded"
                || event_attempt != Some(attempt)
                || event_generation != Some(lease_generation)
            {
                bail!("Process fan-out activity success event is not fenced to its terminal job");
            }
            Ok(row
                .get::<_, Option<Json>>("result_json")
                .ok_or_else(|| anyhow!("succeeded Process fan-out activity has no result"))?)
        } else {
            if job_status != "failed" {
                bail!("Process fan-out activity failure event references a non-failed job");
            }
            let error = row
                .get::<_, Option<Json>>("last_error_json")
                .ok_or_else(|| anyhow!("failed Process fan-out activity has no safe error"))?;
            if snapshot.event_payload.get("error") != Some(&error) {
                bail!("Process fan-out activity failure event differs from the stored safe error");
            }
            Err(error)
        };
        Ok(PreparedTransition::FanOutRequestCompletion(
            PreparedFanOutRequestCompletion {
                snapshot,
                definition,
                state,
                ordinal: row.get("ordinal"),
                item_key: row.get("item_key"),
                item_key_identity: row.get("item_key_identity"),
                item: row.get("item_json"),
                activity_job_id,
                attempt,
                lease_generation,
                result,
            },
        ))
    }

    async fn prepare_request_success_transition(
        &self,
        client: &tokio_postgres::Client,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessRequestState,
    ) -> anyhow::Result<PreparedTransition> {
        let activity_job_id = snapshot
            .event_payload
            .get("activity_job_id")
            .and_then(Json::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| anyhow!("Process activity success event has no valid job ID"))?;
        let attempt = snapshot
            .event_payload
            .get("attempt")
            .and_then(Json::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| anyhow!("Process activity success event has no valid attempt"))?;
        let lease_generation = snapshot
            .event_payload
            .get("lease_generation")
            .and_then(Json::as_i64)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                anyhow!("Process activity success event has no valid lease generation")
            })?;
        let row = client
            .query_opt(
                "
                SELECT
                    result_json,
                    request_fingerprint,
                    connector_instance,
                    operation,
                    state_name,
                    attempts,
                    lease_generation
                FROM donat.process_activity_jobs
                WHERE source_name = $1
                  AND id = $2
                  AND instance_id = $3
                  AND status = 'succeeded'
                ",
                &[&self.source_name, &activity_job_id, &snapshot.instance_id],
            )
            .await
            .context("reading successful Process activity result")?
            .ok_or_else(|| {
                anyhow!(
                    "Process activity success event references no succeeded job `{activity_job_id}`"
                )
            })?;
        let output: Json = row
            .get::<_, Option<Json>>("result_json")
            .ok_or_else(|| anyhow!("succeeded Process activity has no result"))?;
        let connector: String = row.get("connector_instance");
        let operation: String = row.get("operation");
        let state_name: String = row.get("state_name");
        let stored_attempt: i32 = row.get("attempts");
        let stored_generation: i64 = row.get("lease_generation");
        if connector != state.connector
            || operation != state.operation.as_str()
            || state_name != snapshot.current_state
            || stored_attempt != attempt
            || stored_generation != lease_generation
        {
            bail!("Process activity success event differs from its terminal job");
        }
        validate_state_output(&definition, &snapshot, &output)
            .context("validating Process activity state output")?;
        Ok(PreparedTransition::RequestSuccess(
            PreparedRequestSuccessTransition {
                snapshot,
                state,
                activity_job_id,
                attempt,
                lease_generation,
                output,
            },
        ))
    }

    async fn prepare_request_failure_transition(
        &self,
        client: &tokio_postgres::Client,
        snapshot: TransitionSnapshot,
        state: CompiledProcessRequestState,
    ) -> anyhow::Result<PreparedTransition> {
        let activity_job_id = snapshot
            .event_payload
            .get("activity_job_id")
            .and_then(Json::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| anyhow!("Process activity failure event has no valid job ID"))?;
        let row = client
            .query_opt(
                "
                SELECT
                    state_name,
                    connector_instance,
                    operation,
                    attempts,
                    lease_generation,
                    last_error_json
                FROM donat.process_activity_jobs
                WHERE source_name = $1
                  AND id = $2
                  AND instance_id = $3
                  AND status = 'failed'
                ",
                &[&self.source_name, &activity_job_id, &snapshot.instance_id],
            )
            .await
            .context("reading failed Process activity")?
            .ok_or_else(|| {
                anyhow!(
                    "Process activity failure event references no failed job `{activity_job_id}`"
                )
            })?;
        let state_name: String = row.get("state_name");
        let connector: String = row.get("connector_instance");
        let operation: String = row.get("operation");
        let attempt: i32 = row.get("attempts");
        let lease_generation: i64 = row.get("lease_generation");
        let stored_error: Json = row
            .get::<_, Option<Json>>("last_error_json")
            .ok_or_else(|| anyhow!("failed Process activity has no safe error"))?;
        if state_name != snapshot.current_state
            || connector != state.connector
            || operation != state.operation.as_str()
        {
            bail!("Process activity failure event differs from its terminal job");
        }
        let event_error = snapshot
            .event_payload
            .get("error")
            .ok_or_else(|| anyhow!("Process activity failure event has no safe error"))?;
        if event_error != &stored_error {
            bail!("Process activity failure event differs from the stored safe error");
        }
        let error_kind = if snapshot.event_kind == "retry_exhausted" {
            "retry_exhausted"
        } else {
            stored_error
                .get("class")
                .and_then(Json::as_str)
                .ok_or_else(|| anyhow!("Process activity safe error has no class"))?
        };
        let routes = state.on_error.as_ref().ok_or_else(|| {
            anyhow!(
                "Process request state `{}` has no compiled error route",
                snapshot.current_state
            )
        })?;
        let next = routes
            .routes
            .iter()
            .find(|route| {
                route
                    .kinds
                    .iter()
                    .any(|kind| process_error_kind_name(kind) == error_kind)
            })
            .map(|route| route.next.clone())
            .unwrap_or_else(|| routes.fallback.next.clone());
        Ok(PreparedTransition::RequestFailure(
            PreparedRequestFailureTransition {
                snapshot,
                activity_job_id,
                attempt,
                lease_generation,
                next,
                error_kind: error_kind.to_owned(),
            },
        ))
    }

    fn prepare_when_transition(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessWhenState,
    ) -> anyhow::Result<PreparedTransition> {
        let context = process_value_context(&self.source_name, &snapshot);
        let rules = self.planning_snapshot.rules();
        let (comparison, output, decision_row) = if let Some(decision) = &state.decision_table {
            if !definition
                .dependencies
                .decision_tables
                .contains_key(&decision.name)
            {
                bail!(
                    "Process decision table `{}` is absent from the pinned dependency closure",
                    decision.name
                );
            }
            if rules.decision_table(&decision.name).is_none() {
                bail!(
                    "Process decision table `{}` is absent from the immutable rule snapshot",
                    decision.name
                );
            }
            let input = evaluate_process_values(&decision.input, &context)?;
            let result = rules
                .evaluate_decision(&decision.name, &input)
                .map_err(|error| {
                    anyhow!(
                        "evaluating Process decision table `{}`: {error}",
                        decision.name
                    )
                })?;
            (
                result.output.clone(),
                result.output,
                Some(result.matched_row_id),
            )
        } else {
            let output = state
                .literal_output_state
                .as_ref()
                .map(|source_state| state_output(&snapshot.state, source_state))
                .transpose()?
                .unwrap_or_else(|| json!({}));
            (output.clone(), output, None)
        };

        let mut matched_index = None;
        let mut matched_kind = "default".to_owned();
        for (index, case) in state.cases.iter().enumerate() {
            let matched = match &case.predicate {
                CompiledProcessWhenPredicate::Matches(expected) => {
                    json_matches(&comparison, expected)?
                }
                CompiledProcessWhenPredicate::Rule { name, bindings } => {
                    if state.decision_table.is_some() {
                        bail!("compiled decision-table route contains a rule case");
                    }
                    if !definition.dependencies.rules.contains_key(name) {
                        bail!("Process rule `{name}` is absent from the pinned dependency closure");
                    }
                    let rule = rules.rule(name).ok_or_else(|| {
                        anyhow!("Process rule `{name}` is absent from the immutable rule snapshot")
                    })?;
                    let bindings = evaluate_process_values(bindings, &context)?;
                    rules
                        .evaluate_bool(rule, &bindings)
                        .map_err(|error| anyhow!("evaluating Process rule `{name}`: {error}"))?
                }
            };
            if matched {
                matched_index = Some(index);
                matched_kind = match &case.predicate {
                    CompiledProcessWhenPredicate::Matches(_) => "literal".to_owned(),
                    CompiledProcessWhenPredicate::Rule { name, .. } => {
                        format!("rule:{name}")
                    }
                };
                break;
            }
        }
        let next = matched_index
            .map(|index| state.cases[index].next.clone())
            .unwrap_or_else(|| state.default.clone());
        validate_state_output(&definition, &snapshot, &output)
            .context("validating Process when output")?;
        let redacted_context = if let Some(decision) = &state.decision_table {
            json!({
                "kind": "decision_table",
                "name": decision.name,
                "matched_row_id": decision_row,
                "case_index": matched_index,
                "route": matched_kind,
            })
        } else {
            json!({
                "kind": "when",
                "case_index": matched_index,
                "route": matched_kind,
            })
        };

        Ok(PreparedTransition::When(PreparedWhenTransition {
            snapshot,
            next,
            output,
            redacted_context,
        }))
    }

    fn prepare_output_transition(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessOutputState,
    ) -> anyhow::Result<PreparedTransition> {
        let values = evaluate_process_values(
            &state.values,
            &process_value_context(&self.source_name, &snapshot),
        )?;
        let output = Json::Object(values.into_iter().collect());
        validate_state_output(&definition, &snapshot, &output)
            .context("validating Process output state")?;
        definition
            .output
            .validate(&typed_value(&output).context("decoding Process terminal output")?)
            .map_err(|error| anyhow!("Process terminal output violated its contract: {error}"))?;
        Ok(PreparedTransition::Output(PreparedOutputTransition {
            snapshot,
            output,
        }))
    }
}

fn transition_snapshot(row: &Row) -> TransitionSnapshot {
    TransitionSnapshot {
        event_id: row.get("event_id"),
        event_kind: row.get("event_kind"),
        event_payload: row.get("event_payload"),
        instance_id: row.get("instance_id"),
        process_name: row.get("process_name"),
        revision: row.get("revision"),
        current_state: row.get("current_state"),
        input: row.get("input_json"),
        state: row.get("state_json"),
        version: row.get("version"),
        caller_role: row.get("caller_role"),
        caller_session: row.get("caller_session_json"),
        workflow_time: row.get("workflow_time"),
        attempts: row.get("attempts"),
    }
}

fn process_value_context<'a>(
    source_name: &'a str,
    snapshot: &'a TransitionSnapshot,
) -> ProcessValueContext<'a> {
    ProcessValueContext {
        source_name,
        instance_id: snapshot.instance_id,
        input: &snapshot.input,
        state: &snapshot.state,
        caller_session: snapshot.caller_session.as_ref(),
        workflow_time: &snapshot.workflow_time,
        item: None,
        item_key: None,
    }
}

fn fanout_value_context<'a>(
    source_name: &'a str,
    snapshot: &'a TransitionSnapshot,
    item: &'a Json,
    item_key_identity: &'a str,
) -> ProcessValueContext<'a> {
    ProcessValueContext {
        source_name,
        instance_id: snapshot.instance_id,
        input: &snapshot.input,
        state: &snapshot.state,
        caller_session: snapshot.caller_session.as_ref(),
        workflow_time: &snapshot.workflow_time,
        item: Some(item),
        item_key: Some(item_key_identity),
    }
}

struct CanonicalFanOutItemKey {
    identity: String,
    output: String,
}

fn canonical_fanout_item_key(value: &Json) -> Option<CanonicalFanOutItemKey> {
    let output = match value {
        Json::String(value) => value.clone(),
        Json::Number(value) => value.to_string(),
        Json::Bool(value) => value.to_string(),
        _ => return None,
    };
    Some(CanonicalFanOutItemKey {
        identity: serde_json::to_string(value)
            .expect("a scalar Process fan-out item key always serializes"),
        output,
    })
}

fn fanout_event_matches_snapshot(snapshot: &TransitionSnapshot) -> bool {
    snapshot
        .event_payload
        .get("fanout_state")
        .and_then(Json::as_str)
        == Some(snapshot.current_state.as_str())
        && snapshot
            .event_payload
            .get("fanout_version")
            .and_then(Json::as_i64)
            == Some(snapshot.version)
}

fn fanout_event_ordinal(snapshot: &TransitionSnapshot) -> anyhow::Result<i32> {
    snapshot
        .event_payload
        .get("ordinal")
        .and_then(Json::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| *value >= 0)
        .ok_or_else(|| anyhow!("Process fan-out item event has no valid ordinal"))
}

fn fanout_event_item_key_identity(snapshot: &TransitionSnapshot) -> anyhow::Result<&str> {
    snapshot
        .event_payload
        .get("item_key_identity")
        .and_then(Json::as_str)
        .ok_or_else(|| anyhow!("Process fan-out item event has no stable item identity"))
}

fn fanout_event_entry_id(snapshot: &TransitionSnapshot) -> anyhow::Result<Uuid> {
    snapshot
        .event_payload
        .get("entry_event_id")
        .and_then(Json::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| anyhow!("Process fan-out item event has no valid entry event ID"))
}

fn empty_fanout_output() -> Json {
    json!({
        "successful_items": [],
        "failed_items": [],
        "ordered_results": [],
    })
}

fn fanout_logical_activity_id(snapshot: &TransitionSnapshot, item_key_identity: &str) -> String {
    format!(
        "fanout:v1:{}:{}:{}:{}",
        snapshot.revision,
        snapshot.instance_id,
        snapshot.current_state,
        canonical_json_sha256(&Json::String(item_key_identity.to_owned())),
    )
}

fn fanout_item_uuid(domain: &[u8], snapshot: &TransitionSnapshot, item_key_identity: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(snapshot.revision.as_bytes());
    digest.update(b"\0");
    digest.update(snapshot.instance_id.as_bytes());
    digest.update(b"\0");
    digest.update(snapshot.current_state.as_bytes());
    digest.update(b"\0");
    digest.update(item_key_identity.as_bytes());
    let digest = digest.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

fn merge_fanout_success_item(
    item: &Json,
    result: &Json,
    preserve_input: bool,
) -> anyhow::Result<Json> {
    let result = result
        .as_object()
        .ok_or_else(|| anyhow!("Process fan-out activity result is not an object"))?;
    if !preserve_input {
        return Ok(Json::Object(result.clone()));
    }
    let mut merged = item
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("Process fan-out item is not an object"))?;
    for (name, value) in result {
        if merged.get(name).is_some_and(|existing| existing != value) {
            bail!("Process fan-out result field `{name}` conflicts with preserved input");
        }
        merged.insert(name.clone(), value.clone());
    }
    Ok(Json::Object(merged))
}

fn fanout_failure_output(
    item: &Json,
    item_key: &str,
    stage: &str,
    code: &str,
    safe_message: &str,
    requires_reconciliation: bool,
    activity_key: &str,
) -> anyhow::Result<Json> {
    let mut output = item
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("Process fan-out failure item is not an object"))?;
    output.insert("item_key".to_owned(), Json::String(item_key.to_owned()));
    output.insert("stage".to_owned(), Json::String(stage.to_owned()));
    output.insert("code".to_owned(), Json::String(code.to_owned()));
    output.insert(
        "safe_message".to_owned(),
        Json::String(safe_message.to_owned()),
    );
    output.insert(
        "requires_reconciliation".to_owned(),
        Json::Bool(requires_reconciliation),
    );
    output.insert(
        "activity_key".to_owned(),
        Json::String(activity_key.to_owned()),
    );
    Ok(Json::Object(output))
}

fn fanout_request_error_route(
    request: &CompiledProcessRequestState,
    error_kind: &str,
    default: &str,
) -> String {
    let Some(routes) = request.on_error.as_ref() else {
        return default.to_owned();
    };
    routes
        .routes
        .iter()
        .find(|route| {
            route
                .kinds
                .iter()
                .any(|kind| process_error_kind_name(kind) == error_kind)
        })
        .map(|route| route.next.clone())
        .unwrap_or_else(|| routes.fallback.next.clone())
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn prepare_signal_deadline(
    deadline: &CompiledProcessSignalDeadline,
    context: &ProcessValueContext<'_>,
) -> anyhow::Result<PreparedTimerSchedule> {
    match deadline {
        CompiledProcessSignalDeadline::AfterMilliseconds(milliseconds) => {
            Ok(PreparedTimerSchedule::AfterMilliseconds(
                i64::try_from(*milliseconds)
                    .context("Process signal deadline exceeds PostgreSQL interval input")?,
            ))
        }
        CompiledProcessSignalDeadline::At { value, kind } => {
            let value = evaluate_process_value(value, context)
                .context("evaluating absolute Process signal deadline")?;
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("absolute Process signal deadline is not a string"))?;
            let at = match kind {
                CompiledProcessTimestampKind::TimestampTz => DateTime::parse_from_rfc3339(value)
                    .map_err(|error| {
                        anyhow!("invalid Process timestamptz deadline `{value}`: {error}")
                    })?
                    .with_timezone(&Utc),
                CompiledProcessTimestampKind::Timestamp => {
                    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                        .map_err(|error| {
                            anyhow!("invalid Process timestamp deadline `{value}`: {error}")
                        })?
                        .and_utc()
                }
            };
            Ok(PreparedTimerSchedule::At(at))
        }
    }
}

fn wait_event_matches_snapshot(snapshot: &TransitionSnapshot) -> bool {
    snapshot
        .event_payload
        .get("wait_state")
        .and_then(Json::as_str)
        == Some(snapshot.current_state.as_str())
        && snapshot
            .event_payload
            .get("wait_version")
            .and_then(Json::as_i64)
            == Some(snapshot.version)
}

fn state_output(state: &Json, state_name: &str) -> anyhow::Result<Json> {
    state
        .as_object()
        .and_then(|state| state.get(state_name))
        .cloned()
        .ok_or_else(|| anyhow!("compiled Process ancestor state `{state_name}` has no output"))
}

fn json_matches(
    actual: &Json,
    expected: &std::collections::BTreeMap<String, Json>,
) -> anyhow::Result<bool> {
    let object = actual
        .as_object()
        .ok_or_else(|| anyhow!("compiled Process when comparison is not an object"))?;
    Ok(expected
        .iter()
        .all(|(field, expected)| object.get(field) == Some(expected)))
}

fn process_error_kind_name(kind: &ProcessErrorKind) -> &'static str {
    match kind {
        ProcessErrorKind::Authentication => "authentication",
        ProcessErrorKind::Transport => "transport",
        ProcessErrorKind::Timeout => "timeout",
        ProcessErrorKind::Http429 => "http_429",
        ProcessErrorKind::Http5xx => "http_5xx",
        ProcessErrorKind::Validation => "validation",
        ProcessErrorKind::Permanent => "permanent",
        ProcessErrorKind::Invariant => "invariant",
        ProcessErrorKind::RetryExhausted => "retry_exhausted",
    }
}

fn deterministic_event_kind(kind: &str) -> bool {
    matches!(kind, "start" | "continue")
}

fn validate_state_output(
    definition: &CompiledProcessDefinition,
    snapshot: &TransitionSnapshot,
    output: &Json,
) -> anyhow::Result<()> {
    let typed = typed_value(output).context("decoding Process state output")?;
    definition
        .states
        .get(&snapshot.current_state)
        .ok_or_else(|| {
            anyhow!(
                "compiled Process state `{}` disappeared",
                snapshot.current_state
            )
        })?
        .output
        .validate(&typed)
        .map_err(|error| anyhow!("Process state output violated its compiled contract: {error}"))
}

fn process_command_session(
    definition: &CompiledProcessDefinition,
    role: &CompiledProcessCommandRole,
    snapshot: &TransitionSnapshot,
) -> anyhow::Result<Session> {
    match role {
        CompiledProcessCommandRole::Fixed { role } => Ok(Session {
            role: role.clone(),
            vars: HashMap::new(),
            backend_request: false,
        }),
        CompiledProcessCommandRole::Caller {
            required_session_variables,
        } => {
            let role = snapshot
                .caller_role
                .as_ref()
                .ok_or_else(|| anyhow!("caller Process command has no persisted caller role"))?;
            let required = required_session_variables.get(role).ok_or_else(|| {
                anyhow!("persisted caller role `{role}` is not a declared Process role")
            })?;
            let complete_required = definition
                .caller_session_variables
                .get(role)
                .ok_or_else(|| anyhow!("caller role `{role}` has no compiled session contract"))?;
            let object = snapshot
                .caller_session
                .as_ref()
                .and_then(Json::as_object)
                .ok_or_else(|| anyhow!("caller Process command has no persisted session object"))?;
            let actual = object
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>();
            if actual != *complete_required {
                bail!(
                    "persisted Process caller session does not match its compiled closed contract"
                );
            }
            let vars = required
                .iter()
                .map(|name| {
                    let value = object.get(name).and_then(Json::as_str).ok_or_else(|| {
                        anyhow!("persisted Process session variable `{name}` is not a string")
                    })?;
                    Ok((name.clone(), value.to_owned()))
                })
                .collect::<anyhow::Result<HashMap<_, _>>>()?;
            Ok(Session {
                role: role.clone(),
                vars,
                backend_request: false,
            })
        }
    }
}

async fn lock_prepared_snapshot(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
) -> anyhow::Result<bool> {
    let Some(row) = transaction
        .query_opt(
            "
            SELECT instance.version, instance.current_state
            FROM donat.process_events event
            JOIN donat.process_instances instance
              ON instance.source_name = event.source_name
             AND instance.id = event.instance_id
             AND instance.process_name = event.process_name
             AND instance.revision = event.revision
            WHERE event.source_name = $1
              AND event.id = $2
              AND event.instance_id = $3
              AND event.status = 'pending'
              AND event.kind = $6
              AND event.kind IN (
                  'start',
                  'continue',
                  'signal',
                  'timer',
                  'fanout_item',
                  'activity_succeeded',
                  'activity_failed',
                  'retry_exhausted'
              )
              AND event.available_at <= statement_timestamp()
              AND instance.status = 'running'
              AND instance.process_name = $4
              AND instance.revision = $5
            -- Another worker holding this instance is not a reason to wait:
            -- the loser goes back for a different one, which is what makes a
            -- second worker worth having. One instance's transitions stay
            -- serialized because that worker holds the row.
            FOR UPDATE OF event, instance SKIP LOCKED
            ",
            &[
                &source_name,
                &snapshot.event_id,
                &snapshot.instance_id,
                &snapshot.process_name,
                &snapshot.revision,
                &snapshot.event_kind,
            ],
        )
        .await
        .context("locking prepared Process event and instance")?
    else {
        return Ok(false);
    };
    let version: i64 = row.get("version");
    let current_state: String = row.get("current_state");
    if version != snapshot.version || current_state != snapshot.current_state {
        return Ok(false);
    }
    Ok(true)
}

async fn commit_applied_command(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedCommandTransition,
    result: &Json,
) -> anyhow::Result<()> {
    advance_instance(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.state.next,
        result,
    )
    .await?;
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.state.next,
        "command_applied",
        Some(result),
        json!({ "command": prepared.state.name }),
    )
    .await?;
    append_continue_event(
        transaction,
        source_name,
        &prepared.snapshot,
        next_version(&prepared.snapshot)?,
    )
    .await
}

async fn commit_wait_entry(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedWaitEntry,
) -> anyhow::Result<Uuid> {
    let version = next_version(&prepared.snapshot)?;
    let (available_at, delay_milliseconds) = match prepared.schedule {
        PreparedTimerSchedule::AfterMilliseconds(milliseconds) => (None, Some(milliseconds)),
        PreparedTimerSchedule::At(at) => (Some(at), None),
    };
    let idempotency_key = format!(
        "wait-timer:{}:{}:{version}",
        prepared.snapshot.instance_id, prepared.snapshot.current_state
    );
    let timer_event_id: Uuid = transaction
        .query_one(
            "
            WITH wait_clock AS (
                SELECT statement_timestamp() AS at
            )
            INSERT INTO donat.process_events (
                source_name,
                instance_id,
                process_name,
                revision,
                kind,
                payload_json,
                idempotency_key,
                available_at,
                status,
                created_at
            )
            SELECT
                $1,
                $2,
                $3,
                $4,
                'timer',
                $5,
                $6,
                COALESCE(
                    $7::timestamptz,
                    wait_clock.at + ($8::bigint * interval '1 millisecond')
                ),
                'pending',
                wait_clock.at
            FROM wait_clock
            RETURNING id
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.process_name,
                &prepared.snapshot.revision,
                &prepared.timer_payload,
                &idempotency_key,
                &available_at,
                &delay_milliseconds,
            ],
        )
        .await
        .context("scheduling durable Process wait timer")?
        .get("id");

    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET version = $4,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $5
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.current_state,
                &version,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("committing receptive Process wait version")?;
    if updated != 1 {
        bail!("locked Process wait state did not become receptive exactly once");
    }
    if prepared.persist_before_match {
        reopen_persisted_signals(transaction, source_name, prepared).await?;
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "wait_entered",
        None,
        json!({ "timer_event_id": timer_event_id.to_string() }),
    )
    .await?;
    Ok(timer_event_id)
}

/// Offer this wait the signals that arrived before it existed.
///
/// A signal committed while the instance was still working its way to the wait
/// finds nothing receptive. `persist_before_match` is the Process saying that
/// such a signal is not lost: entering the wait returns those exact correlated
/// requests to `pending`, and the ordinary consumer matches them against the
/// marker that now exists.
///
/// Two statuses, because "nothing receptive" has two spellings and the wider
/// window produces the second one. `unmatched` is a signal that found no
/// instance at all. `unexpected_state` is a signal that found the instance —
/// already sitting at this very wait state — while its timer marker had not
/// yet been inserted by the later `continue` event. Reviving only `unmatched`
/// left every signal that landed inside that one queue hop dropped for good,
/// and the instance waited out its deadline for a signal that had arrived.
async fn reopen_persisted_signals(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedWaitEntry,
) -> anyhow::Result<u64> {
    let Some(signal_name) = prepared.timer_payload["signal_name"].as_str() else {
        return Ok(0);
    };
    let correlation = prepared.timer_payload["correlation"].clone();
    let reopened = transaction
        .execute(
            "
            UPDATE donat.process_signal_requests
            SET status = 'pending'
            WHERE source_name = $1
              AND process_name = $2
              AND signal_name = $3
              AND correlation_json = $4
              AND status IN ('unmatched', 'unexpected_state')
            ",
            &[
                &source_name,
                &prepared.snapshot.process_name,
                &signal_name,
                &correlation,
            ],
        )
        .await
        .context("returning early Process signals to the queue")?;
    Ok(reopened)
}

async fn commit_wait_completion(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedWaitCompletion,
) -> anyhow::Result<()> {
    if let Some(output) = &prepared.output {
        advance_instance(
            transaction,
            source_name,
            &prepared.snapshot,
            &prepared.next,
            output,
        )
        .await?;
    } else {
        advance_instance_without_output(
            transaction,
            source_name,
            &prepared.snapshot,
            &prepared.next,
        )
        .await?;
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    let closed_competitors =
        close_competing_wait_events(transaction, source_name, &prepared.snapshot).await?;
    let mut redacted_context = prepared
        .redacted_context
        .as_object()
        .cloned()
        .unwrap_or_default();
    redacted_context.insert(
        "closed_competing_events".to_owned(),
        Json::from(closed_competitors),
    );
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.next,
        prepared.outcome,
        None,
        Json::Object(redacted_context),
    )
    .await?;
    append_continue_event(
        transaction,
        source_name,
        &prepared.snapshot,
        next_version(&prepared.snapshot)?,
    )
    .await
}

async fn close_competing_wait_events(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
) -> anyhow::Result<u64> {
    transaction
        .execute(
            "
            UPDATE donat.process_events
            SET status = 'failed',
                consumed_at = statement_timestamp(),
                attempts = attempts + 1
            WHERE source_name = $1
              AND instance_id = $2
              AND id <> $3
              AND status = 'pending'
              AND kind IN ('signal', 'timer')
              AND payload_json ->> 'wait_state' = $4
              AND payload_json ->> 'wait_version' = $5
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.event_id,
                &snapshot.current_state,
                &snapshot.version.to_string(),
            ],
        )
        .await
        .context("closing stale Process wait competitors")
}

async fn commit_request_schedule(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedRequestTransition,
) -> anyhow::Result<Uuid> {
    let version = next_version(&prepared.snapshot)?;
    let schedule_to_start_ms = i64::try_from(prepared.state.schedule_to_start_ms)
        .context("Process schedule_to_start exceeds PostgreSQL interval input")?;
    let operation = prepared.state.operation.as_str();
    let logical_activity_id = format!(
        "activity:v1:{}:{}:{}:{}",
        prepared.snapshot.revision,
        prepared.snapshot.instance_id,
        prepared.snapshot.event_id,
        prepared.snapshot.current_state
    );
    let activity_job_id = transaction
        .query_one(
            "
            WITH activity_clock AS (
                SELECT statement_timestamp() AS at
            )
            INSERT INTO donat.process_activity_jobs (
                source_name,
                instance_id,
                enqueued_from_event_id,
                state_name,
                logical_activity_id,
                connector_instance,
                operation,
                serialization_key_hash,
                input_json,
                request_fingerprint,
                status,
                available_at,
                schedule_to_start_deadline,
                created_at,
                updated_at
            )
            SELECT
                $1,
                $2,
                $3,
                $4,
                $5,
                $6,
                $7,
                $8,
                $9,
                $10,
                'scheduled',
                activity_clock.at,
                activity_clock.at + ($11::bigint * interval '1 millisecond'),
                activity_clock.at,
                activity_clock.at
            FROM activity_clock
            RETURNING id
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.event_id,
                &prepared.snapshot.current_state,
                &logical_activity_id,
                &prepared.state.connector,
                &operation,
                &prepared.serialization_key_hash,
                &prepared.input,
                &prepared.request_fingerprint,
                &schedule_to_start_ms,
            ],
        )
        .await
        .context("scheduling durable Process activity")?
        .get("id");

    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET version = $4,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $5
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.current_state,
                &version,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("committing Process activity schedule version")?;
    if updated != 1 {
        bail!("locked Process request state did not schedule exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_activity_scheduled_log(transaction, source_name, prepared, activity_job_id).await?;
    Ok(activity_job_id)
}

async fn commit_request_success(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedRequestSuccessTransition,
) -> anyhow::Result<()> {
    advance_instance(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.state.next,
        &prepared.output,
    )
    .await?;
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                event_id,
                activity_job_id,
                activity_attempt,
                activity_lease_generation,
                from_state,
                to_state,
                outcome,
                definition_revision,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                    'activity_succeeded', $9, $10)
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.event_id,
                &prepared.activity_job_id,
                &prepared.attempt,
                &prepared.lease_generation,
                &prepared.snapshot.current_state,
                &prepared.state.next,
                &prepared.snapshot.revision,
                &json!({ "activity_job_id": prepared.activity_job_id.to_string() }),
            ],
        )
        .await
        .context("appending Process activity success transition log")?;
    append_continue_event(
        transaction,
        source_name,
        &prepared.snapshot,
        next_version(&prepared.snapshot)?,
    )
    .await
}

async fn commit_request_failure(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedRequestFailureTransition,
) -> anyhow::Result<()> {
    let version = next_version(&prepared.snapshot)?;
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET current_state = $4,
                version = $5,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.current_state,
                &prepared.next,
                &version,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("routing failed Process activity")?;
    if updated != 1 {
        bail!("locked Process activity failure did not route exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                event_id,
                activity_job_id,
                activity_attempt,
                activity_lease_generation,
                from_state,
                to_state,
                outcome,
                definition_revision,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                    'activity_error_routed', $9, $10)
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.event_id,
                &prepared.activity_job_id,
                &prepared.attempt,
                &prepared.lease_generation,
                &prepared.snapshot.current_state,
                &prepared.next,
                &prepared.snapshot.revision,
                &json!({
                    "error_kind": prepared.error_kind,
                    "route": prepared.next,
                }),
            ],
        )
        .await
        .context("appending Process activity error route log")?;
    append_continue_event(
        transaction,
        source_name,
        &prepared.snapshot,
        next_version(&prepared.snapshot)?,
    )
    .await
}

async fn commit_fanout_expansion(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedFanOutExpansion,
) -> anyhow::Result<TransitionConsumption> {
    if prepared.items.is_empty() {
        let output = empty_fanout_output();
        validate_state_output(&prepared.definition, &prepared.snapshot, &output)
            .context("validating empty Process fan-out output")?;
        advance_instance(
            transaction,
            source_name,
            &prepared.snapshot,
            &prepared.state.next,
            &output,
        )
        .await?;
        consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
        append_transition_log(
            transaction,
            source_name,
            &prepared.snapshot,
            &prepared.state.next,
            "fanout_completed",
            None,
            json!({ "item_count": 0, "scheduled_count": 0 }),
        )
        .await?;
        append_continue_event(
            transaction,
            source_name,
            &prepared.snapshot,
            next_version(&prepared.snapshot)?,
        )
        .await?;
        return Ok(TransitionConsumption::Advanced {
            instance_id: prepared.snapshot.instance_id,
            event_id: prepared.snapshot.event_id,
            from_state: prepared.snapshot.current_state.clone(),
            to_state: prepared.state.next.clone(),
        });
    }

    let version = next_version(&prepared.snapshot)?;
    let scheduled_count = prepared
        .items
        .len()
        .min(prepared.state.max_concurrency as usize);
    match &prepared.state.activity {
        CompiledProcessForEachActivity::Request(request) => {
            insert_request_fanout_items(
                transaction,
                source_name,
                prepared,
                request,
                scheduled_count,
            )
            .await?;
        }
        CompiledProcessForEachActivity::Command(_) => {
            insert_command_fanout_items(
                transaction,
                source_name,
                prepared,
                version,
                scheduled_count,
            )
            .await?;
        }
    }

    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET version = $4,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $5
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.current_state,
                &version,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("committing Process fan-out expansion version")?;
    if updated != 1 {
        bail!("locked Process fan-out state did not expand exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "fanout_expanded",
        None,
        json!({
            "item_count": prepared.items.len(),
            "scheduled_count": scheduled_count,
            "max_concurrency": prepared.state.max_concurrency,
        }),
    )
    .await?;
    Ok(TransitionConsumption::FanOutExpanded {
        instance_id: prepared.snapshot.instance_id,
        event_id: prepared.snapshot.event_id,
        state: prepared.snapshot.current_state.clone(),
        item_count: prepared.items.len(),
        scheduled_count,
    })
}

async fn insert_request_fanout_items(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedFanOutExpansion,
    request: &CompiledProcessRequestState,
    scheduled_count: usize,
) -> anyhow::Result<()> {
    let schedule_to_start_ms = i64::try_from(request.schedule_to_start_ms)
        .context("fan-out schedule_to_start exceeds PostgreSQL interval input")?;
    let descriptors = Json::Array(
        prepared
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let request = item
                    .request
                    .as_ref()
                    .expect("request fan-out item has a prepared activity input");
                let job_id = fanout_item_uuid(
                    b"donat.process.fanout-job.v1\0",
                    &prepared.snapshot,
                    &item.item_key_identity,
                );
                json!({
                    "job_id": job_id.to_string(),
                    "ordinal": item.ordinal,
                    "item_key": item.item_key,
                    "item_key_identity": item.item_key_identity,
                    "item": item.item,
                    "input": request.input,
                    "request_fingerprint": request.request_fingerprint,
                    "serialization_key_hash": request
                        .serialization_key_hash
                        .as_deref()
                        .map(lower_hex),
                    "logical_activity_id": fanout_logical_activity_id(
                        &prepared.snapshot,
                        &item.item_key_identity,
                    ),
                    "active": index < scheduled_count,
                })
            })
            .collect(),
    );
    let operation = request.operation.as_str();
    let rows = transaction
        .query(
            "
            WITH fanout_clock AS (
                SELECT statement_timestamp() AS at
            ),
            descriptors AS (
                SELECT *
                FROM jsonb_to_recordset($8::jsonb) AS item(
                    job_id uuid,
                    ordinal integer,
                    item_key text,
                    item_key_identity text,
                    item jsonb,
                    input jsonb,
                    request_fingerprint text,
                    serialization_key_hash text,
                    logical_activity_id text,
                    active boolean
                )
            ),
            inserted_jobs AS (
                INSERT INTO donat.process_activity_jobs (
                    source_name,
                    id,
                    instance_id,
                    enqueued_from_event_id,
                    state_name,
                    logical_activity_id,
                    connector_instance,
                    operation,
                    serialization_key_hash,
                    input_json,
                    request_fingerprint,
                    status,
                    available_at,
                    schedule_to_start_deadline,
                    created_at,
                    updated_at
                )
                SELECT
                    $1,
                    descriptor.job_id,
                    $2,
                    $3,
                    $4,
                    descriptor.logical_activity_id,
                    $5,
                    $6,
                    CASE
                        WHEN descriptor.serialization_key_hash IS NULL THEN NULL
                        ELSE decode(descriptor.serialization_key_hash, 'hex')
                    END,
                    descriptor.input,
                    descriptor.request_fingerprint,
                    'scheduled',
                    CASE
                        WHEN descriptor.active THEN fanout_clock.at
                        ELSE 'infinity'::timestamptz
                    END,
                    CASE
                        WHEN descriptor.active
                            THEN fanout_clock.at
                              + ($7::bigint * interval '1 millisecond')
                        ELSE 'infinity'::timestamptz
                    END,
                    fanout_clock.at,
                    fanout_clock.at
                FROM descriptors descriptor
                CROSS JOIN fanout_clock
                RETURNING id
            )
            INSERT INTO donat.process_fanout_items (
                source_name,
                instance_id,
                state_name,
                entry_event_id,
                ordinal,
                item_key,
                item_key_identity,
                item_json,
                status,
                activity_job_id
            )
            SELECT
                $1,
                $2,
                $4,
                $3,
                descriptor.ordinal,
                descriptor.item_key,
                descriptor.item_key_identity,
                descriptor.item,
                CASE WHEN descriptor.active THEN 'scheduled' ELSE 'pending' END,
                descriptor.job_id
            FROM descriptors descriptor
            JOIN inserted_jobs job ON job.id = descriptor.job_id
            RETURNING ordinal
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.event_id,
                &prepared.snapshot.current_state,
                &request.connector,
                &operation,
                &schedule_to_start_ms,
                &descriptors,
            ],
        )
        .await
        .context("persisting bounded Process request fan-out")?;
    if rows.len() != prepared.items.len() {
        bail!("Process request fan-out did not persist every bounded item");
    }
    Ok(())
}

async fn insert_command_fanout_items(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedFanOutExpansion,
    version: i64,
    scheduled_count: usize,
) -> anyhow::Result<()> {
    let descriptors = Json::Array(
        prepared
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let active = index < scheduled_count;
                json!({
                    "ordinal": item.ordinal,
                    "item_key": item.item_key,
                    "item_key_identity": item.item_key_identity,
                    "item": item.item,
                    "active": active,
                    "payload": active.then(|| json!({
                        "fanout_state": prepared.snapshot.current_state,
                        "fanout_version": version,
                        "entry_event_id": prepared.snapshot.event_id.to_string(),
                        "ordinal": item.ordinal,
                        "item_key_identity": item.item_key_identity,
                    })),
                    "idempotency_key": active.then(|| format!(
                        "fanout-item:{}:{}:{}:{}",
                        prepared.snapshot.instance_id,
                        prepared.snapshot.current_state,
                        version,
                        canonical_json_sha256(&Json::String(item.item_key_identity.clone())),
                    )),
                })
            })
            .collect(),
    );
    let rows = transaction
        .query(
            "
            WITH descriptors AS (
                SELECT *
                FROM jsonb_to_recordset($7::jsonb) AS item(
                    ordinal integer,
                    item_key text,
                    item_key_identity text,
                    item jsonb,
                    active boolean,
                    payload jsonb,
                    idempotency_key text
                )
            ),
            inserted_items AS (
                INSERT INTO donat.process_fanout_items (
                    source_name,
                    instance_id,
                    state_name,
                    entry_event_id,
                    ordinal,
                    item_key,
                    item_key_identity,
                    item_json,
                    status
                )
                SELECT
                    $1,
                    $2,
                    $5,
                    $3,
                    descriptor.ordinal,
                    descriptor.item_key,
                    descriptor.item_key_identity,
                    descriptor.item,
                    CASE WHEN descriptor.active THEN 'scheduled' ELSE 'pending' END
                FROM descriptors descriptor
                RETURNING ordinal
            ),
            inserted_events AS (
                INSERT INTO donat.process_events (
                    source_name,
                    instance_id,
                    process_name,
                    revision,
                    kind,
                    payload_json,
                    idempotency_key,
                    status
                )
                SELECT
                    $1,
                    $2,
                    $4,
                    $6,
                    'fanout_item',
                    descriptor.payload,
                    descriptor.idempotency_key,
                    'pending'
                FROM descriptors descriptor
                WHERE descriptor.active
                RETURNING id
            )
            SELECT ordinal
            FROM inserted_items
            ORDER BY ordinal
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.event_id,
                &prepared.snapshot.process_name,
                &prepared.snapshot.current_state,
                &prepared.snapshot.revision,
                &descriptors,
            ],
        )
        .await
        .context("persisting bounded Process command fan-out")?;
    if rows.len() != prepared.items.len() {
        bail!("Process command fan-out did not persist every bounded item");
    }
    Ok(())
}

async fn commit_fanout_failure(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedFanOutFailure,
) -> anyhow::Result<()> {
    let version = next_version(&prepared.snapshot)?;
    let failure = json!({
        "kind": "process_failed",
        "code": prepared.code,
        "message": prepared.message,
    });
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET status = 'failed',
                failure_json = $4,
                version = $5,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.current_state,
                &failure,
                &version,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("failing invalid bounded Process fan-out")?;
    if updated != 1 {
        bail!("locked invalid Process fan-out did not fail exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "fanout_invalid",
        None,
        json!({ "code": prepared.code }),
    )
    .await
}

async fn commit_fanout_request_completion(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedFanOutRequestCompletion,
) -> anyhow::Result<TransitionConsumption> {
    let request = match &prepared.state.activity {
        CompiledProcessForEachActivity::Request(request) => request.as_ref(),
        CompiledProcessForEachActivity::Command(_) => {
            bail!("prepared request fan-out completion retained a command activity")
        }
    };
    let outcome = match &prepared.result {
        Ok(result) => {
            if merge_fanout_success_item(&prepared.item, result, prepared.state.preserve_input)
                .is_err()
            {
                Err(FanOutItemFailure {
                    output: fanout_failure_output(
                        &prepared.item,
                        &prepared.item_key,
                        "request",
                        "fanout_result_conflict",
                        "the fan-out result conflicted with preserved input",
                        true,
                        &fanout_logical_activity_id(
                            &prepared.snapshot,
                            &prepared.item_key_identity,
                        ),
                    )?,
                    route: fanout_request_error_route(request, "invariant", &prepared.state.next),
                    error_kind: "invariant".to_owned(),
                })
            } else {
                Ok(result.clone())
            }
        }
        Err(error) => {
            let error_kind = if prepared.snapshot.event_kind == "retry_exhausted" {
                "retry_exhausted"
            } else {
                error
                    .get("class")
                    .and_then(Json::as_str)
                    .unwrap_or("invariant")
            };
            let code = error
                .get("code")
                .and_then(Json::as_str)
                .unwrap_or("activity_failed");
            let safe_message = error
                .get("safe_message")
                .and_then(Json::as_str)
                .unwrap_or("the connector activity failed");
            Err(FanOutItemFailure {
                output: fanout_failure_output(
                    &prepared.item,
                    &prepared.item_key,
                    "request",
                    code,
                    safe_message,
                    true,
                    &fanout_logical_activity_id(&prepared.snapshot, &prepared.item_key_identity),
                )?,
                route: fanout_request_error_route(request, error_kind, &prepared.state.next),
                error_kind: error_kind.to_owned(),
            })
        }
    };
    commit_fanout_item_completion(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.definition,
        &prepared.state,
        prepared.ordinal,
        &prepared.item_key,
        &prepared.item_key_identity,
        outcome,
        Some((
            prepared.activity_job_id,
            prepared.attempt,
            prepared.lease_generation,
        )),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn commit_fanout_item_completion(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    definition: &CompiledProcessDefinition,
    state: &CompiledProcessForEachState,
    ordinal: i32,
    item_key: &str,
    item_key_identity: &str,
    outcome: Result<Json, FanOutItemFailure>,
    activity: Option<(Uuid, i32, i64)>,
) -> anyhow::Result<TransitionConsumption> {
    let (status, result, failure, route, error_kind) = match &outcome {
        Ok(result) => ("succeeded", Some(result), None, None, None),
        Err(failure) => (
            "failed",
            None,
            Some(json!({
                "output": failure.output,
                "route": failure.route,
                "error_kind": failure.error_kind,
            })),
            Some(failure.route.as_str()),
            Some(failure.error_kind.as_str()),
        ),
    };
    let activity_job_id = activity.map(|value| value.0);
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_fanout_items
            SET status = $7,
                result_json = $8,
                failure_json = $9,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND instance_id = $2
              AND state_name = $3
              AND ordinal = $4
              AND item_key = $5
              AND item_key_identity = $6
              AND status = 'scheduled'
              AND activity_job_id IS NOT DISTINCT FROM $10
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.current_state,
                &ordinal,
                &item_key,
                &item_key_identity,
                &status,
                &result,
                &failure,
                &activity_job_id,
            ],
        )
        .await
        .context("committing terminal Process fan-out item")?;
    if updated != 1 {
        bail!("locked Process fan-out item did not complete exactly once");
    }
    consume_event(transaction, source_name, snapshot.event_id).await?;
    let activated_next =
        activate_next_fanout_item(transaction, source_name, snapshot, state).await?;
    let unfinished: i64 = transaction
        .query_one(
            "
            SELECT count(*)
            FROM donat.process_fanout_items
            WHERE source_name = $1
              AND instance_id = $2
              AND state_name = $3
              AND status IN ('pending', 'scheduled')
            ",
            &[&source_name, &snapshot.instance_id, &snapshot.current_state],
        )
        .await
        .context("counting unfinished Process fan-out items")?
        .get(0);
    if unfinished > 0 {
        append_fanout_transition_log(
            transaction,
            source_name,
            snapshot,
            &snapshot.current_state,
            if outcome.is_ok() {
                "fanout_item_succeeded"
            } else {
                "fanout_item_failed"
            },
            match &outcome {
                Ok(result)
                    if matches!(&state.activity, CompiledProcessForEachActivity::Command(_)) =>
                {
                    Some(result)
                }
                _ => None,
            },
            activity,
            json!({
                "ordinal": ordinal,
                "item_key": item_key,
                "activated_next": activated_next,
                "activity_job_id": activity_job_id.map(|id| id.to_string()),
                "route": route,
                "error_kind": error_kind,
            }),
        )
        .await?;
        return Ok(TransitionConsumption::FanOutItemCompleted {
            instance_id: snapshot.instance_id,
            event_id: snapshot.event_id,
            state: snapshot.current_state.clone(),
            ordinal,
        });
    }

    let rows = transaction
        .query(
            "
            SELECT item_json, status, result_json, failure_json
            FROM donat.process_fanout_items
            WHERE source_name = $1
              AND instance_id = $2
              AND state_name = $3
            ORDER BY ordinal
            ",
            &[&source_name, &snapshot.instance_id, &snapshot.current_state],
        )
        .await
        .context("collecting terminal Process fan-out items")?;
    let mut successful_items = Vec::new();
    let mut failed_items = Vec::new();
    let mut ordered_results = Vec::new();
    let mut next = state.next.clone();
    let mut failure_route_selected = false;
    for row in rows {
        let stored_item: Json = row.get("item_json");
        let stored_status: String = row.get("status");
        match stored_status.as_str() {
            "succeeded" => {
                let result = row
                    .get::<_, Option<Json>>("result_json")
                    .ok_or_else(|| anyhow!("successful Process fan-out item has no result"))?;
                successful_items.push(merge_fanout_success_item(
                    &stored_item,
                    &result,
                    state.preserve_input,
                )?);
                ordered_results.push(result);
            }
            "failed" => {
                let failure = row
                    .get::<_, Option<Json>>("failure_json")
                    .ok_or_else(|| anyhow!("failed Process fan-out item has no safe failure"))?;
                failed_items.push(
                    failure
                        .get("output")
                        .cloned()
                        .ok_or_else(|| anyhow!("fan-out failure has no output"))?,
                );
                if !failure_route_selected
                    && let Some(route) = failure.get("route").and_then(Json::as_str)
                {
                    next = route.to_owned();
                    failure_route_selected = true;
                }
            }
            other => bail!("terminal Process fan-out retained unfinished item `{other}`"),
        }
    }
    let output = json!({
        "successful_items": successful_items,
        "failed_items": failed_items,
        "ordered_results": ordered_results,
    });
    validate_state_output(definition, snapshot, &output)
        .context("validating collected Process fan-out output")?;
    advance_instance(transaction, source_name, snapshot, &next, &output).await?;
    append_fanout_transition_log(
        transaction,
        source_name,
        snapshot,
        &next,
        "fanout_completed",
        match &outcome {
            Ok(result) if matches!(&state.activity, CompiledProcessForEachActivity::Command(_)) => {
                Some(result)
            }
            _ => None,
        },
        activity,
        json!({
            "item_count": successful_items.len() + failed_items.len(),
            "successful_count": successful_items.len(),
            "failed_count": failed_items.len(),
            "final_ordinal": ordinal,
            "activity_job_id": activity_job_id.map(|id| id.to_string()),
            "route": next,
        }),
    )
    .await?;
    append_continue_event(transaction, source_name, snapshot, next_version(snapshot)?).await?;
    Ok(TransitionConsumption::Advanced {
        instance_id: snapshot.instance_id,
        event_id: snapshot.event_id,
        from_state: snapshot.current_state.clone(),
        to_state: next,
    })
}

async fn activate_next_fanout_item(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    state: &CompiledProcessForEachState,
) -> anyhow::Result<bool> {
    match &state.activity {
        CompiledProcessForEachActivity::Command(_) => {
            activate_next_command_fanout_item(transaction, source_name, snapshot).await
        }
        CompiledProcessForEachActivity::Request(request) => {
            activate_next_request_fanout_item(
                transaction,
                source_name,
                snapshot,
                request.schedule_to_start_ms,
            )
            .await
        }
    }
}

async fn activate_next_command_fanout_item(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
) -> anyhow::Result<bool> {
    let row = transaction
        .query_opt(
            "
            WITH next_item AS (
                SELECT ordinal, item_key_identity, entry_event_id
                FROM donat.process_fanout_items
                WHERE source_name = $1
                  AND instance_id = $2
                  AND state_name = $3
                  AND status = 'pending'
                ORDER BY ordinal
                FOR UPDATE
                LIMIT 1
            ),
            activated AS (
                UPDATE donat.process_fanout_items item
                SET status = 'scheduled',
                    updated_at = statement_timestamp()
                FROM next_item
                WHERE item.source_name = $1
                  AND item.instance_id = $2
                  AND item.state_name = $3
                  AND item.ordinal = next_item.ordinal
                RETURNING
                    item.ordinal,
                    item.item_key_identity,
                    item.entry_event_id
            )
            INSERT INTO donat.process_events (
                source_name,
                instance_id,
                process_name,
                revision,
                kind,
                payload_json,
                idempotency_key,
                status
            )
            SELECT
                $1,
                $2,
                $4,
                $5,
                'fanout_item',
                jsonb_build_object(
                    'fanout_state', $3::text,
                    'fanout_version', $6::bigint,
                    'entry_event_id', activated.entry_event_id::text,
                    'ordinal', activated.ordinal,
                    'item_key_identity', activated.item_key_identity
                ),
                'fanout-item:' || $2::text || ':' || $3::text || ':'
                  || ($6::bigint)::text
                  || ':' || activated.ordinal::text,
                'pending'
            FROM activated
            RETURNING id
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.current_state,
                &snapshot.process_name,
                &snapshot.revision,
                &snapshot.version,
            ],
        )
        .await
        .context("activating next Process command fan-out item")?;
    Ok(row.is_some())
}

async fn activate_next_request_fanout_item(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    schedule_to_start_ms: u64,
) -> anyhow::Result<bool> {
    let schedule_to_start_ms = i64::try_from(schedule_to_start_ms)
        .context("fan-out schedule_to_start exceeds PostgreSQL interval input")?;
    let row = transaction
        .query_opt(
            "
            WITH fanout_clock AS (
                SELECT statement_timestamp() AS at
            ),
            next_item AS (
                SELECT item.ordinal, item.activity_job_id
                FROM donat.process_fanout_items item
                JOIN donat.process_activity_jobs job
                  ON job.source_name = item.source_name
                 AND job.id = item.activity_job_id
                WHERE item.source_name = $1
                  AND item.instance_id = $2
                  AND item.state_name = $3
                  AND item.status = 'pending'
                  AND job.status = 'scheduled'
                  AND job.available_at = 'infinity'::timestamptz
                ORDER BY item.ordinal
                FOR UPDATE OF item, job
                LIMIT 1
            ),
            activated_item AS (
                UPDATE donat.process_fanout_items item
                SET status = 'scheduled',
                    updated_at = fanout_clock.at
                FROM next_item, fanout_clock
                WHERE item.source_name = $1
                  AND item.instance_id = $2
                  AND item.state_name = $3
                  AND item.ordinal = next_item.ordinal
                RETURNING item.ordinal, item.activity_job_id
            )
            UPDATE donat.process_activity_jobs job
            SET available_at = fanout_clock.at,
                schedule_to_start_deadline =
                    fanout_clock.at + ($4::bigint * interval '1 millisecond'),
                updated_at = fanout_clock.at
            FROM activated_item, fanout_clock
            WHERE job.source_name = $1
              AND job.id = activated_item.activity_job_id
              AND job.status = 'scheduled'
            RETURNING job.id
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.current_state,
                &schedule_to_start_ms,
            ],
        )
        .await
        .context("activating next Process request fan-out item")?;
    Ok(row.is_some())
}

async fn append_activity_scheduled_log(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedRequestTransition,
    activity_job_id: Uuid,
) -> anyhow::Result<()> {
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                event_id,
                activity_job_id,
                from_state,
                to_state,
                outcome,
                definition_revision,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $5, 'activity_scheduled', $6, $7)
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.event_id,
                &activity_job_id,
                &prepared.snapshot.current_state,
                &prepared.snapshot.revision,
                &json!({
                    "connector": prepared.state.connector,
                    "operation": prepared.state.operation.as_str(),
                    "request_fingerprint": prepared.request_fingerprint,
                }),
            ],
        )
        .await
        .context("appending Process activity schedule log")?;
    Ok(())
}

async fn commit_when(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedWhenTransition,
) -> anyhow::Result<()> {
    advance_instance(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.next,
        &prepared.output,
    )
    .await?;
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.next,
        "when_routed",
        None,
        prepared.redacted_context.clone(),
    )
    .await?;
    append_continue_event(
        transaction,
        source_name,
        &prepared.snapshot,
        next_version(&prepared.snapshot)?,
    )
    .await
}

async fn advance_instance(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    next: &str,
    output: &Json,
) -> anyhow::Result<()> {
    let version = next_version(snapshot)?;
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET current_state = $4,
                state_json = jsonb_set(state_json, ARRAY[$3], $5, TRUE),
                version = $6,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $7
              AND status = 'running'
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.current_state,
                &next,
                &output,
                &version,
                &snapshot.version,
            ],
        )
        .await
        .context("advancing Process state")?;
    if updated != 1 {
        bail!("locked Process instance did not advance exactly once");
    }
    Ok(())
}

async fn advance_instance_without_output(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    next: &str,
) -> anyhow::Result<()> {
    let version = next_version(snapshot)?;
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET current_state = $4,
                version = $5,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.current_state,
                &next,
                &version,
                &snapshot.version,
            ],
        )
        .await
        .context("advancing Process state without an output")?;
    if updated != 1 {
        bail!("locked Process instance did not advance exactly once");
    }
    Ok(())
}

async fn commit_output(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedOutputTransition,
) -> anyhow::Result<()> {
    let version = next_version(&prepared.snapshot)?;
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET status = 'terminal',
                state_json = jsonb_set(state_json, ARRAY[$3], $4, TRUE),
                terminal_output_json = $4,
                version = $5,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $3
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.current_state,
                &prepared.output,
                &version,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("terminalizing Process output state")?;
    if updated != 1 {
        bail!("locked Process output state did not terminalize exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "completed",
        None,
        json!({ "terminal": true }),
    )
    .await
}

async fn commit_fail(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedFailTransition,
) -> anyhow::Result<()> {
    let version = next_version(&prepared.snapshot)?;
    let failure = json!({
        "kind": "process_failed",
        "code": prepared.state.code,
        "message": prepared.state.message,
    });
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET status = 'failed',
                failure_json = $3,
                version = $4,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $5
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &failure,
                &version,
                &prepared.snapshot.current_state,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("committing explicit Process failure")?;
    if updated != 1 {
        bail!("locked Process fail state did not fail exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "failed",
        None,
        json!({ "code": prepared.state.code }),
    )
    .await
}

/// Fail an instance whose Command the database refused unrecoverably.
///
/// The journal keeps the safe code only; the relation and constraint that
/// refused it went to the log, where operator detail belongs.
async fn commit_failed_command(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedCommandTransition,
    code: &'static str,
) -> anyhow::Result<()> {
    let version = next_version(&prepared.snapshot)?;
    let failure = json!({
        "kind": "command_failed",
        "code": code,
        "path": prepared.snapshot.current_state,
        "message": "the Process command was refused by the database",
    });
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET status = 'failed',
                failure_json = $3,
                version = $4,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $5
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &failure,
                &version,
                &prepared.snapshot.current_state,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("failing an unrecoverable Process command state")?;
    if updated != 1 {
        bail!("locked failed Process instance did not fail exactly once");
    }
    // No separate journal event: the instance row carries the failure and the
    // transition log carries the outcome. The event kinds are a closed set,
    // and an unrecoverable command is not a new kind of Process fact.
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "command_failed",
        None,
        json!({ "command": prepared.state.name, "code": code }),
    )
    .await
}

async fn commit_rejected_command(
    transaction: &Transaction<'_>,
    source_name: &str,
    prepared: &PreparedCommandTransition,
    error: &CommandBusinessRejection,
) -> anyhow::Result<()> {
    let version = next_version(&prepared.snapshot)?;
    let failure = json!({
        "kind": "command_rejected",
        "code": error.code,
        "path": error.path,
        "message": error.message,
    });
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_instances
            SET status = 'failed',
                failure_json = $3,
                version = $4,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND current_state = $5
              AND version = $6
              AND status = 'running'
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &failure,
                &version,
                &prepared.snapshot.current_state,
                &prepared.snapshot.version,
            ],
        )
        .await
        .context("failing rejected Process command state")?;
    if updated != 1 {
        bail!("locked rejected Process instance did not fail exactly once");
    }
    consume_event(transaction, source_name, prepared.snapshot.event_id).await?;
    transaction
        .execute(
            "
            INSERT INTO donat.process_events (
                source_name,
                instance_id,
                process_name,
                revision,
                kind,
                payload_json,
                idempotency_key,
                status,
                consumed_at
            )
            VALUES ($1, $2, $3, $4, 'command_rejected', $5, $6, 'consumed',
                    statement_timestamp())
            ",
            &[
                &source_name,
                &prepared.snapshot.instance_id,
                &prepared.snapshot.process_name,
                &prepared.snapshot.revision,
                &failure,
                &format!("command-rejected:{}", prepared.snapshot.event_id),
            ],
        )
        .await
        .context("appending rejected Process command event")?;
    append_transition_log(
        transaction,
        source_name,
        &prepared.snapshot,
        &prepared.snapshot.current_state,
        "command_rejected",
        None,
        json!({
            "command": prepared.state.name,
            "code": error.code,
            "path": error.path,
        }),
    )
    .await
}

/// What preparing one due event produced.
enum Preparation {
    /// A transition ready to apply, and this worker's hold on its instance.
    /// Boxed: a prepared transition carries a compiled state and its resolved
    /// values, and the other variants are a word wide.
    Prepared(Box<PreparedTransition>, InFlightGuard),
    /// The event could never be applied, and its instance has been ended.
    Failed(TransitionConsumption),
    NoWork,
}

/// Enough of a snapshot to end the instance a failed transition belonged to.
#[derive(Clone)]
struct FailingTransition {
    instance_id: Uuid,
    event_id: Uuid,
    current_state: String,
    version: i64,
    /// What the event has already survived. The retry schedule and the
    /// give-up point are both read from it.
    attempts: i32,
}

impl FailingTransition {
    fn of(snapshot: &TransitionSnapshot) -> Self {
        Self {
            instance_id: snapshot.instance_id,
            event_id: snapshot.event_id,
            current_state: snapshot.current_state.clone(),
            version: snapshot.version,
            attempts: snapshot.attempts,
        }
    }
}

/// How long an event waits before its next attempt: exponential from the
/// initial interval, capped, with full jitter.
///
/// The jitter is derived from the event and its attempt rather than sampled,
/// so a fleet of workers spreads its retries without any of them holding a
/// random source — and so the same failure reproduces the same schedule.
fn transition_retry_delay_ms(event_id: Uuid, attempts: i32) -> u64 {
    let exponent = u32::try_from(attempts.saturating_sub(1)).unwrap_or(0);
    let upper = TRANSITION_RETRY_INITIAL_MS
        .saturating_mul(1_u64.checked_shl(exponent).unwrap_or(u64::MAX))
        .min(TRANSITION_RETRY_MAXIMUM_MS);
    let material = format!("donat.process.transition-retry.v1\0{event_id}\0{attempts}");
    let digest = Sha256::digest(material.as_bytes());
    let sample = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least eight bytes"),
    );
    // Full jitter, with a floor so a retry is never scheduled for right now.
    (sample % upper.saturating_add(1)).max(TRANSITION_RETRY_INITIAL_MS.min(upper))
}

/// Whether a failure may plausibly succeed on the next attempt.
///
/// Deliberately a short, explicit list. Everything outside it — a constraint,
/// a contract violation, a missing compiled dependency, an invariant — refuses
/// again in exactly the same way, and retrying it is how one instance used to
/// stop a whole deployment.
fn is_transient(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(database) = cause.downcast_ref::<tokio_postgres::Error>() {
            if let Some(database) = database.as_db_error() {
                let code = database.code().code();
                // Serialization failure, deadlock, admin shutdown, out of
                // resources, lock timeout — and every connection class.
                return matches!(code, "40001" | "40P01" | "57P01" | "53300" | "55P03")
                    || code.starts_with("08");
            }
            // A driver error with no SQLSTATE is usually a connection problem,
            // but not always: a row-count mismatch or a parameter that will
            // not serialise is deterministic, and retrying it burns every
            // attempt over several minutes before failing the instance with
            // the wrong cause. Those are decided here rather than waited out.
            return !is_deterministic_driver_error(database);
        }
        cause
            .downcast_ref::<deadpool_postgres::PoolError>()
            .is_some()
    })
}

/// Whether a driver error carries no SQLSTATE because it never reached the
/// server: the request itself was malformed or the response did not match what
/// the caller asked for. Waiting does not change either.
fn is_deterministic_driver_error(error: &tokio_postgres::Error) -> bool {
    // tokio-postgres does not expose its error kinds, and the Display text is
    // the only thing it gives a caller to distinguish them by.
    let text = error.to_string();
    text.contains("query returned an unexpected number of rows")
        || text.contains("error serializing parameter")
        || text.contains("invalid number of parameters")
        || text.contains("cannot convert between the Rust type")
}

fn next_version(snapshot: &TransitionSnapshot) -> anyhow::Result<i64> {
    snapshot
        .version
        .checked_add(1)
        .ok_or_else(|| anyhow!("Process instance version overflow"))
}

async fn consume_event(
    transaction: &Transaction<'_>,
    source_name: &str,
    event_id: Uuid,
) -> anyhow::Result<()> {
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_events
            SET status = 'consumed',
                consumed_at = statement_timestamp(),
                attempts = attempts + 1
            WHERE source_name = $1
              AND id = $2
              AND status = 'pending'
            ",
            &[&source_name, &event_id],
        )
        .await
        .context("consuming Process transition event")?;
    if updated != 1 {
        bail!("locked Process transition event did not consume exactly once");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn append_transition_log(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    to_state: &str,
    outcome: &str,
    command_result: Option<&Json>,
    redacted_context: Json,
) -> anyhow::Result<()> {
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                event_id,
                from_state,
                to_state,
                outcome,
                definition_revision,
                command_result_json,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.event_id,
                &snapshot.current_state,
                &to_state,
                &outcome,
                &snapshot.revision,
                &command_result,
                &redacted_context,
            ],
        )
        .await
        .context("appending Process transition log")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn append_fanout_transition_log(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    to_state: &str,
    outcome: &str,
    command_result: Option<&Json>,
    activity: Option<(Uuid, i32, i64)>,
    redacted_context: Json,
) -> anyhow::Result<()> {
    let (activity_job_id, activity_attempt, activity_lease_generation) = activity
        .map(|(job_id, attempt, generation)| (Some(job_id), Some(attempt), Some(generation)))
        .unwrap_or((None, None, None));
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                event_id,
                activity_job_id,
                activity_attempt,
                activity_lease_generation,
                from_state,
                to_state,
                outcome,
                definition_revision,
                command_result_json,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.event_id,
                &activity_job_id,
                &activity_attempt,
                &activity_lease_generation,
                &snapshot.current_state,
                &to_state,
                &outcome,
                &snapshot.revision,
                &command_result,
                &redacted_context,
            ],
        )
        .await
        .context("appending Process fan-out transition log")?;
    Ok(())
}

async fn append_continue_event(
    transaction: &Transaction<'_>,
    source_name: &str,
    snapshot: &TransitionSnapshot,
    version: i64,
) -> anyhow::Result<()> {
    let idempotency_key = format!("continue:{}:{version}", snapshot.instance_id);
    transaction
        .execute(
            "
            INSERT INTO donat.process_events (
                source_name,
                instance_id,
                process_name,
                revision,
                kind,
                payload_json,
                idempotency_key,
                status
            )
            VALUES ($1, $2, $3, $4, 'continue', '{}'::jsonb, $5, 'pending')
            ",
            &[
                &source_name,
                &snapshot.instance_id,
                &snapshot.process_name,
                &snapshot.revision,
                &idempotency_key,
            ],
        )
        .await
        .context("scheduling next Process state token")?;
    Ok(())
}

#[cfg(test)]
mod retry_schedule_tests {
    use super::{
        TRANSITION_RETRY_INITIAL_MS, TRANSITION_RETRY_MAXIMUM_MS, transition_retry_delay_ms,
    };
    use uuid::Uuid;

    /// The schedule a deferred transition follows: never zero, growing with
    /// the attempt, and capped so a long outage does not push an instance out
    /// to next week.
    #[test]
    fn the_retry_schedule_grows_and_stops_growing() {
        let event = Uuid::from_u128(0x5150);

        let first = transition_retry_delay_ms(event, 1);
        assert!(
            first >= TRANSITION_RETRY_INITIAL_MS.min(TRANSITION_RETRY_INITIAL_MS),
            "a retry is never scheduled for right now: {first}"
        );
        for attempt in 1..24 {
            let delay = transition_retry_delay_ms(event, attempt);
            assert!(
                delay <= TRANSITION_RETRY_MAXIMUM_MS,
                "attempt {attempt} waited past the cap: {delay}"
            );
        }
        // Late attempts sit at the ceiling rather than doubling out of it.
        assert!(
            transition_retry_delay_ms(event, 20) <= TRANSITION_RETRY_MAXIMUM_MS,
            "the cap holds however many attempts have failed"
        );
    }

    /// Two events failing at the same moment must not come back at the same
    /// moment: the jitter is derived from the event, so a fleet spreads out
    /// without any worker holding a random source.
    #[test]
    fn two_events_failing_together_do_not_retry_together() {
        let delays: Vec<u64> = (0..16)
            .map(|seed| transition_retry_delay_ms(Uuid::from_u128(seed), 6))
            .collect();
        let distinct: std::collections::BTreeSet<u64> = delays.iter().copied().collect();

        assert!(
            distinct.len() > delays.len() / 2,
            "the schedule is not spread across events: {delays:?}"
        );
        // And it is a schedule, not a sample: the same event and attempt
        // always answer the same way.
        assert_eq!(
            transition_retry_delay_ms(Uuid::from_u128(3), 6),
            transition_retry_delay_ms(Uuid::from_u128(3), 6)
        );
    }
}
