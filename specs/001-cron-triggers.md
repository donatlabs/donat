# Spec 001 — Cron (scheduled) triggers

Status: in progress. Scope agreed with the user 2026-06-13:

- **Cron triggers only** (recurring, defined in YAML). Table event triggers
  and one-off scheduled events are out of scope for this milestone.
- **Lifecycle: reconcile at boot.** The serving binary materializes upcoming
  cron events from the YAML schedule; DDL (catalog tables) is applied by
  `migrate` (the serving binary never runs DDL).

Compatible with the project's no-admin-role posture: cron triggers are
deploy-time configuration (YAML + `migrate`), no runtime mutation surface.

## 1. Metadata (`crates/metadata`)

Donat exports cron triggers to `cron_triggers.yaml` at the metadata root, a
top-level list. Add `cron_triggers: Vec<CronTrigger>` to `Metadata` and load
it with the existing `load_section(dir, "cron_triggers.yaml")` helper.

```yaml
- name: send_reminders
  webhook: https://example.com/cron      # {{ENV}} templating allowed
  schedule: "* * * * *"                   # 5-field standard cron, UTC
  payload: { kind: reminder }             # arbitrary JSON, default {}
  include_in_metadata: true               # default true
  retry_conf:
    num_retries: 3
    retry_interval_seconds: 10
    timeout_seconds: 60
    tolerance_seconds: 21600
  headers:
    - { name: X-Api-Key, value_from_env: API_KEY }
  comment: optional
```

Types:

- `CronTrigger { name, webhook, schedule, payload (default Null→{}),
  include_in_metadata (default true), retry_conf: Option<CronRetryConf>,
  headers: Vec<ActionHeader> (reused), comment: Option<String> }`
- `CronRetryConf { num_retries=0, retry_interval_seconds=10,
  timeout_seconds=60, tolerance_seconds=21600 }` — Donat `RetryConfST`
  field names and defaults (verified against the engine source).

## 2. Catalog DDL (`migrations/V1__donat_cron.sql`)

Schema `donat` (engine-internal catalog), plus:

- `cron_events(id uuid pk, trigger_name text, scheduled_time timestamptz,
  status text default 'scheduled', tries int default 0,
  next_retry_at timestamptz, created_at timestamptz default now(),
  unique(trigger_name, scheduled_time))`. Status: `scheduled | delivered |
  error | dead`.
- `cron_event_invocation_logs(id uuid pk, event_id uuid fk, status int,
  request jsonb, response jsonb, created_at timestamptz default now())`.

`unique(trigger_name, scheduled_time)` makes materialization idempotent and
guarantees a scheduled occurrence is enqueued at most once across restarts
and across multiple engine instances. `gen_random_uuid()` is built-in on
PG13+.

## 3. Schedule computation (`crates/server/src/cron.rs`)

Add `chrono` + `croner` (5-field cron, UTC) to deps. `next_after(schedule,
after: DateTime<Utc>) -> Option<DateTime<Utc>>`. Unit-tested independently
of the database.

## 4. Delivery (`crates/server/src/cron.rs`, spawned from `main.rs`)

A tokio task started after `sync_sources` (only when metadata has cron
triggers). Poll interval from `DONAT_CRON_POLL_SECONDS` (default 10; the
conformance test sets it low for determinism).

Each tick, against the default pool:

1. **Materialize**: for each cron trigger, `INSERT ... ON CONFLICT DO
   NOTHING` the next occurrence after the latest known `scheduled_time`
   (seed `now`-relative on first run), keeping one future row per trigger.
2. **Claim due**: `SELECT ... WHERE status='scheduled' AND scheduled_time
   <= now() AND (next_retry_at IS NULL OR next_retry_at <= now()) ORDER BY
   scheduled_time FOR UPDATE SKIP LOCKED LIMIT N`.
3. **Tolerance**: if `now() - scheduled_time > tolerance_seconds` on the
   first try, mark `dead` (dropped) and continue.
4. **Deliver**: POST the envelope (below) to the resolved webhook with the
   resolved headers and `timeout_seconds`. Record an invocation log row.
5. **Outcome**: 2xx → `delivered`. Else `tries += 1`; if `tries >
   num_retries` → `error`, else `next_retry_at = now() +
   retry_interval_seconds` (stays `scheduled`).

Webhook body (Donat `ScheduledEventWebhookPayload`, snake_case,
`omitNothingFields`; `created_at` omitted for cron):

```json
{ "id": "<uuid>", "name": "<trigger>", "scheduled_time": "<rfc3339>",
  "payload": <configured payload> }
```

Reuse `remote::resolve_url_template` for `{{ENV}}` in the webhook URL and
header resolution (value / value_from_env) mirroring actions.

## 5. Conformance (`crates/conformance`)

No Donat tests-py fixtures exist for delivery; native tests are the source
of truth (same pattern as `remote_schemas.rs` / actions).

- A native cron webhook stub receiver (records received body + headers),
  modeled on `src/action_webhook.rs`.
- Harness `Builder::with_migrations()` runs `donat migrate
  --migrations-dir <workspace>/migrations` against the suite DB before the
  engine spawns, so `donat.*` exists.
- `tests/cron_triggers.rs`:
  - fires a past-due event → stub receives the exact envelope + header;
    `cron_events` row → `delivered`; an invocation log row exists.
  - stub returns 500 then 200 → delivered after `num_retries`;
    invocation-log count reflects the retries.
  - tolerance: a far-past event with `tolerance_seconds` small → `dead`,
    no delivery.

## 6. Docs

ADR `knowledgebase/embedded-sdk/decisions/006-cron-triggers-yaml-only.md`
(cron via YAML; one-off scheduled events need a runtime create surface we
don't have — deferred). Reclassify the README roadmap line; update PLAN.md.
