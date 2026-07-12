---
type: decision
status: accepted
date: 2026-07-12
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

Query objects use a collision-safe native JSON construction. GraphQL keys are
JSON-escaped fixed SQL literals, each value is rendered with `toJSONString`,
and the complete object text is cast to ClickHouse `JSON`. This avoids the
query-wide alias collisions of nested named tuples while preserving nested
objects as typed JSON rather than double-encoded strings. Lists are aggregated
as `Array(JSON)`. Ordered lists carry a `row_number()` ordinal into
`groupArray`, then sort and project the JSON values inside ClickHouse. Required
compatibility settings are sent as HTTP query parameters. The final GraphQL
data object is returned by one `SELECT` using `FORMAT TabSeparatedRaw`.

## Alternatives

| Option | Why Not |
|--------|---------|
| Dynamic `.so` or wasm plugin | Rust has no stable ABI, wasm complicates native HTTP/database access, and the project owns the supported backends. |
| Out-of-process NDC connector | Adds a network hop and rowset stitching and conflicts with the one-statement, database-side JSON assembly invariant established by ADR 001. |
| Add ClickHouse mutations immediately | ClickHouse mutations are asynchronous data-part rewrites and do not match the v2 insert/update/delete response and returning contracts. |
| Return concatenated JSON as plain strings | String-only assembly would double-encode nested arrays and objects. The selected construction only uses `concat` as an intermediate representation and immediately casts the complete value to native `JSON`; nested values therefore retain their JSON shape. This was verified against ClickHouse 25.8 LTS. |

## Consequences

Tracked ClickHouse tables can serve permission-filtered GraphQL reads through
the same planner and one-statement execution path as other sources. Deployment
remains a single binary, and HTTP URLs can carry the standard ClickHouse
database and basic-auth configuration.

ClickHouse support is intentionally read-only and does not expose relationships
or relay. It requires a ClickHouse version with the native `JSON` type (25.3 or
newer); the HTTP request enables the compatibility setting needed by 25.x.
HTTP and HTTPS are supported through rustls, requests have a five-minute
timeout, and streamed responses are bounded separately for catalog and data.
The catalog's remaining `pg_type` compatibility bridge is technical debt until
the planned logical scalar-type migration reaches catalog and IR.
