-- Engine-internal catalog for cron (scheduled) triggers.
--
-- This is the only place the schema for cron delivery is created; the serving
-- binary never runs DDL. The server materializes cron events from the YAML
-- schedule into `cron_events` and a background poller delivers them to the
-- configured webhook (see crates/server/src/cron.rs).

create schema if not exists dist_api;

-- One row per scheduled occurrence of a cron trigger. The unique
-- (trigger_name, scheduled_time) constraint makes materialization idempotent:
-- an occurrence is enqueued at most once, even across engine restarts and
-- across multiple engine instances racing on the same database.
create table if not exists dist_api.cron_events (
    id            uuid primary key default gen_random_uuid(),
    trigger_name  text        not null,
    scheduled_time timestamptz not null,
    -- scheduled | delivered | error | dead
    status        text        not null default 'scheduled',
    tries         int         not null default 0,
    next_retry_at timestamptz,
    created_at    timestamptz not null default now(),
    unique (trigger_name, scheduled_time)
);

-- Index the poller's claim query: due, deliverable events in time order.
create index if not exists cron_events_due_idx
    on dist_api.cron_events (status, scheduled_time);

-- One row per delivery attempt (audit trail; mirrors Hasura's invocation logs).
create table if not exists dist_api.cron_event_invocation_logs (
    id         uuid primary key default gen_random_uuid(),
    event_id   uuid not null references dist_api.cron_events (id) on delete cascade,
    -- HTTP status of the attempt; null on a transport error (no response).
    status     int,
    request    jsonb,
    response   jsonb,
    created_at timestamptz not null default now()
);

create index if not exists cron_event_invocation_logs_event_idx
    on dist_api.cron_event_invocation_logs (event_id);
