---
type: decision
status: accepted
date: 2026-07-19
features:
  - "[[multi-backend]]"
---

# Batch row-dependent remote and Action relationships

## Context

Remote relationships and relationships on Action output objects were resolved
once per returned row. A list of N rows therefore made N upstream HTTP or
database requests even when rows repeated the same join key. This dominated
latency and backend load while the ordinary relationship planner already kept
relationships inside one database statement.

The fix must preserve explicit-role permissions, per-parent limits, GraphQL
selection order, and the invariant that a participating datasource receives a
single planned statement rather than row-by-row executor calls.

## Decision

Collect relationship placeholders across the complete result, group them by
relationship selection, and deduplicate identical variable maps. Remote
relationships use aliased roots in batches of at most 100 unique keys, with at
most four batches in flight. Action output relationships build one aliased
internal GraphQL operation, which the existing planner executes as one source
statement. Results are mapped back through JSON pointers in original row order.

When an upstream schema returns a recognized GraphQL validation code for the
aliased batch, fall back to the former sequential shape so its error body and
path remain compatible. Transport failures, timeouts, and resolver errors are
returned immediately: retrying them once per key would recreate N+1 precisely
when the upstream is least healthy. Query Actions are independent and run
concurrently; mutation Actions remain sequential.

## Alternatives

| Option | Why Not |
|--------|---------|
| One request per row | Simple but creates an unbounded N+1 multiplier. |
| One broad `_or` table query | Can change per-parent limits and complicates permission-safe partitioning. |
| Unbounded aliases or concurrency | Reduces request count but can create oversized documents and burst-load an upstream. |
| DataLoader cache only | Deduplicates repeated keys but still makes one request per unique key. |

## Consequences

Relationship work is proportional to relationship groups and bounded batches,
not returned rows. Repeated keys are requested once and output ordering remains
stable. A recognized validation rejection can perform one failed combined read
before compatible sequential retries; remote relationship queries must
therefore remain reads. Operational failures never fan out into retries.
