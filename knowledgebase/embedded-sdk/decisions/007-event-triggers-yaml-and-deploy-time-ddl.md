---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[hooks-and-events]]"
---

# Table event triggers: YAML metadata + deploy-time trigger DDL

## Context

Table event triggers (webhooks on row insert/update/delete) are required for
Donat v2 parity. Donat creates them at runtime via the `create_event_trigger`
metadata API, which also issues the `CREATE TRIGGER` DDL on the user table.
This engine has no admin/runtime API and a hard rule that **the serving binary
never runs DDL** (see [[no-admin-role]]). Yet event capture fundamentally needs
a per-table Postgres trigger writing to an event log in the mutation's
transaction (see [[hooks-and-events]] and
[[decisions/002-keep-durable-journal-alongside-in-memory-hooks]]).

## Decision

Declare event triggers in YAML under the table (`event_triggers`, Donat's
directory-format `EventTriggerConf`). Split the work by lifetime:

- **Capture (DDL) is deploy-time.** A migration creates the `donat`
  event-log catalog and one generic `donat.notify_event()` function. The
  per-table `CREATE TRIGGER` statements — the only DDL that depends on
  metadata — are applied by `migrate --metadata-dir` (a `reconcile` step that
  also drops engine-managed triggers no longer declared). This keeps all DDL
  in the deploy-time `migrate` path; the serving binary still never runs DDL.
- **Delivery (DML) is runtime.** The serving binary runs a background poller
  over `donat.event_log` reusing the cron machinery (`FOR UPDATE SKIP
  LOCKED`, retries, invocation logs) — at-least-once, multi-instance safe, no
  leader election (see [[006-cron-triggers-yaml-only]]).

A single generic trigger function (parameterized by trigger name via
`TG_ARGV[0]`) avoids generating one function per trigger; per-table triggers
are thin. Capture is in-transaction, so raw-SQL writes fire events too — a
property in-memory hooks cannot provide.

## Alternatives

| Option | Why Not |
|--------|---------|
| Server creates triggers at boot | Violates "serving binary never runs DDL"; also racy across replicas |
| Runtime `create_event_trigger` API | Reintroduces an admin/runtime mutation surface the project rejects |
| Static `CREATE TRIGGER` in a migration file | Triggers depend on YAML (which tables/ops/columns); can't be static |
| In-memory post-commit hooks instead of a journal | Loses at-least-once and misses raw-SQL writes (rejected in ADR 002) |

## Consequences

Event triggers are fully deploy-time: `migrate --metadata-dir` is now the one
place trigger DDL is reconciled, composing with the existing migrate→validate
→serve order. Delivery shares cron's code and guarantees. Open items
(tracked in `specs/002-event-triggers.md`): session-variable capture (needs a
`SET LOCAL donat.user` wrapper on triggered mutations — a deviation from the
single-statement model, applied only when a trigger exists), column-filtered
payloads, manual/async/transform features, and multi-source reconcile.
