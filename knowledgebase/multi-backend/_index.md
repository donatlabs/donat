# Multi-Backend Data Sources

> Let a tracked data source run on a database other than Postgres (SQLite,
> MySQL/MariaDB, SQL Server) and serve it over the same GraphQL surface.
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
- [[decisions/006-mandatory-conformance-backend-matrix]] — every registered
  datasource backend runs the same applicable conformance cases in an isolated
  CI matrix job; Postgres remains the default local and reference target.

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
