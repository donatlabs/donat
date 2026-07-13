---
type: decision
status: accepted
date: 2026-07-12
amended_by: "[[008-clickhouse-ordered-text-json-assembly]]"
features:
  - "[[multi-backend]]"
---

# ClickHouse as a compiled-in read-only datasource

## Context

The accepted multi-backend design originally deferred analytical engines such
as ClickHouse until the SQL-family abstraction had been proven. Postgres,
SQLite, and MySQL now exercise the shared catalog, dialect, SQL generation,
and runtime dispatch path. Users need tracked ClickHouse tables on the same
permission-aware GraphQL query surface.

ClickHouse differs from the existing OLTP backends: it exposes metadata through
`system.columns`, is commonly accessed through its HTTP interface, has no
Donat-compatible foreign-key or row-returning function catalog, and does not
offer the mutation semantics expected by the v2 GraphQL mutation surface. The
engine must still preserve one native statement per GraphQL query and assemble
the response inside the database.

## Decision

Add ClickHouse as a compiled-in, read-only datasource selected by
`SourceKind::Clickhouse`. It implements the existing in-process `Dialect` and
`Capabilities` boundary rather than introducing a dynamic plugin ABI or an
out-of-process connector protocol. The HTTP interface is the transport for
both `system.columns` introspection and query execution.

ClickHouse advertises flat reads and aggregates, but no mutations, upsert,
returning, distinct-on, relay, relationships, regex/JSON extension operators,
lateral, geo, or nested-insert capabilities. Planner and introspection consume
these capabilities, so unsupported fields and arguments are not exposed.
Introspection maps native ClickHouse types into the catalog's current
logical/Postgres-compatible type names while retaining the exact native type
for SQL casts. Foreign keys and functions remain empty.

Query objects use collision-safe ordered JSON-text construction. GraphQL keys
are JSON-escaped fixed SQL literals, scalar values use `toJSONString`, and
nested objects and lists remain serialized JSON text so ClickHouse cannot
canonicalize their field order. Lists use `groupArray` plus
`arrayStringConcat`; ordered lists carry a `row_number()` ordinal, then sort
and project row text inside ClickHouse. The final GraphQL data object is
returned by one `SELECT` using `FORMAT TabSeparatedRaw`. ADR 008 amends the
original native-`JSON` cast design after conformance exposed key reordering.

## Alternatives

| Option | Why Not |
|--------|---------|
| Dynamic `.so` or wasm plugin | Rust has no stable ABI, wasm complicates native HTTP/database access, and the project owns the supported backends. |
| Out-of-process NDC connector | Adds a network hop and rowset stitching and conflicts with the one-statement, database-side JSON assembly invariant established by ADR 001. |
| Add ClickHouse mutations immediately | ClickHouse mutations are asynchronous data-part rewrites and do not match the v2 insert/update/delete response and returning contracts. |
| Cast assembled responses to native `JSON` | ClickHouse canonicalizes object keys during the cast and violates GraphQL selection order; ADR 008 records the ordered-text replacement. |

## Consequences

Tracked ClickHouse tables can serve permission-filtered GraphQL reads through
the same planner and one-statement execution path as other sources. Deployment
remains a single binary, and HTTP URLs can carry the standard ClickHouse
database and basic-auth configuration.

ClickHouse support is intentionally read-only and does not expose relationships
or relay. The implementation targets ClickHouse 25.8 LTS and does not cast the
assembled response to the native `JSON` type. HTTP and HTTPS are supported
through rustls, requests have a five-minute timeout, and streamed responses are
bounded separately for catalog and data. The catalog's remaining `pg_type`
compatibility bridge is technical debt until the planned logical scalar-type
migration reaches catalog and IR.
