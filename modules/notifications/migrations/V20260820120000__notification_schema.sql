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

-- The channels this deployment delivers on.
--
-- A table rather than a `check (channel in (…))`, because a channel is exactly
-- the thing an adopting deployment adds. Telegram, SMS or a chat webhook is one
-- insert in the application's own migration plus a send state; a CHECK would
-- have made it an edit to this file, which is a fork of the module.
--
-- The two shipped rows are the two the module's delivery Process knows how to
-- send on. A row on its own delivers nothing: it makes the channel nameable, so
-- an opt-out, a delivery row and an address can refer to it.
create table if not exists notification.channel (
    name        text primary key,
    description text not null default ''
);

insert into notification.channel (name, description) values
    ('in_app', 'The recipient''s own feed, read through notification.inbox.'),
    ('email',  'A message posted to the deployment''s mail relay.')
on conflict (name) do nothing;

-- One row per triggered notification: the workflow that fired, who it is for,
-- and the data every channel renders from.
create table if not exists notification.dispatch (
    id           uuid primary key default gen_random_uuid(),
    workflow     text not null,
    recipient_id text not null,
    -- What a message is rendered from, when the caller has structured data
    -- rather than a sentence. The module never reads it: it carries it to
    -- whatever renders, which today is the relay (see the README, and
    -- `plans/004-*` for the renderer the engine cannot reach).
    payload      jsonb,
    -- What the in-app channel shows. These three are the caller's own words,
    -- and nothing in the module authors them.
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
    foreign key (channel) references notification.channel (name),
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
    foreign key (channel) references notification.channel (name)
);

-- The recipient contract: who a person is, and where each channel reaches them.
--
-- Two views rather than one, and the split is what makes a channel addable. The
-- person's own facts — language, timezone — are the same whatever the channel;
-- an address is per channel, and a shape with `email` in it has nowhere to put
-- a chat id. `create or replace view` refuses a replacement whose columns
-- differ, so a single view would have frozen the set of channels into this file
-- forever. An adopting deployment replaces both bodies:
--
--   create or replace view notification.recipient as
--   select u.id::text, u.locale, u.timezone from public.app_user u;
--
--   create or replace view notification.recipient_address as
--   select u.id::text, 'email'::text, u.email, u.email_verified
--   from public.app_user u where u.email is not null;
--
-- Adding Telegram later is a `union all` branch in the second view and a row in
-- `notification.channel` — no change to this module.
--
-- Until they are replaced neither has rows, and `notify` refuses every send
-- with "recipient not found" — loudly, at the first attempt, which is the
-- failure a deployment can act on.
create or replace view notification.recipient as
select
    null::text    as id,
    null::text    as locale,
    null::text    as timezone
where false;

create or replace view notification.recipient_address as
select
    null::text    as recipient_id,
    null::text    as channel,
    null::text    as address,
    null::boolean as verified
where false;

-- Which recipients are owed a digest, and how much of it.
--
-- This is read by the *scheduler*, over the ordinary API under an ordinary
-- permission, and not by the sweep: enumerating groups inside a Process would
-- mean a `for_each` over a read of every pending group, and a fan-out declares
-- a fixed maximum whose overflow fails the whole instance rather than trimming.
-- A client loop is bounded by its own `limit`, so the sweep never has to be.
create or replace view notification.pending_digest as
select
    recipient_id      as recipient_id,
    workflow          as workflow,
    count(*)::bigint  as pending,
    min(recorded_at)  as oldest
from notification.delivery
where channel = 'email' and status = 'deferred'
group by recipient_id, workflow;
