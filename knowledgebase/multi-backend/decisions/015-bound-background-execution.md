---
type: decision
status: accepted
date: 2026-07-20
features:
  - "[[multi-backend]]"
---

# Bound background execution separately from connection lifetime

## Context

MySQL checkout and execution run in `spawn_blocking`. The driver pool limits
physical connections but does not prevent a request burst from filling Tokio's
blocking-worker queue while those tasks wait for a connection. Runtime source
refresh also bypassed normal SQLite and MySQL admission. Separately, a live
subscription held one of up to 1,000 active-task permits for its entire
lifetime, but all of its one-second polls could execute together.

## Decision

Every MySQL blocking operation, including catalog refresh, first acquires an
async permit sized to the source's `max_connections`. The permit moves into the
blocking closure, so cancelling its async caller cannot admit a replacement
while the worker is still checking out or using a connection. SQLite refresh
uses its existing `run_blocking` admission path for the same reason. A runtime
may reuse its permit set only when its URL and pool settings both match.

Subscriptions retain the existing active-task cap. Individual polls acquire a
separate process-wide semaphore, defaulting to 16 and configurable with
`DONAT_GRAPHQL_MAX_CONCURRENT_SUBSCRIPTION_POLLS`. Subscription start phases
are spread across the one-second interval. The poll permit is released before
the task sleeps, so idle subscriptions consume no database execution capacity.

## Alternatives

| Option | Why Not |
|--------|---------|
| Rely only on driver connection pools | Limits sockets but lets blocked work accumulate in Tokio's blocking queue. |
| Hold a poll permit for each subscription lifetime | Caps active sockets instead of backend work and wastes capacity while polling tasks sleep. |
| Abort an active blocking operation on client cancellation | The blocking database driver cannot be safely preempted; retaining admission until it returns avoids oversubscription. |

## Consequences

Burst load is queued in async semaphores with configured timeout behavior
instead of creating unbounded blocked workers. Reload cannot transiently open
an extra SQLite connection or wait indefinitely outside MySQL pool policy.
Subscription initial results can be delayed by at most one polling interval,
which trades a small startup delay for smoother backend load and a bounded
number of concurrent executions.
