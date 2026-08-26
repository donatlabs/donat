---
type: decision
status: accepted
date: 2026-08-26
features:
  - "[[hooks-and-events]]"
---

# A trigger invokes a declared target under a declared session

## Context

Cron triggers ([[006-cron-triggers-yaml-only]]) and table event triggers
([[007-event-triggers-yaml-and-deploy-time-ddl]]) ended at a URL. On the
standalone server that URL is a service somebody writes, runs and secures —
and for the two things a background job most often needs, no such service
could be honest: it cannot read a write-only column (a provider token has no
select field, by design), and it cannot act *as a tenant* without a minted
JWT, because a tenant is a claim and never a header. Every workaround was one
of the wrong designs: an engine calling its own `/v1/graphql`, a static cron
payload pretending to hold a per-row secret, a select permission on the
token, an unauthenticated role. Meanwhile a flow could only be started by a
client calling the command that carries `start_process` — so "every night,
for every active store, start the settlement" had no declaration at all.

## Decision

A trigger may name an `invoke` target instead of a webhook: an existing
action, or an existing command. The declaration also names the classic
session the invocation runs as — a role that must already be on the target's
permissions, and session variables bound from the triggering row — and which
arguments come from which columns. A cron trigger says which rows are its
work items (`foreach`, with a closed `where` grammar and optional `unnest`);
an event trigger's row is the event's.

The engine then takes the path a request would have taken. An action goes
through the one resolver `perform_action`, which the GraphQL field also
calls — transforms, headers, timeout and all. A command goes through the
GraphQL mutation path under that session, so its permissions, guards and
tenant scoping are the ones every client gets. There is no second permission
world: the only privilege the background path has is *reading the row it
binds from* without a select permission, and what it journals about that row
is redacted by the role's own select permission.

Delivery reuses both journals and adds a child, `donat.trigger_invocations`:
the parent claim expands into work items and commits; each work item is
claimed and run on its own, at-least-once, so one tenant's slow handler
never holds the whole occurrence's lock. The parent's `retry_conf` is the
child's.

## Alternatives

| Option | Why Not |
|--------|---------|
| Cron posts to the engine's own `/v1/graphql` with a minted token | Needs a signing key the engine was never meant to hold; the tenant is a claim; a token in a request body is in every log |
| A `worker` role that bypasses permissions | The BLOCKING RULE; and a bypass has no answer to "whose rows did this touch" |
| A cron target that is only an action, with `then` for the write | A schedule that starts a flow would need a fake action in front of the command; the command is the entry point a client already uses |
| Deliver children under the parent's lock | One slow tenant blocks the occurrence; a foreach × HTTP under `FOR UPDATE` is what [[006-cron-triggers-yaml-only]] warned about |
| Store the bound input verbatim for debugging | The point of a write-only column is that its value is never read back; `***` says a token was sent, never which |

## Consequences

"Every five minutes, as each workspace's owner, pull their issues with their
token and ingest them" and "every night, for every active store, start the
settlement" are each one YAML block with no receiver. The cost is the
at-least-once contract on a command, which commands already carried, and a
second poll on the same cadence. The closed `where` grammar will be asked
to grow; each operator added is a decision about what a cross-tenant,
permission-free read may ask, and belongs in this ADR's successor rather
than in a quick patch.
