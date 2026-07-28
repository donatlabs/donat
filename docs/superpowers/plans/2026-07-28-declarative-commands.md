# Declarative Domain Commands Implementation Plan

> **For Codex:** Execute every checkbox in order with RED/GREEN evidence and a
> judge ACCEPT after each commit.

**Goal:** Let a SaaS author declare named, role-authorized business commands
in `commands.yaml`; expose each command as one GraphQL mutation field; execute
all of its data steps as one Postgres statement; and replay an identical
canonical result for the same idempotency key.

**Architecture:** Metadata defines a bounded command step graph. Schema
planning validates the GraphQL input, fixed permission role, rule bindings,
and table relation kind, then emits a dedicated command IR. SQL generation
uses CTEs for `select_one`, `insert`, `insert_many`, `update`, `delete`, and
`assert`; it materializes a JSON result once and atomically stores/replays it
in `donat.command_invocations`. The generic GraphQL mutation executor remains
the transaction owner and translates structured Postgres errors into the exact
Donat error envelope. The durable `start_process` effect is activated in Task
7 of the Processes plan, after its outbox and pinned definition revision exist;
this plan defines and validates its bounded shape but does not fake an
in-memory or direct-start implementation. The same later integration activates
the typed `signal_process` effect; it is the only way a command can recover or
cancel a durable process.

A command or process may accept a decision value only into an exactly typed data destination
or map a declared enum at deploy time to fixed action/state targets; they
never bind a generic output to role/permission/command/connector identifiers.

**Tech stack:** Rust, Postgres 16, GraphQL parser, serde YAML, `insta`, native
conformance harness.

**Prerequisite:** Complete
[`Declarative Rules`](2026-07-28-declarative-rules.md) first.

**Specification:**
[`specs/003-declarative-domain-commands.md`](../../../specs/003-declarative-domain-commands.md)

## Required interfaces

```rust
// metadata/types.rs
pub struct CommandDefinition {
    pub name: String,
    pub permissions: Vec<CommandPermission>,
    pub arguments: Vec<CommandArgument>,
    pub steps: Vec<CommandStep>,
    pub result: Vec<CommandResultField>,
    pub idempotency: Option<CommandIdempotency>,
    pub effects: Vec<CommandEffect>,
}

// ir/lib.rs
pub enum MutationRoot {
    Command { alias: String, command: CommandMutation },
    // existing variants
}

pub struct CommandMutation {
    pub name: String,
    pub role: String,
    pub steps: Vec<CommandStepIr>,
    pub result: CommandResultIr,
    pub idempotency: Option<IdempotencyIr>,
}

// schema/plan.rs
pub(crate) fn try_plan_command_mutation(
    &self,
    field: &GqlField<'static, String>,
    variables: &JsonMap<String, Json>,
    session: &Session,
    path: &str,
) -> Result<Option<MutationRoot>, PlanError>;
```

Commands are available only for a declared explicit role. A command permission
is an additional gate, never an alternative to per-table CRUD permissions.
`run_as_role` is forbidden in GraphQL command metadata; it is an internal
process-only feature defined later. Commands appear only in Postgres source
schemas and only if their entire static result selection can be generated.

### Task 1: Add command YAML and retain relation kind in the catalog

**Files:**
- Modify: `crates/metadata/src/types.rs`, `crates/metadata/src/loader.rs`,
  `crates/catalog/src/lib.rs`, `crates/conformance/src/lib.rs`,
  `crates/server/src/main.rs`
- Test: `crates/metadata/tests/types_serde.rs`,
  `crates/metadata/tests/load_fixture.rs`, `crates/catalog/tests/introspect.rs`
- Add fixtures: `crates/metadata/tests/fixtures/commands/`

- [ ] Add metadata tests for a command with all supported step/value forms,
  omitted `commands.yaml`, quoted `!include`, duplicate static names, and a
  `start_process` effect that lacks idempotency. Add a `signal_process` effect
  with undeclared payload, correlation, or idempotency.
- [ ] Add catalog tests that an ordinary table is `RelationKind::Table` while
  a view and materialized view retain their distinct kinds.
- [ ] RED: run `cargo test -p donat-metadata commands` and
  `cargo test -p donat-catalog relation_kind`. Expected: no command types and
  `TableInfo` cannot distinguish table from view.
- [ ] Add `Metadata::commands` and load `commands.yaml` through `load_section`.
  Extend the conformance directory serializer and every `Metadata` literal.
- [ ] Add a serde `RelationKind` enum to `donat_catalog::TableInfo`; select
  `c.relkind` in `COLUMNS_SQL`, map only the existing `r/v/m/f/p` set, and
  store it when assembling each relation. Fix every `TableInfo` struct literal
  reported by the compiler across schema, sqlgen, server, MCP, and tests.
- [ ] GREEN: run `cargo test -p donat-metadata`,
  `cargo test -p donat-catalog`, and `cargo test --workspace --no-run`.
- [ ] Commit the metadata/catalog compatibility slice and obtain judge ACCEPT.

### Task 2: Compile static command definitions during validation

**Files:**
- Add: `crates/schema/src/commands.rs`, `crates/schema/tests/commands.rs`
- Modify: `crates/schema/src/lib.rs`, `crates/schema/src/plan.rs`,
  `crates/server/src/migrate.rs`, `crates/server/Cargo.toml`
- Test: `crates/server/tests/migrate.rs` or the existing migration validation
  test module

- [ ] Add tests that reject duplicate command names, non-table targets,
  unknown columns/arguments/step references, mutable result shape, missing
  primary-key predicates for update/delete, unsafe object/list binding,
  missing command permission, an effect without mandatory command idempotency,
  and malformed local effect payload/correlation bindings. This task validates
  only the bounded command-side effect form: mandatory idempotency and local
  binding shape, without resolving a process. Defer target-process existence,
  declared signal name, exact signal payload/correlation compatibility, and
  rejection of removal or incompatible replacement of a command pinned by a
  non-terminal process revision to Processes Task 7.
- [ ] RED: run `cargo test -p donat-schema --test commands`.
  Expected: command metadata is ignored or compiler symbols do not exist.
- [ ] Build an immutable compiled command catalog per Postgres source. Validate
  every step's forward-only dependencies, exact scalar type, table kind,
  target permission, result field, rule binding, and idempotency scope before
  accepting a candidate engine.
- [ ] Make `migrate::check_consistency` collect these diagnostics. Keep the
  serving path read-only: it only consumes the already-validated metadata
  snapshot and never creates command tables or changes definitions.
- [ ] GREEN: run `cargo test -p donat-schema --test commands`,
  `cargo test -p donat-server migrate`, and `cargo test -p donat-metadata`.
- [ ] Commit the static compiler slice and obtain judge ACCEPT.

### Task 3: Generate command GraphQL types and plan explicit-role calls

**Files:**
- Modify: `crates/schema/src/introspection.rs`, `crates/schema/src/plan.rs`,
  `crates/schema/src/multi_source.rs`, `crates/ir/src/lib.rs`
- Test: `crates/schema/tests/commands.rs`, existing schema introspection tests

- [ ] Add tests proving that `createOrder` is absent for an unknown or
  unauthorized role, appears exactly once for its permitted role, has the
  declared argument type, returns `CreateOrderResult`, rejects undeclared
  result selections, and is absent from SQLite/MySQL/ClickHouse schemas.
- [ ] Add a planning test that table mutation permissions are still required
  for each data step even when the command permission grants the role.
- [ ] RED: run `cargo test -p donat-schema --test commands graphql_schema`.
  Expected: `createOrder` is unknown and `MutationRoot::Command` is absent.
- [ ] Add command root indexes to the source planner. Generate exact input and
  output objects from the static metadata shape; do not create a generic JSON
  argument or untyped result field.
- [ ] Extend `plan_mutation` to resolve command roots before table mutation
  roots. Validate all GraphQL values/variables against the command's declared
  input types and compile rule bindings from arguments and prior steps.
- [ ] Emit `MutationRoot::Command` only after all command and underlying table
  permissions pass. Preserve aliases and GraphQL path formatting.
- [ ] GREEN: run `cargo test -p donat-schema --test commands` and
  `cargo test -p donat-schema`. Expected: introspection and planning expose
  no command to an unauthorized or non-Postgres source.
- [ ] Commit the schema/IR slice and obtain judge ACCEPT.

### Task 4: Add the deploy-time command catalog migration and error carrier

**Files:**
- Add: `migrations/V3__donat_commands.sql`
- Modify: `crates/server/src/migrate.rs`, `crates/server/src/gql.rs`
- Test: `crates/server/tests/gql.rs` or the existing GraphQL error tests,
  `crates/server/tests/migrate.rs`

- [ ] Add migration tests that `donat.command_invocations` has the exact
  primary key/unique scope, canonical JSON result, input fingerprint, status,
  and retention timestamp. Add error tests for a structured
  `validation-failed` payload and an unchanged legacy permission-check payload.
- [ ] RED: run `cargo test -p donat-server command_invocations` and
  `cargo test -p donat-server graphql_error`. Expected: no command journal and
  `db_error_json` hardcodes `permission-error` for every JSON check payload.
- [ ] Create `donat.command_invocations` and a narrowly scoped
  `donat.raise_graphql_error(code text, path text, message text)` Postgres
  function. The function must raise a dedicated SQLSTATE/message envelope,
  never accept executable SQL, and reside only in the migration catalog.
- [ ] Extend `db_error_json` to decode the dedicated structured payload into
  the code/path/message supplied by SQL. Keep the current `23514`
  permission-check path byte-for-byte compatible.
- [ ] GREEN: run the focused server tests and
  `cargo build -p donat-server --bin donat`. Inspect migration SQL manually;
  it must contain no role bypass and no runtime DDL path.
- [ ] Commit the journal/error-carrier slice and obtain judge ACCEPT.

### Task 5: Generate a single Postgres CTE statement for each command root

**Files:**
- Modify: `crates/sqlgen/src/lib.rs`, `crates/server/src/gql.rs`
- Add: `crates/sqlgen/tests/commands.rs`, `crates/sqlgen/tests/snapshots/`
- Modify: `crates/ir/src/lib.rs`

- [ ] Add snapshot fixtures for: guarded insert, `select_one` then update,
  `insert_many`, guarded assert failure, idempotency first execution, replay,
  and same-key/different-input failure.
- [ ] Add a server execution test that a command alias is decoded like existing
  mutation roots and that SQLite/MySQL dispatch rejects impossible command IR
  before executing any SQL.
- [ ] RED: run `cargo test -p donat-sqlgen --test commands`.
  Expected: command IR has no SQL renderer/snapshots.
- [ ] Render command steps as named CTEs with quoted identifiers and existing
  strict literal helpers. Use the typed rule SQL lowering from `donat-rules`.
  A failed `assert` calls `donat.raise_graphql_error`; a missing `select_one`
  does the same with its declared path.
- [ ] For idempotent commands, compute a canonical input/scope fingerprint,
  lock or insert the invocation row within the same statement, return its
  stored result on exact replay, and raise `validation-failed` if the key is
  reused with different canonical input. Store final root JSON once.
- [ ] Preserve `CommandEffect::StartProcess` and `CommandEffect::SignalProcess`
  as typed IR variants with only the Task 2 command-side validation, but leave
  them non-executable in this core slice. Their target-process compatibility
  validation, SQL lowering, durable outbox insert, and process consumer are
  Task 7 of `2026-07-28-declarative-processes.md`; SQL must never call an HTTP
  connector.
- [ ] Add explicit match arms for `MutationRoot::Command` in all executor
  alias/table-routing matches. The Postgres path uses `mutation_to_sql_opts`;
  non-Postgres matches fail closed rather than falling through.
- [ ] GREEN: run `cargo test -p donat-sqlgen --test commands`,
  `cargo test -p donat-sqlgen`, and `cargo test -p donat-server gql`.
  Review every snapshot with `cargo insta review`; each command root is one
  SQL statement.
- [ ] Commit the one-statement renderer slice and obtain judge ACCEPT.

### Task 6: Prove GraphQL and deploy contracts with native conformance

**Files:**
- Add: `crates/conformance/tests/commands.rs`,
  `crates/conformance/fixtures/commands/`
- Modify: `crates/conformance/src/lib.rs`

- [ ] Create fixtures and calls for positive create-order, invalid rule,
  forbidden role, missing table permission, duplicate idempotent replay,
  same-key/different-input, update/delete primary-key guard, and one-statement
  result shape. Add the `start_process` and `signal_process` deduplication
  fixtures in Process Task 7.
- [ ] RED: run
  `cargo build -p donat-server --bin donat && cargo test -p donat-conformance --test commands`.
  Expected: all newly added cases fail before command implementation is
  complete; capture exact expected bodies in fixtures.
- [ ] Wire no test-only permissions or admin header into the harness. Use the
  existing migration-before-serve lifecycle and role request helpers.
- [ ] GREEN: rebuild the binary and run the focused command suite, then
  `cargo test -p donat-schema --test commands`,
  `cargo test -p donat-sqlgen --test commands`, and
  `cargo test -p donat-server`.
- [ ] Commit conformance-only corrections separately and obtain judge ACCEPT.

### Task 7: Finish the command core before connectors/processes consume it

**Files:**
- Modify only for a test-proven regression.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace --exclude donat-conformance`.
- [ ] Rebuild the engine and run
  `cargo test -p donat-conformance --test commands`. Defer `make conformance`,
  full process-effect cross-validation, and the final effect fixture until
  Processes Task 7 activates `start_process`.
- [ ] Review command SQL snapshots and verify the following manually: no admin
  role, no GraphQL command on non-Postgres sources, no external I/O in SQL,
  exactly one SQL statement per command root, and exact structured error
  envelopes.
- [ ] Obtain the command-core judge ACCEPT before beginning connectors. The
  system-wide judge, full conformance gate, and process-effect compatibility
  checks are in Processes Task 7.
