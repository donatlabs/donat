---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[api-surfaces]]"
---

# Field descriptions come from metadata `column_config.<col>.comment`

## Context

Tracked-table fields had no descriptions anywhere: GraphQL introspection
emitted `"description": null` for every field/type, and the MCP
`describe_table` tool returned only `{name, type, nullable}` per column. Users
want to document each column so the description shows up in tooling (GraphQL
introspection clients, MCP `describe_table`). Donat/Hasura v2 already carries
a per-column `comment` in table metadata under
`configuration.column_config.<column>.comment` (the engine's
`TableConfiguration.column_config` field already exists, but was an untyped
`serde_json::Value` and unused).

Separately, Postgres column COMMENTs (`col_description`) could be a source too,
but the engine's catalog does not introspect them and that is a larger change.

## Decision

Field descriptions are sourced from **metadata
`configuration.column_config.<column>.comment`** — the v2-idiomatic,
YAML-editable place — and surfaced in two places:

- **GraphQL introspection**: a column's `comment` becomes its object-type
  field `description` (replacing the hardcoded `null`).
- **MCP `describe_table`**: each column entry gains a `description` field.

`column_config` is given a typed `ColumnConfig { custom_name, comment }`
(preserving any unknown keys for lossless round-trip), replacing the untyped
`Value`. Absent config / absent comment ⇒ `null` description (unchanged
behaviour).

Postgres-comment introspection (`col_description`/`obj_description`) is **out
of scope here**; if added later it becomes a fallback when no metadata comment
is set (metadata wins, matching Hasura's precedence).

## Alternatives

| Option | Why Not |
|--------|---------|
| Source from Postgres COMMENTs only | Requires catalog introspection changes (new `col_description` queries + `ColumnInfo.comment`); not YAML-editable, which is what the user asked for. Deferred as a future fallback. |
| A new bespoke per-column `description` metadata field | Diverges from v2; exported Hasura metadata uses `column_config.comment`. Reuse the existing field. |
| Keep `column_config` untyped and dig out `comment` ad hoc | Stringly-typed access at each call site; a small typed struct is clearer and round-trip-tested. |

## Consequences

- **Gain:** documenting a field is a one-line YAML edit
  (`column_config.<col>.comment`); the text flows to GraphQL introspection and
  MCP discovery through one source of truth. v2-faithful.
- **Pay:** only columns with a metadata comment are documented (no automatic
  pull from existing DB COMMENTs yet). Typing `column_config` is a small
  metadata-shape change covered by round-trip tests.
- **Boundary:** descriptions are presentation only — they never affect
  permissions or SQL.
