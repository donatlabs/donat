# Multi-Backend Data Sources

> Let a tracked data source run on a database other than Postgres (SQLite,
> MySQL/MariaDB, SQL Server, or read-only ClickHouse) and serve it over the same GraphQL surface.
> Postgres stays the system/default source and the conformance reference.

**Status: design accepted, implementation gated.** The engine is mid-rename
(`dist` → `donat`); multi-backend code starts only after that lands (it
touches `server`/`sqlgen`/`catalog`). Design captured 2026-06-13 so the
reasoning never has to be reconstructed.

## Design Notes

- [[design]] — full design: `Backend`/`Dialect`/`Capabilities` boundary,
  IR de-leak (`pg_type` → logical `ScalarType`, capability-gated jsonb/geo/
  upsert), the per-backend conformance matrix, phasing and risks.

## Decisions

- [[decisions/001-in-process-backend-trait-over-ndc]] — in-process dialect
  trait, NOT an out-of-process NDC-style protocol; performance (preserve the
  one-statement-in-DB invariant, zero IPC hop) is the deciding factor.
- [[decisions/005-clickhouse-read-only-datasource]] — ClickHouse uses the same
  compiled-in backend boundary and HTTP transport, with read-only capabilities
  and native database-side JSON assembly.
- [[decisions/006-mandatory-conformance-backend-matrix]] — every registered
  datasource backend runs the same applicable conformance cases in an isolated
  CI matrix job; Postgres remains the default local and reference target.
- [[decisions/007-mysql-ordered-text-json-assembly]] — MySQL assembles ordered
  JSON text in SQL because native binary JSON canonicalizes object keys and
  cannot preserve the GraphQL selection-order contract.
- [[decisions/008-clickhouse-ordered-text-json-assembly]] — ClickHouse keeps
  response objects and arrays as ordered JSON text because casting them to its
  native `JSON` type canonicalizes object keys.
- [[decisions/009-parallel-conformance-engine-startup]] — parallel conformance
  keeps its test-thread speed while retrying transient per-suite engine
  startup failures with RAII child cleanup and complete diagnostics.
- [[decisions/010-compose-metadata-sources-in-graphql]] — one GraphQL schema
  composes per-source planners, executes one statement per participating
  datasource, and merges only top-level response objects in source order.

## One-paragraph shape

```text
GraphQL → planner → IR (backend-neutral, capability-gated ops)
        → Backend(source.kind):
             Dialect renders IR → ONE native statement (JSON assembled in-DB)
             driver executes it (tokio-postgres | rusqlite | sqlx | tiberius)
        → envelope → response
Capabilities(source.kind) drive schema generation: a source only exposes
operators/features it actually supports (PostGIS geo stays Postgres-only).
```
