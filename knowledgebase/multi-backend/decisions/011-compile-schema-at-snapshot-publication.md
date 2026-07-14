---
type: decision
status: accepted
date: 2026-07-14
features:
  - "[[multi-backend]]"
---

# Compile schemas when publishing an engine snapshot

## Context

The composite GraphQL planner rebuilt the role-independent schema and every
role projection for each tabular request. With production metadata, schema
preparation took about 1.4 seconds before a sub-millisecond SQL statement and
caused multi-second queueing under concurrency. Metadata and catalogs already
changed as a coordinated unit during source synchronization, so request-time
schema compilation provided no freshness benefit.

## Decision

Donat compiles per-source table/function indexes, multi-source ownership,
validation state, and role-specific standard/Relay introspection schemas when a
candidate metadata/catalog snapshot is synchronized. Candidate compilation and
backend routing setup complete before publication, and the engine atomically
swaps metadata, catalogs, runtime handles, and the immutable compiled schema.
Requests create only lightweight planner views that borrow the published
snapshot.

Introspection roots are detected before a cached role schema is selected.
Ordinary operations never compose an introspection schema. Permission checks
remain in the source-local planners; the compiled snapshot grants no additional
access and introduces no admin role.

## Alternatives

| Option | Why Not |
|--------|---------|
| TTL or LRU cache on the request path | Adds eviction, synchronization, and cold-request latency even though schema changes only with the engine snapshot. |
| Hash metadata on every request | Still walks large metadata, complicates catalog identity, and can publish mismatched cached state. |
| Process-global lazy singleton | Makes invalidation and tests stateful and can serve a schema from different metadata/catalog inputs. |
| Cache a complete self-referential planner | Rust ownership becomes unsafe or unnecessarily complex because child planners borrow metadata and catalogs. |

## Consequences

Startup and source synchronization perform schema composition and source index
construction once and can fail before publication. The serving path no longer
scales with total metadata roles or tables. The engine carries the immutable
compiled value and source routing handles, and code that creates an engine
snapshot must explicitly initialize or compile it.

## See Also

- [[010-compose-metadata-sources-in-graphql]]
- [[../_index|Multi-Backend Data Sources]]
