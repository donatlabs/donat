//! One-state-at-a-time execution for deterministic Process states.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use donat_ir::CommandMutation;
use donat_schema::Session;
use serde_json::{Value as Json, json};
use tokio_postgres::{Row, Transaction};
use uuid::Uuid;

use super::start::typed_value;
use super::value::{ProcessValueContext, evaluate_process_values};
use super::{
    CompiledProcessCommandRole, CompiledProcessCommandState, CompiledProcessDefinition,
    CompiledProcessFailState, CompiledProcessOutputState, CompiledProcessStateOperation,
    CompiledProcessWhenPredicate, CompiledProcessWhenState, ProcessCommandOutcome, ProcessRuntime,
    execute_process_command_in_savepoint,
};
use crate::commands::CommandBusinessRejection;

const PREPARATION_BATCH_SIZE: i64 = 128;

#[derive(Debug, Clone, PartialEq)]
pub enum TransitionConsumption {
    NoWork,
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
}

struct TransitionSnapshot {
    event_id: Uuid,
    event_kind: String,
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

struct PreparedOutputTransition {
    snapshot: TransitionSnapshot,
    output: Json,
}

struct PreparedFailTransition {
    snapshot: TransitionSnapshot,
    state: CompiledProcessFailState,
}

enum PreparedTransition {
    Command(PreparedCommandTransition),
    When(PreparedWhenTransition),
    Output(PreparedOutputTransition),
    Fail(PreparedFailTransition),
}

impl PreparedTransition {
    fn snapshot(&self) -> &TransitionSnapshot {
        match self {
            Self::Command(prepared) => &prepared.snapshot,
            Self::When(prepared) => &prepared.snapshot,
            Self::Output(prepared) => &prepared.snapshot,
            Self::Fail(prepared) => &prepared.snapshot,
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
        let Some(prepared) = self.prepare_one_transition().await? else {
            return Ok(TransitionConsumption::NoWork);
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

        let result = match prepared {
            PreparedTransition::Command(prepared) => {
                self.consume_prepared_command(&transaction, &prepared)
                    .await?
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
        };
        transaction
            .commit()
            .await
            .context("committing Process deterministic transition")?;
        Ok(result)
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
                validate_state_output(&prepared.definition, &prepared.snapshot, &result)
                    .context("validating Process command result")?;
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
        }
    }

    async fn prepare_one_transition(&self) -> anyhow::Result<Option<PreparedTransition>> {
        let client = self
            .pool
            .get()
            .await
            .context("reading due Process transitions")?;
        let rows = client
            .query(
                "
                SELECT
                    event.id AS event_id,
                    event.kind AS event_kind,
                    instance.id AS instance_id,
                    instance.process_name,
                    instance.revision,
                    instance.current_state,
                    instance.input_json,
                    instance.state_json,
                    instance.version,
                    instance.caller_role,
                    instance.caller_session_json,
                    to_jsonb(event.available_at) AS workflow_time
                FROM donat.process_events event
                JOIN donat.process_instances instance
                  ON instance.source_name = event.source_name
                 AND instance.id = event.instance_id
                WHERE event.source_name = $1
                  AND event.status = 'pending'
                  AND event.kind IN ('start', 'continue')
                  AND event.available_at <= statement_timestamp()
                  AND instance.status = 'running'
                ORDER BY event.available_at, event.id
                LIMIT $2
                ",
                &[&self.source_name, &PREPARATION_BATCH_SIZE],
            )
            .await
            .context("reading due Process event snapshots")?;

        for row in rows {
            let snapshot = transition_snapshot(&row);
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
                CompiledProcessStateOperation::Command(state) => {
                    Some(self.prepare_command_transition(snapshot, definition, state)?)
                }
                CompiledProcessStateOperation::When(state) => {
                    Some(self.prepare_when_transition(snapshot, definition, state)?)
                }
                CompiledProcessStateOperation::Output(state) => {
                    Some(self.prepare_output_transition(snapshot, definition, state)?)
                }
                CompiledProcessStateOperation::Fail(state) => {
                    Some(PreparedTransition::Fail(PreparedFailTransition {
                        snapshot,
                        state,
                    }))
                }
                CompiledProcessStateOperation::Request
                | CompiledProcessStateOperation::Wait
                | CompiledProcessStateOperation::ForEach => None,
            };
            if prepared.is_some() {
                return Ok(prepared);
            }
        }
        Ok(None)
    }

    fn prepare_command_transition(
        &self,
        snapshot: TransitionSnapshot,
        definition: Arc<CompiledProcessDefinition>,
        state: CompiledProcessCommandState,
    ) -> anyhow::Result<PreparedTransition> {
        let finalized = self
            .finalized_command_catalog
            .source(&self.source_name)
            .and_then(|catalog| catalog.command(&state.name))
            .ok_or_else(|| {
                anyhow!(
                    "finalized Process command `{}.{}` is absent from the immutable snapshot",
                    self.source_name,
                    state.name
                )
            })?;
        let pre_process = self
            .command_catalog
            .source(&self.source_name)
            .and_then(|catalog| catalog.command(&state.name))
            .ok_or_else(|| {
                anyhow!(
                    "compiled Process command `{}.{}` is absent from the immutable snapshot",
                    self.source_name,
                    state.name
                )
            })?;
        let dependency = definition
            .dependencies
            .commands
            .get(&(self.source_name.clone(), state.name.clone()))
            .ok_or_else(|| {
                anyhow!(
                    "Process command `{}.{}` is absent from the pinned dependency closure",
                    self.source_name,
                    state.name
                )
            })?;
        let expected = &dependency.definition_fingerprint;
        if expected != &pre_process.descriptor().definition_fingerprint
            || expected != &finalized.command.descriptor().definition_fingerprint
        {
            bail!(
                "Process command `{}.{}` differs from pinned revision `{}`",
                self.source_name,
                state.name,
                snapshot.revision
            );
        }

        let session = process_command_session(&definition, &state, &snapshot)?;
        let arguments = evaluate_process_values(
            &state.arguments,
            &process_value_context(&self.source_name, &snapshot),
        )?;
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
            .map_err(|error| anyhow!("planning Process command `{}`: {error}", state.name))?;

        Ok(PreparedTransition::Command(PreparedCommandTransition {
            snapshot,
            definition,
            state,
            command,
        }))
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
    }
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
    state: &CompiledProcessCommandState,
    snapshot: &TransitionSnapshot,
) -> anyhow::Result<Session> {
    match &state.role {
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
            WHERE event.source_name = $1
              AND event.id = $2
              AND event.instance_id = $3
              AND event.status = 'pending'
              AND event.kind = $6
              AND event.kind IN ('start', 'continue')
              AND event.available_at <= statement_timestamp()
              AND instance.status = 'running'
              AND instance.process_name = $4
              AND instance.revision = $5
            FOR UPDATE OF event, instance
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
