---
type: plan
status: active
date: 2026-06-13
features:
  - "[[multi-backend]]"
---

# Multi-Backend — Implementation Plan (Phases 1b, 2+)

Executor-ready, TDD-structured. Phase 1a (the `donat-backend` primitives
crate: `ScalarType` + `postgres_scalar`, `Capabilities` + `postgres()`,
`Dialect` + `PostgresDialect`) is **done and committed** (`9f0b54b`). This
plan covers wiring it in and the first non-Postgres backend.

**Hard prerequisite for ALL code below:** the `dist → donat` rename must have
landed on `main`. Every step here edits `crates/sqlgen` / `crates/catalog` /
`crates/schema` / `crates/conformance`, which the rename is rewriting. Rebase
`feat/multi-backend` onto post-rename `main` before starting. **STOP** and
rebase if `git diff main...feat/multi-backend` shows unrelated churn in those
files.

Every step is conformance-first per CLAUDE.md: rebuild the engine binary
(`cargo build -p donat-server --bin donat`) before running
`cargo test -p donat-conformance`, and dispatch the judge after every commit.

---

## Phase 1b — Route Postgres SQL generation through `Dialect` (pure refactor)

**Goal:** `crates/sqlgen` emits Postgres SQL by calling `PostgresDialect`
instead of its inline helpers, with **zero output change**. Exit criterion:
all `sqlgen` insta snapshots unchanged AND full Postgres conformance still
100% green.

### Step 1 — add the dependency, no behavior change
- Add `donat-backend` to `crates/sqlgen/Cargo.toml` deps.
- **Verify:** `cargo build -p donat-sqlgen` → exit 0.

### Step 2 — leaf helpers delegate to the dialect (TDD via existing snapshots)
The snapshot suite (`crates/sqlgen/tests/pipeline.rs`) is the characterization
test. It must stay byte-identical — that IS the red/green signal here.
- Replace the bodies of `sqlgen`'s `quote_ident`/`quote_lit` with calls to
  `PostgresDialect.quote_ident`/`quote_literal` (keep the free-function names
  as thin wrappers so call sites don't churn).
- Replace the LIMIT/OFFSET tail assembly with `PostgresDialect.limit_offset`.
- **Verify:** `cargo test -p donat-sqlgen` → all snapshots pass with NO
  `cargo insta` pending changes (`cargo insta pending-snapshots` empty). A
  changed snapshot here means the dialect is not byte-exact — fix the dialect,
  never accept the snapshot.

### Step 3 — extend `Dialect` with scalar-cast + JSON assembly, delegate
These are the remaining dialect-specific fragments in `sqlgen`. Add methods to
the `Dialect` trait and `PostgresDialect`, each replicating the current
Postgres output byte-for-byte, driven by the snapshot suite:
- `render_scalar(&Scalar, ScalarType) -> String` — mirror `sqlgen::scalar_sql`
  (`crates/sqlgen/src/lib.rs:1003`): `NULL`/`TRUE`/`FALSE`, `({n})::ty`,
  `(quoted)::ty`, geometry/geography object → `geometry_sql`. Requires
  `donat-backend` to depend on `donat-ir` for `Scalar`, and a
  `postgres_native(ScalarType) -> &str` inverse mapping for the cast type
  name. Geometry stays a Postgres capability.
- `json_object(pairs)` / `json_agg(expr)` / `json_array_*` — mirror the
  `json_build_object` / `json_agg` fragments.
- **TDD:** add unit tests in `donat-backend` asserting byte-exact output for
  each, written BEFORE moving the logic; then switch `sqlgen` to call them.
- **Verify:** `cargo test -p donat-backend` green; `cargo test -p donat-sqlgen`
  snapshots unchanged.

### Step 4 — full conformance gate
- `cargo build -p donat-server --bin donat && cargo test -p donat-conformance`
  → exit 0, every suite green. This proves the refactor changed no behavior.
- Commit. Dispatch judge.

**STOP conditions (Phase 1b):** any snapshot changes that you cannot trace to
a byte-difference bug in the dialect (do not accept it); any conformance
regression; the refactor needing to touch permission/planner logic (it should
not — this is rendering only).

---

## Phase 2 — SQLite backend + conformance matrix

**Goal:** a SQLite-backed source serves the same GraphQL surface; the
conformance harness runs the shared cases against SQLite and they pass on the
cross-backend-applicable subset. Exit criterion: SQLite green on the subset,
capability auto-skips counted.

### Step 1 — `SqliteDialect` + capabilities + scalar mapping (TDD, additive)
- In `donat-backend`: `SqliteDialect` impl of `Dialect` (idents `"…"`,
  literals `'…'`, `LIMIT/OFFSET` same; JSON via `json_object` /
  `json_group_array`; scalar casts via SQLite affinity — no `::type`, use
  `CAST(x AS …)` where needed). `sqlite()` `Capabilities` (json_ops: Json,
  geo:false, upsert:None (named-constraint upsert deferred by ADR 003),
  returning:true (3.35+),
  distinct_on:false, lateral:false (correlated subqueries), aggregates:true).
  `sqlite_scalar(native)` mapping (INTEGER/REAL/TEXT/BLOB/NUMERIC + declared
  types).
- **TDD:** unit tests for each rendering + mapping, written first. DB-free.

### Step 2 — `Backend` trait + dispatch
- Define the `Backend` trait in `donat-backend`: `introspect`, `dialect`,
  `capabilities`, `execute`. Implement `PostgresBackend` (wraps current
  catalog introspection + PostgresDialect + the existing pool) and
  `SqliteBackend` (introspect via `sqlite_master`/`pragma_table_info`,
  SqliteDialect, in-process `rusqlite`/`sqlx` driver).
- Dispatch by `metadata::SourceKind` (add `Sqlite` variant + its
  configuration). Move `crates/catalog`'s hardcoded pg_catalog queries behind
  `PostgresBackend::introspect`.
- **TDD:** catalog introspection has no current unit tests — add DB-backed
  introspection tests per backend (small fixture DB) before wiring.

### Step 3 — schema generation consults `Capabilities`
- Gate operator/feature exposure in `crates/schema` on the source's
  capabilities (geo only if `geo`, jsonb ops only per `json_ops`,
  `distinct_on` only if supported, `on_conflict` per `upsert`).
- **TDD:** DB-free planner tests (`crates/schema/tests/planner.rs` pattern)
  asserting a non-geo backend rejects `_st_*` with a field-not-found, and a
  no-distinct_on backend hides `distinct_on`.

### Step 4 — backend-parameterized conformance harness
- Add a target-backend dimension to `crates/conformance` (`CONF_BACKEND` env
  or per-backend iteration). Generalize `Suite::start()` to create a SQLite
  database/file as well as a Postgres `conf_<name>`.
- Express setup schema/seed in a neutral form and generate DDL/INSERT per
  backend via the dialect; fall back to per-backend setup overrides
  (mirroring Hasura's `setup.yaml` vs `setup_mssql.yaml`).
- Capability-driven auto-skip with an explicit, counted report
  (`X passed / Y unsupported / Z known-diff`).
- **Verify:** `CONF_BACKEND=sqlite cargo test -p donat-conformance` → the
  cross-backend subset green; Postgres run still 100%.

### Step 5 — CI
- Add a SQLite matrix leg (in-process, every push). Keep Postgres reference.
- Commit. Dispatch judge.

**STOP conditions (Phase 2):** a query shape that genuinely cannot be one
SQLite statement (revisit R1 — emulate or mark a per-backend known-diff, do
not silently drop); type-mapping mismatches that aren't covered by a counted
known-diff.

---

## Phases 3–4 (later)

MySQL (`information_schema`, `JSON_OBJECT`/`JSON_ARRAYAGG`, lateral 8.0.14+,
`ON DUPLICATE KEY`, `RETURNING` limits) and SQL Server (`FOR JSON PATH`,
`APPLY`, `OUTPUT`, `MERGE`) follow the same shape: new `…Dialect` +
`…Capabilities` + `…Backend` (all TDD), a CI service container, and the
existing `*_mssql` Hasura fixtures mined for known-diffs. Each is its own
spec → plan → TDD cycle and is gated on Phase 2 proving the matrix.

## Roadmap note

`README.md` lists "Not planned (by design): Non-Postgres backends". Once Phase
1b lands, update the roadmap to reflect the accepted direction (see the ADR).
Deferred here to avoid conflicting with the in-flight de-Hasura/rename edits to
`README.md`.
