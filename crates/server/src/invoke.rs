//! `invoke` triggers: a cron tick or a captured row change that runs an
//! action or a command *inside* the engine (spec 010).
//!
//! A webhook trigger ends at a URL and whatever answers there is somebody
//! else's program. An `invoke` trigger ends here: the engine builds the
//! classic session the declaration names — role from YAML, session variables
//! bound from the triggering row — binds the target's arguments from the same
//! row, and runs the target the way a GraphQL request would have. An action
//! goes through [`crate::action::perform_action`]; a command goes through the
//! GraphQL mutation path under that session, so its permissions, guards and
//! tenant scoping are the ones every client gets. There is no second
//! permission world, no minted token and no request back to `/v1/graphql`.
//!
//! Delivery is two-phase, in the journal `migrate` created
//! (`donat.trigger_invocations`). The parent — a `donat.cron_events` row or a
//! `donat.event_log` row — is *expanded* inside its own claim into one work
//! item per row of `foreach` (cron) or one for the event's row, and marked
//! delivered. Each work item is then claimed and run on its own, so the
//! HTTP call and the `then` command of one tenant never hold the lock of the
//! whole occurrence, and a crash after expansion re-expands into nothing.
//! Work items are at-least-once: a handler and a `then` command must be
//! idempotent, as a webhook receiver already had to be.
//!
//! The row is read by an engine-internal query, not through GraphQL: that is
//! what lets a write-only column — a provider token nobody may select — be
//! bound into a call. What is *journaled* is redacted by the same rule the
//! role lives under: a value read from a column the session's role cannot
//! select is stored as `***`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Map as JsonMap, Value as Json, json};
use tokio_postgres::Transaction;
use tokio_postgres::types::ToSql;
use uuid::Uuid;

use donat_metadata::{
    Bind, Columns, Command, CronTrigger, EventTrigger, Foreach, InvokeTarget, QualifiedTable,
    TableEntry, action_visible_to_role,
};
use donat_schema::Session;

use crate::action::{ActionCall, ActionFailure, perform_action};
use crate::state::{Engine, SharedState};

/// Which parent journal a work item hangs off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Cron,
    Event,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Cron => "cron",
            Kind::Event => "event",
        }
    }
}

/// How many work items one poll runs. The remainder waits for the next poll
/// of the same occurrence; nothing is dropped.
pub(crate) fn expand_limit() -> usize {
    std::env::var("DONAT_CRON_INVOKE_EXPAND_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .max(1)
}

/// How many items of one action answer `then` runs a command for. A larger
/// answer is an error on the work item, so a pull that outgrew the cap is
/// visible instead of silently truncated.
fn then_limit() -> usize {
    std::env::var("DONAT_INVOKE_THEN_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100)
        .max(1)
}

fn event_triggers(engine: &Engine) -> impl Iterator<Item = (&EventTrigger, &TableEntry, &str)> {
    engine.metadata.sources.iter().flat_map(|s| {
        s.tables.iter().flat_map(move |t| {
            t.event_triggers
                .iter()
                .map(move |et| (et, t, s.name.as_str()))
        })
    })
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn qualified(table: &QualifiedTable) -> String {
    format!(
        "{}.{}",
        quote_ident(table.schema()),
        quote_ident(table.name())
    )
}

// ------------------------------------------------------------------ expand

/// Insert one work item per row of `foreach` for a cron occurrence. Returns
/// how many the occurrence has (including ones an earlier, crashed
/// expansion already wrote).
pub(crate) async fn expand_cron(
    tx: &Transaction<'_>,
    state: &SharedState,
    engine: &Engine,
    trigger: &CronTrigger,
    invoke: &InvokeTarget,
    parent_id: Uuid,
) -> anyhow::Result<usize> {
    let foreach = invoke
        .foreach
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("cron invoke '{}' has no foreach", trigger.name))?;
    let keys = foreach_row_keys(state, engine, foreach).await?;
    // One statement for the whole occurrence, so a large fan-out is one
    // round-trip under the parent's lock rather than one per tenant.
    tx.execute(
        "INSERT INTO donat.trigger_invocations (kind, parent_id, trigger_name, row_key) \
         SELECT 'cron', $1, $2, e FROM jsonb_array_elements($3::jsonb) AS e \
         ON CONFLICT (kind, parent_id, row_key) DO NOTHING",
        &[&parent_id, &trigger.name, &Json::Array(keys.clone())],
    )
    .await?;
    Ok(keys.len())
}

/// Insert the one work item of a captured row change.
pub(crate) async fn expand_event(
    tx: &Transaction<'_>,
    trigger_name: &str,
    parent_id: Uuid,
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO donat.trigger_invocations (kind, parent_id, trigger_name, row_key) \
         VALUES ('event', $1, $2, $3) ON CONFLICT (kind, parent_id, row_key) DO NOTHING",
        &[
            &parent_id,
            &trigger_name,
            &json!({ "event": parent_id.to_string() }),
        ],
    )
    .await?;
    Ok(())
}

/// The columns that identify one work item: the declared `key`, or the
/// table's primary key plus every unnest alias.
fn key_columns(engine: &Engine, foreach: &Foreach) -> anyhow::Result<Vec<String>> {
    if !foreach.key.is_empty() {
        return Ok(foreach.key.clone());
    }
    let table = engine
        .catalogs
        .get(&foreach.source)
        .and_then(|c| c.table(foreach.table.schema(), foreach.table.name()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "foreach table {}.{} is not in the catalog of source '{}'",
                foreach.table.schema(),
                foreach.table.name(),
                foreach.source
            )
        })?;
    if table.primary_key.is_empty() {
        anyhow::bail!(
            "foreach table {}.{} has no primary key; declare `key`",
            foreach.table.schema(),
            foreach.table.name()
        );
    }
    let mut key = table.primary_key.clone();
    key.extend(foreach.unnest.iter().map(|u| u.as_.clone()));
    Ok(key)
}

/// The closed `where` grammar rendered against alias `t`, with literals as
/// parameters. Validated at load; anything else here is refused again.
fn render_where(where_: &Json, params: &mut Vec<Json>) -> anyhow::Result<String> {
    let Some(map) = where_.as_object() else {
        anyhow::bail!("foreach.where must be an object");
    };
    let mut parts = Vec::new();
    for (key, value) in map {
        if key == "_and" {
            let Some(items) = value.as_array() else {
                anyhow::bail!("foreach.where._and must be a list");
            };
            for item in items {
                parts.push(render_where(item, params)?);
            }
            continue;
        }
        if key.starts_with('_') {
            anyhow::bail!("foreach.where operator '{key}' is outside the closed grammar");
        }
        let Some(ops) = value.as_object() else {
            anyhow::bail!("foreach.where column '{key}' takes an operator object");
        };
        for (op, operand) in ops {
            let column = quote_ident(key);
            match op.as_str() {
                "_is_null" => {
                    let null = operand.as_bool().unwrap_or(true);
                    parts.push(format!(
                        "t.{column} IS {}NULL",
                        if null { "" } else { "NOT " }
                    ));
                }
                "_eq" => {
                    params.push(operand.clone());
                    parts.push(format!("to_jsonb(t.{column}) = ${}::jsonb", params.len()));
                }
                other => {
                    anyhow::bail!("foreach.where operator '{other}' is outside the closed grammar")
                }
            }
        }
    }
    if parts.is_empty() {
        return Ok("TRUE".to_string());
    }
    Ok(format!("({})", parts.join(" AND ")))
}

/// Run the foreach SELECT and return one row key per work item.
///
/// Engine-internal on purpose: no select permission, no tenancy filter. The
/// tick is cross-tenant; isolation is rebuilt per work item from the session
/// the declaration binds.
async fn foreach_row_keys(
    state: &SharedState,
    engine: &Engine,
    foreach: &Foreach,
) -> anyhow::Result<Vec<Json>> {
    let key = key_columns(engine, foreach)?;
    let pool = state
        .source_pool(&foreach.source)
        .await
        .ok_or_else(|| anyhow::anyhow!("source '{}' is not a Postgres source", foreach.source))?;
    let client = pool.get().await?;

    let aliases: Vec<&str> = foreach.unnest.iter().map(|u| u.as_.as_str()).collect();
    // Only the key leaves the database at expansion; the row itself — token
    // included — is read when its work item runs.
    let key_object = key
        .iter()
        .filter(|column| !aliases.contains(&column.as_str()))
        .map(|column| format!("{}, t.{}", quote_literal(column), quote_ident(column)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut select = vec![format!("jsonb_build_object({key_object}) AS row")];
    let mut joins = String::new();
    for (i, unnest) in foreach.unnest.iter().enumerate() {
        let column = quote_ident(&unnest.column);
        select.push(format!("u{i}.e AS {}", quote_ident(&unnest.as_)));
        // `to_jsonb` reads a `text[]` and a `jsonb` array alike; anything
        // that is not an array — null included — is zero work items.
        joins.push_str(&format!(
            " CROSS JOIN LATERAL jsonb_array_elements(CASE WHEN jsonb_typeof(to_jsonb(t.{column})) = 'array' \
             THEN to_jsonb(t.{column}) ELSE '[]'::jsonb END) AS u{i}(e)"
        ));
    }
    let mut params: Vec<Json> = Vec::new();
    let where_ = match &foreach.where_ {
        Some(where_) => render_where(where_, &mut params)?,
        None => "TRUE".to_string(),
    };
    let sql = format!(
        "SELECT {} FROM {} AS t{joins} WHERE {where_}",
        select.join(", "),
        qualified(&foreach.table)
    );
    let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
    let rows = client.query(&sql, &params).await?;

    let mut keys = Vec::with_capacity(rows.len());
    for row in rows {
        let data: Json = row.get("row");
        let mut row_key = JsonMap::new();
        for column in &key {
            let value = if aliases.contains(&column.as_str()) {
                row.get::<_, Json>(column.as_str())
            } else {
                data.get(column).cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "key column '{column}' is not a column of {}.{}",
                        foreach.table.schema(),
                        foreach.table.name()
                    )
                })?
            };
            row_key.insert(column.clone(), value);
        }
        keys.push(Json::Object(row_key));
    }
    Ok(keys)
}

// ----------------------------------------------------------------- deliver

/// The retry policy a work item inherits from its trigger.
struct Retry {
    num_retries: i32,
    interval_seconds: i64,
    /// Cron only: an occurrence this late is dropped on its first attempt.
    tolerance_seconds: Option<i64>,
}

/// A work item's trigger, resolved from the current metadata.
enum Target<'a> {
    Cron {
        trigger: &'a CronTrigger,
        invoke: &'a InvokeTarget,
    },
    Event {
        trigger: &'a EventTrigger,
        table: &'a TableEntry,
        source: &'a str,
        invoke: &'a InvokeTarget,
    },
}

impl Target<'_> {
    fn invoke(&self) -> &InvokeTarget {
        match self {
            Target::Cron { invoke, .. } | Target::Event { invoke, .. } => invoke,
        }
    }

    /// The source a command target is looked up on.
    fn source(&self) -> &str {
        match self {
            Target::Cron { invoke, .. } => invoke
                .foreach
                .as_ref()
                .map(|f| f.source.as_str())
                .unwrap_or("default"),
            Target::Event { source, .. } => source,
        }
    }

    fn retry(&self) -> Retry {
        match self {
            Target::Cron { trigger, .. } => {
                let conf = trigger.retry_conf.clone().unwrap_or_default();
                Retry {
                    num_retries: conf.num_retries as i32,
                    interval_seconds: conf.retry_interval_seconds as i64,
                    tolerance_seconds: Some(conf.tolerance_seconds as i64),
                }
            }
            Target::Event { trigger, .. } => {
                let conf = trigger.retry_conf.clone().unwrap_or_default();
                Retry {
                    num_retries: conf.num_retries as i32,
                    interval_seconds: conf.interval_sec as i64,
                    tolerance_seconds: None,
                }
            }
        }
    }

    /// The table the row comes from, for redaction: the foreach table, or
    /// the event trigger's own.
    fn table<'e>(&self, engine: &'e Engine) -> Option<&'e TableEntry> {
        match self {
            Target::Event { table, .. } => engine
                .metadata
                .sources
                .iter()
                .flat_map(|s| s.tables.iter())
                .find(|t| std::ptr::eq(*t, *table)),
            Target::Cron { invoke, .. } => {
                let foreach = invoke.foreach.as_ref()?;
                engine
                    .metadata
                    .sources
                    .iter()
                    .find(|s| s.name == foreach.source)?
                    .tables
                    .iter()
                    .find(|t| {
                        t.table.schema() == foreach.table.schema()
                            && t.table.name() == foreach.table.name()
                    })
            }
        }
    }
}

/// The bound values a journal must not carry: those read from columns the
/// session's role cannot select. Only text is matched — a number is not a
/// secret anyone can be identified by, and scrubbing `1` out of an answer
/// would destroy it.
fn secret_values(
    table: Option<&TableEntry>,
    invoke: &InvokeTarget,
    row: &JsonMap<String, Json>,
) -> Vec<String> {
    let binds = invoke
        .session
        .vars
        .values()
        .chain(invoke.arguments.values());
    let mut secrets: Vec<String> = binds
        .filter_map(|bind| match bind {
            Bind::Column { column } if !column_selectable(table, invoke, column) => row
                .get(column)
                .and_then(Json::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            _ => None,
        })
        .collect();
    secrets.sort_unstable_by_key(|s| std::cmp::Reverse(s.len()));
    secrets.dedup();
    secrets
}

fn scrub_text(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .fold(text.to_string(), |text, secret| text.replace(secret, "***"))
}

fn scrub(value: Json, secrets: &[String]) -> Json {
    if secrets.is_empty() {
        return value;
    }
    match value {
        Json::String(s) => Json::String(scrub_text(&s, secrets)),
        Json::Array(items) => Json::Array(items.into_iter().map(|v| scrub(v, secrets)).collect()),
        Json::Object(map) => Json::Object(
            map.into_iter()
                .map(|(k, v)| (k, scrub(v, secrets)))
                .collect(),
        ),
        other => other,
    }
}

/// Whether the role may select `column` (an unnest alias inherits its
/// column's visibility).
fn column_selectable(table: Option<&TableEntry>, invoke: &InvokeTarget, column: &str) -> bool {
    let real = invoke
        .foreach
        .as_ref()
        .and_then(|f| f.unnest.iter().find(|u| u.as_ == column))
        .map(|u| u.column.as_str())
        .unwrap_or(column);
    let role = &invoke.session.role;
    table.is_some_and(|table| {
        table.select_permissions.iter().any(|p| {
            &p.role == role
                && match &p.permission.columns {
                    Columns::Star => true,
                    Columns::List(columns) => columns.iter().any(|c| c == real),
                }
        })
    })
}

fn resolve_target<'e>(engine: &'e Engine, kind: Kind, trigger_name: &str) -> Option<Target<'e>> {
    match kind {
        Kind::Cron => {
            let trigger = engine
                .metadata
                .cron_triggers
                .iter()
                .find(|t| t.name == trigger_name)?;
            let invoke = trigger.invoke.as_ref()?;
            Some(Target::Cron { trigger, invoke })
        }
        Kind::Event => {
            let (trigger, table, source) =
                event_triggers(engine).find(|(t, _, _)| t.name == trigger_name)?;
            let invoke = trigger.invoke.as_ref()?;
            Some(Target::Event {
                trigger,
                table,
                source,
                invoke,
            })
        }
    }
}

/// Claim and run due work items of one kind, up to the per-poll cap. Each
/// work item is its own transaction: the claim is held while the target
/// runs, so a crash rolls it back and another instance runs it again.
pub(crate) async fn deliver_due(
    state: &SharedState,
    kind: Kind,
    shutdown: &tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let engine = state.engine_snapshot().await;
    let pool = state
        .default_pool()
        .await
        .ok_or_else(|| anyhow::anyhow!("no default source"))?;
    // A deployment that has not applied the work-item migration — because it
    // declares no `invoke` and never needed to — is left alone.
    let present: Option<String> = pool
        .get()
        .await?
        .query_one("SELECT to_regclass('donat.trigger_invocations')::text", &[])
        .await?
        .get(0);
    if present.is_none() {
        return Ok(());
    }
    for _ in 0..expand_limit() {
        // Between work items is where a drain can take hold: the item being
        // run finishes or is rolled back with its claim, and the next one
        // waits for whoever runs after the restart.
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let mut client = pool.get().await?;
        let tx = client.transaction().await?;
        let Some(row) = tx
            .query_opt(
                "SELECT id, parent_id, trigger_name, row_key, tries \
                 FROM donat.trigger_invocations \
                 WHERE kind = $1 AND status = 'scheduled' \
                   AND (next_retry_at IS NULL OR next_retry_at <= now()) \
                 ORDER BY created_at \
                 FOR UPDATE SKIP LOCKED \
                 LIMIT 1",
                &[&kind.as_str()],
            )
            .await?
        else {
            return Ok(());
        };
        let item = WorkItem {
            id: row.get("id"),
            parent_id: row.get("parent_id"),
            trigger_name: row.get("trigger_name"),
            row_key: row.get("row_key"),
            tries: row.get("tries"),
        };
        run_work_item(state, &engine, kind, &tx, item).await?;
        tx.commit().await?;
    }
    Ok(())
}

struct WorkItem {
    id: Uuid,
    parent_id: Uuid,
    trigger_name: String,
    row_key: Json,
    tries: i32,
}

async fn mark(
    tx: &Transaction<'_>,
    id: Uuid,
    status: &str,
    error: Option<Json>,
) -> anyhow::Result<()> {
    tx.execute(
        "UPDATE donat.trigger_invocations SET status = $2, error = $3, updated_at = now() \
         WHERE id = $1",
        &[&id, &status, &error],
    )
    .await?;
    Ok(())
}

async fn run_work_item(
    state: &SharedState,
    engine: &Engine,
    kind: Kind,
    tx: &Transaction<'_>,
    item: WorkItem,
) -> anyhow::Result<()> {
    let Some(target) = resolve_target(engine, kind, &item.trigger_name) else {
        // The trigger was removed from metadata, or lost its invoke target.
        tracing::warn!(trigger = %item.trigger_name, id = %item.id,
            "work item of a trigger no longer declared; dropped");
        return mark(tx, item.id, "dead", None).await;
    };
    let retry = target.retry();

    // Tolerance: an occurrence run too long after its scheduled time is
    // dropped, on the first attempt only — never mid-retry.
    if let (Some(tolerance), 0) = (retry.tolerance_seconds, item.tries) {
        let scheduled: Option<DateTime<Utc>> = tx
            .query_opt(
                "SELECT scheduled_time FROM donat.cron_events WHERE id = $1",
                &[&item.parent_id],
            )
            .await?
            .map(|r| r.get(0));
        if let Some(scheduled) = scheduled
            && (Utc::now() - scheduled).num_seconds() > tolerance
        {
            tracing::warn!(trigger = %item.trigger_name, id = %item.id,
                "work item past tolerance; dropped");
            return mark(tx, item.id, "dead", None).await;
        }
    }

    // The row, read now rather than at expansion: a token may have rotated,
    // and a row that is gone is a work item that no longer exists.
    let Some(row) = load_row(state, engine, kind, &target, &item).await? else {
        tracing::info!(trigger = %item.trigger_name, id = %item.id,
            "work item's row no longer exists; dropped");
        return mark(tx, item.id, "dead", None).await;
    };

    let invoke = target.invoke();
    let outcome = match build_session(invoke, &row) {
        Err(message) => Err(Failure::permanent(message)),
        Ok(session) => run_target(state, engine, &target, &session, &row).await,
    };
    let table = target.table(engine);
    let input = redacted_input(table, invoke, &row);
    // What the handler answered, or complained, may echo what it was sent —
    // an API that says "invalid token tok-…" is common — so the same
    // values `input` hides are hidden wherever else they appear.
    let secrets = secret_values(table, invoke, &row);
    let outcome = match outcome {
        Ok(result) => Ok(scrub(result, &secrets)),
        Err(failure) => Err(Failure {
            message: scrub_text(&failure.message, &secrets),
            extensions: failure.extensions.map(|e| scrub(e, &secrets)),
            permanent: failure.permanent,
        }),
    };

    match outcome {
        Ok(result) => {
            tx.execute(
                "UPDATE donat.trigger_invocations \
                 SET status = 'delivered', tries = tries + 1, input = $2, result = $3, \
                     error = NULL, updated_at = now() \
                 WHERE id = $1",
                &[&item.id, &input, &result],
            )
            .await?;
        }
        Err(failure) => {
            let new_tries = item.tries + 1;
            let exhausted = new_tries > retry.num_retries || failure.permanent;
            tracing::warn!(trigger = %item.trigger_name, id = %item.id, tries = new_tries,
                error = %failure.message, "invoke work item failed");
            if exhausted {
                tx.execute(
                    "UPDATE donat.trigger_invocations \
                     SET status = 'error', tries = $2, input = $3, error = $4, updated_at = now() \
                     WHERE id = $1",
                    &[&item.id, &new_tries, &input, &failure.detail()],
                )
                .await?;
            } else {
                let next_retry = Utc::now() + chrono::Duration::seconds(retry.interval_seconds);
                tx.execute(
                    "UPDATE donat.trigger_invocations \
                     SET tries = $2, next_retry_at = $3, input = $4, error = $5, updated_at = now() \
                     WHERE id = $1",
                    &[&item.id, &new_tries, &next_retry, &input, &failure.detail()],
                )
                .await?;
            }
        }
    }
    Ok(())
}

/// The triggering row as a JSON object: columns plus unnest aliases.
async fn load_row(
    state: &SharedState,
    engine: &Engine,
    kind: Kind,
    target: &Target<'_>,
    item: &WorkItem,
) -> anyhow::Result<Option<JsonMap<String, Json>>> {
    match kind {
        Kind::Event => {
            let pool = state
                .default_pool()
                .await
                .ok_or_else(|| anyhow::anyhow!("no default source"))?;
            let client = pool.get().await?;
            let Some(row) = client
                .query_opt(
                    "SELECT data_old, data_new FROM donat.event_log WHERE id = $1",
                    &[&item.parent_id],
                )
                .await?
            else {
                return Ok(None);
            };
            let data_new: Option<Json> = row.get("data_new");
            let data_old: Option<Json> = row.get("data_old");
            Ok(data_new.or(data_old).and_then(|v| v.as_object().cloned()))
        }
        Kind::Cron => {
            let foreach = target
                .invoke()
                .foreach
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("cron invoke without foreach"))?;
            let key = key_columns(engine, foreach)?;
            let aliases: Vec<&str> = foreach.unnest.iter().map(|u| u.as_.as_str()).collect();
            let Some(row_key) = item.row_key.as_object() else {
                return Ok(None);
            };
            let mut params: Vec<Json> = Vec::new();
            let mut predicates = Vec::new();
            for column in &key {
                if aliases.contains(&column.as_str()) {
                    continue;
                }
                let Some(value) = row_key.get(column) else {
                    return Ok(None);
                };
                params.push(value.clone());
                predicates.push(format!(
                    "to_jsonb(t.{}) = ${}::jsonb",
                    quote_ident(column),
                    params.len()
                ));
            }
            if predicates.is_empty() {
                return Ok(None);
            }
            let pool = state.source_pool(&foreach.source).await.ok_or_else(|| {
                anyhow::anyhow!("source '{}' is not a Postgres source", foreach.source)
            })?;
            let client = pool.get().await?;
            let sql = format!(
                "SELECT to_jsonb(t) FROM {} AS t WHERE {} LIMIT 1",
                qualified(&foreach.table),
                predicates.join(" AND ")
            );
            let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as _).collect();
            let Some(row) = client.query_opt(&sql, &params).await? else {
                return Ok(None);
            };
            let data: Json = row.get(0);
            let Some(mut data) = data.as_object().cloned() else {
                return Ok(None);
            };
            for alias in aliases {
                if let Some(value) = row_key.get(alias) {
                    data.insert(alias.to_string(), value.clone());
                }
            }
            Ok(Some(data))
        }
    }
}

// ------------------------------------------------------------ the session

/// A session variable is text, whatever the column was.
fn as_var(value: &Json) -> Option<String> {
    match value {
        Json::Null => None,
        Json::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// The classic session this invocation runs as — what a request would have
/// after its JWT, except the variables come from the row.
fn build_session(invoke: &InvokeTarget, row: &JsonMap<String, Json>) -> Result<Session, String> {
    let role = invoke.session.role.clone();
    let mut vars = HashMap::new();
    for (name, bind) in &invoke.session.vars {
        let value = eval_bind(bind, row, &vars, None)?;
        if let Some(text) = as_var(&value) {
            vars.insert(name.to_ascii_lowercase(), text);
        }
    }
    vars.insert("x-donat-role".to_string(), role.clone());
    vars.insert("x-hasura-role".to_string(), role.clone());
    Ok(Session {
        role,
        vars,
        backend_request: false,
    })
}

/// Walk `a.b.c` into a value; a missing step is null.
fn walk_path<'a>(value: &'a Json, path: &str) -> &'a Json {
    let mut current = value;
    let path = path.strip_prefix("$.").unwrap_or(path);
    if path == "$" || path.is_empty() {
        return current;
    }
    for step in path.split('.') {
        current = match current.get(step) {
            Some(next) => next,
            None => return &Json::Null,
        };
    }
    current
}

fn eval_bind(
    bind: &Bind,
    row: &JsonMap<String, Json>,
    vars: &HashMap<String, String>,
    item: Option<&Json>,
) -> Result<Json, String> {
    Ok(match bind {
        Bind::Column { column } => row
            .get(column)
            .cloned()
            .ok_or_else(|| format!("column '{column}' is not in the triggering row"))?,
        Bind::Literal { literal } => literal.clone(),
        Bind::Var { var } => vars
            .get(&var.to_ascii_lowercase())
            .map(|v| Json::String(v.clone()))
            .unwrap_or(Json::Null),
        Bind::Item { item: path } => {
            let item = item.ok_or_else(|| "`item` is bound outside `then`".to_string())?;
            walk_path(item, path).clone()
        }
    })
}

fn bind_arguments(
    binds: &std::collections::BTreeMap<String, Bind>,
    row: &JsonMap<String, Json>,
    session: &Session,
    item: Option<&Json>,
) -> Result<JsonMap<String, Json>, String> {
    let mut out = JsonMap::new();
    for (name, bind) in binds {
        out.insert(name.clone(), eval_bind(bind, row, &session.vars, item)?);
    }
    Ok(out)
}

// -------------------------------------------------------------- the target

struct Failure {
    message: String,
    extensions: Option<Json>,
    /// A failure no retry can change: a declaration problem, a role the
    /// target does not admit.
    permanent: bool,
}

impl Failure {
    fn permanent(message: String) -> Self {
        Failure {
            message,
            extensions: None,
            permanent: true,
        }
    }

    fn transient(message: String) -> Self {
        Failure {
            message,
            extensions: None,
            permanent: false,
        }
    }

    fn detail(&self) -> Json {
        match &self.extensions {
            Some(extensions) => json!({ "message": self.message, "extensions": extensions }),
            None => json!({ "message": self.message }),
        }
    }
}

async fn run_target(
    state: &SharedState,
    engine: &Engine,
    target: &Target<'_>,
    session: &Session,
    row: &JsonMap<String, Json>,
) -> Result<Json, Failure> {
    let invoke = target.invoke();
    let source = target.source();
    if let Some(name) = &invoke.command {
        let command = find_command(engine, name, source)
            .ok_or_else(|| Failure::permanent(format!("command '{name}' does not exist")))?;
        let arguments =
            bind_arguments(&invoke.arguments, row, session, None).map_err(Failure::permanent)?;
        return run_command(state, session, command, arguments).await;
    }
    let name = invoke
        .action
        .as_ref()
        .ok_or_else(|| Failure::permanent("invoke names no target".to_string()))?;
    let action = engine
        .metadata
        .actions
        .iter()
        .find(|a| &a.name == name)
        .ok_or_else(|| Failure::permanent(format!("action '{name}' does not exist")))?;
    if !action_visible_to_role(action, &session.role) {
        return Err(Failure::permanent(format!(
            "role '{}' is not permitted to call action '{name}'",
            session.role
        )));
    }
    let input =
        bind_arguments(&invoke.arguments, row, session, None).map_err(Failure::permanent)?;
    let answer = perform_action(ActionCall {
        state,
        session,
        action,
        input,
        headers: None,
    })
    .await
    .map_err(|failure| match failure {
        ActionFailure::NoHandler(message) | ActionFailure::Transform(message) => {
            Failure::permanent(message)
        }
        ActionFailure::Transport(message) => Failure::transient(message),
        ActionFailure::Handler {
            message,
            extensions,
        } => Failure {
            message,
            extensions: Some(extensions),
            permanent: false,
        },
    })?;

    let Some(then) = &invoke.then else {
        return Ok(answer);
    };
    let command = find_command(engine, &then.command, source)
        .ok_or_else(|| Failure::permanent(format!("command '{}' does not exist", then.command)))?;
    let selected = walk_path(&answer, &then.foreach);
    let items: Vec<&Json> = match selected {
        Json::Null => Vec::new(),
        Json::Array(items) => items.iter().collect(),
        object => vec![object],
    };
    let limit = then_limit();
    if items.len() > limit {
        return Err(Failure::permanent(format!(
            "the action answered {} items and DONAT_INVOKE_THEN_LIMIT is {limit}; \
             nothing was ingested",
            items.len()
        )));
    }
    let mut failures = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let arguments = match bind_arguments(&then.arguments, row, session, Some(item)) {
            Ok(arguments) => arguments,
            Err(message) => {
                failures.push(json!({ "item": index, "message": message }));
                continue;
            }
        };
        if let Err(failure) = run_command(state, session, command, arguments).await {
            failures.push(json!({ "item": index, "message": failure.message }));
        }
    }
    if !failures.is_empty() {
        return Err(Failure {
            message: format!("then: {} of {} items failed", failures.len(), items.len()),
            extensions: Some(json!({ "failed": failures })),
            permanent: false,
        });
    }
    Ok(answer)
}

fn find_command<'e>(engine: &'e Engine, name: &str, source: &str) -> Option<&'e Command> {
    let commands = &engine.metadata.commands;
    commands
        .iter()
        .find(|c| c.name == name && c.source == source)
        .or_else(|| commands.iter().find(|c| c.name == name))
}

/// Run a command as this session through the GraphQL mutation path — the
/// `X-Donat-Role` path every client takes, with the command's permissions,
/// guards and tenant scoping applied by the planner.
async fn run_command(
    state: &SharedState,
    session: &Session,
    command: &Command,
    arguments: JsonMap<String, Json>,
) -> Result<Json, Failure> {
    let mut declarations = Vec::new();
    let mut call = Vec::new();
    let mut variables = JsonMap::new();
    for (name, value) in arguments {
        let type_ = command
            .arguments
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.type_.as_str())
            .ok_or_else(|| {
                Failure::permanent(format!(
                    "'{name}' is not an argument of command '{}'",
                    command.name
                ))
            })?;
        declarations.push(format!("${name}: {type_}"));
        call.push(format!("{name}: ${name}"));
        variables.insert(name, value);
    }
    let query = if declarations.is_empty() {
        format!("mutation {{ {}  {{ __typename }} }}", command.name)
    } else {
        format!(
            "mutation({}) {{ {}({}) {{ __typename }} }}",
            declarations.join(", "),
            command.name,
            call.join(", ")
        )
    };
    let (_, body) = crate::gql::execute(
        state,
        session,
        &json!({ "query": query, "variables": variables }),
    )
    .await;
    if let Some(errors) = body.get("errors") {
        let first = errors.get(0);
        let message = first
            .and_then(|e| e.get("message"))
            .and_then(Json::as_str)
            .unwrap_or("command failed")
            .to_string();
        // A declaration or permission problem is the same on every retry.
        let code = first
            .and_then(|e| e.pointer("/extensions/code"))
            .and_then(Json::as_str)
            .unwrap_or("");
        let permanent = matches!(code, "validation-failed" | "access-denied" | "not-found");
        return Err(Failure {
            message,
            extensions: Some(errors.clone()),
            permanent,
        });
    }
    Ok(body
        .get("data")
        .and_then(|d| d.get(&command.name))
        .cloned()
        .unwrap_or(Json::Null))
}

// --------------------------------------------------------------- redaction

/// The bound arguments as they may be journaled: a value read from a column
/// the session's role cannot select is `***`. Unnest aliases inherit their
/// column's visibility.
fn redacted_input(
    table: Option<&TableEntry>,
    invoke: &InvokeTarget,
    row: &JsonMap<String, Json>,
) -> Json {
    let role = &invoke.session.role;
    let alias_of = |name: &str| -> String {
        invoke
            .foreach
            .as_ref()
            .and_then(|f| f.unnest.iter().find(|u| u.as_ == name))
            .map(|u| u.column.clone())
            .unwrap_or_else(|| name.to_string())
    };
    let selectable = |column: &str| -> bool {
        table.is_some_and(|table| {
            table.select_permissions.iter().any(|p| {
                &p.role == role
                    && match &p.permission.columns {
                        Columns::Star => true,
                        Columns::List(columns) => columns.iter().any(|c| c == column),
                    }
            })
        })
    };
    let mut out = JsonMap::new();
    for (name, bind) in &invoke.arguments {
        let value = match bind {
            Bind::Column { column } if !selectable(&alias_of(column)) => {
                Json::String("***".to_string())
            }
            bind => eval_bind(bind, row, &HashMap::new(), None).unwrap_or(Json::Null),
        };
        out.insert(name.clone(), value);
    }
    Json::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn where_renders_the_closed_grammar_with_parameters() {
        let mut params = Vec::new();
        let sql = render_where(
            &json!({ "_and": [ { "token": { "_is_null": false } }, { "kind": { "_eq": "x" } } ] }),
            &mut params,
        )
        .unwrap();
        assert_eq!(
            sql,
            "((t.\"token\" IS NOT NULL) AND (to_jsonb(t.\"kind\") = $1::jsonb))"
        );
        assert_eq!(params, vec![json!("x")]);
    }

    #[test]
    fn where_refuses_what_the_loader_would_have() {
        let mut params = Vec::new();
        let error = render_where(&json!({ "a": { "_like": "x" } }), &mut params).unwrap_err();
        assert!(error.to_string().contains("_like"));
    }

    #[test]
    fn paths_walk_and_miss_quietly() {
        let value = json!({ "data": { "issues": [1, 2] } });
        assert_eq!(walk_path(&value, "$"), &value);
        assert_eq!(walk_path(&value, "data.issues"), &json!([1, 2]));
        assert_eq!(walk_path(&value, "$.data.issues"), &json!([1, 2]));
        assert_eq!(walk_path(&value, "data.nope.x"), &Json::Null);
    }

    #[test]
    fn a_session_lowercases_and_stringifies_its_variables() {
        let invoke: InvokeTarget = serde_json::from_value(json!({
            "action": "a",
            "session": { "role": "user", "vars": {
                "X-Donat-User-Id": { "column": "id" },
                "x-donat-org": { "literal": "acme" },
                "x-donat-none": { "column": "missing_value" }
            } }
        }))
        .unwrap();
        let row = serde_json::from_value(json!({ "id": 7, "missing_value": null })).unwrap();
        let session = build_session(&invoke, &row).unwrap();
        assert_eq!(session.role, "user");
        assert_eq!(session.var("x-donat-user-id"), Some("7"));
        assert_eq!(session.var("x-donat-org"), Some("acme"));
        assert_eq!(session.var("x-donat-none"), None);
        assert_eq!(session.var("x-donat-role"), Some("user"));
    }

    #[test]
    fn a_secret_is_scrubbed_from_whatever_echoes_it() {
        let secrets = vec!["tok-abc".to_string()];
        let echoed = json!({ "message": "invalid token tok-abc", "items": ["tok-abc", 1] });
        assert_eq!(
            scrub(echoed, &secrets),
            json!({ "message": "invalid token ***", "items": ["***", 1] })
        );
        assert_eq!(scrub(json!({ "n": 1 }), &[]), json!({ "n": 1 }));
    }

    #[test]
    fn a_column_the_role_cannot_select_is_redacted() {
        let table: TableEntry = serde_json::from_value(json!({
            "table": { "schema": "public", "name": "workspace" },
            "select_permissions": [{ "role": "user", "permission": { "columns": ["id"], "filter": {} } }]
        }))
        .unwrap();
        let invoke: InvokeTarget = serde_json::from_value(json!({
            "action": "a",
            "session": { "role": "user" },
            "foreach": {
                "table": { "schema": "public", "name": "workspace" },
                "unnest": [{ "column": "teams", "as": "team" }]
            },
            "arguments": {
                "token": { "column": "secret" },
                "id": { "column": "id" },
                "team": { "column": "team" },
                "fixed": { "literal": "x" }
            }
        }))
        .unwrap();
        let row = serde_json::from_value(json!({ "id": 1, "secret": "s", "team": "T1" })).unwrap();
        assert_eq!(
            redacted_input(Some(&table), &invoke, &row),
            json!({ "token": "***", "id": 1, "team": "***", "fixed": "x" })
        );
    }
}
