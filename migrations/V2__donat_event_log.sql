-- Engine-internal catalog for table event triggers (row insert/update/delete).
--
-- Like cron, the serving binary never runs DDL: this migration creates the
-- event log, its invocation logs, and the generic trigger function. The
-- per-table CREATE TRIGGER statements (which depend on YAML metadata) are
-- applied by the deploy-time `event-triggers reconcile` subcommand.
--
-- Capture is in-transaction: the PG trigger writes the event row in the SAME
-- transaction as the mutation, so a crash loses nothing and raw-SQL writes
-- fire events too. A background poller delivers them (see crates/server).

-- One row per captured row-change, awaiting delivery.
create table if not exists donat.event_log (
    id            uuid primary key default gen_random_uuid(),
    trigger_name  text        not null,
    schema_name   text        not null,
    table_name    text        not null,
    -- INSERT | UPDATE | DELETE | MANUAL
    op            text        not null,
    data_old      jsonb,
    data_new      jsonb,
    session_variables jsonb,
    -- scheduled | delivered | error
    status        text        not null default 'scheduled',
    tries         int         not null default 0,
    next_retry_at timestamptz,
    created_at    timestamptz not null default now()
);

create index if not exists event_log_due_idx
    on donat.event_log (status, created_at);

create table if not exists donat.event_invocation_logs (
    id         uuid primary key default gen_random_uuid(),
    event_id   uuid not null references donat.event_log (id) on delete cascade,
    -- HTTP status of the attempt; null on a transport error (no response).
    status     int,
    request    jsonb,
    response   jsonb,
    created_at timestamptz not null default now()
);

create index if not exists event_invocation_logs_event_idx
    on donat.event_invocation_logs (event_id);

-- Generic capture function, shared by every per-table trigger. The trigger
-- name is passed as the first trigger argument (TG_ARGV[0]); old/new rows are
-- captured as jsonb. Session variables, when the engine sets the `hasura.user`
-- GUC inside the mutation transaction, are captured too (NULL otherwise).
create or replace function donat.notify_event() returns trigger
language plpgsql as $$
declare
    v_old jsonb := null;
    v_new jsonb := null;
begin
    if (tg_op = 'INSERT') then
        v_new := to_jsonb(new);
    elsif (tg_op = 'UPDATE') then
        v_old := to_jsonb(old);
        v_new := to_jsonb(new);
    elsif (tg_op = 'DELETE') then
        v_old := to_jsonb(old);
    end if;

    insert into donat.event_log
        (trigger_name, schema_name, table_name, op, data_old, data_new, session_variables)
    values
        (tg_argv[0], tg_table_schema, tg_table_name, tg_op, v_old, v_new,
         nullif(current_setting('hasura.user', true), '')::jsonb);

    return null;
end;
$$;
