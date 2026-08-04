---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Process starts consume pinned revisions in one source-local transaction

## Context

A successful Command commits a Process start request with an exact executable
revision and a semantic idempotency key. The consumer must survive worker
loss, rolling deployment, and two Command generations racing on the same
semantic start. It must also retain Postgres `jsonb` values exactly: decoding
an arbitrary-precision decimal through Rust's default JSON number
representation can round the durable business input.

The runtime has no metadata mutation surface and cannot repair or reinterpret
a journal row from current YAML. Multiple metadata sources may share one
physical database, including identical process names, UUIDs, and semantic
keys.

## Decision

Build one `ProcessRuntime` per Process-owning Postgres source from one
published Engine snapshot. Construction clones only the concrete Postgres
pool, rejects other backends and cross-source deployed definitions, and
retains the snapshot's deployed revisions, pre-Process Command catalog,
finalized Command catalog, and connector registry together.

One consumer transaction claims one pending source-qualified request with
`FOR UPDATE SKIP LOCKED`. It resolves only the request's exact revision in the
immutable deployed catalog, validates its typed input, and inserts or finds
the instance through
`(source_name, process_name, start_idempotency_key)`. A new instance receives
one pending start event and one `started` history row; an existing semantic
instance receives a `duplicate_start` history row. The request outcome and
instance link commit in that same transaction.

Instance and start-event payloads copy the locked request's `jsonb` directly
with `INSERT ... SELECT`. Rust decoding is used only for contract validation;
it is never the source of a durable payload rewrite. Decimal spelling is
normalized only in the temporary `TypedValue`, while the original database
value and scale remain unchanged.

The Tokio loop polls only to wake the transaction. A dropped transaction
leaves the request pending, while a committed request cannot create another
instance. A missing pinned revision is an invariant error and rolls the claim
back; the worker never substitutes the active revision.

## Alternatives

| Option | Why Not |
| --- | --- |
| Resolve the currently active Process | A rolling deployment would execute old durable intent under new behavior. |
| Decode and re-encode `jsonb` through `serde_json::Value` | Default JSON numbers can round arbitrary-precision Postgres decimals and change business input. |
| Enable arbitrary-precision JSON globally | Cargo feature unification changes JSON-number serialization throughout the workspace, including YAML metadata adapters. |
| Mark the request consumed before creating history | A crash can lose the start or leave an instance without its durable event/audit trail. |
| Use one default pool for every Process | Sources sharing a database can collide, while sources on different databases lose atomicity and database-clock semantics. |

## Consequences

Rolling binaries execute the revision selected by the original Command, and
separate Command generations deduplicate at the Process semantic boundary.
All start state is source-qualified and crash-consistent without an external
workflow service or another process.

The consumer performs several indexed statements inside one short
transaction and retains one immutable Engine snapshot for its lifetime.
Invalid input or an absent revision remains pending and produces an invariant
error until deployment state is corrected; it is not silently failed or
reinterpreted.
