---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[api-surfaces]]"
---

# MCP exposes generic CRUD tools over streamable HTTP

## Context

The engine needs an MCP surface for LLM clients. Two axes had to be decided:
the **tool granularity** (one tool per table vs. generic tools parameterized
by table) and the **transport** (stdio vs. streamable HTTP). The engine is a
long-running server with header-based auth (`X-Donat-Role`/JWT) and a
per-role schema that can hold many tables.

## Decision

Expose a **small fixed set of generic, table-parameterized tools** —
`list_tables`, `describe_table`, `query`, `insert`, `update`, `delete` — over
**streamable HTTP** at `/mcp`, in-process in the existing axum server.

Generic tools take a `table` argument and render to the corresponding
GraphQL operation (see
[[decisions/001-translate-to-graphql-over-direct-ir]]). The role's
permissions gate every call: a table the role cannot access errors through
the normal permission path, so the tool set need not be regenerated per role
for safety (discovery tools still report only what the role may see).

Streamable HTTP keeps MCP in the same process and auth model as GraphQL and
REST — one deployment, one session-resolution path.

## Alternatives

| Option | Why Not |
|--------|---------|
| One tool per table (`query_pet`, `insert_pet`, …) | Tool count explodes with the schema; the list must be regenerated per role; more generated surface for an LLM to wade through. Generic tools + `describe_table` give the same power with a stable, tiny surface. |
| stdio transport (separate `donat-mcp` binary) | The engine is a server, not a desktop sidecar. stdio means a second process and a separate auth story (no HTTP headers). Streamable HTTP reuses `resolve_session` and the running server. A stdio shim can be added later as a thin client if a desktop use-case appears. |
| Read-only tools only | The user asked for full CRUD; mutations go through `insert_/update_/delete_` GraphQL, still fully permission-gated. |

## Consequences

- **Gain:** stable minimal tool surface; same process/auth as the rest of the
  data plane; CRUD without per-table generation.
- **Pay:** the generic tools need expressive enough arguments (`where`,
  `order_by`, `_set`, `objects`) to cover common cases; very table-specific
  ergonomics are traded for uniformity. `describe_table` compensates by
  letting the client learn columns/relationships at runtime.
- **Transport spec:** pin an MCP protocol version; prefer the maintained
  `rmcp` Rust SDK but keep the tool/translation layer SDK-agnostic so the
  transport can be swapped (R2).
