//! Atomic consumption of command-to-Process start hand-offs.

use std::collections::BTreeMap;

use anyhow::{Context, anyhow, bail};
use donat_ir::{CanonicalDecimal, CanonicalNumber, TypedValue};
use serde_json::{Value as Json, json};
use uuid::Uuid;

use super::ProcessRuntime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartConsumption {
    NoWork,
    Started { request_id: Uuid, instance_id: Uuid },
    Duplicate { request_id: Uuid, instance_id: Uuid },
}

struct StartRequest {
    id: Uuid,
    process_name: String,
    revision: String,
    input: Json,
    command_invocation_id: Uuid,
    effect_position: i32,
    idempotency_key: String,
}

impl ProcessRuntime {
    /// Consume at most one pending source-local start request.
    ///
    /// Claim, exact-revision resolution, semantic instance dedupe, history,
    /// and request outcome commit in one short transaction. Dropping this
    /// future before commit rolls every write and the row lock back.
    pub async fn consume_one_start(&self) -> anyhow::Result<StartConsumption> {
        let mut client = self.pool.get().await.with_context(|| {
            format!(
                "checking pending Process starts for source `{}`",
                self.source_name
            )
        })?;
        let transaction = client
            .transaction()
            .await
            .context("starting Process start transaction")?;
        let Some(row) = transaction
            .query_opt(
                "
                SELECT
                    id,
                    process_name,
                    revision,
                    input_json,
                    command_invocation_id,
                    effect_position,
                    idempotency_key
                FROM donat.process_start_requests
                WHERE source_name = $1
                  AND status = 'pending'
                ORDER BY created_at, id
                FOR UPDATE SKIP LOCKED
                LIMIT 1
                ",
                &[&self.source_name],
            )
            .await
            .context("claiming one pending Process start request")?
        else {
            transaction
                .commit()
                .await
                .context("committing empty Process start claim")?;
            return Ok(StartConsumption::NoWork);
        };
        let request = StartRequest {
            id: row.get("id"),
            process_name: row.get("process_name"),
            revision: row.get("revision"),
            input: row.get("input_json"),
            command_invocation_id: row.get("command_invocation_id"),
            effect_position: row.get("effect_position"),
            idempotency_key: row.get("idempotency_key"),
        };

        let Some(definition) = self
            .deployed_catalog
            .revision(&request.process_name, &request.revision)
        else {
            tracing::error!(
                source = %self.source_name,
                process = %request.process_name,
                revision = %request.revision,
                request_id = %request.id,
                "pending Process start references a revision absent from the published snapshot"
            );
            bail!(
                "Process start request `{}` references deployed revision `{}.{}` / `{}` absent from the published Engine snapshot",
                request.id,
                self.source_name,
                request.process_name,
                request.revision
            );
        };
        if definition.source != self.source_name
            || definition.name != request.process_name
            || definition.revision_fingerprint != request.revision
        {
            bail!(
                "published Process revision identity does not match start request `{}`",
                request.id
            );
        }
        let typed_input = typed_value(&request.input).with_context(|| {
            format!(
                "decoding input for Process start `{}.{}` revision `{}`",
                self.source_name, request.process_name, request.revision
            )
        })?;
        definition.input.validate(&typed_input).map_err(|error| {
            anyhow!(
                "invalid input for Process start `{}.{}` revision `{}`: {error}",
                self.source_name,
                request.process_name,
                request.revision
            )
        })?;

        let inserted = transaction
            .query_opt(
                "
                INSERT INTO donat.process_instances (
                    source_name,
                    process_name,
                    revision,
                    source_request_id,
                    start_idempotency_key,
                    status,
                    current_state,
                    input_json,
                    state_json,
                    caller_role,
                    caller_session_json,
                    version
                )
                SELECT
                    $1,
                    $2,
                    $3,
                    $4,
                    $5,
                    'running',
                    $6,
                    request.input_json,
                    '{}'::jsonb,
                    request.caller_role,
                    request.caller_session_json,
                    0
                FROM donat.process_start_requests request
                WHERE request.source_name = $1
                  AND request.id = $4
                ON CONFLICT (
                    source_name,
                    process_name,
                    start_idempotency_key
                )
                DO NOTHING
                RETURNING id
                ",
                &[
                    &self.source_name,
                    &request.process_name,
                    &request.revision,
                    &request.id,
                    &request.idempotency_key,
                    &definition.definition.start_at,
                ],
            )
            .await
            .context("creating semantic Process instance")?;

        let outcome = if let Some(row) = inserted {
            let instance_id: Uuid = row.get("id");
            append_started_history(
                &transaction,
                &self.source_name,
                &request,
                instance_id,
                &definition.definition.start_at,
            )
            .await?;
            mark_request(
                &transaction,
                &self.source_name,
                request.id,
                instance_id,
                "consumed",
            )
            .await?;
            StartConsumption::Started {
                request_id: request.id,
                instance_id,
            }
        } else {
            let existing = transaction
                .query_one(
                    "
                    SELECT id, revision, current_state
                    FROM donat.process_instances
                    WHERE source_name = $1
                      AND process_name = $2
                      AND start_idempotency_key = $3
                    ",
                    &[
                        &self.source_name,
                        &request.process_name,
                        &request.idempotency_key,
                    ],
                )
                .await
                .context("loading semantically duplicate Process instance")?;
            let instance_id: Uuid = existing.get("id");
            let instance_revision: String = existing.get("revision");
            let current_state: String = existing.get("current_state");
            append_duplicate_history(
                &transaction,
                &self.source_name,
                &request,
                instance_id,
                &instance_revision,
                &current_state,
            )
            .await?;
            mark_request(
                &transaction,
                &self.source_name,
                request.id,
                instance_id,
                "duplicate",
            )
            .await?;
            StartConsumption::Duplicate {
                request_id: request.id,
                instance_id,
            }
        };

        transaction
            .commit()
            .await
            .context("committing Process start consumption")?;
        Ok(outcome)
    }
}

async fn append_started_history(
    transaction: &tokio_postgres::Transaction<'_>,
    source_name: &str,
    request: &StartRequest,
    instance_id: Uuid,
    initial_state: &str,
) -> anyhow::Result<()> {
    let inserted = transaction
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
            SELECT
                $1,
                $2,
                $3,
                $4,
                'start',
                request.input_json,
                $6,
                'pending'
            FROM donat.process_start_requests request
            WHERE request.source_name = $1
              AND request.id = $5
            ",
            &[
                &source_name,
                &instance_id,
                &request.process_name,
                &request.revision,
                &request.id,
                &request.idempotency_key,
            ],
        )
        .await
        .context("appending Process start event")?;
    if inserted != 1 {
        bail!(
            "claimed Process start request `{source_name}` / `{}` disappeared before event append",
            request.id
        );
    }
    append_start_log(
        transaction,
        source_name,
        request,
        instance_id,
        None,
        initial_state,
        "started",
        &request.revision,
    )
    .await
}

async fn append_duplicate_history(
    transaction: &tokio_postgres::Transaction<'_>,
    source_name: &str,
    request: &StartRequest,
    instance_id: Uuid,
    instance_revision: &str,
    current_state: &str,
) -> anyhow::Result<()> {
    append_start_log(
        transaction,
        source_name,
        request,
        instance_id,
        Some(current_state),
        current_state,
        "duplicate_start",
        instance_revision,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn append_start_log(
    transaction: &tokio_postgres::Transaction<'_>,
    source_name: &str,
    request: &StartRequest,
    instance_id: Uuid,
    from_state: Option<&str>,
    to_state: &str,
    outcome: &str,
    definition_revision: &str,
) -> anyhow::Result<()> {
    let redacted_context = json!({
        "start_request_id": request.id.to_string(),
        "command_invocation_id": request.command_invocation_id.to_string(),
        "effect_position": request.effect_position
    });
    transaction
        .execute(
            "
            INSERT INTO donat.process_transition_logs (
                source_name,
                instance_id,
                from_state,
                to_state,
                outcome,
                definition_revision,
                redacted_context
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ",
            &[
                &source_name,
                &instance_id,
                &from_state,
                &to_state,
                &outcome,
                &definition_revision,
                &redacted_context,
            ],
        )
        .await
        .context("appending Process start transition history")?;
    Ok(())
}

async fn mark_request(
    transaction: &tokio_postgres::Transaction<'_>,
    source_name: &str,
    request_id: Uuid,
    instance_id: Uuid,
    status: &str,
) -> anyhow::Result<()> {
    let updated = transaction
        .execute(
            "
            UPDATE donat.process_start_requests
            SET status = $3,
                instance_id = $4,
                consumed_at = statement_timestamp()
            WHERE source_name = $1
              AND id = $2
              AND status = 'pending'
            ",
            &[&source_name, &request_id, &status, &instance_id],
        )
        .await
        .context("recording Process start request outcome")?;
    if updated != 1 {
        bail!(
            "claimed Process start request `{source_name}` / `{request_id}` changed before outcome commit"
        );
    }
    Ok(())
}

pub(crate) fn typed_value(value: &Json) -> anyhow::Result<TypedValue> {
    Ok(match value {
        Json::Null => TypedValue::Null,
        Json::Bool(value) => TypedValue::Boolean(*value),
        Json::String(value) => TypedValue::String(value.clone()),
        Json::Number(value) => {
            let number = if let Some(value) = value.as_i64() {
                CanonicalNumber::I64(value)
            } else if let Some(value) = value.as_u64() {
                CanonicalNumber::U64(value)
            } else {
                CanonicalNumber::Decimal(canonical_decimal(value)?)
            };
            TypedValue::Number(number)
        }
        Json::Array(values) => TypedValue::List(
            values
                .iter()
                .map(typed_value)
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        Json::Object(values) => TypedValue::Object(
            values
                .iter()
                .map(|(name, value)| Ok((name.clone(), typed_value(value)?)))
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?,
        ),
    })
}

fn canonical_decimal(value: &serde_json::Number) -> anyhow::Result<CanonicalDecimal> {
    let normalized = normalize_json_decimal(&value.to_string())?;
    CanonicalDecimal::try_new(&normalized)
        .map_err(|error| anyhow!("invalid canonical JSON decimal: {error}"))
}

/// Convert a valid JSON number into the value-contract fixed-point spelling.
///
/// Postgres `jsonb` preserves arbitrary-precision numeric values and may
/// preserve decimal scale. `TypedValue` deliberately stores decimals in one
/// minimal fixed-point form, so validation normalizes the spelling while the
/// original exact JSON value remains untouched for durable instance/event
/// payloads.
fn normalize_json_decimal(value: &str) -> anyhow::Result<String> {
    const MAXIMUM_DECIMAL_BYTES: usize = 262_144;

    let (negative, unsigned) = value
        .strip_prefix('-')
        .map_or((false, value), |value| (true, value));
    let (mantissa, exponent) = if let Some(offset) = unsigned.find(['e', 'E']) {
        let exponent = unsigned[offset + 1..]
            .parse::<i64>()
            .map_err(|_| anyhow!("JSON decimal exponent is out of range"))?;
        (&unsigned[..offset], exponent)
    } else {
        (unsigned, 0_i64)
    };
    let (integer, fraction) = mantissa
        .split_once('.')
        .map_or((mantissa, ""), |(integer, fraction)| (integer, fraction));
    let digits = format!("{integer}{fraction}");
    let point = i64::try_from(integer.len())
        .ok()
        .and_then(|length| length.checked_add(exponent))
        .ok_or_else(|| anyhow!("JSON decimal exponent is out of range"))?;
    if point.unsigned_abs() > MAXIMUM_DECIMAL_BYTES as u64 {
        bail!("canonical JSON decimal exceeds the value-contract size limit");
    }

    let mut fixed = String::new();
    if point <= 0 {
        let zeroes = usize::try_from(-point)
            .map_err(|_| anyhow!("JSON decimal exponent is out of range"))?;
        fixed.reserve(2_usize.saturating_add(zeroes).saturating_add(digits.len()));
        fixed.push_str("0.");
        fixed.extend(std::iter::repeat_n('0', zeroes));
        fixed.push_str(&digits);
    } else {
        let point =
            usize::try_from(point).map_err(|_| anyhow!("JSON decimal exponent is out of range"))?;
        if point >= digits.len() {
            let zeroes = point - digits.len();
            fixed.reserve(digits.len().saturating_add(zeroes));
            fixed.push_str(&digits);
            fixed.extend(std::iter::repeat_n('0', zeroes));
        } else {
            fixed.reserve(digits.len().saturating_add(1));
            fixed.push_str(&digits[..point]);
            fixed.push('.');
            fixed.push_str(&digits[point..]);
        }
    }
    if fixed.len() > MAXIMUM_DECIMAL_BYTES {
        bail!("canonical JSON decimal exceeds the value-contract size limit");
    }

    let (whole, fraction) = fixed
        .split_once('.')
        .map_or((fixed.as_str(), ""), |(whole, fraction)| (whole, fraction));
    let whole = whole.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction.trim_end_matches('0');
    let is_zero = whole == "0" && fraction.is_empty();
    let mut normalized = String::with_capacity(
        usize::from(negative && !is_zero)
            .saturating_add(whole.len())
            .saturating_add(usize::from(!fraction.is_empty()))
            .saturating_add(fraction.len()),
    );
    if negative && !is_zero {
        normalized.push('-');
    }
    normalized.push_str(whole);
    if !fraction.is_empty() {
        normalized.push('.');
        normalized.push_str(fraction);
    }
    Ok(normalized)
}
