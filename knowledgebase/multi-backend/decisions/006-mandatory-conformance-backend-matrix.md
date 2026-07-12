---
type: decision
status: accepted
date: 2026-07-12
features:
  - "[[multi-backend]]"
---

# Mandatory conformance backend matrix

## Context

The conformance harness creates Postgres databases directly and the full
Donat-derived fixture suite only runs against Postgres. SQLite, MySQL, and
ClickHouse have separate runtime tests, some of which are ignored or return
success when an external service is unavailable. This does not prove that a
new backend preserves the shared GraphQL contract and makes it easy to forget
coverage when another datasource kind is added.

Postgres must remain the fast, complete local reference. CI must nevertheless
provide mandatory evidence for every supported database, including explicit
accounting for capabilities a backend does not implement.

## Decision

The conformance crate owns an authoritative backend registry. A single test
process selects one entry through `CONF_BACKEND`, defaulting to Postgres. Main
behavior cases are single-sourced and either run on the selected backend or
produce a counted `unsupported-by-capability` or tracked `known-diff` result.
Silent skips and successful early returns for unavailable configured services
are forbidden.

CI runs one isolated matrix job per registered backend with `fail-fast: false`.
Registry and workflow completeness are tested so adding `SourceKind` support
without a conformance adapter and CI leg fails. Postgres is the default local
target; SQLite, MySQL, and ClickHouse are mandatory CI targets. The detailed
design and rollout are in
`docs/superpowers/specs/2026-07-12-conformance-backend-matrix-design.md` and
`docs/superpowers/plans/2026-07-12-conformance-backend-matrix.md`.

## Alternatives

| Option | Why Not |
|--------|---------|
| Start every database in one CI job | Failures, timeouts, resources, and logs are coupled; one broken service obscures evidence from the others. |
| Generate a Rust test for every case/backend pair | It creates substantial macro and compilation complexity before the shared setup model is stable. |
| Keep backend-specific integration tests only | Coverage drifts and new backends do not automatically inherit the main behavior contract. |
| Run all backends by default locally | It requires external services for routine development and makes the normal TDD loop unnecessarily slow. |

## Consequences

Every supported backend has an independently visible required CI result and
inherits all applicable main cases. Adding a backend requires a lifecycle
adapter, capability declaration, and matrix entry. The harness must gain a
neutral schema/seed representation plus explicit overrides for legacy
backend-specific DDL. CI uses more parallel compute, and capability reporting
becomes part of the test infrastructure rather than informal documentation.
