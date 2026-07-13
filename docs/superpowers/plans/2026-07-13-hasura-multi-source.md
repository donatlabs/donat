# Hasura-Compatible Multi-Source GraphQL Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve every permitted Postgres and ClickHouse metadata source through one Hasura-compatible GraphQL schema, including mixed-source operations and multi-database ClickHouse catalogs.

**Architecture:** Keep the existing planner as the source-local permission and IR authority. Add a composite planner that collects and merges top-level GraphQL fields, partitions them by root owner, delegates to one child planner per source, and returns source-addressed plans plus ordered response slots. Execute one database-assembled statement per participating source and merge only top-level data in operation order.

**Tech Stack:** Rust, graphql-parser, serde_json, axum, tokio-postgres, reqwest ClickHouse HTTP, native Donat conformance harness.

## Global Constraints

- Preserve explicit role permissions; do not add an admin role or permission bypass.
- Keep ClickHouse read-only and capability-gated.
- Execute one native statement per participating source with no per-row round trips.
- Preserve exact Hasura-compatible error `code`, `path`, message, and HTTP status.
- Keep source-local relationships inside the existing planner; cross-source relationships remain unsupported.
- Start each behavior change with a failing test and observe the expected failure before production edits.
- Run the mandatory judge review after every commit.

---

### Task 1: Multi-Database ClickHouse Catalog Discovery

**Files:**
- Modify: `crates/catalog/src/lib.rs`
- Modify: `crates/server/src/state.rs`
- Test: `crates/catalog/src/lib.rs`
- Test: `crates/server/tests/clickhouse_runtime.rs`

**Interfaces:**
- Consumes: tracked `Source.tables` schemas and the resolved ClickHouse URL.
- Produces: `clickhouse_catalog_from_json_each_row(input, fallback_database)` that accepts rows carrying their own `database`; `sync_sources` builds one catalog containing every tracked ClickHouse database.

- [ ] **Step 1: Add a failing catalog parser test**

Add JSONEachRow input containing `analytics.daily` and `logs.events` and assert both keys exist in one `Catalog`:

```rust
let input = concat!(
    r#"{"database":"analytics","table":"daily","name":"date","type":"Date","default_kind":"","is_in_primary_key":1}"#, "\n",
    r#"{"database":"logs","table":"events","name":"event_time","type":"DateTime64(6)","default_kind":"","is_in_primary_key":1}"#,
);
let catalog = clickhouse_catalog_from_json_each_row(input, "default").unwrap();
assert!(catalog.table("analytics", "daily").is_some());
assert!(catalog.table("logs", "events").is_some());
```

- [ ] **Step 2: Run the parser test and verify RED**

Run: `cargo test -p donat-catalog clickhouse_json_each_row_builds_multi_database_catalog -- --exact`

Expected: FAIL because `database` is ignored and both rows are assigned to the fallback schema.

- [ ] **Step 3: Parse an optional database per row**

Extend `ClickhouseColumnRow` with `database: Option<String>`. Use the row database when present and the function argument only for backward-compatible single-database responses. Keep column and primary-key order unchanged.

- [ ] **Step 4: Add a failing runtime introspection test**

Create ClickHouse metadata with one source, a URL without `database=`, and tracked tables in `analytics` and `logs`. Make the HTTP stub assert a single `system.columns` request using `{databases:Array(String)}`, return rows from both databases, and assert neither metadata table is pruned.

- [ ] **Step 5: Run the runtime test and verify RED**

Run: `cargo test -p donat-server --test clickhouse_runtime clickhouse_tracks_tables_across_databases_without_url_database -- --exact`

Expected: FAIL because current introspection requests only `database=default` and prunes both tables.

- [ ] **Step 6: Implement tracked-database discovery**

Carry a stable deduplicated `Vec<String>` of tracked schemas into the ClickHouse arm of `sync_sources`. Query:

```sql
SELECT database, table, name, type, default_kind, is_in_primary_key
FROM system.columns
WHERE database IN {databases:Array(String)}
ORDER BY database, table, position
FORMAT JSONEachRow
```

Append `param_databases` to the request URL using a serialized array value. If no tables are tracked, use the URL `database` and then `default` as fallback.

- [ ] **Step 7: Verify GREEN and regression coverage**

Run:

```bash
cargo test -p donat-catalog
cargo test -p donat-server --test clickhouse_runtime
```

Expected: PASS with the new multi-database test and all existing ClickHouse runtime tests.

- [ ] **Step 8: Commit and judge**

```bash
git add crates/catalog/src/lib.rs crates/server/src/state.rs crates/server/tests/clickhouse_runtime.rs
git commit -m "fix(clickhouse): introspect all tracked databases"
```

Dispatch the mandatory judge with the task requirements and fresh test output; continue only after ACCEPT.

---

### Task 2: Composite Planner and Introspection

**Files:**
- Create: `crates/schema/src/multi_source.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/src/plan.rs`
- Modify: `crates/schema/src/introspection.rs`
- Test: `crates/schema/tests/multi_source.rs`

**Interfaces:**
- Consumes: `Metadata`, `HashMap<String, Catalog>`, parsed GraphQL documents, variables, and `Session`.
- Produces: `MultiSourcePlanner`, `MultiSourcePlan`, `SourceQueryPlan`, and ordered `QueryResponseSlot` values defined by the approved design.

- [ ] **Step 1: Add failing root-composition tests**

Build metadata with Postgres source `default` owning root `public_item` and ClickHouse source `clickhouse` owning `logs_event`. Assert a user can plan:

```graphql
query Mixed($id: Int!) {
  event: logs_event(where: {id: {_eq: $id}}) { id }
  __typename
  item: public_item { id }
}
```

Expected plan: two `SourceQueryPlan`s and response slots ordered `event`, `__typename`, `item`.

- [ ] **Step 2: Run the new test and verify RED**

Run: `cargo test -p donat-schema --test multi_source mixed_query_is_partitioned_by_root_owner -- --exact`

Expected: compile failure because `MultiSourcePlanner` does not exist.

- [ ] **Step 3: Add source-local planner construction**

Refactor `Planner::new` to call:

```rust
pub fn for_source(metadata: &'a Metadata, source: &'a Source, catalog: &'a Catalog) -> Self
```

Keep `Planner::new` as the backward-compatible `default`/first-source wrapper used by existing tests, MCP, migration validation, and single-source callers. Expose read-only query and mutation root-name iterators for composite ownership indexing.

- [ ] **Step 4: Implement top-level field collection and partitioning**

In `multi_source.rs`, select the operation, apply variable defaults, expand root fragments/directives using the existing `flatten` helper, and collect fields by first-seen response key. Merge compatible fields by concatenating selection sets. Reject differing field names or non-equivalent arguments at the original `$.selectionSet.<field>` path before delegation.

Create source-local operation documents containing the selected root fields, original variable definitions, and fragment definitions. Delegate each partition to its child `Planner` and wrap the resulting source-local IR.

- [ ] **Step 5: Add field-merging, typename, mutation, and permission tests**

Cover:

- direct and fragment-provided repeated compatible roots;
- conflicting aliases and conflicting arguments;
- typename-only query and mutation plans with no source;
- mutation roots from one source and rejection across two mutation-capable sources;
- role-specific root visibility on each source;
- session-variable predicates remaining in each source-local IR;
- role `admin` seeing no roots unless metadata explicitly grants `admin`.

- [ ] **Step 6: Add failing composite introspection tests**

Assert `query_root` contains both Postgres and ClickHouse fields, only Postgres mutation fields exist, ClickHouse roots omit `distinct_on`, and duplicate incompatible root/type definitions return a construction error naming both sources.

- [ ] **Step 7: Implement schema composition**

Extract the existing introspection projection so it can consume a prebuilt schema JSON value. Build one role schema per child planner, merge root objects in metadata source order, deduplicate identical named types, and reject incompatible duplicate named types or root fields. Apply relay mode per child capability.

- [ ] **Step 8: Verify schema tests GREEN**

Run:

```bash
cargo test -p donat-schema --test multi_source
cargo test -p donat-schema
```

Expected: PASS with all existing planner and introspection tests unchanged.

- [ ] **Step 9: Commit and judge**

```bash
git add crates/schema/src/lib.rs crates/schema/src/plan.rs crates/schema/src/introspection.rs crates/schema/src/multi_source.rs crates/schema/tests/multi_source.rs
git commit -m "feat(schema): compose GraphQL metadata sources"
```

Dispatch the mandatory judge and continue only after ACCEPT.

---

### Task 3: Source-Aware Runtime Execution

**Files:**
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/src/gql.rs`
- Modify: `crates/server/src/main.rs`
- Test: `crates/server/tests/multi_source_runtime.rs`

**Interfaces:**
- Consumes: `MultiSourcePlan` from Task 2.
- Produces: `AppState::execute_query_json(source, roots)`, source-addressed mutation execution, and operation-order response assembly.

- [ ] **Step 1: Add a failing source-routing runtime test**

Use a temporary SQLite default source and a ClickHouse HTTP stub so no external database is needed. Query alternating roots from both sources plus `__typename`; assert one call per participating source, no call for typename, exact alias order, and the combined JSON body.

- [ ] **Step 2: Run the runtime test and verify RED**

Run: `cargo test -p donat-server --test multi_source_runtime mixed_sources_execute_against_their_own_connections -- --exact`

Expected: FAIL because GraphQL still builds a default-source planner and executor.

- [ ] **Step 3: Add source-addressed state accessors**

Implement exact-name lookups with no fallback:

```rust
async fn source_kind(&self, source: &str) -> Option<SourceKind>;
async fn source_url(&self, source: &str) -> Option<String>;
async fn pool_for(&self, source: &str) -> Option<deadpool_postgres::Pool>;
pub async fn execute_query_json(&self, source: &str, roots: &[RootField]) -> Result<Json, QueryError>;
```

Retain existing default wrappers for MCP and migration callers. Generalize SQLite and MySQL mutation executors to accept a source name; Postgres mutation execution must select the matching pool.

- [ ] **Step 4: Wire composite planning into GraphQL execution**

Construct `MultiSourcePlanner` from the engine metadata and catalogs. Execute source query plans concurrently, collect each returned object by source, then build the final `serde_json::Map` from ordered response slots. Insert local typename values directly. Apply existing remote-join resolution to source fields after the merge.

For mutations, reject multiple datasource owners during planning, execute against the single resolved source, and merge local typename slots in operation order.

- [ ] **Step 5: Add negative runtime coverage**

Assert unknown source lookup never falls back to `default`, a secondary backend error returns the existing `unexpected` shape, denied roots trigger no backend calls, and typename-only operations trigger no backend calls.

- [ ] **Step 6: Verify GREEN and existing runtime tests**

Run:

```bash
cargo test -p donat-server --test multi_source_runtime
cargo test -p donat-server
```

Expected: PASS with exact existing error shapes and no changes to single-source responses.

- [ ] **Step 7: Commit and judge**

```bash
git add crates/server/src/state.rs crates/server/src/gql.rs crates/server/src/main.rs crates/server/tests/multi_source_runtime.rs
git commit -m "feat(server): execute GraphQL plans by datasource"
```

Dispatch the mandatory judge and continue only after ACCEPT.

---

### Task 4: tandt ClickHouse Contract Suite

**Files:**
- Create: `crates/server/tests/tandt_clickhouse_contract.rs`
- Create: `crates/server/tests/fixtures/tandt_clickhouse_metadata.json`
- Create: `crates/server/tests/fixtures/tandt_clickhouse_queries.graphql`
- Modify: `crates/conformance/README.md`

**Interfaces:**
- Consumes: a real ClickHouse URL from `CLICKHOUSE_URL` and the production-shaped Hasura metadata fixture.
- Produces: executable regression coverage for all twelve tandt ClickHouse operations and both tracked databases.

- [ ] **Step 1: Add the production-shaped metadata fixture**

Define a Postgres-compatible placeholder `default` source plus a ClickHouse source using Hasura `configuration.template`. Track representative `analytics_*` and `logs_*` roots with explicit `company` and `l2-executor` select permissions, aggregations, and session-variable company filters.

- [ ] **Step 2: Add the twelve named GraphQL operations**

Include the query documents used by tandt-backend:

`AnalyticsDocumentDailyStats`, `AnalyticsWorkflowExecutions`, `AnalyticsErrors`, `AnalyticsCodeLifecycleEvents`, `AnalyticsAggregationOperations`, `AnalyticsDashboardStats`, `ApplicationLogsList`, `DocumentIntegrationRequests`, `L2JobEvents`, `L2DeviceEvents`, `L2TrafficLogs`, and `L2ProductionEvents`.

Use Date and DateTime variables for dashboard time bounds; do not encode SQL expressions as scalar strings.

- [ ] **Step 3: Add a failing real ClickHouse contract test**

Create isolated `analytics` and `logs` databases and minimal tables/rows matching every selected field. Start `AppState` with a base URL lacking `database=`, run every operation with representative variables and explicit sessions, and assert data exists without GraphQL errors. Include one mixed Postgres/ClickHouse operation when `PG_URL` is available.

- [ ] **Step 4: Run against ClickHouse and verify RED or uncovered incompatibilities**

Run:

```bash
DONAT_EXTERNAL_DB_TESTS=1 CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-server --test tandt_clickhouse_contract -- --test-threads=1 --nocapture
```

Expected before final compatibility fixes: at least one failing operation identifies the unsupported query shape precisely.

- [ ] **Step 5: Fix only demonstrated compatibility gaps**

For each failure, add the smallest unit test in the owning crate before changing production code. Keep backend-specific literal rendering in `donat-backend`, planning semantics in `donat-schema`, SQL assembly in `donat-sqlgen`, and transport in `donat-server`.

- [ ] **Step 6: Verify all twelve operations GREEN**

Re-run the exact contract command and require all operation assertions to pass against real ClickHouse.

- [ ] **Step 7: Document the contract command and commit**

```bash
git add crates/server/tests/tandt_clickhouse_contract.rs crates/server/tests/fixtures crates/conformance/README.md
git add crates/backend crates/schema crates/sqlgen crates/server
git commit -m "test: cover tandt multi-source ClickHouse contract"
```

Dispatch the mandatory judge with the per-operation result map and continue only after ACCEPT.

---

### Task 5: Full Verification and Pull Request

**Files:**
- Modify only files required by review findings.

**Interfaces:**
- Consumes: completed Tasks 1-4.
- Produces: a pushed feature branch and a GitHub pull request targeting `main`.

- [ ] **Step 1: Format and run static checks**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
git diff --check origin/main...HEAD
```

Expected: all commands exit 0.

- [ ] **Step 2: Run workspace tests**

Run: `cargo test --workspace --exclude donat-conformance`

Expected: all unit, integration, runtime, and doc tests pass.

- [ ] **Step 3: Rebuild and run full Postgres conformance**

Run:

```bash
cargo build -p donat-server --bin donat
cargo test -p donat-conformance
```

Expected: every conformance module passes after the fresh server build.

- [ ] **Step 4: Run real ClickHouse conformance and tandt contract**

Run:

```bash
DONAT_EXTERNAL_DB_TESTS=1 CONF_BACKEND=clickhouse \
CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture

DONAT_EXTERNAL_DB_TESTS=1 CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-server --test tandt_clickhouse_contract -- --test-threads=1 --nocapture
```

Expected: backend matrix and all twelve tandt operations pass.

- [ ] **Step 5: Run final judge review**

Provide the judge with the full `origin/main...HEAD` diff, approved design, this plan, all exact verification commands, and fresh outputs. Address every Critical or Important finding with a test-first fix and a reviewed commit.

- [ ] **Step 6: Push and create the PR**

```bash
git status --short --branch
git push -u origin feat/hasura-multi-source-clickhouse
gh pr create --repo donatlabs/donat --base main --head feat/hasura-multi-source-clickhouse \
  --title "feat: add Hasura-compatible multi-source GraphQL" \
  --body-file /tmp/donat-multi-source-pr.md
```

The PR body must include architecture, compatibility guarantees, all twelve
tandt operations, exact verification commands/results, security invariants,
and deployment impact.

- [ ] **Step 7: Verify remote state**

Run:

```bash
gh pr view --repo donatlabs/donat --json number,url,state,headRefName,baseRefName
gh pr checks --repo donatlabs/donat --watch
```

Expected: PR is open from the requested feature branch to `main`; required CI checks are green.
