# Durable Processes Implementation Plan

> **For Codex:** Execute every checkbox in order with RED/GREEN evidence and a
> judge ACCEPT after each commit. The last integration slice completes the
> `start_process` command effect left intentionally inactive by the commands
> core plan.

**Goal:** Execute declarative, long-running SaaS workflows inside the one
Donat Rust binary with durable state, pinned definition revisions, timers,
signed inbound events, explicit-role commands, connector activities, and
at-least-once delivery that is safe across crashes and multiple engine
instances.

**Architecture:** `processes.yaml` declares finite state-machine definitions.
At deploy time, `migrate --metadata-dir` stores a canonical definition revision
containing the process, referenced rules, connector operation versions, and
non-secret connector-config fingerprints. The revision also pins the process
runtime ABI and command fingerprints. Runtime workers claim a single journal
row with `FOR UPDATE SKIP LOCKED`, first reserve a Postgres-coordinated
operation permit, commit an expiring lease, then do external I/O outside the
transaction. A completion is recorded only when its lease token still owns the
job; state transitions append events/logs and create the next
command/activity/timer work atomically. Existing process instances read their
pinned revision, so deployment never rewrites an in-flight process.

**Tech stack:** Rust, Tokio, Axum, Postgres 16, serde YAML/JSON, `reqwest`,
native conformance harness.

**Prerequisites:** Complete the Rules, Commands core, and In-Binary Connectors
plans. The process worker consumes their public APIs; it never reimplements
CEL-like rules, command SQL, or HTTP/Stripe protocol handling.

**Specification:**
[`specs/005-durable-processes.md`](../../../specs/005-durable-processes.md)

## Required interfaces and journal ownership

```rust
// metadata/types.rs
pub struct ProcessDefinition {
    pub name: String,
    pub source: Option<String>,
    pub input: BTreeMap<String, ProcessInputType>,
    pub initial_state: String,
    pub states: BTreeMap<String, ProcessState>,
}

// server/processes.rs
pub async fn reconcile(
    database_url: &str,
    metadata: &Metadata,
    connectors: &ConnectorRegistry,
    rules: &RuleCatalog,
) -> anyhow::Result<()>;

pub fn spawn(state: SharedState);

// This boundary is callable by a command effect, not by HTTP clients.
pub async fn enqueue_start(
    tx: &tokio_postgres::Transaction<'_>,
    request: ProcessStartRequest,
) -> Result<(), ProcessError>;
```

The internal `donat` schema owns these tables:

| Table | Durable purpose |
|---|---|
| `process_definition_versions` | canonical revision and retirement state |
| `process_instances` | immutable input, mutable state, pinned revision |
| `process_events` | ordered audit/event history |
| `process_activity_jobs` | leased connector activity attempts/outcomes |
| `process_transition_logs` | exact guard/action transition audit |
| `process_inbound_events` | verified provider event dedupe and correlation |
| `process_start_requests` | idempotent command-to-process outbox request |
| `process_signal_requests` | idempotent declarative command-to-process signal outbox request |
| `process_capacity_reservations` | global connector-operation concurrency and rate-limit permits |

No table stores a resolved secret. No request handler may create arbitrary
instances, change a definition, or set an instance role.

### Task 1: Add process YAML, static graph validation, and metadata writer

**Files:**
- Modify: `crates/metadata/src/types.rs`, `crates/metadata/src/loader.rs`,
  `crates/server/src/migrate.rs`, `crates/server/src/main.rs`,
  `crates/conformance/src/lib.rs`
- Add: `crates/server/src/processes/definition.rs`,
  `crates/server/tests/process_definition.rs`
- Test: `crates/metadata/tests/types_serde.rs`,
  `crates/metadata/tests/load_fixture.rs`
- Add fixtures: `crates/metadata/tests/fixtures/processes/`

- [ ] Add metadata tests for an absent `processes.yaml`, quoted include,
  duplicate names/states, unknown transition target, no initial state, missing
  source, non-terminal state with no transition, nondeterministic signal
  correlation, unknown connector/rule/command, missing fixed `run_as_role`,
  missing required `on_rejection` command transition, missing ordered
  `on_error` fallback, unsupported error class, invalid schedule-to-start or
  start-to-close timeout, missing capacity declaration, and cancellation
  signal without an `on_cancel` state.
- [ ] RED: run `cargo test -p donat-metadata processes` and
  `cargo test -p donat-server --test process_definition`. Expected: no process
  metadata section or compiler exists.
- [ ] Define full serde types for inputs, states, activity/timer/signal
  transitions, rule guards, explicit command calls, retry policy, typed
  `on_error` routes, separate activity timeouts, cancellation, and
  `on_rejection`. Load `processes.yaml` with `load_section`, update all
  `Metadata` literals, and write the file from the conformance builder when
  non-empty.
- [ ] Compile a process definition catalog during `check_consistency` and
  serving candidate construction. Verify every referenced command has a
  command permission for its `run_as_role` and that each command's underlying
  table permissions are valid through its existing compiler. For every
  command-provided signal, validate exact payload and correlation typing.
- [ ] Require an explicit `source` that resolves to Postgres. Reject all
  non-Postgres process definitions in this release. Validate a total,
  deterministic transition graph before any journal row can be created.
- [ ] GREEN: run `cargo test -p donat-metadata`,
  `cargo test -p donat-server --test process_definition`, and
  `cargo test --workspace --no-run`.
- [ ] Commit the process-metadata/compiler slice and obtain judge ACCEPT.

### Task 2: Create migrations and reconcile immutable definition revisions

**Files:**
- Add: `migrations/V4__donat_processes.sql`,
  `crates/server/src/processes/reconcile.rs`,
  `crates/server/tests/process_reconcile.rs`
- Modify: `crates/server/src/main.rs`, `crates/server/src/migrate.rs`,
  `crates/server/src/processes/definition.rs`

- [ ] Add migration tests asserting columns, foreign keys, uniqueness, and
  indexes for every journal table. Cover `(definition_name, revision)`,
  instance event ordering, logical activity identity, separate
  schedule-to-start/start-to-close deadlines, activity lease ownership,
  `(connector_instance, provider_event_id)` dedupe, command-outbox idempotency,
  and operation-scoped capacity reservations.
- [ ] Add reconciliation tests for first deploy, no-op same deploy, changed
  canonical process creating the next revision, retired process rejecting new
  starts, an active instance retaining its old revision, a module/operation ABI
  change fencing an old worker, and deployment rejection when a non-terminal
  revision references a removed or incompatible command/catalog dependency or
  a changed wait/cancellation signal contract.
- [ ] RED: run `cargo test -p donat-server --test process_reconcile`.
  Expected: migration/reconciliation modules do not exist.
- [ ] Create `V4__donat_processes.sql`. It must create journal tables and
  indexes only; no trigger starts a process and no DDL appears in the serving
  path. If the command plan already created `process_start_requests`, make the
  migration tolerant of that exact schema and assert it rather than silently
  changing it.
- [ ] Canonicalize each compiled definition to stable JSON, include referenced
  rule text/version, command-definition fingerprints, connector module/runtime
  ABI and operation versions, endpoint and credential identities, and a hash
  of non-secret connector configuration. Insert a revision only when this
  canonical value changes; mark omitted definitions retired without deleting
  historical revisions.
- [ ] Call `processes::reconcile` only in the `migrate --metadata-dir` branch,
  after event-trigger reconciliation. `validate` checks definitions but writes
  nothing; `serve` performs neither reconciliation nor DDL.
- [ ] GREEN: run `cargo test -p donat-server --test process_reconcile` and
  `cargo build -p donat-server --bin donat`.
- [ ] Commit the migration/revision slice and obtain judge ACCEPT.

### Task 3: Persist and consume idempotent starts with a pinned revision

**Files:**
- Add: `crates/server/src/processes/start.rs`,
  `crates/server/tests/process_start.rs`
- Modify: `crates/server/src/processes/mod.rs`,
  `crates/server/src/processes/reconcile.rs`

- [ ] Add tests for a unique start request creating one instance, duplicate
  delivery producing no second instance, missing/retired definition failure,
  invalid typed input failure, initial-state event ordering, and revision pin
  selection after a new deployment. Include a command request written under
  revision A but consumed after revision B deploys; it must instantiate A.
- [ ] RED: run `cargo test -p donat-server --test process_start`.
  Expected: an outbox request cannot become an instance.
- [ ] Implement an idempotent start-request consumer inside one short database
  transaction: lock one pending request with `FOR UPDATE SKIP LOCKED`, resolve
  its exact stored definition revision, validate the immutable input against
  that revision, insert the instance/event/transition log, and mark the
  request consumed atomically.
- [ ] Use the request's stable key plus process name as its unique identity.
  Preserve request and canonical input fingerprints for audit; never derive an
  identity from the current clock or worker ID.
- [ ] Add bounded worker scheduling that repeatedly claims pending starts but
  immediately releases the transaction after persistence. A crash before
  commit leaves the request pending; a crash after commit cannot create a
  duplicate instance.
- [ ] GREEN: run `cargo test -p donat-server --test process_start` and the
  process reconciliation suite.
- [ ] Commit the durable-start slice and obtain judge ACCEPT.

### Task 4: Add command transitions with fixed roles and rule guards

**Files:**
- Add: `crates/server/src/processes/transition.rs`,
  `crates/server/tests/process_transition.rs`
- Modify: `crates/server/src/processes/mod.rs`, `crates/schema/src/commands.rs`,
  `crates/server/src/gql.rs` only to extract a reusable command executor

- [ ] Add tests that a process command uses only its metadata `run_as_role`,
  cannot use a GraphQL/session-provided role, records a false guard as
  `guard_false` with no connector job, executes `on_rejection` exactly once,
  and produces the same command result/error envelope as GraphQL execution.
- [ ] RED: run `cargo test -p donat-server --test process_transition`.
  Expected: no process transition executor exists.
- [ ] Extract the command planning/execution path needed by both GraphQL and
  internal workers into a narrow internal API accepting only a compiled command,
  immutable input, and explicit `Session { role, vars, backend_request: false }`.
  It must still execute the command's one generated Postgres statement and use
  standard GraphQL error translation.
- [ ] Evaluate guards through `donat-rules` against closed `input`, `state`,
  prior command result, and verified event context. A false guard consumes the
  triggering event, writes a transition log, and schedules no activity/retry.
- [ ] In one transaction, append transition/result events, change state,
  enqueue next work, or execute the declared `on_rejection` branch. Never use
  a generic script, raw SQL, or callable arbitrary GraphQL operation.
- [ ] GREEN: run `cargo test -p donat-server --test process_transition`,
  `cargo test -p donat-schema --test commands`, and
  `cargo test -p donat-sqlgen --test commands`.
- [ ] Commit the command-transition slice and obtain judge ACCEPT.

### Task 5: Claim connector activities with leases and no transaction over HTTP

**Files:**
- Add: `crates/server/src/processes/activity.rs`,
  `crates/server/tests/process_activity.rs`
- Modify: `crates/server/src/processes/mod.rs`,
  `crates/server/src/connectors/mod.rs`

- [ ] Add tests for claim by one worker, concurrent second worker skip,
  expired-lease reclamation, stale completion ignored/audited, retryable
  failure backoff, typed authentication/validation/permanent error routes,
  schedule-to-start expiry without a provider call, start-to-close stale
  completion, global operation capacity and configured same-resource
  serialization across two engine processes,
  deterministic jitter, ambiguous write outcome retaining the same provider
  idempotency key, and no open SQL transaction while a controlled HTTP endpoint
  blocks.
- [ ] RED: run `cargo test -p donat-server --test process_activity`.
  Expected: no lease/worker implementation exists.
- [ ] Claim one due activity with `FOR UPDATE SKIP LOCKED`; reject an expired
  schedule-to-start deadline before I/O. In the same short transaction reserve
  a connector-operation capacity permit and, when declared, a canonical
  serialization-key permit; generate a lease token, increment attempt count,
  persist `start_to_close_deadline` and a lease that outlives it by a fixed
  safety margin using the database clock, then commit before calling
  `ConnectorRegistry::execute`. If either permit is unavailable, leave the job
  scheduled without holding a transaction.
- [ ] Pass a stable key derived once from the logical activity ID—not its
  attempt—to the connector module. On completion, begin a new short
  transaction that updates the job only where its lease token/current state
  still match, appends an event, and schedules the next transition.
- [ ] Use retry policy from the pinned process revision. Exponential delay is
  calculated with deterministic full jitter from logical activity ID and
  persisted attempt count; do not use unrecorded random state. Retry-After may
  postpone but not accelerate. Exhausted retries and all non-retried typed
  connector failures follow the declared ordered `on_error` route. Phase 1
  implements no heartbeat extension for a long HTTP call.
- [ ] GREEN: run `cargo test -p donat-server --test process_activity` and the
  connector HTTP/Stripe test suites. Verify existing cron/event loops were not
  reused if they keep a database transaction across HTTP.
- [ ] Commit the lease/activity slice and obtain judge ACCEPT.

### Task 6: Process timers and verified inbound events

**Files:**
- Add: `crates/server/src/processes/timer.rs`,
  `crates/server/src/processes/inbound.rs`,
  `crates/server/tests/process_timer.rs`,
  `crates/server/tests/process_inbound.rs`
- Modify: `crates/server/src/connector_webhook.rs`,
  `crates/server/src/processes/mod.rs`, `crates/server/src/main.rs`

- [ ] Add timer tests for database-clock due selection, one claim across two
  workers, and a crash/retry without duplicate transition. Add inbound tests
  for signature verification before parse, provider-event dedupe, correlation
  to exactly one instance, accepted/duplicate/unmatched/ambiguous/guard-false
  and unexpected-state audit outcomes, and retrying a post-verification database failure without
  acknowledging HTTP. Add a command-signal test proving typed cancellation
  takes its declared on_cancel transition, cancels only unclaimed work, and
  makes an in-flight completion audit-only.
- [ ] RED: run `cargo test -p donat-server --test process_timer` and
  `cargo test -p donat-server --test process_inbound`. Expected: the connector
  route cannot durably accept events and no timer worker exists.
- [ ] Implement timers as journal rows due according to `now()` in Postgres;
  claim and transition them through the same short-transaction pattern as
  activities. Do not use a local timer wheel as source of truth.
- [ ] Replace the connector route's temporary verified-`503` branch with one
  transaction that writes/deduplicates the verified raw event identity,
  resolves correlation using pinned revision metadata, appends the instance
  event, and enqueues the next transition. Return a success acknowledgement
  only after commit; record one explicit audit outcome for accepted, duplicate,
  unmatched, ambiguous, guard-false, and unexpected-state verification paths.
- [ ] Preserve raw body only under an explicit bounded retention policy and
  redact sensitive fields from application logs. Store signature status and
  provider ID for audit without storing webhook secrets.
- [ ] Add the internal command-signal consumer. It accepts only the typed,
  idempotent `signal_process` outbox row from a completed declarative command;
  it never adds an operator CLI or generic HTTP recovery endpoint.
- [ ] GREEN: run the focused timer/inbound tests, connector webhook tests, and
  `cargo test -p donat-server`.
- [ ] Commit the timer/inbound slice and obtain judge ACCEPT.

### Task 7: Activate process start and domain signals atomically

**Files:**
- Modify: `crates/schema/src/commands.rs`, `crates/ir/src/lib.rs`,
  `crates/sqlgen/src/lib.rs`, `crates/server/src/processes/start.rs`,
  `crates/server/tests/process_start.rs`, `crates/sqlgen/tests/commands.rs`

- [ ] Add a SQL snapshot and execution test where a successful idempotent
  command writes its canonical result and exactly one process-start or typed
  process-signal outbox row; replay returns the stored command result and
  writes no additional row; same-key/different-input writes neither a result
  nor outbox row.
- [ ] Add a compiler test rejecting an effect whose referenced process has no
  compiled definition, whose start input or signal payload/correlation cannot
  be bound exactly, whose signal is not declared by the target process, or
  whose command lacks mandatory idempotency configuration.
- [ ] RED: run `cargo test -p donat-sqlgen --test commands start_process` and
  `cargo test -p donat-server --test process_start command_effect`.
  Expected: command effects are intentionally unavailable until this slice.
- [ ] Complete static cross-validation now that `ProcessDefinition` exists.
  Lower both effects as inserts into narrowly typed `donat.process_start_requests`
  and `donat.process_signal_requests` rows in the same CTE statement as the
  command result and command invocation. A start row stores the compiled active
  process revision; each unique key includes the command invocation identity and
  effect position.
- [ ] Make the process start and signal workers consume this outbox exactly as
  Task 3 and Task 6; never start, recover, or cancel a process directly from a
  GraphQL request, SQL function, or operator endpoint.
- [ ] GREEN: run focused command SQL/process tests, rebuild the server, and
  run `cargo test -p donat-conformance --test commands` after adding the
  previously deferred `start_process` fixtures.
- [ ] Commit the cross-plan integration slice and obtain judge ACCEPT.

### Task 8: Add read-only operational inspection and history verification

**Files:**
- Add: `crates/server/src/processes/inspect.rs`,
  `crates/server/src/processes/verify_history.rs`,
  `crates/server/tests/process_inspect.rs`
- Modify: `crates/server/src/main.rs`, `crates/server/src/processes/mod.rs`

- [ ] Add tests for a redacted instance timeline, a queued/running activity
  view with lease and next-retry metadata, and a history-verification mismatch
  produced by a tampered transition state hash. Assert that neither path calls a
  connector, command executor, or mutation query.
- [ ] RED: run `cargo test -p donat-server --test process_inspect`. Expected:
  no operational inspection or verifier exists.
- [ ] Add read-only `donat process inspect --source <name> --instance <uuid>`
  and `donat process verify-history --source <name> --instance <uuid>`
  subcommands. inspect emits redacted journal-derived JSON; verify-history
  deterministically re-applies stored events and stored results to the pinned
  definition and exits non-zero on a before/after-state-hash mismatch.
- [ ] The subcommands must issue only read queries, accept no role, arbitrary
  SQL, or mutation option, and publish no GraphQL, REST, MCP, or HTTP route.
  They are diagnosis only—not retry, replay, repair, cancel, or process
  administration.
- [ ] GREEN: run `cargo test -p donat-server --test process_inspect` and an
  integration test against a process with a completed activity and a redacted
  failure. Inspect the SQL logs to prove all statements are read-only.
- [ ] Commit the operational-observability slice and obtain judge ACCEPT.

### Task 9: Prove crash, concurrency, and upgrade semantics in conformance

**Files:**
- Add: `crates/conformance/tests/processes.rs`,
  `crates/conformance/fixtures/processes/`
- Modify: `crates/conformance/src/lib.rs`

- [ ] Add native suites for happy-path Checkout, false guard, command
  rejection, typed retryable/permanent/ambiguous connector failures, duplicate
  signed webhook, every webhook audit outcome, timer, command-to-process start
  and signal idempotency, domain cancellation, two engine instances, global
  operation capacity, crash after claim before HTTP, both activity timeout
  classes, stale completion, definition upgrade with old instance pinning,
  ABI-fenced worker, retired definition rejection, incompatible active
  dependency rejection, read-only history verification, and missing explicit
  role.
- [ ] RED: run
  `cargo build -p donat-server --bin donat && cargo test -p donat-conformance --test processes -- --test-threads=1`.
  Expected: each fixture fails until its runtime contract is implemented;
  preserve exact status/body expectations.
- [ ] Extend the harness only with controlled local connector endpoints and
  explicit test clocks/leases where already supported. Do not introduce an
  in-memory process backend or an admin endpoint for test setup.
- [ ] GREEN: rebuild and run the focused suite with one test thread for
  lifecycle determinism, then run it with `--test-threads=2` to exercise
  independent database isolation and two-worker cases.
- [ ] Commit conformance corrections separately and obtain judge ACCEPT.

### Task 10: Final full-system acceptance

**Files:**
- Modify only for a failing regression with a new test proving it.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace --exclude donat-conformance`.
- [ ] Rebuild with `cargo build -p donat-server --bin donat`.
- [ ] Start Postgres with
  `docker compose -f docker-compose.conformance.yml up -d --wait`.
- [ ] Run `cargo test -p donat-conformance --test rules`,
  `cargo test -p donat-conformance --test commands`,
  `cargo test -p donat-conformance --test connectors`, and
  `cargo test -p donat-conformance --test processes -- --test-threads=1`.
- [ ] Run `make conformance` and review all snapshot changes using
  `cargo insta review`.
- [ ] Verify the operational invariants from a clean database: migrate with
  metadata, serve without any DDL, active revision pinned across a metadata
  upgrade, no role bypass, no transaction held over HTTP, and no resolved
  secret in output/log capture.
- [ ] Obtain final judge ACCEPT for the complete declarative SaaS runtime.
