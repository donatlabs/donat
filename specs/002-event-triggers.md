# Spec 002 — Table event triggers

Status: in progress. Table event triggers (webhooks on row insert/update/
delete), TDD-ported from Hasura `tests-py/test_events.py`. Reuses the cron
delivery machinery (the `donat` journal + `FOR UPDATE SKIP LOCKED` poller +
retries).

Compatible with the no-admin-role posture: triggers are declared in YAML under
the table (`event_triggers`); the per-table Postgres triggers (DDL) are
created by the deploy-time `migrate --metadata-dir` step, never by the serving
binary. Donat's runtime `create_event_trigger` metadata API is intentionally
absent.

## 1. Metadata (`crates/metadata`)

`event_triggers: Vec<EventTrigger>` on `TableEntry` (Donat directory-format
`EventTriggerConf`):

```yaml
event_triggers:
  - name: t1_all
    definition:
      enable_manual: false
      insert: { columns: '*' }
      update: { columns: [c2] }   # selected columns → fires only on those
      delete: { columns: '*' }
    retry_conf: { num_retries: 0, interval_sec: 10, timeout_sec: 60 }
    webhook: '{{EVENT_WEBHOOK_HANDLER}}'   # or webhook_from_env: ENV
    headers: [{ name: X-Api-Key, value_from_env: API_KEY }]
```

`EventRetryConf` is Donat `RetryConf` — note the field names differ from
cron's `RetryConfST` (`interval_sec`/`timeout_sec`, not
`retry_interval_seconds`/`timeout_seconds`).

## 2. Catalog DDL (`migrations/V2__donat_event_log.sql`)

- `donat.event_log` (id, trigger_name, schema_name, table_name, op,
  data_old, data_new, session_variables, status, tries, next_retry_at,
  created_at).
- `donat.event_invocation_logs` (per attempt).
- `donat.notify_event()` — one generic PL/pgSQL function shared by every
  per-table trigger. Captures `to_jsonb(OLD/NEW)` and inserts an `event_log`
  row **in the mutation's transaction** (so nothing is lost on crash, and raw
  SQL writes fire events too). Reads the `donat.user` GUC for session
  variables when set (NULL otherwise — see Limitations).

## 3. Reconcile (deploy-time DDL, `crates/server/src/events.rs::reconcile`)

Run from `migrate --metadata-dir`: for each `event_triggers` entry, create the
per-table `AFTER INSERT/UPDATE/DELETE` triggers calling
`donat.notify_event('<name>')`; selected-column updates become `AFTER
UPDATE OF <cols>`. Engine-managed triggers (name prefix
`donat_notify_`) that are no longer declared are dropped. Identifiers and
the trigger-name literal are quoted/escaped.

## 4. Delivery (`crates/server/src/events.rs`, spawned from `main.rs`)

Background loop (poll `DONAT_EVENTS_POLL_SECONDS`, default 10). Claim due
`event_log` rows with `FOR UPDATE SKIP LOCKED`, POST the Donat event
envelope, retry per `retry_conf`, write invocation logs. Same multi-instance
properties as cron (at-least-once; handlers must be idempotent).

Envelope (Donat shape):

```json
{ "id": "<uuid>", "created_at": "<ts>",
  "table": { "schema": "...", "name": "..." },
  "trigger": { "name": "..." },
  "event": { "op": "INSERT|UPDATE|DELETE", "data": { "old": ..., "new": ... },
             "session_variables": ... },
  "delivery_info": { "current_retry": <n>, "max_retries": <n> } }
```

INSERT → `old:null`; DELETE → `new:null`; UPDATE → both.

## 5. Conformance (`crates/conformance`)

Reuses the recording webhook stub (`/fail` always-500 for retry exhaustion).
Harness `Suite::with_event_webhook()` (exposes `EVENT_WEBHOOK_HANDLER`,
implies migrations+reconcile, 1s poll) and `Running::add_event_trigger()`.
`tests/event_triggers.rs` ports `test_events.py`:

- `TestCreateEventQuery::test_basic` — insert/update/delete envelopes.
- `TestEventRetryConf` — failing webhook retried `num_retries` times, then
  marked `error`, with one invocation log per attempt.

## Limitations / follow-ups

- **Session variables** not yet captured (engine does not set the
  `donat.user` GUC inside the mutation transaction); `session_variables` is
  null. Donat asserts the role here; tracked as a follow-up (needs wrapping
  mutations with `SET LOCAL` when a trigger exists on the target table).
- **Column-filtered payloads** (`columns: [..]` limiting the delivered row,
  and `payload`) not yet applied — the full row is delivered. Update *firing*
  on selected columns is implemented (the `AFTER UPDATE OF` clause).
- **Manual events**, **async flood/ordering**, **transforms** not ported yet.
- Multi-source: reconcile targets the single `migrate` database URL.
