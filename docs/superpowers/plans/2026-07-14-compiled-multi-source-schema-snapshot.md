# Compiled Multi-Source Schema Snapshot Implementation Plan

> **For Codex:** Execute every checkbox in order with RED/GREEN evidence and a
> judge gate after each commit.

**Goal:** Remove schema composition and source-index construction from GraphQL
request paths while preserving multi-source permissions, Relay, introspection,
action relationships, and atomic engine state.

**Architecture:** `sync_sources` stages runtime handles and catalogs, then
compiles reusable source planner indexes, ownership, and role schemas. One
immutable `Engine` snapshot publishes all of them. Requests create lightweight
planner views over that snapshot and only select an introspection schema after
detecting an introspection root.

**Tech Stack:** Rust, Tokio, Axum, `graphql-parser`, Cargo tests, Docker-backed
conformance tests.

**Required interfaces and ownership:**

```rust
// schema/plan.rs: immutable and safe to share across request planners.
pub(crate) struct PlannerIndex { /* capabilities and root/table/function maps */ }

impl<'a> Planner<'a> {
    pub(crate) fn compile_index(source: &Source) -> Arc<PlannerIndex>;
    pub(crate) fn for_source_with_index(
        metadata: &'a Metadata,
        source: &'a Source,
        catalog: &'a Catalog,
        index: Arc<PlannerIndex>,
    ) -> Self;
}

// schema/multi_source.rs: owns every compiled source index and schema value.
pub struct CompiledMultiSourceSchema { /* Arc indexes, ownership, role schemas */ }

impl CompiledMultiSourceSchema {
    pub fn compile(
        metadata: &Metadata,
        catalogs: &HashMap<String, Catalog>,
        infer_function_permissions: bool,
    ) -> Result<Self, PlanError>;

    pub fn source_planner<'a>(
        &'a self,
        metadata: &'a Metadata,
        catalogs: &'a HashMap<String, Catalog>,
        source_name: &str,
    ) -> Result<Planner<'a>, PlanError>;
}

impl<'a> MultiSourcePlanner<'a> {
    pub fn from_compiled(
        metadata: &'a Metadata,
        catalogs: &'a HashMap<String, Catalog>,
        compiled: &'a CompiledMultiSourceSchema,
    ) -> Result<Self, PlanError>;
}

// server/state.rs: runtime handles are cloneable but published only in Engine.
#[derive(Clone)]
pub enum SourceRuntime {
    Postgres { url: String, pool: deadpool_postgres::Pool },
    Sqlite { path: String },
    Mysql { url: String },
    Clickhouse { url: String },
}

pub struct Engine {
    pub metadata: Metadata,
    pub catalogs: HashMap<String, Catalog>,
    pub compiled: Option<Arc<CompiledMultiSourceSchema>>,
    pub runtimes: HashMap<String, SourceRuntime>,
}
```

`sync_sources` constructs a complete candidate `Engine` outside the write lock.
The only publication statement is `*self.engine.write().await = candidate` after
all connections, introspection, pruning, and compilation succeed. Request
planners own only cheap `Arc<PlannerIndex>` clones and borrow metadata/catalogs
from one engine read guard; no compiled object borrows from `Engine`.
`PlannerIndex` stays crate-private: action relationship resolution uses the
narrow public `CompiledMultiSourceSchema::source_planner` factory.

---

### Task 1: Detect introspection before schema materialization

**Files:**
- Modify/test: `crates/schema/src/introspection.rs`
- Modify: `crates/schema/src/multi_source.rs`

- [ ] Add tests
  `ordinary_operations_do_not_materialize_an_introspection_schema` and
  `introspection_materializes_the_schema_once` using a counted schema provider.
- [ ] RED: run `cargo test -p donat-schema lazy_schema_tests`.
  Expected: compile failure because `execute_introspection_schema_lazy` does not
  exist.
- [ ] Implement `is_introspection_operation` and
  `execute_introspection_schema_lazy`; make composite introspection perform the
  root check before requesting a schema.
- [ ] GREEN: run `cargo test -p donat-schema lazy_schema_tests` and
  `cargo test -p donat-schema --test multi_source`.
  Expected: all tests pass and ordinary operations invoke the provider zero
  times.
- [ ] Commit this slice and obtain judge PASS before the next commit.

### Task 2: Compile and reuse every source planner index

**Files:**
- Modify: `crates/schema/src/plan.rs`
- Modify: `crates/schema/src/multi_source.rs`
- Modify: `crates/schema/src/lib.rs`
- Test: `crates/schema/tests/multi_source.rs`

- [ ] Add tests `compiled_snapshot_reuses_source_indexes`,
  `compiled_snapshot_caches_role_backend_and_unknown_schemas`, and
  `compiled_snapshot_prevalidates_relay`.
- [ ] Migrate collision assertions to
  `compiled_snapshot_rejects_root_type_role_backend_and_relay_collisions`.
- [ ] RED: run
  `cargo test -p donat-schema --test multi_source compiled_snapshot`.
  Expected: compile failure because `CompiledMultiSourceSchema` and the compiled
  planner constructor do not exist.
- [ ] Introduce an immutable `Arc<PlannerIndex>` containing capabilities,
  table/function lookup, and query/mutation root indexes. Keep `Planner::new`
  and `Planner::for_source` compatible by compiling an owned index once.
- [ ] Implement `CompiledMultiSourceSchema::compile(metadata, catalogs,
  infer_function_permissions)` and `MultiSourcePlanner::from_compiled`.
- [ ] Precompute standard/Relay ownership and role schemas for both
  `backend_request` values plus one denied unknown-role schema. Configure
  function permission inference before validation/composition.
- [ ] Remove metadata-wide schema work and table/function index construction
  from `from_compiled`.
- [ ] GREEN: run
  `cargo test -p donat-schema --test multi_source` and
  `cargo test -p donat-schema`.
  Expected: all schema tests pass, including collision and permission cases.
- [ ] Commit this slice and obtain judge PASS before the next commit.

### Task 3: Publish schema and runtime routing atomically

**Files:**
- Modify/test: `crates/server/src/state.rs`
- Modify: `crates/server/src/main.rs`
- Update fixture: `crates/server/src/mcp.rs`
- Update fixture: `crates/server/tests/clickhouse_runtime.rs`
- Update fixture: `crates/server/tests/mysql_mutations.rs`
- Update fixture: `crates/server/tests/mysql_runtime.rs`
- Update fixture: `crates/server/tests/multi_source_runtime.rs`
- Update fixture: `crates/server/tests/sqlite_mutations.rs`
- Update fixture: `crates/server/tests/sqlite_runtime.rs`

- [ ] Add tests `failed_candidate_preserves_entire_engine_snapshot` and
  `valid_candidate_publishes_entire_engine_snapshot`.
- [ ] RED: run `cargo test -p donat-server state::snapshot_tests`.
  Expected: compile failure because runtime handles and compiled schema are not
  members of `Engine` and candidate publication is not available.
- [ ] Add cloneable `SourceRuntime` variants for PostgreSQL pool, SQLite path,
  MySQL URL, and ClickHouse URL; move routing state from `AppState` into
  `Engine`.
- [ ] Add explicit bootstrap state for the compiled schema. Stage/reuse runtime
  handles, introspect catalogs, prune cloned metadata, and compile the complete
  candidate before taking the engine write lock.
- [ ] Publish metadata, catalogs, compiled indexes/schemas, and runtime handles
  with one `Engine` replacement. On every error leave the old value untouched.
- [ ] Update all listed fixtures to use a bootstrap or fully compiled engine.
- [ ] GREEN: run `cargo test -p donat-server state::snapshot_tests`,
  `cargo test -p donat-server --test sqlite_runtime`, and
  `cargo test -p donat-server --test multi_source_runtime`.
  Expected: positive and negative publication tests and runtime tests pass.
- [ ] Commit this slice and obtain judge PASS before the next commit.

### Task 4: Use compiled planners on every GraphQL request path

**Files:**
- Modify: `crates/server/src/gql.rs`
- Test: `crates/server/tests/multi_source_runtime.rs`
- Test action relationships: existing `crates/server` action tests selected by
  `cargo test -p donat-server action`

- [ ] Add test `ordinary_requests_reuse_the_compiled_snapshot` for repeated
  `execute_full` calls and test
  `internal_action_select_reuses_the_compiled_default_source` for
  `execute_select_internal`.
- [ ] RED: run
  `cargo test -p donat-server ordinary_requests_reuse_the_compiled_snapshot`
  and
  `cargo test -p donat-server internal_action_select_reuses_the_compiled_default_source`.
  Expected: compile failure until request APIs require the compiled snapshot.
- [ ] Replace local multi-source and mixed remote-introspection construction
  with `MultiSourcePlanner::from_compiled`.
- [ ] Replace `Planner::new` in action output relationship resolution with
  `CompiledMultiSourceSchema::source_planner` for the default source from the
  same engine snapshot.
- [ ] Use cached role schemas through `execute_introspection_schema_lazy` and
  return an explicit initialization error if bootstrap has not completed.
- [ ] GREEN: run the two focused tests, `cargo test -p donat-server action`, and
  `cargo test -p donat-server --test multi_source_runtime`.
  Expected: all pass without any request-time compile entry point.
- [ ] Commit this slice and obtain judge PASS before the next commit.

### Task 5: Verify all behavior and security contracts

**Files:**
- Modify only when a failing regression test proves a defect.

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace --exclude donat-conformance`.
- [ ] Rebuild the engine with `cargo build -p donat-server --bin donat`.
- [ ] Start dependencies with
  `docker compose -f docker-compose.conformance.yml up -d --wait`.
- [ ] Run the complete conformance crate against the rebuilt engine:
  `cargo test -p donat-conformance -- --test-threads=4`.
- [ ] Run PostgreSQL:
  `CONF_BACKEND=postgres cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture`.
- [ ] Run SQLite:
  `CONF_BACKEND=sqlite cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture`.
- [ ] Run MySQL:
  `DONAT_EXTERNAL_DB_TESTS=1 CONF_BACKEND=mysql MYSQL_URL=mysql://root:root@127.0.0.1:13306/donat cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture`.
- [ ] Run ClickHouse:
  `DONAT_EXTERNAL_DB_TESTS=1 CONF_BACKEND=clickhouse CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture`.
- [ ] Run the mixed production contract:
  `DONAT_EXTERNAL_DB_TESTS=1 PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 cargo test -p donat-conformance --test tandt_clickhouse_contract -- --test-threads=1 --nocapture`.
- [ ] Run ClickHouse multi-database composition:
  `DONAT_EXTERNAL_DB_TESTS=1 CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 cargo test -p donat-conformance --test clickhouse_multi_database -- --test-threads=1 --nocapture`.
- [ ] Run mixed-source routing:
  `DONAT_EXTERNAL_DB_TESTS=1 PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 cargo test -p donat-conformance --test multi_source -- --test-threads=1 --nocapture`.
- [ ] Confirm explicit roles, unknown-role denial, backend-only permissions,
  Relay, mixed-source routing, actions, and introspection all remain green.
- [ ] Commit any test-proven correction separately and obtain judge PASS.

### Task 6: Reproduce and close the latency regression

**Files:**
- Add a benchmark artifact only if an established repository location exists;
  otherwise preserve exact commands and summaries in verification evidence.

- [ ] Start the freshly built Donat against a clean PostgreSQL database and the
  complete tandt L3 metadata.
- [ ] Record cold startup duration separately.
- [ ] Run 100 sequential ordinary `company_company` requests without aggregate
  fields and record p50/p95/max.
- [ ] Run a bounded concurrent sample and confirm the previous 1.4-second
  baseline and four-second queueing spikes are gone.
- [ ] Profile one request and confirm no schema composition, role enumeration,
  or source-index construction appears on the request path.

### Task 7: Publish and verify deployment

**Files:**
- Review the complete `origin/main...HEAD` diff and committed knowledgebase.

- [ ] Run `git diff --check origin/main...HEAD` and inspect for unrelated
  changes.
- [ ] Obtain final judge PASS over the full diff and fresh verification output.
- [ ] Push `fix/multi-source-schema-cache`, create/merge the PR according to
  repository policy, and wait for CI.
- [ ] Build/publish the public Docker image, redeploy tandt, and repeat the
  authenticated production query measurement.
