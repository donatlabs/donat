//! Atomic verified-webhook delivery into receptive durable Process waits.
//!
//! The connector boundary authenticates and normalizes an event before this
//! module sees it. This module owns the source-local transaction that records
//! every verified delivery, deduplicates the provider identity, selects at
//! most one exact wait version, and appends its Process event. Raw bodies,
//! signature material, and resolved credentials are structurally absent.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{DateTime, Utc};
use donat_connector_abi::{TriggerId, VerifiedInboundEvent};
use donat_connector_catalog::TriggerSpec;
use donat_ir::{CanonicalNumber, TypedValue};
use donat_metadata::ProcessWebhookGuardValue;
use donat_rules::RuleCatalog;
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as Json, json};
use tokio_postgres::{Row, Transaction};
use uuid::Uuid;

use crate::connectors::ConnectorRegistry;
use crate::state::{Engine, SourceRuntime};

use super::value::{ProcessValueContext, evaluate_process_value, evaluate_process_values};
use super::{
    CompiledProcessDefinition, CompiledProcessStateOperation, CompiledProcessWaitState,
    CompiledProcessWebhookGuard, CompiledProcessWebhookWait, DeployedSourceProcessCatalog,
    ProcessRuntime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidSignatureStatus {
    Missing,
    Invalid,
    Expired,
    Malformed,
    Unsupported,
}

impl InvalidSignatureStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Malformed => "malformed",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundPersistence {
    Accepted {
        delivery_id: Uuid,
        instance_id: Uuid,
        process_event_id: Uuid,
    },
    Duplicate {
        delivery_id: Uuid,
    },
    Unmatched {
        delivery_id: Uuid,
    },
    Ambiguous {
        delivery_id: Uuid,
    },
    GuardFalse {
        delivery_id: Uuid,
    },
    UnexpectedState {
        delivery_id: Uuid,
    },
}

impl InboundPersistence {
    fn audit_only(delivery_id: Uuid, outcome: InboundOutcome) -> Self {
        match outcome {
            InboundOutcome::Unmatched => Self::Unmatched { delivery_id },
            InboundOutcome::Ambiguous => Self::Ambiguous { delivery_id },
            InboundOutcome::GuardFalse => Self::GuardFalse { delivery_id },
            InboundOutcome::UnexpectedState => Self::UnexpectedState { delivery_id },
        }
    }
}

#[derive(Clone, Copy)]
enum InboundOutcome {
    Unmatched,
    Ambiguous,
    GuardFalse,
    UnexpectedState,
}

impl InboundOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unmatched => "unmatched",
            Self::Ambiguous => "ambiguous",
            Self::GuardFalse => "guard_false",
            Self::UnexpectedState => "unexpected_state",
        }
    }
}

struct InboundRuntime<'a> {
    source_name: &'a str,
    pool: &'a deadpool_postgres::Pool,
    deployed_catalog: &'a DeployedSourceProcessCatalog,
    rules: &'a RuleCatalog,
    connectors: &'a ConnectorRegistry,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd)]
struct WebhookWaitAddress {
    process_name: String,
    revision: String,
    state_name: String,
}

struct InboundCandidate {
    instance_id: Uuid,
    process_name: String,
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
    pending_signal: bool,
}

struct MatchingCandidate {
    instance_id: Uuid,
    process_name: String,
    revision: String,
    current_state: String,
    version: i64,
    input: Json,
    state: Json,
    caller_session: Option<Json>,
    workflow_time: Json,
    guard: Option<CompiledProcessWebhookGuard>,
    receptive: bool,
}

impl ProcessRuntime {
    /// Persist one provider-authenticated event and, when exactly one current
    /// wait version receives it, append that Process event in the same commit.
    pub async fn persist_verified_inbound(
        &self,
        connector_instance: &str,
        trigger: &TriggerSpec,
        event: VerifiedInboundEvent,
    ) -> anyhow::Result<InboundPersistence> {
        InboundRuntime {
            source_name: &self.source_name,
            pool: &self.pool,
            deployed_catalog: self.deployed_catalog.as_ref(),
            rules: self.planning_snapshot.rules(),
            connectors: self.connector_registry.as_ref(),
        }
        .persist_verified(connector_instance, trigger, event)
        .await
    }

    /// Persist a bounded, non-identifying audit row for an event that did not
    /// cross the connector authentication boundary.
    pub async fn persist_invalid_inbound(
        &self,
        connector_instance: &str,
        status: InvalidSignatureStatus,
        payload_digest: [u8; 32],
        redacted_metadata: BTreeMap<String, TypedValue>,
    ) -> anyhow::Result<Uuid> {
        InboundRuntime {
            source_name: &self.source_name,
            pool: &self.pool,
            deployed_catalog: self.deployed_catalog.as_ref(),
            rules: self.planning_snapshot.rules(),
            connectors: self.connector_registry.as_ref(),
        }
        .persist_invalid(
            connector_instance,
            status,
            &payload_digest,
            &typed_value_to_json(&TypedValue::Object(redacted_metadata))?,
        )
        .await
    }
}

/// Persist verified ingress from the exact immutable Engine snapshot captured
/// by an HTTP request. This deliberately builds no worker and clones no
/// metadata or catalog.
pub(crate) async fn persist_verified_from_engine(
    engine: &Engine,
    connectors: &ConnectorRegistry,
    source_name: &str,
    connector_instance: &str,
    trigger: &TriggerSpec,
    event: VerifiedInboundEvent,
) -> anyhow::Result<InboundPersistence> {
    inbound_runtime(engine, connectors, source_name)?
        .persist_verified(connector_instance, trigger, event)
        .await
}

/// Best-effort invalid-signature audit for the provider HTTP boundary.
pub(crate) async fn persist_invalid_from_engine(
    engine: &Engine,
    connectors: &ConnectorRegistry,
    source_name: &str,
    connector_instance: &str,
    status: InvalidSignatureStatus,
    payload_digest: &[u8; 32],
    redacted_metadata: &BTreeMap<String, TypedValue>,
) -> anyhow::Result<Uuid> {
    let metadata = typed_value_to_json(&TypedValue::Object(redacted_metadata.clone()))?;
    inbound_runtime(engine, connectors, source_name)?
        .persist_invalid(connector_instance, status, payload_digest, &metadata)
        .await
}

fn inbound_runtime<'a>(
    engine: &'a Engine,
    connectors: &'a ConnectorRegistry,
    source_name: &'a str,
) -> anyhow::Result<InboundRuntime<'a>> {
    let SourceRuntime::Postgres { pool, .. } = engine
        .runtimes
        .get(source_name)
        .ok_or_else(|| anyhow!("Process source `{source_name}` has no published runtime"))?
    else {
        bail!("Process webhook source `{source_name}` must use Postgres");
    };
    let deployed_catalog = engine
        .deployed_process_catalog
        .source(source_name)
        .ok_or_else(|| anyhow!("Process source `{source_name}` has no deployed catalog"))?;
    Ok(InboundRuntime {
        source_name,
        pool,
        deployed_catalog,
        rules: engine.rule_catalog.as_ref(),
        connectors,
    })
}

impl InboundRuntime<'_> {
    async fn persist_verified(
        &self,
        connector_instance: &str,
        trigger: &TriggerSpec,
        event: VerifiedInboundEvent,
    ) -> anyhow::Result<InboundPersistence> {
        let trigger_id = validate_verified_event(self, connector_instance, trigger, &event)?;
        let output = typed_value_to_json(event.output())
            .context("converting normalized webhook output to durable JSON")?;
        let redacted_metadata = typed_value_to_json(event.redacted_metadata())
            .context("converting redacted webhook metadata to durable JSON")?;
        let provider_event_id = event.provider_event_id().to_owned();
        let event_type = event.event_type().to_owned();
        let payload_digest = event.payload_digest().as_bytes().to_vec();
        let wait_addresses =
            compatible_webhook_waits(self, connector_instance, trigger_id, trigger)?;

        let mut client = self.pool.get().await.with_context(|| {
            format!(
                "opening verified webhook transaction for source `{}`",
                self.source_name
            )
        })?;
        let transaction = client
            .transaction()
            .await
            .context("starting verified webhook transaction")?;
        let received_at: DateTime<Utc> = transaction
            .query_one("SELECT statement_timestamp()", &[])
            .await
            .context("pinning verified webhook receipt time")?
            .get(0);
        transaction
            .query_one(
                "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
                &[&length_prefixed_inbound_key(
                    self.source_name,
                    connector_instance,
                    &provider_event_id,
                )],
            )
            .await
            .context("serializing verified provider event identity")?;

        if inbound_event_exists(
            &transaction,
            self.source_name,
            connector_instance,
            &provider_event_id,
        )
        .await?
        {
            let delivery_id = insert_delivery(
                &transaction,
                DeliveryRecord {
                    source_name: self.source_name,
                    connector_instance,
                    provider_event_id: Some(&provider_event_id),
                    payload_digest: &payload_digest,
                    signature_status: "verified",
                    outcome: "duplicate",
                    instance_id: None,
                    process_event_id: None,
                    redacted_metadata: &redacted_metadata,
                    received_at,
                },
            )
            .await?;
            transaction
                .commit()
                .await
                .context("committing duplicate verified webhook audit")?;
            return Ok(InboundPersistence::Duplicate { delivery_id });
        }

        let candidates = lock_candidates(
            &transaction,
            self.source_name,
            connector_instance,
            trigger_id,
            &wait_addresses,
            received_at,
            &output,
        )
        .await?;
        let mut matching = Vec::new();
        for candidate in candidates {
            let definition = self
                .deployed_catalog
                .revision(&candidate.process_name, &candidate.revision)
                .ok_or_else(|| {
                    anyhow!(
                        "locked webhook candidate references absent revision `{}.{}`",
                        candidate.process_name,
                        candidate.revision
                    )
                })?;
            let wait = webhook_wait(
                definition,
                &candidate.current_state,
                connector_instance,
                trigger_id,
            )?;
            if candidate.marker_id.is_some()
                && candidate.marker_payload.as_ref().is_none_or(|payload| {
                    payload.get("connector_instance").and_then(Json::as_str)
                        != Some(connector_instance)
                        || payload.get("trigger").and_then(Json::as_str)
                            != Some(trigger_id.as_str())
                        || payload.get("route").and_then(Json::as_str) != Some("timeout")
                })
            {
                bail!(
                    "receptive Process webhook marker for instance `{}` differs from its pinned trigger",
                    candidate.instance_id
                );
            }
            let context = candidate_context(self.source_name, &candidate);
            let correlation = if candidate.marker_id.is_some() {
                candidate
                    .marker_payload
                    .as_ref()
                    .and_then(|payload| payload.get("correlation"))
                    .cloned()
                    .ok_or_else(|| {
                        anyhow!(
                            "receptive Process webhook marker for instance `{}` has no correlation",
                            candidate.instance_id
                        )
                    })?
            } else {
                Json::Object(
                    evaluate_process_values(&wait.correlate, &context)?
                        .into_iter()
                        .collect(),
                )
            };
            if !correlation_matches_output(&correlation, &output)? {
                continue;
            }
            let receptive = candidate.marker_id.is_some()
                && candidate
                    .marker_created_at
                    .is_some_and(|entered_at| entered_at <= received_at)
                && candidate
                    .marker_available_at
                    .is_some_and(|deadline| received_at <= deadline)
                && !candidate.pending_signal;
            matching.push(MatchingCandidate {
                instance_id: candidate.instance_id,
                process_name: candidate.process_name,
                revision: candidate.revision,
                current_state: candidate.current_state,
                version: candidate.version,
                input: candidate.input,
                state: candidate.state,
                caller_session: candidate.caller_session,
                workflow_time: candidate.workflow_time,
                guard: wait.guard.clone(),
                receptive,
            });
        }

        let receptive = matching
            .iter()
            .filter(|candidate| candidate.receptive)
            .collect::<Vec<_>>();
        let selected = match receptive.as_slice() {
            [candidate] => Some(*candidate),
            _ => None,
        };
        let audit_outcome = match receptive.as_slice() {
            [candidate] => {
                if evaluate_guard(
                    self,
                    definition_for_candidate(self, candidate)?,
                    candidate,
                    &output,
                )? {
                    None
                } else {
                    Some(InboundOutcome::GuardFalse)
                }
            }
            [_, _, ..] => Some(InboundOutcome::Ambiguous),
            [] => {
                let mut known_targets = matching
                    .iter()
                    .map(|candidate| candidate.instance_id)
                    .collect::<BTreeSet<_>>();
                known_targets.extend(
                    historical_targets(
                        &transaction,
                        self.source_name,
                        connector_instance,
                        trigger_id,
                        &wait_addresses,
                        &output,
                    )
                    .await?,
                );
                Some(if known_targets.is_empty() {
                    InboundOutcome::Unmatched
                } else if known_targets.len() == 1 {
                    InboundOutcome::UnexpectedState
                } else {
                    InboundOutcome::Ambiguous
                })
            }
        };

        let result = if let Some(outcome) = audit_outcome {
            let delivery_id = insert_delivery(
                &transaction,
                DeliveryRecord {
                    source_name: self.source_name,
                    connector_instance,
                    provider_event_id: Some(&provider_event_id),
                    payload_digest: &payload_digest,
                    signature_status: "verified",
                    outcome: outcome.as_str(),
                    instance_id: None,
                    process_event_id: None,
                    redacted_metadata: &redacted_metadata,
                    received_at,
                },
            )
            .await?;
            insert_inbound_event(
                &transaction,
                self.source_name,
                connector_instance,
                &provider_event_id,
                delivery_id,
                &payload_digest,
                received_at,
            )
            .await?;
            InboundPersistence::audit_only(delivery_id, outcome)
        } else {
            let candidate =
                selected.expect("one receptive candidate exists when no audit outcome was chosen");
            let process_event_id = insert_process_event(
                &transaction,
                self.source_name,
                connector_instance,
                trigger_id,
                &provider_event_id,
                &event_type,
                &output,
                candidate,
                received_at,
            )
            .await?;
            let delivery_id = insert_delivery(
                &transaction,
                DeliveryRecord {
                    source_name: self.source_name,
                    connector_instance,
                    provider_event_id: Some(&provider_event_id),
                    payload_digest: &payload_digest,
                    signature_status: "verified",
                    outcome: "accepted",
                    instance_id: Some(candidate.instance_id),
                    process_event_id: Some(process_event_id),
                    redacted_metadata: &redacted_metadata,
                    received_at,
                },
            )
            .await?;
            insert_inbound_event(
                &transaction,
                self.source_name,
                connector_instance,
                &provider_event_id,
                delivery_id,
                &payload_digest,
                received_at,
            )
            .await?;
            InboundPersistence::Accepted {
                delivery_id,
                instance_id: candidate.instance_id,
                process_event_id,
            }
        };
        transaction
            .commit()
            .await
            .context("committing verified webhook delivery")?;
        Ok(result)
    }

    async fn persist_invalid(
        &self,
        connector_instance: &str,
        status: InvalidSignatureStatus,
        payload_digest: &[u8; 32],
        redacted_metadata: &Json,
    ) -> anyhow::Result<Uuid> {
        let webhook = self
            .connectors
            .webhook_instance(connector_instance)
            .filter(|instance| instance.source_name() == self.source_name)
            .ok_or_else(|| {
                anyhow!(
                    "connector instance `{connector_instance}` has no webhook in source `{}`",
                    self.source_name
                )
            })?;
        let _ = webhook;
        let mut client = self.pool.get().await.with_context(|| {
            format!(
                "opening invalid webhook audit transaction for source `{}`",
                self.source_name
            )
        })?;
        let transaction = client
            .transaction()
            .await
            .context("starting invalid webhook audit transaction")?;
        let received_at: DateTime<Utc> = transaction
            .query_one("SELECT statement_timestamp()", &[])
            .await
            .context("pinning invalid webhook receipt time")?
            .get(0);
        let delivery_id = insert_delivery(
            &transaction,
            DeliveryRecord {
                source_name: self.source_name,
                connector_instance,
                provider_event_id: None,
                payload_digest,
                signature_status: status.as_str(),
                outcome: "invalid_signature",
                instance_id: None,
                process_event_id: None,
                redacted_metadata,
                received_at,
            },
        )
        .await?;
        transaction
            .commit()
            .await
            .context("committing invalid webhook audit")?;
        Ok(delivery_id)
    }
}

fn validate_verified_event(
    runtime: &InboundRuntime<'_>,
    connector_instance: &str,
    trigger: &TriggerSpec,
    event: &VerifiedInboundEvent,
) -> anyhow::Result<TriggerId> {
    let TriggerSpec::Webhook {
        trigger: trigger_id,
        event_id,
        event_type,
        output,
        ..
    } = trigger
    else {
        bail!("a verified HTTP webhook cannot use a poll trigger");
    };
    let live = runtime
        .connectors
        .trigger_spec_handle(runtime.source_name, connector_instance, *trigger_id)
        .ok_or_else(|| {
            anyhow!(
                "connector trigger `{}.{}` is absent from source `{}`",
                connector_instance,
                trigger_id.as_str(),
                runtime.source_name
            )
        })?;
    if !std::ptr::eq(trigger, live.as_ref()) {
        bail!(
            "connector trigger `{}.{}` is not the immutable registry snapshot",
            connector_instance,
            trigger_id.as_str()
        );
    }
    event_id
        .validate(&singleton_contract_value(
            event_id,
            event.provider_event_id(),
            "provider event ID",
        )?)
        .map_err(|error| anyhow!("verified provider event ID violated its contract: {error}"))?;
    event_type
        .validate(&singleton_contract_value(
            event_type,
            event.event_type(),
            "provider event type",
        )?)
        .map_err(|error| anyhow!("verified provider event type violated its contract: {error}"))?;
    output
        .validate(event.output())
        .map_err(|error| anyhow!("normalized webhook output violated its contract: {error}"))?;
    Ok(*trigger_id)
}

fn singleton_contract_value(
    contract: &donat_ir::ValueContractCatalog,
    value: &str,
    identity: &str,
) -> anyhow::Result<TypedValue> {
    if contract.roots.len() != 1 {
        bail!("{identity} contract must declare exactly one root field");
    }
    let field = contract
        .roots
        .keys()
        .next()
        .expect("a singleton contract has one root")
        .clone();
    Ok(TypedValue::Object(BTreeMap::from([(
        field,
        TypedValue::String(value.to_owned()),
    )])))
}

fn compatible_webhook_waits(
    runtime: &InboundRuntime<'_>,
    connector_instance: &str,
    trigger_id: TriggerId,
    trigger: &TriggerSpec,
) -> anyhow::Result<Vec<WebhookWaitAddress>> {
    let live_fingerprint = runtime
        .connectors
        .trigger_configuration_fingerprint(connector_instance, trigger_id)
        .ok_or_else(|| {
            anyhow!(
                "connector trigger `{}.{}` has no deployment fingerprint",
                connector_instance,
                trigger_id.as_str()
            )
        })?;
    let mut addresses = BTreeSet::new();
    for definition in runtime
        .deployed_catalog
        .active
        .values()
        .chain(runtime.deployed_catalog.live_retired.values())
    {
        let key = (
            runtime.source_name.to_owned(),
            connector_instance.to_owned(),
            trigger_id,
        );
        let Some(dependency) = definition.dependencies.connector_triggers.get(&key) else {
            continue;
        };
        if dependency.source != runtime.source_name
            || dependency.instance != connector_instance
            || !std::ptr::eq(dependency.spec.as_ref(), trigger)
            || dependency.deployment_fingerprint != live_fingerprint
        {
            bail!(
                "Process `{}.{}` retained a connector trigger different from its published dependency",
                definition.name,
                definition.revision_fingerprint
            );
        }
        for (state_name, state) in &definition.states {
            if matches!(
                &state.operation,
                CompiledProcessStateOperation::Wait(wait)
                    if matches!(
                        wait.as_ref(),
                        CompiledProcessWaitState::Webhook(wait)
                            if wait.connector == connector_instance
                                && wait.trigger == trigger_id
                    )
            ) {
                addresses.insert(WebhookWaitAddress {
                    process_name: definition.name.clone(),
                    revision: definition.revision_fingerprint.clone(),
                    state_name: state_name.clone(),
                });
            }
        }
    }
    Ok(addresses.into_iter().collect())
}

async fn lock_candidates(
    transaction: &Transaction<'_>,
    source_name: &str,
    connector_instance: &str,
    trigger_id: TriggerId,
    addresses: &[WebhookWaitAddress],
    received_at: DateTime<Utc>,
    output: &Json,
) -> anyhow::Result<Vec<InboundCandidate>> {
    if addresses.is_empty() {
        return Ok(Vec::new());
    }
    let process_names = addresses
        .iter()
        .map(|address| address.process_name.clone())
        .collect::<Vec<_>>();
    let revisions = addresses
        .iter()
        .map(|address| address.revision.clone())
        .collect::<Vec<_>>();
    let state_names = addresses
        .iter()
        .map(|address| address.state_name.clone())
        .collect::<Vec<_>>();
    let rows = transaction
        .query(
            "
            SELECT
                instance.id AS instance_id,
                instance.process_name,
                instance.revision,
                instance.current_state,
                instance.version,
                instance.input_json,
                instance.state_json,
                instance.caller_session_json,
                COALESCE(
                    to_jsonb(entry.available_at),
                    to_jsonb($7::timestamptz)
                ) AS workflow_time,
                marker.id AS marker_id,
                marker.created_at AS marker_created_at,
                marker.available_at AS marker_available_at,
                marker.payload_json AS marker_payload,
                EXISTS (
                    SELECT 1
                    FROM donat.process_events accepted
                    WHERE accepted.source_name = instance.source_name
                      AND accepted.instance_id = instance.id
                      AND accepted.kind = 'signal'
                      AND accepted.status = 'pending'
                      AND accepted.payload_json ->> 'wait_state'
                          = instance.current_state
                      AND accepted.payload_json ->> 'wait_version'
                          = instance.version::text
                ) AS pending_signal
            FROM donat.process_instances instance
            JOIN unnest($2::text[], $3::text[], $4::text[])
                AS target(process_name, revision, state_name)
              ON target.process_name = instance.process_name
             AND target.revision = instance.revision
             AND target.state_name = instance.current_state
            LEFT JOIN LATERAL (
                SELECT event.available_at
                FROM donat.process_events event
                WHERE event.source_name = instance.source_name
                  AND event.instance_id = instance.id
                  AND event.status = 'pending'
                  AND (
                      (instance.version = 0 AND event.kind = 'start')
                      OR (
                          event.kind = 'continue'
                          AND event.idempotency_key =
                              'continue:' || instance.id::text || ':' ||
                              instance.version::text
                      )
                  )
                ORDER BY event.available_at, event.id
                LIMIT 1
            ) entry ON TRUE
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
              AND instance.status = 'running'
              AND (
                  marker.id IS NULL
                  OR marker.payload_json ->> 'connector_instance'
                      IS DISTINCT FROM $5
                  OR marker.payload_json ->> 'trigger' IS DISTINCT FROM $6
                  OR marker.payload_json -> 'correlation' IS NULL
                  OR marker.payload_json -> 'correlation' <@ $8::jsonb
              )
            ORDER BY instance.id
            FOR UPDATE OF instance
            ",
            &[
                &source_name,
                &process_names,
                &revisions,
                &state_names,
                &connector_instance,
                &trigger_id.as_str(),
                &received_at,
                &output,
            ],
        )
        .await
        .context("locking compatible Process webhook candidates")?;
    Ok(rows.iter().map(inbound_candidate).collect())
}

fn inbound_candidate(row: &Row) -> InboundCandidate {
    InboundCandidate {
        instance_id: row.get("instance_id"),
        process_name: row.get("process_name"),
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
        pending_signal: row.get("pending_signal"),
    }
}

fn candidate_context<'a>(
    source_name: &'a str,
    candidate: &'a InboundCandidate,
) -> ProcessValueContext<'a> {
    ProcessValueContext {
        source_name,
        instance_id: candidate.instance_id,
        input: &candidate.input,
        state: &candidate.state,
        caller_session: candidate.caller_session.as_ref(),
        workflow_time: &candidate.workflow_time,
        item: None,
        item_key: None,
    }
}

fn webhook_wait<'a>(
    definition: &'a CompiledProcessDefinition,
    state_name: &str,
    connector_instance: &str,
    trigger_id: TriggerId,
) -> anyhow::Result<&'a CompiledProcessWebhookWait> {
    definition
        .states
        .get(state_name)
        .and_then(|state| match &state.operation {
            CompiledProcessStateOperation::Wait(wait) => match wait.as_ref() {
                CompiledProcessWaitState::Webhook(wait)
                    if wait.connector == connector_instance && wait.trigger == trigger_id =>
                {
                    Some(wait)
                }
                _ => None,
            },
            _ => None,
        })
        .ok_or_else(|| {
            anyhow!(
                "compiled webhook wait `{state_name}` disappeared from revision `{}`",
                definition.revision_fingerprint
            )
        })
}

fn definition_for_candidate<'a>(
    runtime: &'a InboundRuntime<'_>,
    candidate: &MatchingCandidate,
) -> anyhow::Result<&'a CompiledProcessDefinition> {
    runtime
        .deployed_catalog
        .revision(&candidate.process_name, &candidate.revision)
        .map(Arc::as_ref)
        .ok_or_else(|| {
            anyhow!(
                "selected webhook candidate references absent revision `{}.{}`",
                candidate.process_name,
                candidate.revision
            )
        })
}

fn correlation_matches_output(correlation: &Json, output: &Json) -> anyhow::Result<bool> {
    let correlation = correlation
        .as_object()
        .ok_or_else(|| anyhow!("persisted Process webhook correlation is not an object"))?;
    let output = output
        .as_object()
        .ok_or_else(|| anyhow!("normalized Process webhook output is not an object"))?;
    Ok(correlation
        .iter()
        .all(|(field, expected)| output.get(field) == Some(expected)))
}

fn evaluate_guard(
    runtime: &InboundRuntime<'_>,
    definition: &CompiledProcessDefinition,
    candidate: &MatchingCandidate,
    output: &Json,
) -> anyhow::Result<bool> {
    let Some(guard) = &candidate.guard else {
        return Ok(true);
    };
    if !definition.dependencies.rules.contains_key(&guard.rule) {
        bail!(
            "Process webhook guard `{}` is absent from the pinned dependency closure",
            guard.rule
        );
    }
    let rule = runtime.rules.rule(&guard.rule).ok_or_else(|| {
        anyhow!(
            "Process webhook guard `{}` is absent from the immutable rule snapshot",
            guard.rule
        )
    })?;
    let context = ProcessValueContext {
        source_name: runtime.source_name,
        instance_id: candidate.instance_id,
        input: &candidate.input,
        state: &candidate.state,
        caller_session: candidate.caller_session.as_ref(),
        workflow_time: &candidate.workflow_time,
        item: None,
        item_key: None,
    };
    let output = output
        .as_object()
        .ok_or_else(|| anyhow!("normalized Process webhook output is not an object"))?;
    let mut bindings = BTreeMap::new();
    for (name, binding) in &guard.bindings {
        let value = match binding {
            ProcessWebhookGuardValue::Event { event } => output
                .get(event)
                .cloned()
                .ok_or_else(|| anyhow!("normalized webhook field `{event}` is absent"))?,
            ProcessWebhookGuardValue::Process(value) => evaluate_process_value(value, &context)
                .with_context(|| format!("evaluating webhook guard binding `{name}`"))?,
        };
        bindings.insert(name.clone(), value);
    }
    runtime
        .rules
        .evaluate_bool(rule, &bindings)
        .map_err(|error| anyhow!("evaluating Process webhook guard `{}`: {error}", guard.rule))
}

async fn historical_targets(
    transaction: &Transaction<'_>,
    source_name: &str,
    connector_instance: &str,
    trigger_id: TriggerId,
    addresses: &[WebhookWaitAddress],
    output: &Json,
) -> anyhow::Result<Vec<Uuid>> {
    if addresses.is_empty() {
        return Ok(Vec::new());
    }
    let process_names = addresses
        .iter()
        .map(|address| address.process_name.clone())
        .collect::<Vec<_>>();
    let revisions = addresses
        .iter()
        .map(|address| address.revision.clone())
        .collect::<Vec<_>>();
    let state_names = addresses
        .iter()
        .map(|address| address.state_name.clone())
        .collect::<Vec<_>>();
    transaction
        .query(
            "
            SELECT DISTINCT event.instance_id
            FROM donat.process_events event
            JOIN unnest($2::text[], $3::text[], $4::text[])
                AS target(process_name, revision, state_name)
              ON target.process_name = event.process_name
             AND target.revision = event.revision
             AND target.state_name = event.payload_json ->> 'wait_state'
            WHERE event.source_name = $1
              AND event.kind = 'timer'
              AND event.payload_json ? 'connector_instance'
              AND event.payload_json ? 'trigger'
              AND event.payload_json @> jsonb_build_object(
                  'connector_instance', $5::text,
                  'trigger', $6::text
              )
              AND event.payload_json -> 'correlation' <@ $7::jsonb
            ORDER BY event.instance_id
            ",
            &[
                &source_name,
                &process_names,
                &revisions,
                &state_names,
                &connector_instance,
                &trigger_id.as_str(),
                &output,
            ],
        )
        .await
        .context("checking historical Process webhook correlations")
        .map(|rows| rows.iter().map(|row| row.get(0)).collect())
}

async fn inbound_event_exists(
    transaction: &Transaction<'_>,
    source_name: &str,
    connector_instance: &str,
    provider_event_id: &str,
) -> anyhow::Result<bool> {
    Ok(transaction
        .query_one(
            "
            SELECT EXISTS (
                SELECT 1
                FROM donat.process_inbound_events
                WHERE source_name = $1
                  AND connector_instance = $2
                  AND provider_event_id = $3
            )
            ",
            &[&source_name, &connector_instance, &provider_event_id],
        )
        .await
        .context("checking verified provider event duplicate")?
        .get(0))
}

struct DeliveryRecord<'a> {
    source_name: &'a str,
    connector_instance: &'a str,
    provider_event_id: Option<&'a str>,
    payload_digest: &'a [u8],
    signature_status: &'a str,
    outcome: &'a str,
    instance_id: Option<Uuid>,
    process_event_id: Option<Uuid>,
    redacted_metadata: &'a Json,
    received_at: DateTime<Utc>,
}

async fn insert_delivery(
    transaction: &Transaction<'_>,
    record: DeliveryRecord<'_>,
) -> anyhow::Result<Uuid> {
    Ok(transaction
        .query_one(
            "
            INSERT INTO donat.process_inbound_deliveries (
                source_name,
                connector_instance,
                provider_event_id,
                payload_digest,
                signature_status,
                outcome,
                instance_id,
                process_event_id,
                redacted_metadata,
                received_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING id
            ",
            &[
                &record.source_name,
                &record.connector_instance,
                &record.provider_event_id,
                &record.payload_digest,
                &record.signature_status,
                &record.outcome,
                &record.instance_id,
                &record.process_event_id,
                &record.redacted_metadata,
                &record.received_at,
            ],
        )
        .await
        .context("inserting Process inbound delivery")?
        .get("id"))
}

async fn insert_inbound_event(
    transaction: &Transaction<'_>,
    source_name: &str,
    connector_instance: &str,
    provider_event_id: &str,
    first_delivery_id: Uuid,
    payload_digest: &[u8],
    verified_at: DateTime<Utc>,
) -> anyhow::Result<()> {
    transaction
        .execute(
            "
            INSERT INTO donat.process_inbound_events (
                source_name,
                connector_instance,
                provider_event_id,
                first_delivery_id,
                payload_digest,
                verified_at
            )
            VALUES ($1, $2, $3, $4, $5, $6)
            ",
            &[
                &source_name,
                &connector_instance,
                &provider_event_id,
                &first_delivery_id,
                &payload_digest,
                &verified_at,
            ],
        )
        .await
        .context("inserting verified Process inbound identity")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_process_event(
    transaction: &Transaction<'_>,
    source_name: &str,
    connector_instance: &str,
    trigger_id: TriggerId,
    provider_event_id: &str,
    event_type: &str,
    output: &Json,
    candidate: &MatchingCandidate,
    received_at: DateTime<Utc>,
) -> anyhow::Result<Uuid> {
    let idempotency_key = format!("webhook:{}:{}", connector_instance, provider_event_id);
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
            VALUES ($1, $2, $3, $4, 'signal', $5, $6, $7, 'pending', $7)
            RETURNING id
            ",
            &[
                &source_name,
                &candidate.instance_id,
                &candidate.process_name,
                &candidate.revision,
                &json!({
                    "wait_state": candidate.current_state,
                    "wait_version": candidate.version,
                    "connector_instance": connector_instance,
                    "trigger": trigger_id.as_str(),
                    "provider_event_id": provider_event_id,
                    "event_type": event_type,
                    "output": output,
                }),
                &idempotency_key,
                &received_at,
            ],
        )
        .await
        .context("appending accepted Process webhook event")?
        .get("id"))
}

fn length_prefixed_inbound_key(
    source_name: &str,
    connector_instance: &str,
    provider_event_id: &str,
) -> String {
    format!(
        "donat.process.inbound.v1:{}:{source_name}{}:{connector_instance}{}:{provider_event_id}",
        source_name.len(),
        connector_instance.len(),
        provider_event_id.len(),
    )
}

fn typed_value_to_json(value: &TypedValue) -> anyhow::Result<Json> {
    Ok(match value {
        TypedValue::Null => Json::Null,
        TypedValue::Boolean(value) => Json::Bool(*value),
        TypedValue::String(value) => Json::String(value.clone()),
        TypedValue::Number(CanonicalNumber::I64(value)) => Json::from(*value),
        TypedValue::Number(CanonicalNumber::U64(value)) => Json::from(*value),
        TypedValue::Number(CanonicalNumber::Decimal(value)) => {
            let number = JsonNumber::from_str(value.as_str())
                .context("converting canonical connector decimal to JSON")?;
            if number.to_string() != value.as_str() {
                bail!(
                    "canonical connector decimal `{}` cannot be represented exactly as durable JSON",
                    value.as_str()
                );
            }
            Json::Number(number)
        }
        TypedValue::List(values) => Json::Array(
            values
                .iter()
                .map(typed_value_to_json)
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        TypedValue::Object(values) => Json::Object(
            values
                .iter()
                .map(|(name, value)| Ok((name.clone(), typed_value_to_json(value)?)))
                .collect::<anyhow::Result<JsonMap<_, _>>>()?,
        ),
        TypedValue::InlineBytes(value) => json!({
            "bytes_base64": BASE64.encode(value.as_slice()),
            "media_type": value.media_type(),
            "file_name": value.file_name(),
        }),
    })
}
