---
type: design
status: accepted
date: 2026-06-13
features:
  - "[[multi-backend]]"
---

# Multi-Backend Data Sources — Design

## Goal & scope

Add the ability to register data sources backed by databases **other than
Postgres** and serve them over the existing GraphQL surface. A user's tracked
tables may live in SQLite, MySQL/MariaDB, or SQL Server; the engine's own
operation and the default source stay on Postgres.

This is the heterogeneous multi-source model (already present in metadata as
`SourceKind`, currently `Postgres`-only): each source declares a backend
kind, and catalog introspection + SQL generation + execution dispatch **per
source's backend**. Postgres becomes one backend among several — and remains
the system/default source and the conformance reference.

**In scope:** the SQL family — SQLite (first), MySQL/MariaDB, SQL Server.
**Hard requirement:** every backend runs in the conformance harness — the
same GraphQL cases validated against each database (see "Conformance
matrix").

**Out of scope (for now):** analytical engines (ClickHouse, BigQuery) and
non-SQL stores (MongoDB) — different data/execution models, deferred until
the SQL family is proven. Also out of scope: an out-of-process NDC-style
connector protocol (see [[decisions/001-in-process-backend-trait-over-ndc]]).

**Direction note:** this reverses the README's "Not planned (by design):
Non-Postgres backends". The roadmap must be updated when implementation
starts. It realizes PLAN.md's stated seam — *"a second data backend, if ever
needed, implements the IR instead of rewriting the engine."*

## Key constraint: performance

Minimal latency is a first-class requirement. That decides the module
boundary in favour of an **in-process dialect trait** over an out-of-process
protocol: it preserves the M4 invariant (**one SQL statement per operation,
response JSON assembled inside the database**) and adds zero IPC hop or
IR/rowset (de)serialization per request. Rationale and the rejected NDC
alternative: [[decisions/001-in-process-backend-trait-over-ndc]].

## Architecture — the abstraction boundary

A new abstraction crate (working name `donat-backend`; final names follow the
in-flight rename) defines three things. Each concrete backend is its own
crate (`…-postgres`, `…-sqlite`, `…-mysql`, `…-mssql`), selected by
`SourceKind`.

- **`Backend` (trait)** — the source connection point:
  - `introspect() -> Catalog` — per-backend system-catalog queries producing
    the shared `Catalog` (today this is `crates/catalog`'s hardcoded
    `pg_catalog` queries; it becomes a backend method).
  - `dialect() -> &dyn Dialect`
  - `capabilities() -> Capabilities`
  - `execute(stmt) -> rows/json` — runs the single native statement.
- **`Dialect`** — only the dialect-specific rendering:
  - identifier quoting (`"…"` / `` `…` `` / `[…]`)
  - **JSON assembly**: `json_build_object`/`json_agg` ↔ `JSON_OBJECT`/
    `JSON_ARRAYAGG` ↔ `FOR JSON PATH` ↔ `json_object`/`json_group_array`
  - scalar literal rendering + casts (logical `ScalarType` → native)
  - upsert rendering
  - pagination (`LIMIT/OFFSET` ↔ `OFFSET … FETCH` / `TOP`)
  - comparison/extension operator rendering
  - relay cursor encode
- **`Capabilities`** — a per-backend feature descriptor (the mechanism
  borrowed from NDC). Schema generation and the planner both consult it, so a
  source only exposes what it actually supports.

`crates/sqlgen` today *is* the Postgres dialect. It splits into a
**backend-neutral IR → SQL assembler** that calls `Dialect` for every
dialect-specific fragment, with Postgres as one `Dialect` impl. The
single-statement invariant is preserved per source.

## IR de-leak

The IR (`crates/ir`) currently leaks Postgres in three places; each is
generalized so the IR is a genuinely backend-neutral contract.

1. **Types: `pg_type: String` → logical `ScalarType`.** Stringly-typed
   Postgres type names are threaded through ~60 sites (FieldValue,
   OutputField, AggregateColumn, etc.). Replace with a backend-neutral
   `ScalarType` (Int, BigInt, Float, Decimal, Bool, String, Uuid, Json,
   Timestamp, Date, Time, Bytes, plus capability types Geometry/Geography).
   Introspection maps native → logical at the boundary; `Dialect` maps
   logical → native casts. Native type names live only in catalog + dialect.

2. **Operators: split `CompareOp`.**
   - *Core (every SQL backend):* Eq/Neq, Gt/Gte/Lt/Lte, In/Nin, IsNull,
     Like/Ilike, Between, column-compare.
   - *Capability-gated extensions:* `JsonOps` (HasKey, Contains, `@>`, …),
     `GeoOps` (StOp, StDWithin).
   Schema-gen exposes an extension operator only if the source advertises it;
   the planner rejects an unsupported op (validation error); each `Dialect`
   renders only its own. jsonb/PostGIS stop being "always Postgres".

3. **Upsert: `OnConflict` → neutral `Upsert`.** Target columns/constraint +
   action (`ignore | update-set`). `Dialect` renders: PG `ON CONFLICT … DO
   UPDATE/NOTHING` + `EXCLUDED`; MySQL `ON DUPLICATE KEY UPDATE`; SQLite `ON
   CONFLICT …`; MSSQL `MERGE`. Capability `upsert: {none|ignore|update}`
   gates the `on_conflict` argument in the schema.

## Capabilities document

Per-backend descriptor consulted by schema generation and the planner:

| Field | Meaning / cross-backend variance |
|---|---|
| `comparison_operators` | core set + which extensions |
| `json_ops` | none / which json operators (PG jsonb, SQLite/MySQL json, MSSQL json) |
| `geo` | PostGIS geometry/geography — Postgres only for now |
| `upsert` | none / ignore / update |
| `returning` | PG/SQLite `RETURNING`; MySQL limited; MSSQL `OUTPUT` |
| `distinct_on` | Postgres-only; others omit/emulate |
| `lateral` | PG always; MySQL 8.0.14+; MSSQL `APPLY`; SQLite via correlated subqueries |
| `aggregates`, `nested_inserts` | feature presence |

This is the systematic cure for leakage: nothing Postgres-specific is
*assumed*; everything is *advertised*.

## Execution

Per-backend driver behind the `Backend` trait — heterogeneous drivers are a
detail, not a fork: Postgres keeps `tokio-postgres`/`deadpool`; SQLite uses
`rusqlite`/`sqlx`; MySQL `sqlx`/`mysql_async`; SQL Server `tiberius` (no
first-class `sqlx` support). Per-source pools already exist.

## Conformance matrix

The top priority: one set of GraphQL cases runs against **every** backend.

1. **Backend-parameterized suites.** The harness gains a target backend (PG,
   SQLite, MySQL, MSSQL); each suite runs once per backend (e.g.
   `CONF_BACKEND=sqlite`, or internal iteration). `Suite::start()` — which
   creates `conf_<name>` directly via the `postgres` crate today —
   generalizes to per-backend database/file creation. Metadata accumulation
   is already backend-neutral (`SourceKind`-tagged); the lazy engine spawn
   stays.
2. **Per-backend setup without N copies.** Primary path: express schema +
   seed in a **neutral form** (table tracking already in metadata; seed as
   typed rows), and let each backend's `Dialect` emit DDL/INSERT. Fallback:
   a per-backend setup override for cases needing backend-specific schema —
   exactly Hasura's own `setup.yaml` vs `setup_mssql.yaml` (such MSSQL
   variants already exist in our vendored fixtures and can be mined).
3. **Shared request/response + per-backend known-diffs.** The GraphQL request
   and expected JSON are single-sourced. Genuine differences (type rendering,
   error text) become a `# donat:`-commented known-diff. **Skips are explicit
   and counted — no silent omission.**
4. **Capability-driven auto-skip.** A case touching an unadvertised
   capability (geo on SQLite) auto-skips via the capability model. The report
   shows per backend: `X passed / Y unsupported-by-capability / Z known-diff`.
5. **CI.** SQLite is in-process (no service) → runs on every push. MySQL and
   SQL Server are CI service containers (like the current `postgis` service);
   the matrix fans out per backend. Postgres stays the 100% reference.
6. **First-backend gate.** Before MySQL/MSSQL: SQLite must pass the full
   cross-backend-applicable subset (everything except PG-only capabilities).
   That is the spike's exit criterion and the proof the abstraction is real.

## Phasing

Each phase is its own spec → plan → TDD cycle (conformance-first, judge after
every commit).

- **Phase 0 (prereq):** the `dist → donat` rename lands. Code starts after
  (avoids conflicts on `server`/`sqlgen`/`catalog`). Design proceeds now.
- **Phase 1 — abstraction seam, Postgres stays green.** Extract
  `Backend`/`Dialect`/`Capabilities`; refactor `sqlgen` (Postgres → one
  `Dialect`); de-leak IR; make introspection a backend method. **Exit: full
  Postgres conformance still 100% green, zero behavior change** (pure
  refactor — the biggest, riskiest step).
- **Phase 2 — SQLite + matrix.** Implement the SQLite backend (introspection
  via `sqlite_master`/pragma, json1 dialect, in-process driver); parameterize
  the harness; SQLite passes the shared subset. **Exit: proves the
  abstraction.**
- **Phase 3 — MySQL.** information_schema introspection, `JSON_OBJECT`/
  `JSON_ARRAYAGG`, lateral 8.0.14+, `ON DUPLICATE KEY` upsert; flag
  `RETURNING` limitations. CI container. `ndc-mysql` as a dialect reference
  (license checked first).
- **Phase 4 — SQL Server.** `FOR JSON PATH`, `APPLY`, `OUTPUT`, `MERGE`. CI
  container. Reuse Hasura's `*_mssql` fixtures for known-diffs.

## Risks

- **R1 — single-statement invariant per backend.** lateral / `RETURNING` /
  JSON-agg vary. The SQLite spike (Phase 2) is the early proof; where one
  statement is genuinely impossible for an operation shape, emulate or carry a
  per-backend known-diff/capability. Do not promise the matrix before Phase 2
  passes.
- **R2 — IR de-leak scope.** ~60 `pg_type` sites. Mitigate: Phase 1 is a pure
  refactor gated on unchanged Postgres conformance; introduce `ScalarType`
  alongside and migrate site-by-site; review every snapshot.
- **R3 — type-mapping fidelity** (MySQL `TINYINT(1)` = bool, MSSQL `bit`,
  date/time variants). Caught by conformance + known-diffs.
- **R4 — per-backend error shapes** (Hasura/Donat error text is Postgres-
  flavoured in places). Resolved via known-diffs; the matrix makes them
  explicit.
- **R5 — coupling with the rename and parallel REST/MCP work** (all touch
  `crates/server`). Sequence: rename → branch from post-rename → merge
  execution-dispatch and transports via branches.
- **R6 — CI cost.** SQLite on every push (fast); MySQL/MSSQL on a separate or
  nightly job plus pre-merge.

## Open questions (for the spike / implementation plan)

- Exact `ScalarType` set and the native↔logical mapping tables per backend.
- How neutral setup schema/seed is expressed and DDL-generated per backend.
- MySQL `RETURNING` strategy (version floor vs emulation) for mutation output.
- MSSQL single-statement feasibility for the heaviest nested-relationship +
  aggregate shapes (`FOR JSON` + `APPLY`).
- Whether to derive dialect code from Apache-2.0 `ndc-*` connectors (license
  must be confirmed per connector; they target the NDC IR, not our v2 IR — so
  reference, not drop-in).
