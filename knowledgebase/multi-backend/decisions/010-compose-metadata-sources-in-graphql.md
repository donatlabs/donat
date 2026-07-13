---
type: decision
status: accepted
date: 2026-07-13
features:
  - "[[multi-backend]]"
---

# Compose metadata sources in one GraphQL surface

## Context

Hasura v2 metadata may declare multiple datasources whose permitted root fields
appear in one GraphQL schema. Donat already loads multiple sources and maintains
one catalog per source, but its planner, introspection, and executor select only
the source named `default`. A common deployment has Postgres as `default` and a
read-only ClickHouse source tracking tables from several ClickHouse databases.

The single-source planner contains mature permission, naming, capability, and
error-shaping behavior. Rewriting those rules into a new global planner would
duplicate security-sensitive logic. At the same time, a mixed-source operation
cannot satisfy the original literal interpretation of one SQL statement per
GraphQL operation because no database can execute a statement against another
datasource's connection.

## Decision

Compose one child planner per metadata source behind a `MultiSourcePlanner`.
The composite layer owns root-field discovery, GraphQL-compatible top-level
field collection and merging, source partitioning, composite introspection,
and deterministic response assembly. Each child planner remains responsible
for validation, permissions, IR, and source-local relationships.

A mixed read executes one database-assembled statement per participating
source and merges only the top-level data objects in Rust. This scopes the
one-statement invariant to each source while preserving no-N+1 behavior and
database-side row filtering and JSON assembly. Compatible repeated response
fields merge according to GraphQL field-selection rules; conflicts fail before
execution. Root `__typename` is represented as a source-less local response
slot. Mutations may target at most one datasource, preserving the existing
source transaction semantics without claiming distributed atomicity.

ClickHouse catalog discovery derives databases from tracked table metadata and
introspects them together. The URL database remains a fallback only when no
tables are tracked. This preserves Hasura metadata without deployment-specific
URL rewrites.

## Alternatives

| Option | Why Not |
|--------|---------|
| Flatten every source into one monolithic planner | Requires pervasive source indexes in permission, relationship, mutation, and introspection code and risks changing proven single-source semantics. |
| Select one datasource per request | Cannot execute valid Hasura operations containing roots from multiple sources and omits secondary roots from introspection. |
| Run one Donat instance per source behind a GraphQL gateway | Duplicates authentication and schema semantics, introduces another deployment component, and still needs correct GraphQL field merging. |
| Permit mutations across sources | Cannot provide a transaction across independent databases and would expose partial-commit behavior not represented by the current API. |

## Consequences

Multi-source reads preserve per-source permissions and SQL generation while
adding one bounded top-level merge per participating datasource. A mixed query
uses more than one native statement overall, but exactly one per source and no
per-row round trips. Cross-source relationships remain out of scope and must be
modeled as remote relationships. Root/type ownership conflicts become startup
errors instead of silently choosing one source. ClickHouse remains read-only,
and no admin role or permission bypass is introduced.
