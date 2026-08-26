-- Work items of `invoke` triggers (spec 010).
--
-- A cron occurrence or a captured row change is the *parent*; when its
-- trigger names an action or command instead of a webhook, the parent is
-- expanded into one row here per work item — one per row of `foreach`, or
-- one for the event's row — and marked delivered. Each work item is then
-- claimed and run on its own, so the HTTP call and the `then` command of one
-- tenant never hold the lock of the whole occurrence, and a crash after
-- expansion re-expands into nothing (the unique key below).
--
-- Bound secrets are never stored: `input` is the redacted argument map (a
-- value read from a column the session's role cannot select is `***`).
create table if not exists donat.trigger_invocations (
    id            uuid primary key default gen_random_uuid(),
    -- cron | event
    kind          text not null check (kind in ('cron', 'event')),
    -- donat.cron_events.id or donat.event_log.id
    parent_id     uuid not null,
    trigger_name  text not null,
    -- the work item's identity: Foreach.key (or the primary key) as an object
    row_key       jsonb not null,
    -- scheduled | delivered | error | dead
    status        text not null default 'scheduled'
        check (status in ('scheduled', 'delivered', 'error', 'dead')),
    tries         int not null default 0,
    next_retry_at timestamptz,
    input         jsonb,
    result        jsonb,
    error         jsonb,
    created_at    timestamptz not null default now(),
    updated_at    timestamptz not null default now(),
    unique (kind, parent_id, row_key)
);

create index if not exists trigger_invocations_due_idx
    on donat.trigger_invocations (kind, status, created_at);
