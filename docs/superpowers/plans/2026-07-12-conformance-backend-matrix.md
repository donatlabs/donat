# Conformance Backend Matrix Implementation Plan

> Execute conformance-first. Rebuild `donat` before every conformance run and
> dispatch the judge after every commit.

**Goal:** Make every registered datasource backend run the same applicable
main conformance cases, with Postgres as the local default and an isolated,
parallel, mandatory CI job per backend.

**Decision:** `knowledgebase/multi-backend/decisions/006-mandatory-conformance-backend-matrix.md`

## Task 1: Backend registry and selection contract

- Add failing tests for default Postgres selection, supported
  `CONF_BACKEND` values, invalid values, and registry coverage of every
  `SourceKind`.
- Add `BackendId`, capability requirements, and the authoritative registry.
- Add strict mode: an explicitly selected external backend must fail when its
  service is unavailable.
- Verify `cargo test -p donat-conformance --lib`.

## Task 2: Backend-owned suite lifecycle

- Add adapter tests for unique database names, source metadata, setup
  execution, and cleanup.
- Move Postgres creation/PostGIS setup behind `PostgresTarget` without
  behavior changes.
- Implement in-process `SqliteTarget`.
- Implement service-backed `MysqlTarget` and `ClickHouseTarget`.
- Make `Suite::start` select the target and produce backend-correct metadata.
- Verify lifecycle tests against each configured backend.

## Task 3: Neutral schema and seed model

- Add failing rendering/execution tests for representative scalar, JSON,
  nullable, primary-key, and relationship schemas.
- Add neutral table/column/key/row setup types.
- Implement DDL and typed insert execution in each adapter.
- Support explicit per-backend SQL overrides for non-neutral legacy setup.
- Reject a missing applicable override.

## Task 4: Case manifest and capability accounting

- Add tests proving every main suite/case is classified and no case can be
  silently skipped.
- Add required-capability declarations and known-difference records.
- Emit deterministic per-backend counts and fail on any unclassified case.
- Replace ignored/early-return service tests with strict matrix semantics.

## Task 5: Shared core behavior

- Create neutral core fixtures for introspection, ordered list, by-pk,
  filters, pagination, aggregate nodes/count, permissions, and exact errors.
- Run these fixtures unchanged on Postgres, SQLite, MySQL, and ClickHouse.
- Remove duplicate ClickHouse/MySQL/SQLite assertions once equivalent shared
  coverage is green.

## Task 6: Shared write and extended behavior

- Move insert/update/delete cases to the shared matrix for mutable backends.
- Classify ClickHouse writes as unsupported by its read-only capability.
- Move relationships, JSON, REST/MCP, websocket/subscription, migration, and
  trigger suites where their backend contracts apply.
- Keep truly backend-specific lifecycle/driver tests outside the shared set
  and list them in the manifest.

## Task 7: Mandatory parallel CI matrix

- Split unit tests from conformance jobs.
- Add `postgres`, `sqlite`, `mysql`, and `clickhouse` matrix entries with
  `fail-fast: false` and isolated service startup.
- Run `CONF_BACKEND=${{ matrix.backend }}` in every leg.
- Add a test that compares registry ids to workflow matrix ids.
- Add a stable final `Conformance matrix` gate job.

## Task 8: Full verification

- Run formatting and all workspace unit/snapshot tests.
- Run complete Postgres conformance after rebuilding the engine.
- Run every applicable conformance group on SQLite, MySQL, and ClickHouse.
- Confirm reports contain no unclassified or unexplained skipped cases.
- Review snapshots, workflow, and all diffs; dispatch final judge review.
