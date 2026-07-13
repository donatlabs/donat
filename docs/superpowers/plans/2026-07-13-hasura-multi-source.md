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

Run: `cargo test -p donat-catalog clickhouse_tests::clickhouse_json_each_row_builds_multi_database_catalog -- --exact`

Expected: exactly one test runs and FAILS because `database` is ignored and
both rows are assigned to the fallback schema. Zero executed tests is not RED.

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

### Task 2: Composite Planner, Introspection, and Runtime

**Files:**
- Create: `crates/schema/src/multi_source.rs`
- Modify: `crates/schema/src/lib.rs`
- Modify: `crates/schema/src/plan.rs`
- Modify: `crates/schema/src/introspection.rs`
- Test: `crates/schema/tests/multi_source.rs`
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/src/gql.rs`
- Modify: `crates/server/src/main.rs`
- Test: `crates/server/tests/multi_source_runtime.rs`
- Create: `crates/conformance/tests/multi_source.rs`
- Create: `crates/conformance/fixtures/multi_source/mixed_query.graphql`
- Create: `crates/conformance/fixtures/multi_source/mixed_query_expected.json`

**Interfaces:**
- Consumes: `Metadata`, `HashMap<String, Catalog>`, parsed GraphQL documents, variables, and `Session`.
- Produces: `MultiSourcePlanner`, `MultiSourcePlan`, `SourceQueryPlan`, ordered
  `QueryResponseSlot` values, source-addressed execution, and operation-order
  response assembly.

- [ ] **Step 1: Add the failing native conformance contract first**

Create a conformance test that provisions one isolated Postgres database and a
ClickHouse HTTP stub, starts the real Donat binary with two metadata sources,
and sends an authenticated mixed query containing alternating Postgres and
ClickHouse roots plus `__typename`. The expected fixture must assert the exact
ordered response body and exactly one data request to each source. Add explicit
cases for a missing role header and an ungranted `admin` role, both with exact
error bodies and zero data requests.

- [ ] **Step 2: Add every planner behavior test before implementation**

Build metadata with Postgres source `default` owning root `public_item` and ClickHouse source `clickhouse` owning `logs_event`. Assert a user can plan:

```graphql
query Mixed($id: Int!) {
  event: logs_event(where: {id: {_eq: $id}}) { id }
  __typename
  item: public_item { id }
}
```

Expected plan: two `SourceQueryPlan`s and response slots ordered `event`, `__typename`, `item`.

In the same test file, add all approved planner behavior cases before changing
production code:

- direct and fragment-provided repeated compatible roots;
- conflicting aliases and conflicting arguments;
- typename-only query and mutation plans with no source;
- one-source mutations and rejection across two mutation-capable sources;
- role-specific root visibility on each source;
- session-variable predicates retained in each source-local IR;
- `admin` denied unless explicitly granted in metadata;
- composite introspection containing both query roots, Postgres-only mutations,
  source-specific capability arguments, and root/type collision rejection.

Add every runtime case before production edits. Use mandatory Postgres from
`PG_URL` and a ClickHouse HTTP stub for alternating mixed roots plus
`__typename`, exact response order, and one data request per source. Add exact
allowed/session-filtered responses for both sources, HTTP missing-role denial,
ungranted and explicitly granted `admin` cases, unknown-source no-fallback,
secondary backend failure shape, typename-only zero-call behavior, and a
non-default mutation routing test using two distinct temporary SQLite stores.

- [ ] **Step 3: Run conformance and planner tests and verify RED**

Run:

```bash
cargo build -p donat-server --bin donat
DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
  cargo test -p donat-conformance --test multi_source -- --test-threads=1 --nocapture
cargo test -p donat-schema --test multi_source
DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
  cargo test -p donat-server --test multi_source_runtime -- --test-threads=1 --nocapture
```

Expected: conformance fails with the secondary ClickHouse root absent; schema
and runtime tests fail to compile because `MultiSourcePlanner` and
source-addressed executors do not exist. Record all failures before production
edits.

- [ ] **Step 4: Add source-local planner construction**

Refactor `Planner::new` to call:

```rust
pub fn for_source(metadata: &'a Metadata, source: &'a Source, catalog: &'a Catalog) -> Self
```

Keep `Planner::new` as the backward-compatible `default`/first-source wrapper used by existing tests, MCP, migration validation, and single-source callers. Expose read-only query and mutation root-name iterators for composite ownership indexing.

- [ ] **Step 5: Implement top-level field collection and partitioning**

In `multi_source.rs`, select the operation, apply variable defaults, expand root fragments/directives using the existing `flatten` helper, and collect fields by first-seen response key. Merge compatible fields by concatenating selection sets. Reject differing field names or non-equivalent arguments at the original `$.selectionSet.<field>` path before delegation.

Create source-local operation documents containing the selected root fields, original variable definitions, and fragment definitions. Delegate each partition to its child `Planner` and wrap the resulting source-local IR.

- [ ] **Step 6: Implement schema composition**

Extract the existing introspection projection so it can consume a prebuilt schema JSON value. Build one role schema per child planner, merge root objects in metadata source order, deduplicate identical named types, and reject incompatible duplicate named types or root fields. Apply relay mode per child capability.

Add exact-name state accessors for source kind, URL, Postgres pool, SQLite path,
and MySQL URL with no fallback. Change query and mutation executors to accept a
source name. Wire `MultiSourcePlanner` into GraphQL, execute one statement per
source, merge returned objects and local typename slots in response-slot order,
and apply existing remote-join resolution after the merge. A mutation containing
datasource fields must execute only against its single resolved source.

- [ ] **Step 7: Verify planner and native conformance tests GREEN**

Run:

```bash
cargo test -p donat-schema --test multi_source
cargo test -p donat-schema
cargo build -p donat-server --bin donat
DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
  cargo test -p donat-conformance --test multi_source -- --test-threads=1 --nocapture
DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
  cargo test -p donat-server --test multi_source_runtime -- --test-threads=1 --nocapture
cargo test -p donat-server
```

Expected: PASS with all existing planner/introspection tests unchanged and the
exact mixed-source conformance responses matching the fixtures.

- [ ] **Step 8: Commit and judge**

```bash
git add crates/schema/src/lib.rs crates/schema/src/plan.rs crates/schema/src/introspection.rs crates/schema/src/multi_source.rs crates/schema/tests/multi_source.rs crates/server/src/state.rs crates/server/src/gql.rs crates/server/src/main.rs crates/server/tests/multi_source_runtime.rs crates/conformance/tests/multi_source.rs crates/conformance/fixtures/multi_source
git commit -m "feat(schema): compose GraphQL metadata sources"
```

Dispatch the mandatory judge and continue only after ACCEPT.

---

### Task 3: tandt ClickHouse Contract Suite

**Files:**
- Create: `crates/server/tests/tandt_clickhouse_contract.rs`
- Create: `crates/server/tests/fixtures/tandt_clickhouse_metadata.json`
- Create: `crates/server/tests/fixtures/tandt_clickhouse_queries.graphql`
- Modify: `crates/conformance/README.md`

**Interfaces:**
- Consumes: a real ClickHouse URL from `CLICKHOUSE_URL` and the production-shaped Hasura metadata fixture.
- Produces: executable regression coverage for all twelve tandt ClickHouse operations and both tracked databases.

- [ ] **Step 1: Pin the upstream contract and add production-shaped fixtures**

Record tandt-backend revision
`c780834e50f53e5b4e94f1f33e88748a443f98ec` and these source paths in the
fixture header:

- `ui/apps/l3-l4-web-ui/src/shared/api/entities/analytics/api.ts`
- `ui/apps/l3-l4-web-ui/src/shared/api/entities/logs/api.ts`
- `ui/apps/l3-l4-web-ui/src/shared/api/entities/document/integration-request-api.ts`
- `ui/packages/l2-api/src/entities/l2-job-event/queries.ts`
- `ui/packages/l2-api/src/entities/l2-device-event/queries.ts`
- `ui/packages/l2-api/src/entities/l2-traffic-log/queries.ts`
- `ui/packages/l2-api/src/entities/l2-production-event/queries.ts`

Define a Postgres `default` source plus a ClickHouse source using Hasura
`configuration.template`. Track the exact roots and columns with explicit
`company` and `l2-executor` permissions, aggregations, and session filters.

- [ ] **Step 2: Add the twelve named GraphQL operations**

Include the query documents used by tandt-backend:

`AnalyticsDocumentDailyStats`, `AnalyticsWorkflowExecutions`, `AnalyticsErrors`, `AnalyticsCodeLifecycleEvents`, `AnalyticsAggregationOperations`, `AnalyticsDashboardStats`, `ApplicationLogsList`, `DocumentIntegrationRequests`, `L2JobEvents`, `L2DeviceEvents`, `L2TrafficLogs`, and `L2ProductionEvents`.

Use Date and DateTime variables for dashboard time bounds; do not encode SQL expressions as scalar strings.

Create a case table with exact variables and exact ordered expected JSON:

| Operation | Required oracle |
|---|---|
| `AnalyticsDocumentDailyStats` | two seeded statuses returned `date desc`, filtered by company, with exact count, users, and nullable duration |
| `AnalyticsWorkflowExecutions` | `_like` name filter and descending `start_time` return exactly one row |
| `AnalyticsErrors` | unresolved/company predicate and descending `error_time` return the seeded error only |
| `AnalyticsCodeLifecycleEvents` | DateTime lower bound excludes the old event and returns the new event |
| `AnalyticsAggregationOperations` | limit/offset and descending time return the second seeded page exactly |
| `AnalyticsDashboardStats` | four aliased aggregate roots return exact counts, sums, and nodes for Date/DateTime bounds |
| `ApplicationLogsList` | exact JSON/Map/Tuple/Array `context` and ordered log fields round-trip |
| `DocumentIntegrationRequests` | document filter and descending request time return the matching OMS row only |
| `L2JobEvents` | `_and`, pagination, and aggregate `count` return one node and total two |
| `L2DeviceEvents` | equipment/device filter and aggregate `count` return the exact node and total |
| `L2TrafficLogs` | order, offset, and aggregate count return the exact payload page and total |
| `L2ProductionEvents` | work-order filter, descending event time, and aggregate count return exact rows and total |

Store exact request and response JSON for every case. Tests compare full
`serde_json::Value` equality, not merely absence of errors.

- [ ] **Step 3: Add a failing real ClickHouse contract test**

Create isolated `analytics` and `logs` databases and deterministic tables/rows
matching every selected field. Start `AppState` with a base URL lacking
`database=`, run all operations with pinned variables and explicit sessions,
and compare each response to its exact JSON oracle. Provision Postgres from
mandatory `PG_URL` and run a mixed Postgres/ClickHouse operation; missing
Postgres must fail when `DONAT_EXTERNAL_DB_TESTS=1`.

- [ ] **Step 4: Run against ClickHouse and verify RED or uncovered incompatibilities**

Run:

```bash
DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-server --test tandt_clickhouse_contract -- --test-threads=1 --nocapture
```

Expected before final compatibility fixes: at least one failing operation identifies the unsupported query shape precisely.

- [ ] **Step 5: Fix only demonstrated compatibility gaps**

For each failure, add the smallest unit test in the owning crate before changing production code. Keep backend-specific literal rendering in `donat-backend`, planning semantics in `donat-schema`, SQL assembly in `donat-sqlgen`, and transport in `donat-server`.

- [ ] **Step 6: Verify all twelve exact response oracles GREEN**

Re-run the exact contract command and require all operation assertions to pass against real ClickHouse.

- [ ] **Step 7: Document the contract command and commit**

```bash
git add crates/server/tests/tandt_clickhouse_contract.rs crates/server/tests/fixtures crates/conformance/README.md
git add crates/backend crates/schema crates/sqlgen crates/server
git commit -m "test: cover tandt multi-source ClickHouse contract"
```

Dispatch the mandatory judge with the per-operation result map and continue only after ACCEPT.

---

### Task 4: Full Verification and Pull Request

**Files:**
- Modify only files required by review findings.

**Interfaces:**
- Consumes: completed Tasks 1-3.
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

- [ ] **Step 4: Run every registered backend matrix leg**

Run:

```bash
docker compose -f docker-compose.conformance.yml up -d --wait

CONF_BACKEND=sqlite \
  cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture

DONAT_EXTERNAL_DB_TESTS=1 CONF_BACKEND=mysql \
MYSQL_URL=mysql://root:root@127.0.0.1:13306/donat \
  cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture

DONAT_EXTERNAL_DB_TESTS=1 CONF_BACKEND=clickhouse \
CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-conformance --test backend_matrix -- --test-threads=1 --nocapture

DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-server --test tandt_clickhouse_contract -- --test-threads=1 --nocapture
```

Expected: SQLite, MySQL, and ClickHouse matrix legs pass, and all twelve tandt
operations match their exact response oracles.

- [ ] **Step 5: Run final judge review**

Provide the judge with the full `origin/main...HEAD` diff, approved design, this plan, all exact verification commands, and fresh outputs. Address every Critical or Important finding with a test-first fix and a reviewed commit.

- [ ] **Step 6: Build and validate the PR body**

Create `/tmp/donat-multi-source-pr.md` from the final fresh verification
outputs. It must contain Summary, Architecture, Security, Compatibility,
Twelve-operation contract results, exact Commands/Results, and Deployment
Impact sections. Read the completed file and verify that all four backend
results and all twelve operation names are present before PR creation.

Use `apply_patch` to create the file, then validate it:

```bash
test -s /tmp/donat-multi-source-pr.md
for section in Summary Architecture Security Compatibility Commands Deployment; do
  rg -q "^## $section" /tmp/donat-multi-source-pr.md
done
for backend in Postgres SQLite MySQL ClickHouse; do
  rg -q "$backend" /tmp/donat-multi-source-pr.md
done
for operation in AnalyticsDocumentDailyStats AnalyticsWorkflowExecutions AnalyticsErrors AnalyticsCodeLifecycleEvents AnalyticsAggregationOperations AnalyticsDashboardStats ApplicationLogsList DocumentIntegrationRequests L2JobEvents L2DeviceEvents L2TrafficLogs L2ProductionEvents; do
  rg -q "$operation" /tmp/donat-multi-source-pr.md
done
```

Expected: every validation command exits 0.

- [ ] **Step 7: Push and create the PR**

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

- [ ] **Step 8: Verify remote state**

Run:

```bash
gh pr view --repo donatlabs/donat --json number,url,state,headRefName,baseRefName
gh pr checks --repo donatlabs/donat --watch
```

Expected: PR is open from the requested feature branch to `main`; required CI checks are green.
