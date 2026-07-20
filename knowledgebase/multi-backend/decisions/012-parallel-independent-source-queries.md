---
type: decision
status: accepted
date: 2026-07-19
features:
  - "[[multi-backend]]"
---

# Execute independent source queries concurrently

## Context

A mixed-source GraphQL read is partitioned into one database-assembled query
per participating source. The server awaited those independent source queries
one at a time, so end-to-end latency approached the sum of the backend
latencies. This was unnecessary because reads do not share a transaction or
mutable state across sources.

## Decision

Start all source query futures concurrently and wait for every result. Preserve
the source-plan order in the returned vector and, when multiple sources fail,
return the first error in source-plan order. Response assembly therefore stays
deterministic while mixed-source latency approaches the slowest participating
source rather than their sum.

Mutations remain restricted to one source. Per-source execution still emits
exactly one native statement and database-side JSON assembly remains unchanged.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep sequential execution | Adds independent backend latencies and leaves one connection idle while another source runs. |
| Return the first error to complete | Lower failure latency, but makes the externally visible error nondeterministic when multiple sources fail. |
| Spawn detached Tokio tasks | Requires owned plans and executor state, adds task-management overhead, and complicates cancellation without improving concurrency. |

## Consequences

Mixed-source reads use backend capacity concurrently and preserve response and
error ordering. A request may hold one connection or HTTP request per
participating source at the same time, so future load testing must measure pool
pressure for broad multi-source operations.
