---
type: decision
status: accepted
date: 2026-07-19
features:
  - "[[multi-backend]]"
---

# Reuse backend connections and compile request metadata in snapshots

## Context

The PostgreSQL runtime already used a persistent pool, but MySQL opened a new
connection and repeated two session `SET` statements for every operation, and
SQLite opened a file connection inside every blocking task. Ordinary requests
also reparsed every allowed query, REST saved query, and remote permission SDL.
These costs are configuration-derived and do not need to be repeated.

## Decision

Each immutable engine snapshot owns bounded PostgreSQL, MySQL, and SQLite
runtime pools. MySQL session initialization runs when a physical connection is
created; SQLite file connections are returned to a small semaphore-bounded
idle pool. `max_connections` and `pool_timeout` from metadata configure pool
capacity and supported wait deadlines. A runtime is reused across snapshot
publication only when both its resolved URL and pool settings match.
SQLite blocking jobs own their semaphore permits directly. Cancelling the
async request drops its join handle but cannot release capacity until the
non-cancellable blocking job has finished and returned its connection.

Allowlist normal forms, REST documents and variable definitions, and remote
permission SDL documents are compiled before snapshot publication. Request
paths use indexed lookups and borrow or clone compiled documents. Polling
subscriptions parse their document once per subscription and reuse it on every
tick while still reading the latest engine snapshot.

## Alternatives

| Option | Why Not |
|--------|---------|
| Open connections per request | Repeats handshakes, setup round trips, and SQLite file initialization. |
| One global pool per backend kind | Sources have independent URLs, credentials, limits, and publication lifetimes. |
| Cache outside the engine snapshot | Requires invalidation and can mix old metadata with a newly published schema. |
| Cache complete result plans forever | Variables, role/session permissions, and snapshot replacement require a more specific invalidation design. |

## Consequences

Steady-state requests avoid connection setup and configuration parsing. Pool
pressure is bounded and metadata changes atomically replace all derived state.
Pools retain live backend resources for the lifetime of a snapshot, and MySQL
session state must remain engine-owned because connection reset is disabled to
preserve initialization and avoid its checkout round trip. SQLite cancellation
can outlive the request, but it cannot exceed the configured pool capacity.
