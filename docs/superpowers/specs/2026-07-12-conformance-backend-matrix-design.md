# Conformance Backend Matrix Design

**Date:** 2026-07-12
**Status:** Approved

## Goal

Run the same main conformance behavior against every registered datasource
backend. Postgres remains the default local target and the complete reference
implementation. CI runs one isolated job per backend in parallel.

Adding a backend must automatically enroll it in the conformance contract. A
backend cannot be considered supported while its CI matrix leg or capability
classification is missing.

## Decisions

### One selected backend per process

`CONF_BACKEND` selects one backend for a conformance invocation. It defaults
to `postgres`, preserving `make conformance` and direct `cargo test` behavior.
CI fans out independent invocations for `postgres`, `sqlite`, `mysql`, and
`clickhouse`. This gives each backend an isolated service lifecycle, timeout,
log, and required status check.

The CI matrix uses `fail-fast: false`: one backend failure must not cancel the
remaining evidence.

### Authoritative backend registry

The conformance crate owns a registry whose entries contain:

- stable backend id and `SourceKind`;
- connection/environment requirements;
- isolated database lifecycle and metadata connection configuration;
- schema, seed, and teardown execution;
- advertised capabilities;
- CI service configuration key.

Selection and CI validation derive from this registry. Tests fail when a
registered `SourceKind` is absent from the conformance registry or the CI
matrix.

### Shared cases with explicit applicability

Main behavior cases are single-sourced. A case declares the capabilities it
requires, such as reads, mutations, relationships, aggregates, JSON, geo,
subscriptions, triggers, or migrations.

For every selected backend a case has exactly one result:

- `passed`;
- `unsupported-by-capability`, with a registry capability as evidence;
- `known-diff`, with a tracked reason;
- `failed`.

There are no silent omissions. `#[ignore]`, early success when a configured
database is unavailable, or filename-based backend filtering are not valid
matrix outcomes.

ClickHouse is read-only. Mutation cases are explicitly unsupported, while
applicable read, permission, aggregate, and introspection cases must run.

### Neutral setup first

Metadata operations remain shared. Schema and seed setup are represented as
neutral typed tables and rows where practical, then rendered/executed by the
selected backend adapter. Existing SQL setup fixtures may use explicit
backend overrides while they are migrated. Missing an applicable override is
a failure, not a skip.

Backend-specific driver, transport, migration, and trigger tests remain
separate from the shared behavior matrix. Their classification is explicit in
the suite manifest.

### Reporting and enforcement

Each invocation emits a backend summary:

`passed / unsupported / known-diff / failed`

CI uploads test output on failure and exposes one required check per backend.
The workflow has a final matrix-gate job so branch protection can require a
stable check name while still retaining individual backend diagnostics.

## Local and CI Commands

```text
make conformance
CONF_BACKEND=postgres cargo test -p donat-conformance
CONF_BACKEND=sqlite cargo test -p donat-conformance
CONF_BACKEND=mysql cargo test -p donat-conformance
CONF_BACKEND=clickhouse cargo test -p donat-conformance
```

Postgres is the only default local target. The full database set is mandatory
in CI.

## Rollout

1. Introduce and test backend selection, registry completeness, capability
   classification, and strict service availability.
2. Generalize suite database lifecycle and setup execution.
3. Move the core read behavior to shared neutral fixtures, followed by
   mutations and capability-specific groups.
4. Replace dedicated duplicate coverage with shared cases; retain only true
   backend-specific tests.
5. Add the parallel CI matrix and matrix completeness guard.
6. Run Postgres completely and every applicable suite on all other backends.

At every rollout point Postgres conformance remains green. A temporary
classification must be explicit and counted; the final matrix contains no
unexplained skips.

## Invariants

- No admin role or permission bypass.
- Exact Donat response and error shapes remain the contract.
- One SQL statement per operation, subject only to documented backend ADRs.
- Shared requests and expected responses are not copied per backend.
- Snapshot changes are reviewed individually.
