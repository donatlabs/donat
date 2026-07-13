---
type: decision
status: accepted
date: 2026-07-12
features:
  - "[[multi-backend]]"
---

# MySQL assembles ordered JSON text in the database

## Context

Donat's conformance contract preserves GraphQL selection order inside response
objects. MySQL's native binary `JSON` representation does not preserve the
argument order passed to `JSON_OBJECT`; it canonicalizes object keys (including
sorting by key length). Casting that value back to text therefore returns a
different field order even though the JSON object is semantically equivalent.

The shared permission matrix also exposed that MySQL reports `BOOLEAN` as
`tinyint(1)` and native `JSON_OBJECT` emits those values as `0`/`1`, while the
GraphQL type and response contract require `Boolean` and `true`/`false`.

## Decision

MySQL query SQL assembles the response as ordered JSON **text** inside the
database. SQLgen serializes scalar values by logical type (`JSON_QUOTE` for
strings, JSON literals for booleans, raw text for numbers and JSON columns),
`MySqlDialect::json_object` concatenates keys and serialized values in
selection order, and `json_array_agg` uses `GROUP_CONCAT` to preserve requested
row order. The runtime still executes one SQL statement and only parses its
single JSON-text result.

MySQL introspection maps `tinyint(1)` to the logical `bool` type so the same
type information drives GraphQL schema generation, predicates, and output
serialization. The existing runtime session raises `group_concat_max_len` to
its maximum before query and mutation execution.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep `JSON_OBJECT` / `JSON_ARRAYAGG` | MySQL binary JSON irreversibly canonicalizes object keys and emits booleans as numbers. |
| Reorder decoded JSON in Rust | It moves response assembly out of the database and creates a backend-specific post-processing path. |
| Accept a MySQL known difference | Field order and GraphQL Boolean shape are exact shared API contracts, not optional capabilities. |
| Encode each row as native JSON, then concatenate | Casting native objects to text has already lost selection order. |

## Consequences

MySQL now returns the same field order and scalar JSON shapes as Postgres and
SQLite while preserving the one-statement read invariant. Nested objects and
arrays remain database-assembled and are not double-encoded. The cost is more
verbose generated SQL and reliance on `GROUP_CONCAT`; every MySQL runtime
connection must keep the configured large `group_concat_max_len`, and new
logical scalar types need an explicit JSON-text serialization decision.
