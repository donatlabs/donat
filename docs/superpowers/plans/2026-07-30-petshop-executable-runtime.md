# Executable Petshop Declarative Runtime Implementation Plan

**Goal:** Make the complete checked-in Petshop metadata executable in one
`donat` Rust binary plus Postgres, then prove every store module through native
conformance tests.

**Source of truth:** The active contract is
`examples/petshop/metadata/{commands,flows,connectors,rules}.yaml` and its
included files. The accepted architecture is recorded in declarative-SaaS ADRs
001, 002, 005-010, and 013-015. This plan implements the product-required slice;
it does not add a second workflow grammar or application-specific Rust.

**TDD rule:** Every task starts with one focused failing test, records the
expected failure, adds the smallest generic implementation, then runs the
focused and enclosing suites. The engine binary is rebuilt before native
conformance. Perform one whole-range review after the implementation is green.

## Fixed inventory

- 65 synchronous Commands.
- 11 durable Processes.
- 5 fixed-origin HTTP connector instances.
- 58 Rules, 10 decision tables, 40 named types, and 8 signal contracts.
- Existing command forms: `select_one`, `select_many`, `aggregate`, `insert`,
  `insert_many`, `update`, `update_many`, `delete`, and `assert`.
- Required additional closed command forms: `project`, `project_many`,
  `fixed_rows`, `decision`, `decision_many`, `allocate_many`, `update_when`,
  `insert_when`, and `assert_when`.
- Process state forms: `command`, `request`, `wait`, `when`, bounded
  `for_each`, `output`, and `fail`.
- No scripts, free SQL, dynamic relation names, arbitrary HTTP requests,
  plugins, subflows, recursion, unbounded loops, runtime DDL, admin role, or
  permission bypass.

## Runtime invariants

- Every Command is one source-local Postgres statement with in-database JSON
  assembly and ordinary explicit-role permissions.
- A Command never performs network I/O. `start_process` and `signal_process`
  append intent in the same statement as domain DML.
- Process state is source-local and pins an immutable compiled revision.
- Activity leases are committed before HTTP; completion is fenced by lease
  generation. Delivery is at least once with a stable provider key.
- Provider-idempotent sends persist the first-attempt database time before
  network I/O and never send after the compiled/provider horizon.
- Signals are accepted only by a matching receptive wait. Timers and signals
  race through one transactional state claim.
- Bounded fan-out has a finite input, stable unique item key, declared
  `max_items` and `max_concurrency`, one command or request body, and ordered
  bounded results.
- REST and MCP reuse the compiled GraphQL contract; there is no generic process
  management endpoint.

---

### Task 1: Finish bounded relational Command IR and SQL

**Plan dependency:** Complete Tasks 5 and 6 of
`2026-07-30-petshop-store-core.md`.

**RED:** `cargo test -p donat-schema --test commands relational_batch`.

**GREEN:** Compile and render `select_many`, `aggregate`, `update_many`, and
`current_column`; enforce relation kind, total order, permission, primary-key,
type, duplicate-key, and exact affected-row gates.

**Verify:**

```bash
cargo test -p donat-schema --test commands
cargo test -p donat-sqlgen --test commands
cargo test --workspace --no-run
```

---

### Task 2: Load the complete active Petshop grammar

**Files:** `crates/metadata/src/{types,loader}.rs`,
`crates/metadata/tests/petshop_contract.rs`.

**RED:** Add a test that loads the real Petshop directory and asserts the fixed
inventory above. It must fail on the first unsupported command or missing
`flows.yaml` section.

**GREEN:** Add closed serde types for the nine additional Command forms, the
seven Process states, typed values, ownership, signals, retry/error routes,
timers, and bounded fan-out. Extend existing connector metadata only for the
fields present in the five active instances. Unknown fields remain rejected.

**Verify:**

```bash
cargo test -p donat-metadata petshop_contract
cargo test -p donat-metadata
```

---

### Task 3: Compile pure, conditional, and decision Command forms

**Files:** `crates/ir/src/lib.rs`,
`crates/schema/src/{commands,plan_mutation,introspection}.rs`,
`crates/schema/tests/commands.rs`.

**RED:** One test per rejected forward reference, wrong cardinality, type
mismatch, unauthorized output, invalid condition, non-finite decision result,
and bound overflow.

**GREEN:** Lower `project`, `project_many`, `fixed_rows`, `decision`,
`decision_many`, `update_when`, `insert_when`, and `assert_when` to typed,
SQL-free IR. Rules and decision tables are resolved at deployment. Conditional
writes are materialized gates, never Rust branching after SQL.

**Verify:**

```bash
cargo test -p donat-schema --test commands petshop
cargo test -p donat-schema --test commands
cargo test -p donat-ir
```

---

### Task 4: Compile deterministic allocation and render all Command forms

**Files:** `crates/ir/src/lib.rs`, `crates/schema/src/commands.rs`,
`crates/sqlgen/src/lib.rs`, command snapshots and live SQL tests.

**RED:** Cover stable grouping/order, exact quantity conservation, duplicate
allocation IDs, maximum-row overflow, false conditional gates, decision lists,
and idempotent replay.

**GREEN:** Implement `allocate_many` as a bounded deterministic relational
operation. Render every new form inside the existing single CTE statement and
assemble lists/objects in Postgres. No row loop or result assembly in Rust.

**Verify:**

```bash
cargo test -p donat-sqlgen --test commands
cargo test -p donat-server command
```

---

### Task 5: Add the complete Petshop domain schema

**Files:** `examples/petshop/migrations/`,
`examples/petshop/metadata/databases/`,
`crates/conformance/tests/petshop.rs`.

**RED:** Add catalog and constraint cases for every relation referenced by the
65 Commands.

**GREEN:** Add ordered refinery migrations and tracked-table permissions for
pricing, multi-location inventory, payment lifecycle, returns, subscriptions,
B2B, marketplace, booking, prescription, and operations. Money is integer
minor units plus currency; ownership remains explicit; Petshop remains
single-tenant.

**Verify:**

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop
```

---

### Task 6: Compile immutable Process revisions

**Files:** server-owned process compiler modules, value-contract adapters,
schema command-effect finalization, compiler tests.

**RED:** Compile the 11 real Process definitions and reject unknown states,
cycles, forward state values, type/cardinality errors, cross-source commands,
dynamic roles, missing permissions, missing connector operations, invalid
signals, unbounded fan-out, and provider-horizon overflow.

**GREEN:** Build a source-local immutable revision containing canonical
definition, command/rule/connector fingerprints, owner/input/output contracts,
signal contracts, runtime ABI, and derived activity horizons. Finalize command
effects only against that catalog.

**Verify:**

```bash
cargo test -p donat-server process_compiler
cargo test -p donat-schema command_effect
cargo test --workspace --no-run
```

---

### Task 7: Add process journals and atomic Command intents

**Files:** root `migrations/`, `crates/server/src/migrate.rs`,
`crates/sqlgen/src/lib.rs`, migration and command conformance tests.

**RED:** Prove start/signal intent rolls back with rejected domain DML, exact
replay produces no duplicate intent, expired re-execution gets a new
invocation generation, and source/role identity cannot cross.

**GREEN:** Add source-qualified revisions, instances, state history, jobs,
activity attempts, timers, start/signal intents, inbound dedupe/delivery audit,
and capacity reservations. Add invocation generation UUIDs and render resolved
start/signal outbox CTEs in the same Command statement.

**Verify:**

```bash
cargo test -p donat-server --test migrate
cargo test -p donat-sqlgen --test commands process_effect
cargo test -p donat-conformance --test commands
```

---

### Task 8: Execute command/when/output/fail states and public run fields

**Files:** `crates/server/src/processes/`, candidate state, GraphQL planner and
executor, conformance fixtures.

**RED:** Start the real checkout Process, enforce owner visibility, execute a
command transition, route Rules/decisions, and return terminal output or the
same running handle. Disconnect must not cancel the instance.

**GREEN:** Add a bounded per-source worker. Claim and commit one state, execute
commands with ordinary compiled roles and a savepoint, fence transitions, and
expose `start_*`, bounded `execute_*`, and owner-scoped status through the
compiled schema.

**Verify:**

```bash
cargo test -p donat-server process_runtime
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_process checkout
```

---

### Task 9: Execute fixed-origin HTTP activities

**Files:** connector metadata/compiler adapters, existing server HTTP
connector, process activity worker, deterministic conformance recorder.

**RED:** Cover exact method/path/body/header, typed response normalization,
timeouts, `500 -> retry -> success`, disconnect, stable idempotency key,
redaction, capacity, same-resource serialization, and ambiguous-outcome lookup.

**GREEN:** Reuse the existing fixed-origin HTTP executor. Commit lease,
capacity, attempt, first-provider-attempt, and deadlines before I/O; perform
I/O outside transactions; fence completion; apply declared retry/error routes;
refuse sends after either persisted horizon.

**Verify:**

```bash
cargo test -p donat-server connectors_http
cargo test -p donat-server process_activity
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop_process payment
```

---

### Task 10: Execute waits, timers, signals, webhooks, and bounded fan-out

**Files:** process worker/ingress modules, connector webhook route, command
signal consumer, conformance recorder and fixtures.

**RED:** Cover signal-vs-timeout race, duplicate/early/late/unmatched/
ambiguous signals, invalid signatures, restart-safe timers, bounded ordered
fan-out, concurrency cap, per-item stable keys, partial collection, and worker
takeover.

**GREEN:** Persist receptive waits before acknowledgement, atomically audit and
dedupe verified inbound deliveries, consume command signals idempotently, use
database-clock timers, and expand only the declared finite fan-out items.

**Verify:**

```bash
cargo test -p donat-server process_signal
cargo test -p donat-server process_timer
cargo test -p donat-server process_fanout
cargo test -p donat-conformance --test connectors
```

---

### Task 11: Make all Petshop modules executable

**Files:** module-scoped native conformance fixtures/tests and any generic
runtime correction revealed by them.

Add failing-to-green suites for:

1. checkout, tax, authorization, decline, cancellation, and reconciliation;
2. allocation, partial shipment, label lookup, capture, and delivery;
3. return approval, receipt, inspection, refund, exchange, and rejection;
4. subscription renewal, dunning, pause, and cancellation;
5. B2B automatic/approver/finance/escalation/rejection routes;
6. vendor split, payout fan-out, lookup, and dispute;
7. grooming confirm, timeout, reschedule, cancel, and no-show;
8. prescription approve, reject, expire, and privacy;
9. payment/fraud/notification operational recovery.

Every suite includes exact replay, changed-input rejection, permission
isolation, a concurrency race, and one crash/retry case relevant to the module.

**Verify:**

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test petshop
cargo test -p donat-conformance --test petshop_process
```

---

### Task 12: Full verification and whole-range review

Run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p donat-server --bin donat
cargo test -p donat-conformance
git diff --check
```

Review the complete implementation range once for the no-admin boundary,
single-statement Commands, source locality, lease fencing, stable keys,
bounded values, redaction, exact error shapes, and absence of public process
management or dynamic connector execution. Do not push.
