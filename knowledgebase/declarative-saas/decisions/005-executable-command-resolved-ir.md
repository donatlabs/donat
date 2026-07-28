---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Executable commands use a resolved SQL-free IR

## Context

The initial command planner emitted raw `metadata::Command`, GraphQL argument
JSON, and a client result projection. That representation is sufficient for
schema generation but not for execution. A safe Postgres renderer also needs
the concrete types of target columns, the explicit role's row filters and
checks, precompiled Rule expressions with closed bindings, and the resolved
session values that an idempotency scope declares.

Letting SQLgen reconstruct those facts would require it to inspect mutable
metadata, the catalog, request headers, or Rule source at execution time. That
would cross the SQL-free planner boundary, make permission enforcement depend
on an implicit lookup, and allow raw command metadata to become a second SQL
input language.

## Decision

The request planner lowers a validated command into a compact, source-local
execution IR before SQLgen is called. The IR contains only facts needed by the
renderer: tracked relation and column SQL types, typed resolved values,
operation-specific role filters/checks, and safe pre-lowered Rule expressions
or closed Rule bindings. It also contains only declared idempotency key/scope
inputs after session substitution and a canonical, redacted-safe representation
used to derive the scope and input fingerprints.

SQLgen consumes that immutable execution IR. It does not parse command YAML,
consult a catalog or Rules catalog, read a request/session, or resolve a role.
All command identifiers still originate in deploy-time validated metadata and
all request values still pass the existing SQL literal escaping helpers. The
renderer remains responsible only for the one-statement Postgres CTE layout
and JSON projection.

## Alternatives

| Option | Why not |
| --- | --- |
| Pass raw command metadata to SQLgen and re-derive execution facts | requires prohibited runtime metadata/catalog/Rules lookup and leaves authorization facts implicit |
| Add a `donat-rules` dependency to SQLgen | creates a dependency cycle because Rules already use SQLgen's escaping API, and still leaves session/catalog resolution in the wrong layer |
| Store SQL fragments in metadata | creates an arbitrary SQL surface and bypasses deploy-time type and permission validation |
| Reuse generic CRUD mutation IR directly | loses command step ordering, prior-step value references, declared error paths, and idempotency semantics |

## Consequences

The command planner and IR gain narrowly typed execution structures and
planner tests. SQLgen becomes simpler to audit because it receives no raw
metadata or ambient runtime capability. The new IR must remain source-local,
serializable for tests, and closed over trusted planner output; it must not
grow an admin role, runtime DDL, process effect execution, or a general SQL
escape hatch.
