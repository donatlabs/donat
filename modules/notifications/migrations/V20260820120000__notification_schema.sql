-- The notification module's own schema.
--
-- This is application DDL, not engine DDL: it lives in an application schema
-- rather than `donat`, because `donat` is the engine's and the shape of what is
-- in it is the engine's compatibility surface. A deployment applies this the
-- same way it applies its own migrations — `donat migrate --migrations-dir`,
-- deploy-time only. The serving engine runs no DDL.
--
-- Who a recipient *is* is deliberately not here. The application owns its users,
-- so it supplies `notification.recipient` as a view over whatever table already
-- holds them (see the module README). Declaring it as a tracked table means a
-- deployment that forgets is refused by `donat validate` rather than discovered
-- at the first send.

create schema if not exists notification;

create extension if not exists pgcrypto;

-- One row per triggered notification: the workflow that fired, who it is for,
-- and the data every channel renders from.
create table if not exists notification.dispatch (
    id           uuid primary key default gen_random_uuid(),
    workflow     text not null,
    recipient_id text not null,
    payload      jsonb not null default '{}'::jsonb,
    -- What the in-app channel shows. Email renders from `payload` and a
    -- template instead; these three are the caller's own words, and nothing in
    -- the module authors them.
    title        text not null,
    body         text not null,
    url          text,
    -- Whether this notification's email is batched. The bell still rings
    -- immediately; only the mail waits for the sweep.
    digest       boolean not null default false,
    created_at   timestamptz not null default now()
);

-- The in-app channel's message. `seen_at` is "the bell showed it", `read_at` is
-- "they opened it" — Novu's distinction, and the one an email-after-delay step
-- needs to ask about.
create table if not exists notification.inbox (
    id           uuid primary key default gen_random_uuid(),
    dispatch_id  uuid not null references notification.dispatch (id) on delete cascade,
    recipient_id text not null,
    title        text not null,
    body         text not null,
    url          text,
    created_at   timestamptz not null default now(),
    seen_at      timestamptz,
    read_at      timestamptz,
    archived_at  timestamptz,
    -- Derived, so that "has this been seen" is a value a command can select on.
    -- A command's `by` is equality only, deliberately, and the alternative
    -- would be a null predicate in the command grammar for one caller's sake.
    seen         boolean generated always as (seen_at is not null) stored
);

-- Serves the unread count and the feed's own ordering, which are the two reads
-- every client makes on every page.
create index if not exists inbox_unread_idx
    on notification.inbox (recipient_id, read_at, created_at desc);

-- What actually happened per channel, including the channels that were never
-- tried. `unknown` is the outcome an at-most-once send routes to when nobody
-- can know whether it arrived (ADR 063); it is a status, not a failure.
create table if not exists notification.delivery (
    id                  uuid primary key default gen_random_uuid(),
    dispatch_id         uuid not null references notification.dispatch (id) on delete cascade,
    -- Carried from the dispatch rather than joined to it. The digest sweep
    -- groups by exactly this pair, and a command's predicate is equality only:
    -- denormalising here is what keeps the sweep a declaration instead of a
    -- query someone has to write by hand.
    recipient_id        text not null,
    workflow            text not null,
    channel             text not null,   -- in_app | email
    -- The email channel's own state machine: `deferred` when the digest holds
    -- it back, `sending` once a sweep has claimed it, `sent` when the relay
    -- accepted it. The claim is a status transition rather than a second table,
    -- so there is one place to ask what happened to a notification.
    status              text not null,   -- suppressed | sent | failed | skipped | deferred | sending | unknown
    provider_message_id text,
    error_code          text,
    -- Which sweep claimed this row. A sweep records only what it claimed, so
    -- two overlapping sweeps cannot mark each other's in-flight rows sent.
    claim_id            uuid,
    recorded_at         timestamptz not null default now(),
    unique (dispatch_id, channel),
    constraint delivery_channel_is_known
        check (channel in ('in_app', 'email')),
    constraint delivery_status_is_known
        check (status in ('sent', 'sending', 'suppressed', 'skipped',
                          'deferred', 'failed', 'unknown'))
);

-- The sweep's own read: which recipients are owed a digest, and how much of it.
-- The same index serves the claim (`deferred` → `sending`) and the record
-- (`sending` → `sent`), because both group by the same pair.
create index if not exists delivery_deferred_idx
    on notification.delivery (channel, status, recipient_id, workflow);

-- Opt-out, per recipient per workflow per channel. Absent means enabled: a
-- deployment that adds a workflow does not have to backfill a row for everyone.
create table if not exists notification.preference (
    recipient_id text not null,
    workflow     text not null,
    channel      text not null,
    enabled      boolean not null default true,
    updated_at   timestamptz not null default now(),
    primary key (recipient_id, workflow, channel),
    constraint preference_channel_is_known
        check (channel in ('in_app', 'email'))
);

-- The recipient contract, and the one thing an adopting application must
-- supply. The module ships the *shape* and no rows; the application replaces
-- the body with a view over whatever table already holds its users:
--
--   create or replace view notification.recipient as
--   select u.id, u.email, u.email_verified, u.locale, u.timezone
--   from public.app_user u;
--
-- `create or replace view` is what makes this a contract rather than a
-- convention: Postgres refuses a replacement whose column names or types
-- differ, so a binding that does not fit is a failed migration rather than a
-- notification that goes nowhere. Until it is replaced this view has no rows,
-- and `notify` refuses every send with "recipient not found" — loudly, at the
-- first attempt, which is the failure a deployment can act on.
create or replace view notification.recipient as
select
    null::text    as id,
    null::text    as email,
    null::boolean as email_verified,
    null::text    as locale,
    null::text    as timezone
where false;

-- Which recipients are owed a digest, and how much of it.
--
-- This is read by the *scheduler*, over the ordinary API under an ordinary
-- permission, and not by the sweep: enumerating groups inside a Process would
-- mean a `for_each` over an unbounded read, and a fan-out declares a fixed
-- maximum whose overflow fails the whole instance rather than trimming. A
-- client loop is bounded by its own `limit`, so the sweep never has to be.
create or replace view notification.pending_digest as
select
    recipient_id      as recipient_id,
    workflow          as workflow,
    count(*)::bigint  as pending,
    min(recorded_at)  as oldest
from notification.delivery
where channel = 'email' and status = 'deferred'
group by recipient_id, workflow;
