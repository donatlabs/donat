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

Use a server-orchestrated two-stage immutable candidate build. The unpublished
lower `donat-value-contract` crate is the single owner of the closed
`ValueType`/`TypeRef` language, `TypedValue`, bounded canonical constructors,
deterministic object order, canonical size accounting, and inline-byte
representation. It is `no_std + alloc`, forbids unsafe code, has empty default
features and no `std` feature, build script, or third-party runtime
dependency, and exposes no `serde_json::Value`. `donat-ir` depends on and
re-exports those types without a second representation; metadata/Rule
normalization adapters may live in IR.

The type-reference identifier grammar has no implicit reserved-prefix
exception, so `__bad` remains valid. Decimal values use a private
`CanonicalDecimal` constructor and an already minimal fixed-point spelling:
no exponent, leading plus, noncanonical integer leading zero, negative zero,
trailing point, or trailing fractional zero. Canonical sizing counts that
checked spelling, not an arbitrary `String`.

Inline bytes are sized against their exact future RFC 8785 representation:
the byte string is RFC 4648 base64url without padding, and the root object is
`{"$binary":"...","media_type":"..."}` or
`{"$binary":"...","file_name":"...","media_type":"..."}` in that JCS order.
Production and independent test-oracle helpers use checked arithmetic for
base64 expansion and JCS string escaping; the lower crate does not acquire a
JSON dependency or external adapter.

`donat-schema` compiles commands to public pre-process descriptors whose
canonical fingerprints include raw effect declarations but no resolved process
revision. The accepted connector-factory plan owns exact ABI IDs and the
catalog-owned `OperationSpec`, `TriggerSpec`, effects, bounds, and generated
entries. Its server `ConnectorRegistry` is the one catalog consumed by every
process compiler and runtime stage. The Process Task-2 ledger records the
reviewed ABI/catalog/executor/registry/webhook commits; it does not create
server-local string descriptors or a second effect model. Inventory-only
entries are not executable, and Stripe mutation remains inventory-only unless
its separate immutable-evidence and executable-migration gates both pass. An
independently accepted webhook trigger grants no mutation capability.

The server-owned process compiler consumes that registry and Rules, derives
revisions, and exposes them through a schema-owned neutral effect-contract
interface. Schema then finalizes command effects and compiles the serving
schema. Each `Engine` snapshot retains both the pre-process
`CompiledCommandCatalog` and the corresponding `FinalizedCommandCatalog`, the
metadata process catalog, and a hash-verified `DeployedProcessCatalog`
containing the declared active and every non-terminal live-retired revision.
Each `ProcessRuntime` receives both command catalogs from that same snapshot;
process command execution resolves only `FinalizedCompiledCommand`. No
compiler is duplicated and schema never depends on server. Persisted Rule
dependencies retain their source definitions and named-type closure so a
fresh binary can recompile and verify an old revision rather than interpreting
it through current metadata.

The neutral effect catalog is built by the server-owned free function
`build_process_effect_contract_catalog(&CompiledProcessCatalog) ->
Result<ProcessEffectContractCatalog, PlanError>`. It constructs the
schema-owned value through its public fields or checked schema constructors.
It is not an inherent implementation on the foreign schema type, and it adds
no schema-to-server dependency.

Processes are strictly source-local. Start, transition, start-effect, and
signal-effect commands resolve only in the process's Postgres source. A
connector instance used by processes is bound to one source, and workers,
database clocks, capacity reservations, webhook routing, and journal pools are
created per source. Deployment explicitly selects one real source with
`migrate --metadata-dir <dir> --source <name>` or
`validate --metadata-dir <dir> --source <name>`; omission is valid only for one
unambiguous Postgres source. Reconciliation changes only that source, while
serve reads and validates every real source without issuing DDL.
Metadata-free `migrate --migrations-dir <dir>` remains a separate
refinery-only mode using an explicit database URL; it performs no source
selection or metadata reconciliation.

Every process journal key, foreign key, semantic key, ingress row, and query
is qualified by metadata `source_name`, including when two source names share
one physical database. Each persisted connector dependency also records its
bound source. Current and live-retired dependency closures are checked
together, so a connector cannot rebind to another source and split capacity
coordination while old work remains.

V6 gives each completed command execution generation a durable
`invocation_id uuid`. Exact replay preserves it; expired-key re-execution gets
a new UUID. The V6 migration and compatible one-statement writer are one
implementation task and commit: first/expired successful generations
explicitly supply a fresh database UUID, replay selects the stored UUID, and
the full command/conformance regressions pass before the task is green. The
next task exposes that already-correct identity through the typed internal
result decoder rather than repairing the writer.

Every process-effect outbox copies the generation UUID and is unique by
invocation and effect position, with no retention-coupling foreign key to the
command journal. A process command runs its finalized command and effects in
one statement inside a savepoint owned by the outer process transition.
Applied domain DML, generation, and start/signal outboxes commit together.
Only the established valid `P0D01` `donat.graphql-error.v1` rejection is
rolled back to that savepoint and turned into one committed `on_rejection`
transition; every command DML/journal/outbox write rolls back, and every other
database error aborts the outer transaction.

`crates/server/src/commands.rs` owns the shared strict decoder
`decode_command_business_rejection(&tokio_postgres::Error) ->
Option<CommandBusinessRejection>`. It succeeds only for SQLSTATE `P0D01` whose
primary message is an object with exactly four string fields: `kind`, `code`,
`path`, and `message`; `kind` must equal `donat.graphql-error.v1`, `code` must
be non-empty, and `path` must begin with `$`. The GraphQL renderer and process
savepoint consume that typed result. Permission SQLSTATE `23514`, malformed
reserved payloads, and all other errors return `None`.

V6 owns the `donat.check_violation(text)` helper currently installed from the
serve candidate path. Serve replaces creation with a read-only compatibility
check and can start under a principal that lacks schema-create privileges.
Activity admission serializes claim decisions by locking one durable
source/connector/operation token-bucket row, then applies exact token refill,
max-in-flight, and serialization checks before inserting a reservation.

Executable connector effects are closed to headerless `ReadOnly` and
`ProviderIdempotent`. The latter records exactly one fixed header/body binding,
provider scope, conservative minimum retention, and positive clock margin for
every side-effecting compiled step. Process compilation calculates and pins a
complete checked `maximum_send_horizon_ms` for each such step, including
schedule/capacity/rate/serialization bounds, start-to-close through lease
expiry, one terminal takeover grace for every possible attempt, and every retry
backoff/jitter bound. The attempt's start-to-close deadline is non-renewing:
lease takeover changes lease generation but not the configured attempt
ordinal or deadline. The terminal grace is present even for
`max_attempts = 1`. Compilation accepts equality with
`minimum_retention_ms - clock_safety_margin_ms` and rejects missing,
unbounded, overflowed, or one-millisecond-over policies. The source-local
activity journal commits each step's database-clock
`first_provider_attempt_at` before its first send and persists both the
compiled maximum-send deadline and provider usable-window deadline. Later
sends reuse the fixed key. Equality does not by itself expire either bound;
both comparisons are strict and both bounds are still evaluated. After both
bounds, the usable-provider-window check runs first and returns
`connector_idempotency_window_exhausted`; between unequal deadlines, only the
compiled horizon has expired and the typed timeout route wins. Both refusals
happen before network I/O.

Commands contain no connector/provider business logic. They may commit only
closed process start/signal intent alongside their domain statement.
Provider-specific execution is reached only by a durable activity after its
job, lease, and capacity intent commit. Connector pagination is bounded
transport inside that activity; it is not a logical node, workflow branch,
retry/timer, database write, or item stream. Process metadata has no
If/Switch/Merge/Code/Wait nodes, loops, item/paired-item model, batching as
workflow logic, or subworkflows.

Inbound persistence is split. `process_inbound_events` is only the verified
provider-event dedupe ledger.
`process_inbound_deliveries` is append-only audit for every accepted,
duplicate, unmatched, ambiguous, guard-false, unexpected-state, or
invalid-signature attempt. Invalid signatures write audit only; verified
deliveries write audit and dedupe atomically. An accepted delivery stores
source-qualified instance and event foreign keys in that same transaction so
instance inspection never infers history from redacted payloads.

Raw connector verification keeps its exact empty `404` unknown/no-verifier,
`413` oversized-body, and `400` verification-rejection responses; successful
verification remains empty `503` until durable ingress exists. Thereafter
every committed `accepted`, `duplicate`, `unmatched`, `ambiguous`,
`guard_false`, or `unexpected_state` verified outcome returns empty `204`.
A post-verification persistence/transition database failure returns empty
`503`. No verified outcome is acknowledged before its source-local
audit/dedupe/transition transaction commits.

Stopping new starts is explicit metadata lifecycle, not omission.
`lifecycle: retired` keeps the definition and dependencies resolvable while a
materialized command effect gate rejects a new start before domain DML.
Omitting a process with a non-terminal instance is a deployment error.

## Alternatives

| Option | Why Not |
| --- | --- |
| Put the process compiler in `donat-schema` | It would pull connector/runtime ownership into schema or duplicate the server connector compiler. |
| Define shared values in `donat-ir` | Connector ABI/catalog crates would either depend upward on IR or create a second value representation; the lower `no_std + alloc` owner keeps the graph acyclic. |
| Let `donat-schema` depend on `donat-server` | Creates a crate dependency cycle and reverses the planner/runtime boundary. |
| Hash commands after resolving process revisions | A process revision includes command fingerprints, so this creates a fingerprint cycle. |
| Reuse one introspected catalog or worker pool for all sources | Can validate or mutate the wrong database and makes atomic process behavior require distributed transactions. |
| Key process tables only by process name or UUID | Two metadata sources may share one physical database, so unqualified rows and predicates can collide. |
| Key outboxes by the reusable idempotency tuple | Expired-key re-execution would collide with historical effects. |
| Catch a command exception without a savepoint | PostgreSQL leaves the outer transaction aborted, so `on_rejection` cannot commit safely. |
| Store one inbound row per provider event | A duplicate or invalid-signature attempt cannot be audited without overwriting dedupe history or trusting an unverified provider ID. |
| Remove retired metadata and keep only hashes | A restarted binary cannot execute the pinned definition or recompile its Rule guards deterministically. |
| Admit a non-idempotent provider side effect with one attempt | Worker loss can leave an ambiguous external outcome even when configured attempts equal one. |
| Put provider calls or workflow nodes in commands/connectors | It bypasses committed durable intent and recreates an unbounded workflow runtime rather than the closed Process grammar. |

## Consequences

Candidate construction gains explicit descriptor, finalization, and deployed
revision-loading stages, and V6 gains more source-local audit/coordination
tables. Metadata-aware deployment tooling must select each Postgres source and
rolling binaries must support the pinned runtime ABI.

In return, the dependency graph is cycle-free, revisions are deterministic,
every command effect identifies one actual execution, command rejection has a
valid PostgreSQL transaction boundary, and ingress preserves both dedupe and
complete delivery history. The design remains one Rust binary with Postgres,
one statement per command, explicit classic roles, no runtime DDL, no admin
surface, and no external I/O inside a journal transaction.
