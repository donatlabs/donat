---
type: decision
status: accepted
date: 2026-07-29
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Durable processes use two-stage compilation and source-local journals

## Context

Process definitions need exact command, Rule, and connector contracts before a
revision can be derived. Command effects in turn need that derived revision
before the serving schema can execute them. The command compiler lives in
`donat-schema`, while the process and connector implementations live in
`donat-server`; making schema depend on server would create a crate cycle.

The original process plan also left deployment and worker transactions
implicitly tied to one database URL. That cannot preserve command atomicity,
database-clock semantics, capacity coordination, or webhook routing when
metadata has multiple Postgres sources. Its command outboxes reused the
idempotency tuple instead of identifying one execution generation, its
rejection path lacked a savepoint, and one inbound row could not retain both
provider-event dedupe and every delivery attempt.

## Decision

Use a server-orchestrated two-stage immutable candidate build. Shared recursive
value-contract types live in `donat-ir`. `donat-schema` compiles commands to
public pre-process descriptors whose canonical fingerprints include raw effect
declarations but no resolved process revision. The existing connector registry
publishes public typed operation/event descriptors. The server-owned process
compiler consumes those descriptors and Rules, derives revisions, and exposes
them through a schema-owned neutral effect-contract interface. Schema then
finalizes command effects and compiles the serving schema. Each `Engine`
snapshot retains one immutable source-qualified process catalog; no compiler
is duplicated and schema never depends on server.

Processes are strictly source-local. Start, transition, start-effect, and
signal-effect commands resolve only in the process's Postgres source. A
connector instance used by processes is bound to one source, and workers,
database clocks, capacity reservations, webhook routing, and journal pools are
created per source. Deployment explicitly selects one real source with
`migrate --metadata-dir <dir> --source <name>` or
`validate --metadata-dir <dir> --source <name>`; omission is valid only for one
unambiguous Postgres source. Reconciliation changes only that source, while
serve reads and validates every real source without issuing DDL.

V6 gives each completed command execution generation a durable
`invocation_id uuid`. Exact replay preserves it; expired-key re-execution gets
a new UUID. Every process-effect outbox copies it and is unique by invocation
and effect position, with no retention-coupling foreign key to the command
journal. A process command runs its one statement in a savepoint owned by the
outer process transition. Only the established valid `P0D01`
`donat.graphql-error.v1` rejection is rolled back to that savepoint and turned
into one committed `on_rejection` transition; every other database error
aborts the outer transaction.

Inbound persistence is split. `process_inbound_events` is only the verified
provider-event dedupe ledger.
`process_inbound_deliveries` is append-only audit for every accepted,
duplicate, unmatched, ambiguous, guard-false, unexpected-state, or
invalid-signature attempt. Invalid signatures write audit only; verified
deliveries write audit and dedupe atomically.

## Alternatives

| Option | Why Not |
| --- | --- |
| Put the process compiler in `donat-schema` | It would pull connector/runtime ownership into schema or duplicate the server connector compiler. |
| Let `donat-schema` depend on `donat-server` | Creates a crate dependency cycle and reverses the planner/runtime boundary. |
| Hash commands after resolving process revisions | A process revision includes command fingerprints, so this creates a fingerprint cycle. |
| Reuse one introspected catalog or worker pool for all sources | Can validate or mutate the wrong database and makes atomic process behavior require distributed transactions. |
| Key outboxes by the reusable idempotency tuple | Expired-key re-execution would collide with historical effects. |
| Catch a command exception without a savepoint | PostgreSQL leaves the outer transaction aborted, so `on_rejection` cannot commit safely. |
| Store one inbound row per provider event | A duplicate or invalid-signature attempt cannot be audited without overwriting dedupe history or trusting an unverified provider ID. |

## Consequences

Candidate construction gains explicit descriptor and finalization stages, and
V6 gains more source-local audit tables. Deployment tooling must select each
Postgres source and rolling binaries must support the pinned runtime ABI.

In return, the dependency graph is cycle-free, revisions are deterministic,
every command effect identifies one actual execution, command rejection has a
valid PostgreSQL transaction boundary, and ingress preserves both dedupe and
complete delivery history. The design remains one Rust binary with Postgres,
one statement per command, explicit classic roles, no runtime DDL, no admin
surface, and no external I/O inside a journal transaction.
