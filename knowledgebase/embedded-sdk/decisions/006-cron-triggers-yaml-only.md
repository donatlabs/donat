---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[hooks-and-events]]"
---

# Cron triggers are YAML-only and multi-instance-safe via the Postgres journal

## Context

The user needs scheduled (cron) webhooks but explicitly does not want an
admin role or any runtime mutation surface (see [[no-admin-role]]). Donat v2
has two scheduled-trigger flavors: **cron triggers** (recurring, defined in
metadata) and **one-off scheduled events** (created at runtime via the
metadata API `create_scheduled_event`). The engine also runs as multiple
replicas (pods), so delivery must not double-fire or be lost when several
instances run the same loop.

## Decision

Implement **cron triggers only**, configured in YAML metadata
(`cron_triggers`, Donat `CronTriggerMetadata` shape) and delivered by a
background loop in the serving binary. The durable state lives in a Postgres
catalog schema `donat` (tables `cron_events`,
`cron_event_invocation_logs`), created by `migrate` — the serving binary
never runs DDL.

Multi-instance correctness comes from two database mechanisms, no leader
election:

1. **Idempotent materialization.** Each instance inserts the next occurrence
   with `INSERT ... ON CONFLICT (trigger_name, scheduled_time) DO NOTHING`
   against a unique constraint, so concurrent instances converge on exactly
   one row per occurrence.
2. **Exclusive claim.** Due events are claimed with `FOR UPDATE SKIP
   LOCKED`; delivery happens inside the claiming transaction. One instance
   delivers each event; a crash mid-delivery rolls the claim back and another
   instance redelivers. This is **at-least-once** — handlers must be
   idempotent (same contract as Donat).

Time comparisons (`scheduled_time <= now()`, `next_retry_at`) use the
database clock, the single source of truth across pods.

**One-off scheduled events are out of scope**: they require a runtime
creation endpoint, which contradicts the no-admin-API posture. Revisit only
if a deploy-time seed format (one-offs declared in YAML) is wanted.

## Alternatives

| Option | Why Not |
|--------|---------|
| In-memory scheduler, no journal | Loses correctness across restarts; with N pods every pod fires every occurrence (N× duplicate deliveries) |
| Leader election (one pod schedules) | Operational complexity (lease/lock service) for what `FOR UPDATE SKIP LOCKED` already gives for free |
| One-off scheduled events too | Needs a runtime `create_scheduled_event` mutation surface; rejected by the no-admin-role decision |
| Deliver after commit (claim→commit→POST) | Lower connection hold time, but a crash after commit and before POST loses the event unless a "locked, reclaim-if-stale" sweep is added — deferred until event-trigger volumes justify it |

## Consequences

Cron scales to many pods with zero coordination, paying at-least-once (so the
idempotency requirement must be documented for webhook authors). The delivery
loop holds a pooled connection and a row lock for the duration of each HTTP
call (up to `timeout_seconds`); negligible at cron volumes, but a known
scaling edge to revisit when durable **table** event triggers (high event
rate) are built on the same machinery — see [[hooks-and-events]].
