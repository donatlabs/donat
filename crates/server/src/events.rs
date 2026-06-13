//! Table event triggers: webhooks fired when rows change (Hasura event
//! triggers), configured deploy-time in YAML under each table.
//!
//! Two halves:
//!
//! - [`reconcile`] (deploy-time): creates/drops the per-table Postgres
//!   triggers that capture row changes into `dist_api.event_log`. This is
//!   DDL, so it runs from the `migrate` subcommand path — never from the
//!   serving binary.
//! - [`spawn`] (runtime): a background loop that delivers captured events to
//!   their webhook with the Hasura event envelope, retries per `retry_conf`,
//!   and records invocation logs. Reuses the same claim pattern as cron
//!   (`FOR UPDATE SKIP LOCKED`), so it is multi-instance safe and
//!   at-least-once.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::{Value as Json, json};
use tokio_postgres::NoTls;

use dist_metadata::{Columns, EventTrigger, Metadata};

use crate::cron::resolve_headers;
use crate::remote::resolve_url_template;
use crate::state::SharedState;

// ----------------------------------------------------------- deploy-time DDL

/// Create the per-table Postgres triggers for every `event_triggers` entry in
/// the metadata, and drop any engine-managed triggers no longer declared.
/// Run from `migrate` (deploy-time); the serving binary never runs DDL.
pub async fn reconcile(database_url: &str, metadata: &Metadata) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(database_url, NoTls).await?;
    let conn = tokio::spawn(async move { conn.await });

    // Desired triggers: (pg_trigger_name, schema, table) -> CREATE statement.
    let mut desired: HashMap<String, (String, String, String)> = HashMap::new();
    let mut creates: Vec<String> = vec![];
    for source in &metadata.sources {
        for table in &source.tables {
            let schema = table.table.schema().to_string();
            let name = table.table.name().to_string();
            for et in &table.event_triggers {
                for stmt in create_statements(et, &schema, &name) {
                    desired.insert(stmt.pg_name.clone(), (stmt.pg_name.clone(), schema.clone(), name.clone()));
                    creates.push(stmt.sql);
                }
            }
        }
    }

    // Drop managed triggers (our naming prefix) that are no longer desired.
    let rows = client
        .query(
            "SELECT t.tgname, n.nspname, c.relname \
             FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE NOT t.tgisinternal AND t.tgname LIKE 'dist_api_notify_%'",
            &[],
        )
        .await?;
    for row in rows {
        let tgname: String = row.get(0);
        let schema: String = row.get(1);
        let table: String = row.get(2);
        if !desired.contains_key(&tgname) {
            client
                .batch_execute(&format!(
                    "DROP TRIGGER IF EXISTS {} ON {}.{}",
                    quote_ident(&tgname),
                    quote_ident(&schema),
                    quote_ident(&table)
                ))
                .await?;
        }
    }

    // Create (or replace) the desired triggers.
    for sql in creates {
        client.batch_execute(&sql).await?;
    }

    conn.abort();
    Ok(())
}

struct CreateStmt {
    pg_name: String,
    sql: String,
}

/// The CREATE TRIGGER statements for one event trigger (one per enabled op).
fn create_statements(et: &EventTrigger, schema: &str, table: &str) -> Vec<CreateStmt> {
    let mut out = vec![];
    let qtable = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let arg = quote_literal(&et.name);

    let mut push = |op: &str, event_clause: String| {
        let pg_name = trigger_pg_name(&et.name, op);
        let sql = format!(
            "CREATE OR REPLACE TRIGGER {name} {event_clause} ON {qtable} \
             FOR EACH ROW EXECUTE FUNCTION dist_api.notify_event({arg})",
            name = quote_ident(&pg_name),
        );
        out.push(CreateStmt { pg_name, sql });
    };

    if et.definition.insert.is_some() {
        push("insert", "AFTER INSERT".to_string());
    }
    if let Some(spec) = &et.definition.update {
        // Selected columns: fire only when one of them changes.
        let clause = match &spec.columns {
            Columns::List(cols) if !cols.is_empty() => {
                let list = cols
                    .iter()
                    .map(|c| quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("AFTER UPDATE OF {list}")
            }
            _ => "AFTER UPDATE".to_string(),
        };
        push("update", clause);
    }
    if et.definition.delete.is_some() {
        push("delete", "AFTER DELETE".to_string());
    }
    out
}

/// Postgres trigger name for `(trigger, op)`, kept within the 63-byte
/// identifier limit.
fn trigger_pg_name(trigger: &str, op: &str) -> String {
    let base = format!("dist_api_notify_{trigger}_{op}");
    if base.len() <= 63 {
        base
    } else {
        // Truncate the trigger portion, keep the op suffix unambiguous.
        let keep = 63 - format!("dist_api_notify__{op}").len();
        format!("dist_api_notify_{}_{op}", &trigger[..keep.min(trigger.len())])
    }
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn quote_literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

// --------------------------------------------------------------- delivery

/// Start the event-trigger delivery loop. No-op (the task exits) when no table
/// declares an event trigger.
pub fn spawn(state: SharedState) {
    tokio::spawn(async move { run(state).await });
}

async fn run(state: SharedState) {
    let has_triggers = {
        let engine = state.engine.read().await;
        engine
            .metadata
            .sources
            .iter()
            .any(|s| s.tables.iter().any(|t| !t.event_triggers.is_empty()))
    };
    if !has_triggers {
        return;
    }
    let interval = std::env::var("DIST_API_EVENTS_POLL_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10)
        .max(1);
    let interval = Duration::from_secs(interval);
    tracing::info!(poll_seconds = interval.as_secs(), "event delivery loop started");
    loop {
        if let Err(e) = tick(&state).await {
            tracing::warn!(error = %e, "event tick failed");
        }
        tokio::time::sleep(interval).await;
    }
}

async fn tick(state: &SharedState) -> anyhow::Result<()> {
    // Index event triggers by name (across all tables/sources).
    let triggers: HashMap<String, EventTrigger> = {
        let engine = state.engine.read().await;
        engine
            .metadata
            .sources
            .iter()
            .flat_map(|s| s.tables.iter())
            .flat_map(|t| t.event_triggers.iter())
            .map(|et| (et.name.clone(), et.clone()))
            .collect()
    };
    if triggers.is_empty() {
        return Ok(());
    }
    let pool = state
        .default_pool()
        .await
        .ok_or_else(|| anyhow::anyhow!("no default source"))?;
    let mut client = pool.get().await?;

    let tx = client.transaction().await?;
    let rows = tx
        .query(
            "SELECT id, trigger_name, schema_name, table_name, op, data_old, data_new, \
                    session_variables, tries, created_at \
             FROM dist_api.event_log \
             WHERE status = 'scheduled' \
               AND (next_retry_at IS NULL OR next_retry_at <= now()) \
             ORDER BY created_at \
             FOR UPDATE SKIP LOCKED \
             LIMIT 50",
            &[],
        )
        .await?;

    for row in rows {
        let id: uuid::Uuid = row.get("id");
        let trigger_name: String = row.get("trigger_name");
        let schema_name: String = row.get("schema_name");
        let table_name: String = row.get("table_name");
        let op: String = row.get("op");
        let data_old: Option<Json> = row.get("data_old");
        let data_new: Option<Json> = row.get("data_new");
        let session_variables: Option<Json> = row.get("session_variables");
        let tries: i32 = row.get("tries");
        let created_at: DateTime<Utc> = row.get("created_at");

        let Some(trigger) = triggers.get(&trigger_name) else {
            // Trigger removed from metadata: drop the orphaned event.
            tx.execute(
                "UPDATE dist_api.event_log SET status = 'error' WHERE id = $1",
                &[&id],
            )
            .await?;
            continue;
        };
        let retry = trigger.retry_conf.clone().unwrap_or_default();

        let envelope = json!({
            "id": id.to_string(),
            "created_at": created_at.to_rfc3339_opts(chrono::SecondsFormat::Micros, false),
            "table": { "schema": schema_name, "name": table_name },
            "trigger": { "name": trigger_name },
            "event": {
                "op": op,
                "data": { "old": data_old, "new": data_new },
                "session_variables": session_variables,
            },
            "delivery_info": { "current_retry": tries, "max_retries": retry.num_retries },
        });

        let (http_status, response_body) = deliver(state, trigger, &envelope).await;
        let success = http_status.map(|s| (200..300).contains(&s)).unwrap_or(false);

        tx.execute(
            "INSERT INTO dist_api.event_invocation_logs (event_id, status, request, response) \
             VALUES ($1, $2, $3, $4)",
            &[&id, &http_status, &envelope, &response_body],
        )
        .await?;

        if success {
            tx.execute(
                "UPDATE dist_api.event_log SET status = 'delivered', tries = tries + 1 \
                 WHERE id = $1",
                &[&id],
            )
            .await?;
        } else {
            let new_tries = tries + 1;
            if new_tries > retry.num_retries as i32 {
                tx.execute(
                    "UPDATE dist_api.event_log SET status = 'error', tries = $2 WHERE id = $1",
                    &[&id, &new_tries],
                )
                .await?;
            } else {
                let next_retry =
                    Utc::now() + chrono::Duration::seconds(retry.interval_sec as i64);
                tx.execute(
                    "UPDATE dist_api.event_log SET tries = $2, next_retry_at = $3 WHERE id = $1",
                    &[&id, &new_tries, &next_retry],
                )
                .await?;
            }
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Resolve the webhook URL (literal/template or `webhook_from_env`).
fn webhook_url(trigger: &EventTrigger) -> String {
    if let Some(env) = &trigger.webhook_from_env {
        std::env::var(env).unwrap_or_default()
    } else {
        resolve_url_template(trigger.webhook.as_deref().unwrap_or_default())
    }
}

async fn deliver(state: &SharedState, trigger: &EventTrigger, envelope: &Json) -> (Option<i32>, Json) {
    let url = webhook_url(trigger);
    let timeout = trigger
        .retry_conf
        .as_ref()
        .map(|r| r.timeout_sec)
        .unwrap_or(60);
    let mut req = state
        .http
        .post(&url)
        .timeout(Duration::from_secs(timeout))
        .json(envelope);
    for (name, value) in resolve_headers(&trigger.headers) {
        req = req.header(name, value);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16() as i32;
            let body = resp.json::<Json>().await.unwrap_or(Json::Null);
            (Some(status), body)
        }
        Err(e) => (None, json!({ "error": e.to_string() })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dist_metadata::{EventTriggerDefinition, OperationSpec};

    fn trig(name: &str, def: EventTriggerDefinition) -> EventTrigger {
        EventTrigger {
            name: name.into(),
            definition: def,
            webhook: Some("http://h".into()),
            webhook_from_env: None,
            retry_conf: None,
            headers: vec![],
            comment: None,
        }
    }

    #[test]
    fn create_statements_cover_enabled_ops() {
        let def = EventTriggerDefinition {
            enable_manual: false,
            insert: Some(OperationSpec { columns: Columns::Star, payload: None }),
            update: Some(OperationSpec {
                columns: Columns::List(vec!["c2".into()]),
                payload: None,
            }),
            delete: Some(OperationSpec { columns: Columns::Star, payload: None }),
        };
        let stmts = create_statements(&trig("t1_all", def), "hge_tests", "test_t1");
        assert_eq!(stmts.len(), 3);
        let joined = stmts.iter().map(|s| s.sql.as_str()).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("AFTER INSERT ON \"hge_tests\".\"test_t1\""));
        // Selected-column update fires only on that column.
        assert!(joined.contains("AFTER UPDATE OF \"c2\" ON"));
        assert!(joined.contains("AFTER DELETE ON"));
        assert!(joined.contains("dist_api.notify_event('t1_all')"));
    }

    #[test]
    fn star_update_has_no_column_list() {
        let def = EventTriggerDefinition {
            enable_manual: false,
            insert: None,
            update: Some(OperationSpec { columns: Columns::Star, payload: None }),
            delete: None,
        };
        let stmts = create_statements(&trig("t", def), "public", "x");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].sql.contains("AFTER UPDATE ON"));
        assert!(!stmts[0].sql.contains("AFTER UPDATE OF"));
    }

    #[test]
    fn identifiers_and_literals_are_escaped() {
        assert_eq!(quote_ident(r#"a"b"#), r#""a""b""#);
        assert_eq!(quote_literal("a'b"), "'a''b'");
    }
}
