# ClickHouse Datasource Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a compiled-in, read-only ClickHouse datasource that can introspect tracked tables and execute GraphQL queries through the existing backend abstraction.

**Architecture:** Extend the existing `Dialect`, scalar mapping, and capability model with ClickHouse primitives. Add ClickHouse HTTP transport and `system.columns` introspection behind small testable parsing functions, then register the source in `AppState` alongside Postgres, SQLite, and MySQL. Keep one ClickHouse SQL statement per GraphQL operation and assemble response JSON inside ClickHouse.

**Tech Stack:** Rust, reqwest, serde_json, axum, ClickHouse HTTP interface, cargo tests, native conformance harness.

## Global Constraints

- Repository content is English.
- Every production behavior starts with a failing test and observed RED result.
- ClickHouse support is read-only; mutations and unsupported operators are not exposed.
- Every query remains one native SQL statement with database-side JSON assembly.
- User values enter SQL only through dialect quoting helpers.
- No admin role or permission bypass is introduced.
- Rebuild `donat-server` before running conformance tests.
- After every commit, run the repository judge gate and continue only after ACCEPT.

---

### Task 1: ClickHouse Backend Primitives

**Files:**
- Modify: `crates/backend/src/dialect.rs`
- Modify: `crates/backend/src/scalar.rs`
- Modify: `crates/backend/src/capabilities.rs`
- Modify: `crates/backend/src/lib.rs`

**Interfaces:**
- Consumes: `Dialect`, `AnyDialect`, `ScalarType`, and `Capabilities`.
- Produces: `ClickhouseDialect`, `AnyDialect::Clickhouse`, `clickhouse_scalar(&str)`, and `capabilities::clickhouse()`.

- [ ] **Step 1: Write failing backend tests**

Add tests proving ClickHouse identifier/literal escaping, pagination, scalar rendering, JSON object/array assembly, scalar type wrappers (`Nullable` and `LowCardinality`), and read-only capabilities.

- [ ] **Step 2: Run tests to verify RED**

Run: `cargo test -p donat-backend clickhouse`

Expected: compilation fails because ClickHouse backend symbols do not exist.

- [ ] **Step 3: Implement minimal backend primitives**

Implement the tested dialect methods, recursively unwrap ClickHouse type wrappers, map native types to logical scalar types, and delegate every `Dialect` method through `AnyDialect::Clickhouse`.

- [ ] **Step 4: Run backend tests to verify GREEN**

Run: `cargo test -p donat-backend clickhouse && cargo test -p donat-backend`

Expected: all backend tests pass.

- [ ] **Step 5: Commit and judge**

Commit only Task 1 files, then dispatch the mandatory judge review and continue only after ACCEPT.

---

### Task 2: ClickHouse Catalog Introspection

**Files:**
- Modify: `crates/catalog/Cargo.toml`
- Modify: `crates/catalog/src/lib.rs`

**Interfaces:**
- Consumes: ClickHouse `system.columns` rows encoded as `JSONEachRow`.
- Produces: `clickhouse_introspect(base_url: &str, database: &str) -> Result<Catalog, ClickhouseCatalogError>` and an internal pure row-to-`Catalog` parser.

- [ ] **Step 1: Write failing catalog parser tests**

Use representative `system.columns` rows to prove table grouping, column order, nullability from `Nullable(T)`, defaults, and primary-key column extraction.

- [ ] **Step 2: Run tests to verify RED**

Run: `cargo test -p donat-catalog clickhouse`

Expected: compilation fails because the parser and introspection entry point do not exist.

- [ ] **Step 3: Implement parser and HTTP introspection**

Issue one parameterized metadata query through the ClickHouse HTTP interface, request `JSONEachRow`, reject non-success status with bounded response text, and convert rows into the shared `Catalog`. Leave foreign keys and functions empty because ClickHouse does not expose relational FK enforcement or Donat-compatible table functions.

- [ ] **Step 4: Run catalog tests to verify GREEN**

Run: `cargo test -p donat-catalog clickhouse && cargo test -p donat-catalog`

Expected: all catalog tests pass.

- [ ] **Step 5: Commit and judge**

Commit only Task 2 files, then dispatch the mandatory judge review and continue only after ACCEPT.

---

### Task 3: Runtime Registration and Query Execution

**Files:**
- Modify: `crates/server/src/state.rs`
- Modify: `crates/server/Cargo.toml`
- Create: `crates/server/tests/clickhouse_runtime.rs`

**Interfaces:**
- Consumes: `ClickhouseDialect`, ClickHouse catalog introspection, source URL and database configuration.
- Produces: registered ClickHouse source state and HTTP query execution returning one JSON response object.

- [ ] **Step 1: Write failing runtime test**

Start a local axum HTTP stub that records ClickHouse requests, serves `system.columns` rows during `sync_sources`, serves a single JSON query result during execution, and assert that `kind: clickhouse` is retained and queried with exactly one SQL statement.

- [ ] **Step 2: Run test to verify RED**

Run: `cargo test -p donat-server --test clickhouse_runtime`

Expected: test fails because ClickHouse sources are skipped and query execution returns `NoDefaultSource`.

- [ ] **Step 3: Implement runtime registration**

Store ClickHouse source endpoints, call catalog introspection during `sync_sources`, render query SQL with `AnyDialect::Clickhouse`, POST it once to ClickHouse with an explicit output format, and map transport/status/JSON failures into the existing backend query error path without adding an admin bypass.

- [ ] **Step 4: Run runtime tests to verify GREEN**

Run: `cargo test -p donat-server --test clickhouse_runtime && cargo test -p donat-server --test mysql_runtime && cargo test -p donat-server --test sqlite_runtime`

Expected: ClickHouse and existing datasource runtime tests pass.

- [ ] **Step 5: Commit and judge**

Commit only Task 3 files, then dispatch the mandatory judge review and continue only after ACCEPT.

---

### Task 4: SQL Generation Contract and Conformance Coverage

**Files:**
- Modify: `crates/sqlgen/tests/pipeline.rs`
- Create: `crates/conformance/tests/clickhouse.rs`
- Create: `crates/conformance/fixtures/clickhouse/metadata.yaml`
- Create: `crates/conformance/fixtures/clickhouse/query.graphql`
- Create: `crates/conformance/fixtures/clickhouse/expected.json`

**Interfaces:**
- Consumes: ClickHouse dialect and registered datasource runtime.
- Produces: snapshot/unit coverage for generated SQL and a native harness case enabled by `CLICKHOUSE_URL`.

- [ ] **Step 1: Write failing SQL and conformance tests**

Assert one-statement SQL for filtered, ordered, paginated selection and exact GraphQL response/error shape through a real ClickHouse service when `CLICKHOUSE_URL` is configured.

- [ ] **Step 2: Run tests to verify RED**

Run: `cargo test -p donat-sqlgen clickhouse` and, with ClickHouse available, `cargo build -p donat-server --bin donat && cargo test -p donat-conformance --test clickhouse`.

Expected: failures identify remaining ClickHouse SQL/runtime incompatibilities.

- [ ] **Step 3: Implement only compatibility fixes exposed by RED**

Adjust dialect leaf rendering or source setup without weakening permissions, error contracts, or the one-statement invariant.

- [ ] **Step 4: Verify GREEN and regressions**

Run: `cargo test -p donat-sqlgen`, `cargo build -p donat-server --bin donat`, `cargo test -p donat-conformance --test clickhouse`, and `cargo test -p donat-conformance`.

Expected: all applicable suites pass; unavailable external ClickHouse is reported as an explicit test prerequisite, never a silent pass.

- [ ] **Step 5: Commit and judge**

Commit Task 4 files and fixes, then dispatch the mandatory judge review and continue only after ACCEPT.

---

### Task 5: Architecture Decision and Final Verification

**Files:**
- Create: `knowledgebase/multi-backend/decisions/005-clickhouse-read-only-datasource.md`
- Modify: `knowledgebase/multi-backend/design.md`
- Modify: `knowledgebase/multi-backend/_index.md`

**Interfaces:**
- Consumes: verified implementation constraints and trade-offs.
- Produces: accepted design record explaining why ClickHouse is now in scope as a compiled-in read-only analytical datasource.

- [ ] **Step 1: Write the ADR**

Record context, decision, alternatives, one-statement JSON assembly, HTTP transport, unsupported mutation/relationship semantics, and consequences using `knowledgebase/_templates/decision.md`.

- [ ] **Step 2: Update design navigation**

Remove the stale blanket “ClickHouse out of scope” statement and link the new ADR without rewriting unrelated historical decisions.

- [ ] **Step 3: Run final verification**

Run: `cargo fmt --check`, `cargo test -p donat-backend`, `cargo test -p donat-catalog`, `cargo test -p donat-sqlgen`, `cargo test -p donat-server`, `cargo build -p donat-server --bin donat`, and `cargo test -p donat-conformance`.

Expected: formatting and all applicable tests pass.

- [ ] **Step 4: Commit and judge**

Commit documentation, dispatch the mandatory judge review, and finish only after ACCEPT.
