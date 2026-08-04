-- Deploy-time catalog for declarative domain-command idempotency.
--
-- The command compiler writes and replays this row inside its single
-- transaction (implemented in the SQL-generation slice).  This migration is
-- intentionally applied only by `donat migrate`; the serving binary must not
-- create this table or the structured-error helper while staging a snapshot.

create schema if not exists donat;

create table if not exists donat.command_invocations (
    -- The canonical command idempotency scope.  Hashes are binary digests of
    -- canonical JSON, so raw scope values and input are never stored here.
    command_name      text        not null,
    scope_hash        bytea       not null,
    key               text        not null,
    input_fingerprint bytea       not null,
    -- The completed, canonical command-root JSON value.  A replay projects
    -- this value in SQL rather than re-running user-table writes.
    result            jsonb       not null,
    status            text        not null default 'succeeded',
    expires_at        timestamptz not null,
    created_at        timestamptz not null default now(),
    primary key (command_name, scope_hash, key)
);

-- Retention cleanup only needs to find expired command invocations; it must
-- not weaken or replace the idempotency primary key above.
create index if not exists command_invocations_expires_at_idx
    on donat.command_invocations (expires_at);

-- Raise a deliberately narrow, structured business rejection from a command
-- CTE.  The SQLSTATE plus envelope kind form an internal protocol understood
-- only by the GraphQL database-error decoder.  The function accepts typed
-- text values and uses json_build_object; it never interpolates or executes
-- caller-provided SQL.
create or replace function donat.raise_graphql_error(
    code text,
    path text,
    message text
) returns jsonb
language plpgsql
as $$
begin
    -- Keep malformed calls inside the reserved SQLSTATE too.  The decoder
    -- rejects this non-envelope safely instead of exposing PostgreSQL text.
    if code is null
       or path is null
       or left(path, 1) <> '$'
       or message is null then
        raise exception using
            errcode = 'P0D01',
            message = '{"kind":"donat.graphql-error.v1","invalid":true}';
    end if;

    raise exception using
        errcode = 'P0D01',
        message = json_build_object(
            'kind', 'donat.graphql-error.v1',
            'code', code,
            'path', path,
            'message', message
        )::text;

    return null;
end;
$$;
