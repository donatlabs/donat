---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[api-surfaces]]"
---

# REST and MCP translate to GraphQL, not directly to IR

## Context

The IR (`crates/ir`) and `sqlgen` are transport-neutral: a REST handler or
MCP tool could, in principle, construct a `SelectQuery`/`InsertMutation`
directly and call `operation_to_sql_opts`, skipping GraphQL parsing. The
planner (`crates/schema`) is what applies per-role permissions (column masks,
row filters, limits), resolves relationships, the allowlist, remote joins,
and the exact Donat v2 error shapes — all keyed off a parsed GraphQL
operation.

## Decision

REST and MCP build a **GraphQL operation** and execute it through the
existing `gql::execute_full`, rather than constructing IR directly.

- REST: the endpoint's saved query (from `query_collections`) is the
  operation; the request supplies its variables.
- MCP: each CRUD tool call is rendered into the equivalent GraphQL
  query/mutation string with variables.

This keeps a single execution path. Permissions, filtering, type checking,
the allowlist, remote joins, the M4 single-statement invariant, and the
v2 error/`code`/`path` contract are all inherited rather than reproduced.
Conformance parity with GraphQL becomes structural — the new surfaces are
thin adapters over a path that is already tested exhaustively.

## Alternatives

| Option | Why Not |
|--------|---------|
| Build IR directly from REST/MCP inputs | Would duplicate the planner's permission application, relationship resolution, allowlist, and error-shape logic in two more places — three code paths to keep conformant instead of one. High risk of subtle divergence (an error code or row filter applied in GraphQL but not REST). |
| A shared "neutral request" layer below GraphQL that all three target | Real long-term shape, but it means extracting the planner's entry point into a transport-neutral API now. Premature: GraphQL operation text is already that neutral contract, and v2 REST is *defined* in terms of saved GraphQL queries. Revisit if a surface needs something GraphQL can't express. |

## Consequences

- **Gain:** minimal new code; automatic conformance with the permission and
  error model; v2 `rest_endpoints` semantics (saved query + variables) map
  one-to-one.
- **Pay:** a GraphQL parse per request on each surface (REST can cache the
  parsed saved query at boot; MCP renders a string then parses it). The MCP
  tool→GraphQL renderer is an extra translation layer to test, but it is pure
  and unit-testable in isolation.
- **Boundary to watch:** if a future surface needs behaviour GraphQL can't
  express, that is the trigger to extract a neutral planner entry point
  (the rejected alternative #2).
