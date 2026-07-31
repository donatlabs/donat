//! Durable connector activity claims and fenced completion.
//!
//! Every database transaction in this module is short. In particular, the
//! activity lease, capacity reservation, and provider-idempotency record are
//! committed before [`ProcessActivityExecutor::execute`] is entered.

use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use base64::Engine as _;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use donat_connector_catalog::{OperationEffect, OperationSpec};
use donat_metadata::ProcessErrorKind;
use futures_util::future::BoxFuture;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use tokio_postgres::Transaction;
use uuid::Uuid;

use crate::connectors::{
    ConnectorErrorClass, ConnectorFailure, ConnectorRegistry, ConnectorSuccess,
    canonical_json_sha256,
};

use super::start::typed_value;
use super::{
    CompiledProcessForEachActivity, CompiledProcessRequestState, CompiledProcessStateOperation,
    ProcessRuntime,
};

/// The narrow side-effect boundary available to the Process activity worker.
///
/// Implementations receive only a deployment-selected connector operation,
/// closed input, a fixed provider key, and a deadline. They cannot mutate the
/// journal or choose transport details.
pub trait ProcessActivityExecutor: Send + Sync {
    fn execute<'a>(
        &'a self,
        instance: &'a str,
        operation: &'a str,
        input: Json,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>>;
}

impl ProcessActivityExecutor for ConnectorRegistry {
    fn execute<'a>(
        &'a self,
        instance: &'a str,
        operation: &'a str,
        input: Json,
        idempotency_key: &'a str,
        deadline: tokio::time::Instant,
    ) -> BoxFuture<'a, Result<ConnectorSuccess, ConnectorFailure>> {
        Box::pin(async move {
            ConnectorRegistry::execute(self, instance, operation, input, idempotency_key, deadline)
                .await
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityConsumption {
    NoWork,
    CapacityDeferred {
        activity_job_id: Uuid,
    },
    ScheduleToStartTimedOut {
        instance_id: Uuid,
        activity_job_id: Uuid,
    },
    Succeeded {
        instance_id: Uuid,
        activity_job_id: Uuid,
        attempt: i32,
        lease_generation: i64,
    },
    RetryScheduled {
        instance_id: Uuid,
        activity_job_id: Uuid,
        failed_attempt: i32,
        next_attempt: i32,
    },
    RetryExhausted {
        instance_id: Uuid,
        activity_job_id: Uuid,
        attempt: i32,
        lease_generation: i64,
        last_class: ConnectorErrorClass,
    },
    Failed {
        instance_id: Uuid,
        activity_job_id: Uuid,
        attempt: i32,
        lease_generation: i64,
        class: ConnectorErrorClass,
    },
    StaleCompletion {
        instance_id: Uuid,
        activity_job_id: Uuid,
        attempt: i32,
        lease_generation: i64,
    },
}

enum ActivityClaim {
    NoWork,
    Resolved(ActivityConsumption),
    CapacityDeferred {
        activity_job_id: Uuid,
    },
    ScheduleToStartTimedOut {
        instance_id: Uuid,
        activity_job_id: Uuid,
    },
    Claimed(Box<ClaimedActivity>),
}

struct ClaimedActivity {
    instance_id: Uuid,
    activity_job_id: Uuid,
    process_name: String,
    revision: String,
    state_name: String,
    logical_activity_id: String,
    connector: String,
    operation: String,
    input: Json,
    request_fingerprint: String,
    attempt: i32,
    lease_generation: i64,
    lease_token: Uuid,
    start_to_close_deadline: DateTime<Utc>,
    completion_deadline: DateTime<Utc>,
    state: CompiledProcessRequestState,
    spec: Arc<OperationSpec>,
    maximum_send_horizon_ms: Option<u64>,
}

impl ProcessRuntime {
    /// Claim and execute at most one source-local connector activity.
    pub async fn consume_one_activity(&self) -> anyhow::Result<ActivityConsumption> {
        let claim = self.claim_one_activity().await?;
        let claimed = match claim {
            ActivityClaim::NoWork => return Ok(ActivityConsumption::NoWork),
            ActivityClaim::Resolved(consumption) => return Ok(consumption),
            ActivityClaim::CapacityDeferred { activity_job_id } => {
                return Ok(ActivityConsumption::CapacityDeferred { activity_job_id });
            }
            ActivityClaim::ScheduleToStartTimedOut {
                instance_id,
                activity_job_id,
            } => {
                return Ok(ActivityConsumption::ScheduleToStartTimedOut {
                    instance_id,
                    activity_job_id,
                });
            }
            ActivityClaim::Claimed(claimed) => claimed,
        };

        let authorization = self.authorize_provider_send(&claimed).await?;
        let (idempotency_key, deadline_ms) = match authorization {
            ProviderAuthorization::Authorized {
                idempotency_key,
                deadline_ms,
            } => (idempotency_key, deadline_ms),
            ProviderAuthorization::Refused(failure) => {
                return self
                    .complete_terminal_activity_failure(&claimed, failure)
                    .await;
            }
        };
        let deadline = tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(deadline_ms))
            .ok_or_else(|| anyhow!("Process connector deadline overflow"))?;
        let outcome = self
            .activity_executor
            .execute(
                &claimed.connector,
                &claimed.operation,
                claimed.input.clone(),
                &idempotency_key,
                deadline,
            )
            .await;

        match outcome {
            Ok(success) => {
                if success.request_fingerprint != claimed.request_fingerprint {
                    let failure = ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_invariant",
                        "connector returned a mismatched request fingerprint",
                    );
                    return self.complete_activity_failure(&claimed, failure).await;
                }
                let valid_output = typed_value(&success.output)
                    .ok()
                    .and_then(|output| claimed.spec.output.validate(&output).ok())
                    .is_some();
                if !valid_output {
                    let failure = ConnectorFailure::new(
                        ConnectorErrorClass::Invariant,
                        "connector_output_contract_violation",
                        "connector output violated its declared operation contract",
                    );
                    return self.complete_activity_failure(&claimed, failure).await;
                }
                if self
                    .complete_activity_success(&claimed, &success.output)
                    .await?
                {
                    Ok(ActivityConsumption::Succeeded {
                        instance_id: claimed.instance_id,
                        activity_job_id: claimed.activity_job_id,
                        attempt: claimed.attempt,
                        lease_generation: claimed.lease_generation,
                    })
                } else {
                    Ok(ActivityConsumption::StaleCompletion {
                        instance_id: claimed.instance_id,
                        activity_job_id: claimed.activity_job_id,
                        attempt: claimed.attempt,
                        lease_generation: claimed.lease_generation,
                    })
                }
            }
            Err(failure) => self.complete_activity_failure(&claimed, failure).await,
        }
    }

    async fn claim_one_activity(&self) -> anyhow::Result<ActivityClaim> {
        let mut client = self
            .pool
            .get()
            .await
            .context("checking due Process activities")?;
        let transaction = client
            .transaction()
            .await
            .context("starting Process activity claim transaction")?;
        let Some(row) = transaction
            .query_opt(
                "
                SELECT
                    job.id,
                    job.instance_id,
                    job.state_name,
                    job.logical_activity_id,
                    job.connector_instance,
                    job.operation,
                    job.serialization_key_hash,
                    job.input_json,
                    job.request_fingerprint,
                    job.status,
                    job.attempts,
                    job.lease_generation,
                    job.schedule_to_start_deadline,
                    job.start_to_close_deadline,
                    instance.process_name,
                    instance.revision,
                    instance.current_state,
                    statement_timestamp() AS db_now
                FROM donat.process_activity_jobs job
                JOIN donat.process_instances instance
                  ON instance.source_name = job.source_name
                 AND instance.id = job.instance_id
                WHERE job.source_name = $1
                  AND instance.status = 'running'
                  AND (
                    (
                      job.status = 'scheduled'
                      AND job.available_at <= statement_timestamp()
                    )
                    OR
                    (
                      job.status = 'running'
                      AND job.available_at <= statement_timestamp()
                      AND job.lease_expires_at <= statement_timestamp()
                    )
                  )
                ORDER BY job.available_at, job.id
                FOR UPDATE OF job SKIP LOCKED
                LIMIT 1
                ",
                &[&self.source_name],
            )
            .await
            .context("claiming one due Process activity")?
        else {
            transaction
                .commit()
                .await
                .context("committing empty Process activity claim")?;
            return Ok(ActivityClaim::NoWork);
        };

        let activity_job_id: Uuid = row.get("id");
        let instance_id: Uuid = row.get("instance_id");
        let state_name: String = row.get("state_name");
        let logical_activity_id: String = row.get("logical_activity_id");
        let connector: String = row.get("connector_instance");
        let operation: String = row.get("operation");
        let serialization_key_hash: Option<Vec<u8>> = row.get("serialization_key_hash");
        let input: Json = row.get("input_json");
        let request_fingerprint: String = row.get("request_fingerprint");
        let job_status: String = row.get("status");
        let prior_attempts: i32 = row.get("attempts");
        let prior_lease_generation: i64 = row.get("lease_generation");
        let schedule_to_start_deadline: DateTime<Utc> = row.get("schedule_to_start_deadline");
        let prior_start_to_close_deadline: Option<DateTime<Utc>> =
            row.get("start_to_close_deadline");
        let process_name: String = row.get("process_name");
        let revision: String = row.get("revision");
        let current_state: String = row.get("current_state");
        let db_now: DateTime<Utc> = row.get("db_now");

        let definition = self
            .deployed_catalog
            .revision(&process_name, &revision)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "Process activity `{activity_job_id}` references absent revision `{process_name}` / `{revision}`"
                )
            })?;
        let compiled = definition.states.get(&state_name).ok_or_else(|| {
            anyhow!("Process activity `{activity_job_id}` references absent state `{state_name}`")
        })?;
        let (state, fanout) = match &compiled.operation {
            CompiledProcessStateOperation::Request(state) => (state.as_ref().clone(), false),
            CompiledProcessStateOperation::ForEach(state) => match &state.activity {
                CompiledProcessForEachActivity::Request(request) => {
                    (request.as_ref().clone(), true)
                }
                CompiledProcessForEachActivity::Command(_) => {
                    bail!(
                        "Process activity `{activity_job_id}` references command fan-out state `{state_name}`"
                    )
                }
            },
            _ => {
                bail!(
                    "Process activity `{activity_job_id}` references non-request state `{state_name}`"
                )
            }
        };
        if current_state != state_name
            || state.connector != connector
            || state.operation.as_str() != operation
            || canonical_json_sha256(&input) != request_fingerprint
        {
            bail!("Process activity `{activity_job_id}` differs from its pinned request state");
        }
        if fanout
            && transaction
                .query_opt(
                    "
                    SELECT ordinal
                    FROM donat.process_fanout_items
                    WHERE source_name = $1
                      AND instance_id = $2
                      AND state_name = $3
                      AND activity_job_id = $4
                      AND status = 'scheduled'
                    FOR UPDATE
                    ",
                    &[
                        &self.source_name,
                        &instance_id,
                        &state_name,
                        &activity_job_id,
                    ],
                )
                .await
                .context("validating claimed Process fan-out activity")?
                .is_none()
        {
            bail!(
                "Process activity `{activity_job_id}` has no active fan-out item in state `{state_name}`"
            );
        }
        let dependency = definition
            .dependencies
            .connector_operations
            .get(&(self.source_name.clone(), connector.clone(), state.operation))
            .ok_or_else(|| {
                anyhow!("Process activity `{activity_job_id}` has no pinned connector dependency")
            })?;
        let live_spec = self
            .connector_registry
            .operation_spec_handle(&self.source_name, &connector, state.operation)
            .ok_or_else(|| {
                anyhow!("Process activity `{activity_job_id}` connector operation is absent")
            })?;
        let live_fingerprint = self
            .connector_registry
            .configuration_fingerprint(&connector, &operation)
            .ok_or_else(|| {
                anyhow!("Process activity `{activity_job_id}` has no deployment fingerprint")
            })?;
        if !Arc::ptr_eq(&dependency.spec, &live_spec)
            || dependency.deployment_fingerprint != live_fingerprint
            || dependency.serialization_key_input.as_deref()
                != self
                    .connector_registry
                    .serialization_key_input(&connector, &operation)
        {
            bail!(
                "Process activity `{activity_job_id}` differs from pinned connector revision `{revision}`"
            );
        }
        dependency
            .spec
            .input
            .validate(&typed_value(&input).context("decoding claimed Process activity input")?)
            .map_err(|error| {
                anyhow!("claimed Process activity input violated its contract: {error}")
            })?;
        let expected_serialization_key_hash = dependency
            .serialization_key_input
            .as_deref()
            .map(|field| process_serialization_key_hash(&input, field))
            .transpose()?;
        if serialization_key_hash != expected_serialization_key_hash {
            bail!(
                "Process activity `{activity_job_id}` serialization key differs from its pinned input"
            );
        }

        if job_status == "scheduled" && schedule_to_start_deadline <= db_now {
            expire_scheduled_activity(
                &transaction,
                &self.source_name,
                activity_job_id,
                instance_id,
                &process_name,
                &revision,
                &state_name,
            )
            .await?;
            transaction
                .commit()
                .await
                .context("committing Process schedule-to-start timeout")?;
            return Ok(ActivityClaim::ScheduleToStartTimedOut {
                instance_id,
                activity_job_id,
            });
        }

        let is_takeover = job_status == "running";
        let (start_to_close_deadline, completion_deadline) = if is_takeover {
            let deadline = prior_start_to_close_deadline.ok_or_else(|| {
                anyhow!("running Process activity has no start_to_close deadline")
            })?;
            let takeover_deadline = deadline
                .checked_add_signed(ChronoDuration::milliseconds(
                    i64::try_from(super::MAXIMUM_ACTIVITY_TAKEOVER_DELAY_MS)
                        .expect("runtime takeover bound fits i64"),
                ))
                .ok_or_else(|| anyhow!("Process takeover deadline overflow"))?;
            if db_now > takeover_deadline {
                let consumption = resolve_late_running_activity(
                    &transaction,
                    &self.source_name,
                    activity_job_id,
                    instance_id,
                    &process_name,
                    &revision,
                    &state_name,
                    &logical_activity_id,
                    prior_attempts,
                    prior_lease_generation,
                    &state,
                    db_now,
                )
                .await?;
                transaction
                    .commit()
                    .await
                    .context("committing late Process activity takeover resolution")?;
                return Ok(ActivityClaim::Resolved(consumption));
            }
            (deadline, takeover_deadline)
        } else {
            let deadline = db_now
                .checked_add_signed(ChronoDuration::milliseconds(
                    i64::try_from(state.start_to_close_ms)
                        .context("start_to_close exceeds chrono range")?,
                ))
                .ok_or_else(|| anyhow!("Process start_to_close deadline overflow"))?;
            (deadline, deadline)
        };
        let capacity_wait_deadline = if is_takeover {
            completion_deadline
        } else {
            schedule_to_start_deadline
        };

        let policy_fingerprint = capacity_policy_fingerprint(
            &dependency.spec,
            dependency.serialization_key_input.as_deref(),
        );
        let burst = i64::from(dependency.spec.rate.burst.get());
        let refill_interval_ms = i64::try_from(dependency.spec.rate.refill_interval_ms.get())
            .context("connector refill interval exceeds i64")?;
        let maximum_in_flight = i64::from(dependency.spec.capacity.maximum_in_flight.get());
        transaction
            .execute(
                "
                INSERT INTO donat.process_capacity_buckets (
                    source_name,
                    connector_instance,
                    operation,
                    available_tokens,
                    last_refill_at,
                    policy_fingerprint
                )
                VALUES ($1, $2, $3, $4::bigint::numeric, $5, $6)
                ON CONFLICT (source_name, connector_instance, operation)
                DO NOTHING
                ",
                &[
                    &self.source_name,
                    &connector,
                    &operation,
                    &burst,
                    &db_now,
                    &policy_fingerprint,
                ],
            )
            .await
            .context("initializing Process activity capacity bucket")?;
        let stored_policy: String = transaction
            .query_one(
                "
                SELECT policy_fingerprint
                FROM donat.process_capacity_buckets
                WHERE source_name = $1
                  AND connector_instance = $2
                  AND operation = $3
                FOR UPDATE
                ",
                &[&self.source_name, &connector, &operation],
            )
            .await
            .context("locking Process activity capacity bucket")?
            .get(0);
        if stored_policy != policy_fingerprint {
            bail!("Process activity capacity policy changed while live work references its bucket");
        }
        transaction
            .execute(
                "
                UPDATE donat.process_capacity_reservations
                SET released_at = $4
                WHERE source_name = $1
                  AND connector_instance = $2
                  AND operation = $3
                  AND released_at IS NULL
                  AND expires_at <= $4
                ",
                &[&self.source_name, &connector, &operation, &db_now],
            )
            .await
            .context("releasing expired Process activity capacity")?;
        let capacity = transaction
            .query_one(
                "
                SELECT
                    count(*),
                    coalesce(
                        bool_or(
                            $5::bytea IS NOT NULL
                            AND serialization_key_hash = $5::bytea
                        ),
                        FALSE
                    ),
                    min(expires_at),
                    min(expires_at) FILTER (
                        WHERE $5::bytea IS NOT NULL
                          AND serialization_key_hash = $5::bytea
                    )
                FROM donat.process_capacity_reservations
                WHERE source_name = $1
                  AND connector_instance = $2
                  AND operation = $3
                  AND released_at IS NULL
                  AND expires_at > $4
                ",
                &[
                    &self.source_name,
                    &connector,
                    &operation,
                    &db_now,
                    &serialization_key_hash,
                ],
            )
            .await
            .context("counting Process activity capacity")?;
        let in_flight: i64 = capacity.get(0);
        let serialization_busy: bool = capacity.get(1);
        let earliest_capacity_expiry: Option<DateTime<Utc>> = capacity.get(2);
        let earliest_serialization_expiry: Option<DateTime<Utc>> = capacity.get(3);
        let rate = transaction
            .query_one(
                "
                UPDATE donat.process_capacity_buckets
                SET available_tokens = least(
                        $4::bigint::numeric,
                        available_tokens
                          + greatest(
                                0::numeric,
                                extract(epoch FROM ($5::timestamptz - last_refill_at))
                                  * 1000
                            ) / $6::bigint::numeric
                    ),
                    last_refill_at = $5
                WHERE source_name = $1
                  AND connector_instance = $2
                  AND operation = $3
                RETURNING
                    available_tokens >= 1 AS has_token,
                    $5::timestamptz
                      + (
                            ceil(
                                greatest(
                                    0::numeric,
                                    1::numeric - available_tokens
                                ) * $6::bigint::numeric
                            )::bigint
                            * interval '1 millisecond'
                        ) AS next_token_at
                ",
                &[
                    &self.source_name,
                    &connector,
                    &operation,
                    &burst,
                    &db_now,
                    &refill_interval_ms,
                ],
            )
            .await
            .context("refilling Process activity capacity bucket")?;
        let has_token: bool = rate.get("has_token");
        let next_token_at: DateTime<Utc> = rate.get("next_token_at");
        if in_flight >= maximum_in_flight || serialization_busy || !has_token {
            let next_available_at = if serialization_busy {
                earliest_serialization_expiry
            } else if in_flight >= maximum_in_flight {
                earliest_capacity_expiry
            } else {
                Some(next_token_at)
            }
            .ok_or_else(|| anyhow!("Process activity capacity deferral has no wake-up time"))?;
            transaction
                .execute(
                    "
                    UPDATE donat.process_activity_jobs
                    SET available_at = least(
                            $3::timestamptz,
                            $4::timestamptz
                        ),
                        updated_at = $5
                    WHERE source_name = $1
                      AND id = $2
                      AND status IN ('scheduled', 'running')
                    ",
                    &[
                        &self.source_name,
                        &activity_job_id,
                        &next_available_at,
                        &capacity_wait_deadline,
                        &db_now,
                    ],
                )
                .await
                .context("deferring Process activity for capacity")?;
            transaction
                .commit()
                .await
                .context("committing Process activity capacity deferral")?;
            return Ok(ActivityClaim::CapacityDeferred { activity_job_id });
        }

        transaction
            .execute(
                "
                UPDATE donat.process_capacity_buckets
                SET available_tokens = available_tokens - 1
                WHERE source_name = $1
                  AND connector_instance = $2
                  AND operation = $3
                  AND available_tokens >= 1
                ",
                &[&self.source_name, &connector, &operation],
            )
            .await
            .context("consuming Process activity rate token")?;
        let lease_token: Uuid = transaction
            .query_one("SELECT gen_random_uuid()", &[])
            .await
            .context("allocating Process activity lease token")?
            .get(0);
        let attempt = if is_takeover {
            prior_attempts
        } else {
            prior_attempts
                .checked_add(1)
                .ok_or_else(|| anyhow!("Process activity attempt overflow"))?
        };
        let lease_generation = prior_lease_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("Process activity lease generation overflow"))?;
        let updated = transaction
            .execute(
                "
                UPDATE donat.process_activity_jobs
                SET status = 'running',
                    attempts = $3,
                    lease_generation = $4,
                    start_to_close_deadline = $5,
                    lease_token = $6,
                    lease_expires_at = $5,
                    available_at = $9,
                    updated_at = $7
                WHERE source_name = $1
                  AND id = $2
                  AND status = $8
                ",
                &[
                    &self.source_name,
                    &activity_job_id,
                    &attempt,
                    &lease_generation,
                    &start_to_close_deadline,
                    &lease_token,
                    &db_now,
                    &job_status,
                    &completion_deadline,
                ],
            )
            .await
            .context("assigning Process activity lease")?;
        if updated != 1 {
            bail!("locked Process activity did not acquire exactly one lease");
        }
        transaction
            .execute(
                "
                INSERT INTO donat.process_capacity_reservations (
                    source_name,
                    activity_job_id,
                    connector_instance,
                    operation,
                    serialization_key_hash,
                    lease_token,
                    reserved_at,
                    expires_at
                )
                SELECT
                    $1,
                    job.id,
                    job.connector_instance,
                    job.operation,
                    job.serialization_key_hash,
                    $3,
                    $4,
                    $5
                FROM donat.process_activity_jobs job
                WHERE job.source_name = $1
                  AND job.id = $2
                ",
                &[
                    &self.source_name,
                    &activity_job_id,
                    &lease_token,
                    &db_now,
                    &completion_deadline,
                ],
            )
            .await
            .context("reserving Process activity capacity")?;
        transaction
            .commit()
            .await
            .context("committing Process activity lease and capacity")?;

        let spec = dependency.spec.clone();
        let maximum_send_horizon_ms = match &spec.effect {
            OperationEffect::ReadOnly => None,
            OperationEffect::ProviderIdempotent { side_effect_steps } => {
                if side_effect_steps.len() != 1 {
                    bail!(
                        "current connector executor requires exactly one provider-idempotent step"
                    );
                }
                Some(
                    *compiled
                        .maximum_send_horizons_ms
                        .get(side_effect_steps[0].step.as_str())
                        .ok_or_else(|| {
                            anyhow!(
                                "Process activity `{activity_job_id}` has no pinned send horizon"
                            )
                        })?,
                )
            }
        };
        Ok(ActivityClaim::Claimed(Box::new(ClaimedActivity {
            instance_id,
            activity_job_id,
            process_name,
            revision,
            state_name,
            logical_activity_id,
            connector,
            operation,
            input,
            request_fingerprint,
            attempt,
            lease_generation,
            lease_token,
            start_to_close_deadline,
            completion_deadline,
            state,
            spec,
            maximum_send_horizon_ms,
        })))
    }

    async fn authorize_provider_send(
        &self,
        claimed: &ClaimedActivity,
    ) -> anyhow::Result<ProviderAuthorization> {
        let mut client = self
            .pool
            .get()
            .await
            .context("authorizing Process provider step")?;
        let transaction = client
            .transaction()
            .await
            .context("starting Process provider-step transaction")?;
        let (idempotency_key, provider_refusal) = match &claimed.spec.effect {
            OperationEffect::ReadOnly => (claimed.logical_activity_id.clone(), None),
            OperationEffect::ProviderIdempotent { side_effect_steps } => {
                let step = side_effect_steps.first().ok_or_else(|| {
                    anyhow!("provider-idempotent Process activity has no compiled step")
                })?;
                let horizon_ms = claimed.maximum_send_horizon_ms.ok_or_else(|| {
                    anyhow!("provider-idempotent Process activity has no send horizon")
                })?;
                let usable_window_ms = step
                    .minimum_retention_ms
                    .get()
                    .checked_sub(step.clock_safety_margin_ms.get())
                    .ok_or_else(|| anyhow!("provider idempotency window underflow"))?;
                let key = provider_step_key(
                    &claimed.logical_activity_id,
                    step.scope.as_str(),
                    step.step.as_str(),
                );
                let horizon_ms =
                    i64::try_from(horizon_ms).context("provider send horizon exceeds i64")?;
                let usable_window_ms = i64::try_from(usable_window_ms)
                    .context("provider usable window exceeds i64")?;
                transaction
                    .execute(
                        "
                        WITH provider_clock AS (
                            SELECT statement_timestamp() AS at
                        )
                        INSERT INTO donat.process_activity_provider_steps (
                            source_name,
                            activity_job_id,
                            logical_activity_id,
                            compiled_step_id,
                            idempotency_key,
                            first_provider_attempt_at,
                            maximum_send_deadline_at,
                            usable_window_expires_at,
                            created_at
                        )
                        SELECT
                            $1,
                            $2,
                            $3,
                            $4,
                            $5,
                            provider_clock.at,
                            provider_clock.at
                              + ($6::bigint * interval '1 millisecond'),
                            provider_clock.at
                              + ($7::bigint * interval '1 millisecond'),
                            provider_clock.at
                        FROM provider_clock
                        ON CONFLICT (
                            source_name,
                            logical_activity_id,
                            compiled_step_id
                        )
                        DO NOTHING
                        ",
                        &[
                            &self.source_name,
                            &claimed.activity_job_id,
                            &claimed.logical_activity_id,
                            &step.step.as_str(),
                            &key,
                            &horizon_ms,
                            &usable_window_ms,
                        ],
                    )
                    .await
                    .context("recording first Process provider-step authorization")?;
                let row = transaction
                    .query_one(
                        "
                        SELECT
                            idempotency_key,
                            maximum_send_deadline_at,
                            usable_window_expires_at,
                            statement_timestamp() AS db_now
                        FROM donat.process_activity_provider_steps
                        WHERE source_name = $1
                          AND activity_job_id = $2
                          AND logical_activity_id = $3
                          AND compiled_step_id = $4
                        ",
                        &[
                            &self.source_name,
                            &claimed.activity_job_id,
                            &claimed.logical_activity_id,
                            &step.step.as_str(),
                        ],
                    )
                    .await
                    .context("reading Process provider-step authorization")?;
                let stored_key: String = row.get("idempotency_key");
                let maximum_send_deadline: DateTime<Utc> = row.get("maximum_send_deadline_at");
                let usable_window_expires: DateTime<Utc> = row.get("usable_window_expires_at");
                let db_now: DateTime<Utc> = row.get("db_now");
                if stored_key != key {
                    bail!("persisted Process provider step has a different fixed key");
                }
                let refusal = if db_now > usable_window_expires {
                    Some(ConnectorFailure::new(
                        ConnectorErrorClass::Permanent,
                        "connector_idempotency_window_exhausted",
                        "the provider idempotency window was exhausted",
                    ))
                } else if db_now > maximum_send_deadline {
                    Some(ConnectorFailure::new(
                        ConnectorErrorClass::Timeout,
                        "provider_send_horizon_exhausted",
                        "the compiled provider send horizon was exhausted",
                    ))
                } else {
                    None
                };
                (key, refusal)
            }
        };
        let row = transaction
            .query_opt(
                "
                SELECT
                    start_to_close_deadline,
                    statement_timestamp() AS db_now
                FROM donat.process_activity_jobs
                WHERE source_name = $1
                  AND id = $2
                  AND status = 'running'
                  AND lease_token = $3
                  AND lease_generation = $4
                  AND attempts = $5
                ",
                &[
                    &self.source_name,
                    &claimed.activity_job_id,
                    &claimed.lease_token,
                    &claimed.lease_generation,
                    &claimed.attempt,
                ],
            )
            .await
            .context("fencing Process provider-step authorization")?
            .ok_or_else(|| anyhow!("Process activity lease changed before provider send"))?;
        let deadline: DateTime<Utc> = row.get("start_to_close_deadline");
        let db_now: DateTime<Utc> = row.get("db_now");
        if deadline != claimed.start_to_close_deadline {
            bail!("Process activity start_to_close deadline changed before provider send");
        }
        let activity_refusal = (db_now > claimed.completion_deadline).then(|| {
            ConnectorFailure::new(
                ConnectorErrorClass::Timeout,
                "start_to_close_timeout",
                "the activity start_to_close deadline was exhausted before provider send",
            )
        });
        let refusal = provider_refusal.or(activity_refusal);
        let deadline_ms = if refusal.is_none() {
            let remaining_ms = (claimed.completion_deadline - db_now)
                .num_milliseconds()
                .max(1) as u64;
            remaining_ms.min(claimed.spec.bounds.deadline_ms.get())
        } else {
            0
        };
        transaction
            .commit()
            .await
            .context("committing Process provider-step authorization")?;
        Ok(match refusal {
            Some(failure) => ProviderAuthorization::Refused(failure),
            None => ProviderAuthorization::Authorized {
                idempotency_key,
                deadline_ms,
            },
        })
    }

    async fn complete_activity_success(
        &self,
        claimed: &ClaimedActivity,
        output: &Json,
    ) -> anyhow::Result<bool> {
        let mut client = self
            .pool
            .get()
            .await
            .context("completing successful Process activity")?;
        let transaction = client
            .transaction()
            .await
            .context("starting Process activity completion transaction")?;
        if !lock_current_activity_lease(&transaction, &self.source_name, claimed).await? {
            append_activity_completion_log(
                &transaction,
                &self.source_name,
                claimed,
                "activity_stale_completion",
                json!({ "request_fingerprint": claimed.request_fingerprint }),
            )
            .await?;
            transaction
                .commit()
                .await
                .context("committing stale Process activity success audit")?;
            return Ok(false);
        }
        let updated = transaction
            .execute(
                "
                UPDATE donat.process_activity_jobs
                SET status = 'succeeded',
                    result_json = $6,
                    last_error_json = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    updated_at = statement_timestamp()
                WHERE source_name = $1
                  AND id = $2
                  AND status = 'running'
                  AND lease_token = $3
                  AND lease_generation = $4
                  AND attempts = $5
                ",
                &[
                    &self.source_name,
                    &claimed.activity_job_id,
                    &claimed.lease_token,
                    &claimed.lease_generation,
                    &claimed.attempt,
                    &output,
                ],
            )
            .await
            .context("storing Process activity success")?;
        if updated != 1 {
            bail!("locked Process activity success did not complete exactly once");
        }
        release_capacity(&transaction, &self.source_name, claimed).await?;
        append_activity_event(
            &transaction,
            &self.source_name,
            claimed,
            "activity_succeeded",
            json!({
                "activity_job_id": claimed.activity_job_id.to_string(),
                "attempt": claimed.attempt,
                "lease_generation": claimed.lease_generation,
            }),
        )
        .await?;
        append_activity_completion_log(
            &transaction,
            &self.source_name,
            claimed,
            "activity_completed",
            json!({ "request_fingerprint": claimed.request_fingerprint }),
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing successful Process activity")?;
        Ok(true)
    }

    async fn complete_activity_failure(
        &self,
        claimed: &ClaimedActivity,
        failure: ConnectorFailure,
    ) -> anyhow::Result<ActivityConsumption> {
        self.complete_activity_failure_with_retry(claimed, failure, true)
            .await
    }

    async fn complete_terminal_activity_failure(
        &self,
        claimed: &ClaimedActivity,
        failure: ConnectorFailure,
    ) -> anyhow::Result<ActivityConsumption> {
        self.complete_activity_failure_with_retry(claimed, failure, false)
            .await
    }

    async fn complete_activity_failure_with_retry(
        &self,
        claimed: &ClaimedActivity,
        failure: ConnectorFailure,
        retry_allowed: bool,
    ) -> anyhow::Result<ActivityConsumption> {
        let mut client = self
            .pool
            .get()
            .await
            .context("completing failed Process activity")?;
        let transaction = client
            .transaction()
            .await
            .context("starting failed Process activity transaction")?;
        if !lock_current_activity_lease(&transaction, &self.source_name, claimed).await? {
            append_activity_completion_log(
                &transaction,
                &self.source_name,
                claimed,
                "activity_stale_completion",
                json!({
                    "class": connector_error_class_name(failure.class),
                    "code": failure.code,
                }),
            )
            .await?;
            transaction
                .commit()
                .await
                .context("committing stale Process activity failure audit")?;
            return Ok(ActivityConsumption::StaleCompletion {
                instance_id: claimed.instance_id,
                activity_job_id: claimed.activity_job_id,
                attempt: claimed.attempt,
                lease_generation: claimed.lease_generation,
            });
        }
        let retryable =
            retry_allowed && activity_failure_is_retryable(&claimed.state, failure.class);
        let attempt = u32::try_from(claimed.attempt)
            .context("Process activity attempt cannot be negative")?;
        let mut failure = failure;
        if retryable && attempt < claimed.state.retry.max_attempts {
            let retry_upper_ms = retry_delay_upper_ms(&claimed.state, claimed.attempt)?;
            let jitter_ms = deterministic_full_jitter_ms(
                &claimed.logical_activity_id,
                claimed.attempt,
                retry_upper_ms,
            );
            let retry_after_ms = failure
                .retry_after
                .map(|delay| u64::try_from(delay.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            if retry_after_ms <= retry_upper_ms {
                let delay_ms = jitter_ms.max(retry_after_ms);
                let error = connector_failure_json(&failure);
                reschedule_activity_retry(
                    &transaction,
                    &self.source_name,
                    claimed,
                    &error,
                    delay_ms,
                )
                .await?;
                release_capacity(&transaction, &self.source_name, claimed).await?;
                append_activity_completion_log(
                    &transaction,
                    &self.source_name,
                    claimed,
                    "activity_retry_scheduled",
                    json!({
                        "class": connector_error_class_name(failure.class),
                        "code": failure.code,
                        "delay_ms": delay_ms,
                    }),
                )
                .await?;
                transaction
                    .commit()
                    .await
                    .context("committing Process activity retry")?;
                return Ok(ActivityConsumption::RetryScheduled {
                    instance_id: claimed.instance_id,
                    activity_job_id: claimed.activity_job_id,
                    failed_attempt: claimed.attempt,
                    next_attempt: claimed
                        .attempt
                        .checked_add(1)
                        .ok_or_else(|| anyhow!("Process activity next attempt overflow"))?,
                });
            }
            failure = ConnectorFailure {
                class: ConnectorErrorClass::Timeout,
                code: "retry_after_exceeds_retry_bound",
                safe_message: "provider Retry-After exceeds the declared retry delay bound"
                    .to_owned(),
                retry_after: failure.retry_after,
            };
        }
        let mut error = connector_failure_json(&failure);
        let retry_exhausted = retryable && attempt >= claimed.state.retry.max_attempts;
        let event_kind = if retry_exhausted {
            error = json!({
                "class": "retry_exhausted",
                "code": "activity_retry_exhausted",
                "safe_message": "the activity retry budget was exhausted",
                "retry_after_ms": null,
                "last_failure": {
                    "class": connector_error_class_name(failure.class),
                    "code": failure.code,
                }
            });
            "retry_exhausted"
        } else {
            "activity_failed"
        };
        let updated = transaction
            .execute(
                "
                UPDATE donat.process_activity_jobs
                SET status = 'failed',
                    last_error_json = $6,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    updated_at = statement_timestamp()
                WHERE source_name = $1
                  AND id = $2
                  AND status = 'running'
                  AND lease_token = $3
                  AND lease_generation = $4
                  AND attempts = $5
                ",
                &[
                    &self.source_name,
                    &claimed.activity_job_id,
                    &claimed.lease_token,
                    &claimed.lease_generation,
                    &claimed.attempt,
                    &error,
                ],
            )
            .await
            .context("storing Process activity failure")?;
        if updated != 1 {
            bail!("locked Process activity failure did not complete exactly once");
        }
        release_capacity(&transaction, &self.source_name, claimed).await?;
        append_activity_event(
            &transaction,
            &self.source_name,
            claimed,
            event_kind,
            json!({
                "activity_job_id": claimed.activity_job_id.to_string(),
                "attempt": claimed.attempt,
                "lease_generation": claimed.lease_generation,
                "error": error,
            }),
        )
        .await?;
        append_activity_completion_log(&transaction, &self.source_name, claimed, event_kind, error)
            .await?;
        transaction
            .commit()
            .await
            .context("committing failed Process activity")?;
        if retry_exhausted {
            Ok(ActivityConsumption::RetryExhausted {
                instance_id: claimed.instance_id,
                activity_job_id: claimed.activity_job_id,
                attempt: claimed.attempt,
                lease_generation: claimed.lease_generation,
                last_class: failure.class,
            })
        } else {
            Ok(ActivityConsumption::Failed {
                instance_id: claimed.instance_id,
                activity_job_id: claimed.activity_job_id,
                attempt: claimed.attempt,
                lease_generation: claimed.lease_generation,
                class: failure.class,
            })
        }
    }
}

enum ProviderAuthorization {
    Authorized {
        idempotency_key: String,
        deadline_ms: u64,
    },
    Refused(ConnectorFailure),
}

fn capacity_policy_fingerprint(
    spec: &OperationSpec,
    serialization_key_input: Option<&str>,
) -> String {
    let material = format!(
        "donat.process.capacity.v1\0{}:{}:{}:{}",
        spec.capacity.maximum_in_flight,
        spec.rate.burst,
        spec.rate.refill_interval_ms,
        serialization_key_input.unwrap_or("")
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

pub(super) fn process_serialization_key_hash(input: &Json, field: &str) -> anyhow::Result<Vec<u8>> {
    let value = input
        .as_object()
        .and_then(|input| input.get(field))
        .ok_or_else(|| anyhow!("Process serialization input `{field}` is missing"))?;
    if value.is_null() || value.is_array() || value.is_object() {
        bail!("Process serialization input `{field}` is not a non-null scalar");
    }
    let canonical =
        serde_json::to_vec(value).context("serializing Process activity serialization key")?;
    let mut material = b"donat.process.serialization-key.v1\0".to_vec();
    material.extend_from_slice(&canonical);
    Ok(Sha256::digest(material).to_vec())
}

fn activity_failure_is_retryable(
    state: &CompiledProcessRequestState,
    class: ConnectorErrorClass,
) -> bool {
    state.retry.retry_on.iter().any(|kind| {
        matches!(
            (kind, class),
            (ProcessErrorKind::Transport, ConnectorErrorClass::Transport)
                | (ProcessErrorKind::Timeout, ConnectorErrorClass::Timeout)
                | (ProcessErrorKind::Http429, ConnectorErrorClass::Http429)
                | (ProcessErrorKind::Http5xx, ConnectorErrorClass::Http5xx)
        )
    })
}

fn retry_delay_upper_ms(
    state: &CompiledProcessRequestState,
    failed_attempt: i32,
) -> anyhow::Result<u64> {
    let exponent = u32::try_from(failed_attempt.saturating_sub(1))
        .context("Process activity attempt cannot be negative")?;
    let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Ok(state
        .initial_retry_interval_ms
        .saturating_mul(multiplier)
        .min(state.maximum_retry_interval_ms))
}

fn deterministic_full_jitter_ms(
    logical_activity_id: &str,
    failed_attempt: i32,
    upper_ms: u64,
) -> u64 {
    let material =
        format!("donat.process.retry-jitter.v1\0{logical_activity_id}\0{failed_attempt}");
    let digest = Sha256::digest(material.as_bytes());
    let sample = u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 has at least eight bytes"),
    );
    sample % upper_ms.saturating_add(1)
}

fn provider_step_key(logical_activity_id: &str, scope: &str, step: &str) -> String {
    let canonical = serde_json::to_vec(&std::collections::BTreeMap::from([
        ("logical_activity_id", logical_activity_id),
        ("scope", scope),
        ("step", step),
    ]))
    .expect("provider key material always serializes");
    let mut material = b"donat.connector.idempotency.step.v1\0".to_vec();
    material.extend_from_slice(&canonical);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(material))
}

async fn reschedule_activity_retry(
    transaction: &Transaction<'_>,
    source_name: &str,
    claimed: &ClaimedActivity,
    error: &Json,
    delay_ms: u64,
) -> anyhow::Result<()> {
    let delay_ms = i64::try_from(delay_ms).context("Process retry delay exceeds i64")?;
    let schedule_to_start_ms = i64::try_from(claimed.state.schedule_to_start_ms)
        .context("Process retry schedule_to_start exceeds i64")?;
    let updated = transaction
        .execute(
            "
            WITH retry_clock AS (
                SELECT statement_timestamp() AS at
            ),
            retry_schedule AS (
                SELECT
                    retry_clock.at
                      + ($6::bigint * interval '1 millisecond') AS available_at
                FROM retry_clock
            )
            UPDATE donat.process_activity_jobs job
            SET status = 'scheduled',
                available_at = retry_schedule.available_at,
                schedule_to_start_deadline =
                    retry_schedule.available_at
                      + ($7::bigint * interval '1 millisecond'),
                start_to_close_deadline = NULL,
                lease_token = NULL,
                lease_expires_at = NULL,
                last_error_json = $8,
                updated_at = retry_clock.at
            FROM retry_clock, retry_schedule
            WHERE job.source_name = $1
              AND job.id = $2
              AND job.status = 'running'
              AND job.lease_token = $3
              AND job.lease_generation = $4
              AND job.attempts = $5
            ",
            &[
                &source_name,
                &claimed.activity_job_id,
                &claimed.lease_token,
                &claimed.lease_generation,
                &claimed.attempt,
                &delay_ms,
                &schedule_to_start_ms,
                &error,
            ],
        )
        .await
        .context("rescheduling retryable Process activity")?;
    if updated != 1 {
        bail!("locked retryable Process activity did not reschedule exactly once");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn resolve_late_running_activity(
    transaction: &Transaction<'_>,
    source_name: &str,
    activity_job_id: Uuid,
    instance_id: Uuid,
    process_name: &str,
    revision: &str,
    state_name: &str,
    logical_activity_id: &str,
    attempt: i32,
    lease_generation: i64,
    state: &CompiledProcessRequestState,
    db_now: DateTime<Utc>,
) -> anyhow::Result<ActivityConsumption> {
    let failure = ConnectorFailure::new(
        ConnectorErrorClass::Timeout,
        "start_to_close_timeout",
        "the activity exceeded start_to_close and its takeover grace",
    );
    let retryable = activity_failure_is_retryable(state, failure.class);
    let attempt_ordinal =
        u32::try_from(attempt).context("Process activity attempt cannot be negative")?;
    let failure_json = connector_failure_json(&failure);

    if retryable && attempt_ordinal < state.retry.max_attempts {
        let retry_upper_ms = retry_delay_upper_ms(state, attempt)?;
        let delay_ms = deterministic_full_jitter_ms(logical_activity_id, attempt, retry_upper_ms);
        let available_at = db_now
            .checked_add_signed(ChronoDuration::milliseconds(
                i64::try_from(delay_ms).context("Process retry delay exceeds chrono range")?,
            ))
            .ok_or_else(|| anyhow!("Process retry available_at overflow"))?;
        let schedule_to_start_deadline = available_at
            .checked_add_signed(ChronoDuration::milliseconds(
                i64::try_from(state.schedule_to_start_ms)
                    .context("schedule_to_start exceeds chrono range")?,
            ))
            .ok_or_else(|| anyhow!("Process retry schedule_to_start deadline overflow"))?;
        let updated = transaction
            .execute(
                "
                UPDATE donat.process_activity_jobs
                SET status = 'scheduled',
                    available_at = $7,
                    schedule_to_start_deadline = $8,
                    start_to_close_deadline = NULL,
                    lease_token = NULL,
                    lease_expires_at = NULL,
                    last_error_json = $9,
                    updated_at = $10
                WHERE source_name = $1
                  AND id = $2
                  AND instance_id = $3
                  AND status = 'running'
                  AND attempts = $4
                  AND lease_generation = $5
                  AND logical_activity_id = $6
                ",
                &[
                    &source_name,
                    &activity_job_id,
                    &instance_id,
                    &attempt,
                    &lease_generation,
                    &logical_activity_id,
                    &available_at,
                    &schedule_to_start_deadline,
                    &failure_json,
                    &db_now,
                ],
            )
            .await
            .context("rescheduling Process activity after terminal takeover grace")?;
        if updated != 1 {
            bail!("late Process activity did not reschedule exactly once");
        }
        release_abandoned_capacity(transaction, source_name, activity_job_id, db_now).await?;
        append_unclaimed_activity_log(
            transaction,
            source_name,
            instance_id,
            activity_job_id,
            attempt,
            lease_generation,
            state_name,
            revision,
            "activity_retry_scheduled",
            json!({
                "class": "timeout",
                "code": "start_to_close_timeout",
                "delay_ms": delay_ms,
            }),
        )
        .await?;
        return Ok(ActivityConsumption::RetryScheduled {
            instance_id,
            activity_job_id,
            failed_attempt: attempt,
            next_attempt: attempt
                .checked_add(1)
                .ok_or_else(|| anyhow!("Process activity next attempt overflow"))?,
        });
    }

    let retry_exhausted = retryable && attempt_ordinal >= state.retry.max_attempts;
    let (event_kind, terminal_error) = if retry_exhausted {
        (
            "retry_exhausted",
            json!({
                "class": "retry_exhausted",
                "code": "activity_retry_exhausted",
                "safe_message": "the activity retry budget was exhausted",
                "retry_after_ms": null,
                "last_failure": {
                    "class": "timeout",
                    "code": "start_to_close_timeout",
                }
            }),
        )
    } else {
        ("activity_failed", failure_json)
    };
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_activity_jobs
            SET status = 'failed',
                lease_token = NULL,
                lease_expires_at = NULL,
                last_error_json = $6,
                updated_at = $7
            WHERE source_name = $1
              AND id = $2
              AND instance_id = $3
              AND status = 'running'
              AND attempts = $4
              AND lease_generation = $5
            ",
            &[
                &source_name,
                &activity_job_id,
                &instance_id,
                &attempt,
                &lease_generation,
                &terminal_error,
                &db_now,
            ],
        )
        .await
        .context("terminalizing Process activity after takeover grace")?;
    if updated != 1 {
        bail!("late Process activity did not terminalize exactly once");
    }
    release_abandoned_capacity(transaction, source_name, activity_job_id, db_now).await?;
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
            VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending')
            ",
            &[
                &source_name,
                &instance_id,
                &process_name,
                &revision,
                &event_kind,
                &json!({
                    "activity_job_id": activity_job_id.to_string(),
                    "attempt": attempt,
                    "lease_generation": lease_generation,
                    "error": terminal_error,
                }),
                &format!("activity:{activity_job_id}:{attempt}:{lease_generation}:{event_kind}"),
            ],
        )
        .await
        .context("appending late Process activity failure event")?;
    append_unclaimed_activity_log(
        transaction,
        source_name,
        instance_id,
        activity_job_id,
        attempt,
        lease_generation,
        state_name,
        revision,
        event_kind,
        terminal_error,
    )
    .await?;

    if retry_exhausted {
        Ok(ActivityConsumption::RetryExhausted {
            instance_id,
            activity_job_id,
            attempt,
            lease_generation,
            last_class: ConnectorErrorClass::Timeout,
        })
    } else {
        Ok(ActivityConsumption::Failed {
            instance_id,
            activity_job_id,
            attempt,
            lease_generation,
            class: ConnectorErrorClass::Timeout,
        })
    }
}

async fn release_abandoned_capacity(
    transaction: &Transaction<'_>,
    source_name: &str,
    activity_job_id: Uuid,
    db_now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let released = transaction
        .execute(
            "
            UPDATE donat.process_capacity_reservations
            SET released_at = $3
            WHERE source_name = $1
              AND activity_job_id = $2
              AND released_at IS NULL
            ",
            &[&source_name, &activity_job_id, &db_now],
        )
        .await
        .context("releasing abandoned Process activity capacity")?;
    if released != 1 {
        bail!("late Process activity did not release exactly one reservation");
    }
    Ok(())
}

async fn expire_scheduled_activity(
    transaction: &Transaction<'_>,
    source_name: &str,
    activity_job_id: Uuid,
    instance_id: Uuid,
    process_name: &str,
    revision: &str,
    state_name: &str,
) -> anyhow::Result<()> {
    let error = json!({
        "class": "timeout",
        "code": "schedule_to_start_timeout",
        "safe_message": "activity did not start before its schedule_to_start deadline",
        "retry_after_ms": null,
    });
    let row = transaction
        .query_one(
            "
            UPDATE donat.process_activity_jobs
            SET status = 'failed',
                last_error_json = $3,
                lease_token = NULL,
                lease_expires_at = NULL,
                updated_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND status IN ('scheduled', 'running')
            RETURNING attempts, lease_generation
            ",
            &[&source_name, &activity_job_id, &error],
        )
        .await
        .context("expiring unstarted Process activity")?;
    let attempt: i32 = row.get("attempts");
    let lease_generation: i64 = row.get("lease_generation");
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
            VALUES ($1, $2, $3, $4, 'activity_failed', $5, $6, 'pending')
            ",
            &[
                &source_name,
                &instance_id,
                &process_name,
                &revision,
                &json!({
                    "activity_job_id": activity_job_id.to_string(),
                    "state": state_name,
                    "error": error,
                }),
                &format!("activity:{activity_job_id}:schedule-to-start-timeout"),
            ],
        )
        .await
        .context("appending Process schedule-to-start timeout event")?;
    append_unclaimed_activity_log(
        transaction,
        source_name,
        instance_id,
        activity_job_id,
        attempt,
        lease_generation,
        state_name,
        revision,
        "activity_schedule_to_start_timed_out",
        error,
    )
    .await?;
    Ok(())
}

async fn lock_current_activity_lease(
    transaction: &Transaction<'_>,
    source_name: &str,
    claimed: &ClaimedActivity,
) -> anyhow::Result<bool> {
    Ok(transaction
        .query_opt(
            "
            SELECT id
            FROM donat.process_activity_jobs
            WHERE source_name = $1
              AND id = $2
              AND status = 'running'
              AND lease_token = $3
              AND lease_generation = $4
              AND attempts = $5
              AND statement_timestamp() <= $6
            FOR UPDATE
            ",
            &[
                &source_name,
                &claimed.activity_job_id,
                &claimed.lease_token,
                &claimed.lease_generation,
                &claimed.attempt,
                &claimed.completion_deadline,
            ],
        )
        .await
        .context("fencing Process activity completion")?
        .is_some())
}

async fn release_capacity(
    transaction: &Transaction<'_>,
    source_name: &str,
    claimed: &ClaimedActivity,
) -> anyhow::Result<()> {
    let released = transaction
        .execute(
            "
            UPDATE donat.process_capacity_reservations
            SET released_at = statement_timestamp()
            WHERE source_name = $1
              AND activity_job_id = $2
              AND lease_token = $3
              AND released_at IS NULL
            ",
            &[&source_name, &claimed.activity_job_id, &claimed.lease_token],
        )
        .await
        .context("releasing Process activity capacity")?;
    if released != 1 {
        bail!("Process activity completion did not release exactly one reservation");
    }
    Ok(())
}

async fn append_activity_event(
    transaction: &Transaction<'_>,
    source_name: &str,
    claimed: &ClaimedActivity,
    kind: &str,
    payload: Json,
) -> anyhow::Result<()> {
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
            VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending')
            ",
            &[
                &source_name,
                &claimed.instance_id,
                &claimed.process_name,
                &claimed.revision,
                &kind,
                &payload,
                &format!(
                    "activity:{}:{}:{}:{kind}",
                    claimed.activity_job_id, claimed.attempt, claimed.lease_generation
                ),
            ],
        )
        .await
        .context("appending Process activity completion event")?;
    Ok(())
}

async fn append_activity_completion_log(
    transaction: &Transaction<'_>,
    source_name: &str,
    claimed: &ClaimedActivity,
    outcome: &str,
    redacted_context: Json,
) -> anyhow::Result<()> {
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                activity_job_id,
                activity_attempt,
                activity_lease_generation,
                from_state,
                to_state,
                outcome,
                definition_revision,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9)
            ",
            &[
                &source_name,
                &claimed.instance_id,
                &claimed.activity_job_id,
                &claimed.attempt,
                &claimed.lease_generation,
                &claimed.state_name,
                &outcome,
                &claimed.revision,
                &redacted_context,
            ],
        )
        .await
        .context("appending Process activity completion log")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn append_unclaimed_activity_log(
    transaction: &Transaction<'_>,
    source_name: &str,
    instance_id: Uuid,
    activity_job_id: Uuid,
    attempt: i32,
    lease_generation: i64,
    state_name: &str,
    revision: &str,
    outcome: &str,
    redacted_context: Json,
) -> anyhow::Result<()> {
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                activity_job_id,
                activity_attempt,
                activity_lease_generation,
                from_state,
                to_state,
                outcome,
                definition_revision,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9)
            ",
            &[
                &source_name,
                &instance_id,
                &activity_job_id,
                &attempt,
                &lease_generation,
                &state_name,
                &outcome,
                &revision,
                &redacted_context,
            ],
        )
        .await
        .context("appending unclaimed Process activity log")?;
    Ok(())
}

fn connector_failure_json(failure: &ConnectorFailure) -> Json {
    json!({
        "class": connector_error_class_name(failure.class),
        "code": failure.code,
        "safe_message": failure.safe_message,
        "retry_after_ms": failure.retry_after.map(|delay| delay.as_millis() as u64),
    })
}

fn connector_error_class_name(class: ConnectorErrorClass) -> &'static str {
    match class {
        ConnectorErrorClass::Transport => "transport",
        ConnectorErrorClass::Timeout => "timeout",
        ConnectorErrorClass::Http429 => "http_429",
        ConnectorErrorClass::Http5xx => "http_5xx",
        ConnectorErrorClass::Authentication => "authentication",
        ConnectorErrorClass::Validation => "validation",
        ConnectorErrorClass::Permanent => "permanent",
        ConnectorErrorClass::Invariant => "invariant",
    }
}
