//! Atomic command-outbox delivery into a receptive durable Process wait.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};
use tokio_postgres::{Row, Transaction};
use uuid::Uuid;

use super::start::typed_value;
use super::value::{ProcessValueContext, evaluate_process_values};
use super::{CompiledProcessStateOperation, CompiledProcessWaitState, ProcessRuntime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalConsumption {
    NoWork,
    Accepted {
        request_id: Uuid,
        instance_id: Uuid,
        event_id: Uuid,
    },
    Duplicate {
        request_id: Uuid,
    },
    Unmatched {
        request_id: Uuid,
    },
    Ambiguous {
        request_id: Uuid,
    },
    #[allow(dead_code)] // Reserved for the verified-ingress guard path.
    GuardFalse {
        request_id: Uuid,
    },
    UnexpectedState {
        request_id: Uuid,
    },
}

struct SignalRequest {
    id: Uuid,
    process_name: String,
    process_revision: String,
    signal_name: String,
    correlation: Json,
    payload: Json,
    idempotency_key: String,
    created_at: DateTime<Utc>,
}

struct SignalCandidate {
    instance_id: Uuid,
    revision: String,
    current_state: String,
    version: i64,
    input: Json,
    state: Json,
    caller_session: Option<Json>,
    workflow_time: Json,
    marker_id: Option<Uuid>,
    marker_created_at: Option<DateTime<Utc>>,
    marker_available_at: Option<DateTime<Utc>>,
    marker_payload: Option<Json>,
}

struct MatchingCandidate {
    instance_id: Uuid,
    revision: String,
    current_state: String,
    version: i64,
    receptive: bool,
}

impl ProcessRuntime {
    /// Consume one typed `signal_process` outbox row.
    ///
    /// The request is serialized by its semantic idempotency key, checked
    /// against the pinned signal contract, correlated only with compatible
    /// deployed revisions, and turned into one version-qualified Process
    /// event only when the wait marker predates the request.
    pub async fn consume_one_signal(&self) -> anyhow::Result<SignalConsumption> {
        let mut client = self.pool.get().await.with_context(|| {
            format!(
                "checking pending Process signals for source `{}`",
                self.source_name
            )
        })?;
        let transaction = client
            .transaction()
            .await
            .context("starting Process signal transaction")?;
        let Some(row) = transaction
            .query_opt(
                "
                SELECT
                    id,
                    process_name,
                    process_revision,
                    signal_name,
                    correlation_json,
                    payload_json,
                    idempotency_key,
                    created_at
                FROM donat.process_signal_requests
                WHERE source_name = $1
                  AND status = 'pending'
                ORDER BY created_at, id
                FOR UPDATE SKIP LOCKED
                LIMIT 1
                ",
                &[&self.source_name],
            )
            .await
            .context("claiming one pending Process signal request")?
        else {
            transaction
                .commit()
                .await
                .context("committing empty Process signal claim")?;
            return Ok(SignalConsumption::NoWork);
        };
        let request = signal_request(&row);

        let semantic_lock = length_prefixed_signal_key(
            &self.source_name,
            &request.process_name,
            &request.signal_name,
            &request.idempotency_key,
        );
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&semantic_lock],
            )
            .await
            .context("serializing Process signal semantic key")?;
        let duplicate = transaction
            .query_one(
                "
                SELECT EXISTS (
                    SELECT 1
                    FROM donat.process_signal_requests prior
                    WHERE prior.source_name = $1
                      AND prior.process_name = $2
                      AND prior.signal_name = $3
                      AND prior.idempotency_key = $4
                      AND prior.id <> $5
                      AND prior.status <> 'pending'
                )
                ",
                &[
                    &self.source_name,
                    &request.process_name,
                    &request.signal_name,
                    &request.idempotency_key,
                    &request.id,
                ],
            )
            .await
            .context("checking Process signal semantic duplicate")?
            .get::<_, bool>(0);
        if duplicate {
            mark_signal_request(&transaction, &self.source_name, request.id, "duplicate").await?;
            transaction
                .commit()
                .await
                .context("committing duplicate Process signal")?;
            return Ok(SignalConsumption::Duplicate {
                request_id: request.id,
            });
        }

        let anchor = self
            .deployed_catalog
            .revision(&request.process_name, &request.process_revision)
            .ok_or_else(|| {
                anyhow!(
                    "Process signal request `{}` references deployed revision `{}.{}` / `{}` absent from the published Engine snapshot",
                    request.id,
                    self.source_name,
                    request.process_name,
                    request.process_revision
                )
            })?;
        if anchor.source != self.source_name
            || anchor.name != request.process_name
            || anchor.revision_fingerprint != request.process_revision
        {
            bail!(
                "published Process revision identity does not match signal request `{}`",
                request.id
            );
        }
        let anchor_signal = anchor.signals.get(&request.signal_name).ok_or_else(|| {
            anyhow!(
                "Process signal request `{}` names absent signal `{}`",
                request.id,
                request.signal_name
            )
        })?;
        anchor_signal
            .correlation
            .validate(
                &typed_value(&request.correlation)
                    .context("decoding Process signal request correlation")?,
            )
            .map_err(|error| {
                anyhow!(
                    "Process signal request `{}` has invalid correlation: {error}",
                    request.id
                )
            })?;
        anchor_signal
            .payload
            .validate(
                &typed_value(&request.payload)
                    .context("decoding Process signal request payload")?,
            )
            .map_err(|error| {
                anyhow!(
                    "Process signal request `{}` has invalid payload: {error}",
                    request.id
                )
            })?;

        let wait_states = compatible_wait_states(
            self,
            &request.process_name,
            &request.signal_name,
            &anchor_signal.contract_fingerprint,
        );
        let revisions = wait_states.keys().cloned().collect::<Vec<_>>();
        let state_names = wait_states
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let rows = if revisions.is_empty() || state_names.is_empty() {
            Vec::new()
        } else {
            transaction
                .query(
                    "
                    SELECT
                        instance.id AS instance_id,
                        instance.revision,
                        instance.current_state,
                        instance.version,
                        instance.input_json,
                        instance.state_json,
                        instance.caller_session_json,
                        to_jsonb(statement_timestamp()) AS workflow_time,
                        marker.id AS marker_id,
                        marker.created_at AS marker_created_at,
                        marker.available_at AS marker_available_at,
                        marker.payload_json AS marker_payload
                    FROM donat.process_instances instance
                    LEFT JOIN LATERAL (
                        SELECT
                            event.id,
                            event.created_at,
                            event.available_at,
                            event.payload_json
                        FROM donat.process_events event
                        WHERE event.source_name = instance.source_name
                          AND event.instance_id = instance.id
                          AND event.kind = 'timer'
                          AND event.status = 'pending'
                          AND event.payload_json ->> 'wait_state'
                              = instance.current_state
                          AND event.payload_json ->> 'wait_version'
                              = instance.version::text
                        ORDER BY event.created_at DESC, event.id
                        LIMIT 1
                    ) marker ON TRUE
                    WHERE instance.source_name = $1
                      AND instance.process_name = $2
                      AND instance.status = 'running'
                      AND instance.revision = ANY($3::text[])
                      AND instance.current_state = ANY($4::text[])
                    ORDER BY instance.id
                    FOR UPDATE OF instance
                    ",
                    &[
                        &self.source_name,
                        &request.process_name,
                        &revisions,
                        &state_names,
                    ],
                )
                .await
                .context("locking compatible Process signal candidates")?
        };

        let mut matching = Vec::new();
        for row in rows {
            let candidate = signal_candidate(&row);
            let Some(definition) = self
                .deployed_catalog
                .revision(&request.process_name, &candidate.revision)
            else {
                bail!(
                    "locked Process signal candidate references absent revision `{}`",
                    candidate.revision
                );
            };
            if !wait_states
                .get(&candidate.revision)
                .is_some_and(|states| states.contains(&candidate.current_state))
            {
                continue;
            }
            let wait = definition
                .states
                .get(&candidate.current_state)
                .and_then(|state| match &state.operation {
                    CompiledProcessStateOperation::Wait(wait) => match wait.as_ref() {
                        CompiledProcessWaitState::Signal(wait)
                            if wait.signal == request.signal_name =>
                        {
                            Some(wait)
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .ok_or_else(|| {
                    anyhow!(
                        "compiled signal wait `{}` disappeared from revision `{}`",
                        candidate.current_state,
                        candidate.revision
                    )
                })?;
            let context = ProcessValueContext {
                source_name: &self.source_name,
                instance_id: candidate.instance_id,
                input: &candidate.input,
                state: &candidate.state,
                caller_session: candidate.caller_session.as_ref(),
                workflow_time: &candidate.workflow_time,
                item: None,
            };
            let expected = Json::Object(
                evaluate_process_values(&wait.correlate, &context)?
                    .into_iter()
                    .collect(),
            );
            if expected != request.correlation {
                continue;
            }
            let receptive = candidate.marker_id.is_some()
                && candidate
                    .marker_created_at
                    .is_some_and(|entered_at| entered_at <= request.created_at)
                && candidate
                    .marker_available_at
                    .is_some_and(|deadline| request.created_at <= deadline)
                && candidate.marker_payload.as_ref().is_some_and(|payload| {
                    payload.get("signal_name").and_then(Json::as_str)
                        == Some(request.signal_name.as_str())
                        && payload.get("correlation") == Some(&request.correlation)
                });
            matching.push(MatchingCandidate {
                instance_id: candidate.instance_id,
                revision: candidate.revision,
                current_state: candidate.current_state,
                version: candidate.version,
                receptive,
            });
        }

        let receptive = matching
            .iter()
            .filter(|candidate| candidate.receptive)
            .collect::<Vec<_>>();
        let mut known_targets = matching
            .iter()
            .map(|candidate| candidate.instance_id)
            .collect::<BTreeSet<_>>();
        if receptive.is_empty() && !revisions.is_empty() {
            let rows = transaction
                .query_one(
                    "
                    SELECT array_agg(DISTINCT event.instance_id)
                    FROM donat.process_events event
                    JOIN donat.process_instances instance
                      ON instance.source_name = event.source_name
                     AND instance.id = event.instance_id
                    WHERE event.source_name = $1
                      AND event.process_name = $2
                      AND event.revision = ANY($3::text[])
                      AND event.kind = 'timer'
                      AND event.payload_json ? 'signal_name'
                      AND event.payload_json @> jsonb_build_object(
                          'signal_name', $4::text,
                          'correlation', $5::jsonb
                      )
                    ",
                    &[
                        &self.source_name,
                        &request.process_name,
                        &revisions,
                        &request.signal_name,
                        &request.correlation,
                    ],
                )
                .await
                .context("checking historical Process signal correlations")?
                .get::<_, Option<Vec<Uuid>>>(0)
                .unwrap_or_default();
            known_targets.extend(rows);
        }

        let outcome = match receptive.as_slice() {
            [] if known_targets.is_empty() => {
                mark_signal_request(&transaction, &self.source_name, request.id, "unmatched")
                    .await?;
                SignalConsumption::Unmatched {
                    request_id: request.id,
                }
            }
            [] if known_targets.len() == 1 => {
                mark_signal_request(
                    &transaction,
                    &self.source_name,
                    request.id,
                    "unexpected_state",
                )
                .await?;
                SignalConsumption::UnexpectedState {
                    request_id: request.id,
                }
            }
            [] | [_, _, ..] => {
                mark_signal_request(&transaction, &self.source_name, request.id, "ambiguous")
                    .await?;
                SignalConsumption::Ambiguous {
                    request_id: request.id,
                }
            }
            [candidate] => {
                let event_id =
                    append_signal_event(&transaction, &self.source_name, &request, candidate)
                        .await?;
                mark_signal_request(&transaction, &self.source_name, request.id, "consumed")
                    .await?;
                SignalConsumption::Accepted {
                    request_id: request.id,
                    instance_id: candidate.instance_id,
                    event_id,
                }
            }
        };
        transaction
            .commit()
            .await
            .context("committing Process signal consumption")?;
        Ok(outcome)
    }
}

fn length_prefixed_signal_key(
    source_name: &str,
    process_name: &str,
    signal_name: &str,
    idempotency_key: &str,
) -> String {
    format!(
        "donat.process.signal.v1:{}:{source_name}{}:{process_name}{}:{signal_name}{}:{idempotency_key}",
        source_name.len(),
        process_name.len(),
        signal_name.len(),
        idempotency_key.len(),
    )
}

fn compatible_wait_states(
    runtime: &ProcessRuntime,
    process_name: &str,
    signal_name: &str,
    contract_fingerprint: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    runtime
        .deployed_catalog
        .active
        .values()
        .chain(runtime.deployed_catalog.live_retired.values())
        .filter(|definition| {
            definition.name == process_name
                && definition
                    .signals
                    .get(signal_name)
                    .is_some_and(|signal| signal.contract_fingerprint == contract_fingerprint)
        })
        .map(|definition| {
            let states = definition
                .states
                .iter()
                .filter_map(|(name, state)| match &state.operation {
                    CompiledProcessStateOperation::Wait(wait) => match wait.as_ref() {
                        CompiledProcessWaitState::Signal(wait) if wait.signal == signal_name => {
                            Some(name.clone())
                        }
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            (definition.revision_fingerprint.clone(), states)
        })
        .collect()
}

fn signal_request(row: &Row) -> SignalRequest {
    SignalRequest {
        id: row.get("id"),
        process_name: row.get("process_name"),
        process_revision: row.get("process_revision"),
        signal_name: row.get("signal_name"),
        correlation: row.get("correlation_json"),
        payload: row.get("payload_json"),
        idempotency_key: row.get("idempotency_key"),
        created_at: row.get("created_at"),
    }
}

fn signal_candidate(row: &Row) -> SignalCandidate {
    SignalCandidate {
        instance_id: row.get("instance_id"),
        revision: row.get("revision"),
        current_state: row.get("current_state"),
        version: row.get("version"),
        input: row.get("input_json"),
        state: row.get("state_json"),
        caller_session: row.get("caller_session_json"),
        workflow_time: row.get("workflow_time"),
        marker_id: row.get("marker_id"),
        marker_created_at: row.get("marker_created_at"),
        marker_available_at: row.get("marker_available_at"),
        marker_payload: row.get("marker_payload"),
    }
}

async fn append_signal_event(
    transaction: &Transaction<'_>,
    source_name: &str,
    request: &SignalRequest,
    candidate: &MatchingCandidate,
) -> anyhow::Result<Uuid> {
    let idempotency_key = format!("signal-request:{}", request.id);
    Ok(transaction
        .query_one(
            "
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
            VALUES (
                $1,
                $2,
                $3,
                $4,
                'signal',
                $5,
                $6,
                $7,
                'pending',
                statement_timestamp()
            )
            RETURNING id
            ",
            &[
                &source_name,
                &candidate.instance_id,
                &request.process_name,
                &candidate.revision,
                &json!({
                    "wait_state": candidate.current_state,
                    "wait_version": candidate.version,
                    "signal_name": request.signal_name,
                    "correlation": request.correlation,
                    "payload": request.payload,
                    "signal_request_id": request.id.to_string(),
                }),
                &idempotency_key,
                &request.created_at,
            ],
        )
        .await
        .context("appending accepted Process signal event")?
        .get("id"))
}

async fn mark_signal_request(
    transaction: &Transaction<'_>,
    source_name: &str,
    request_id: Uuid,
    status: &str,
) -> anyhow::Result<()> {
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_signal_requests
            SET status = $3,
                consumed_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND status = 'pending'
            ",
            &[&source_name, &request_id, &status],
        )
        .await
        .context("recording Process signal request outcome")?;
    if updated != 1 {
        bail!(
            "claimed Process signal request `{source_name}` / `{request_id}` changed before outcome commit"
        );
    }
    Ok(())
}
