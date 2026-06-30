# API Surfaces — REST & MCP

> The engine serves the same per-role data plane over additional transports
> beyond GraphQL: Donat v2-style **RESTified endpoints** and a **Model
> Context Protocol (MCP)** server. Both reuse the existing
> GraphQL execution pipeline rather than re-implementing permissions or SQL.

**Status: in progress (June 2026).** Branched from the post-rename `main`
(see multi-backend R5 — all transports touch `crates/server`; they land as
separate branches and merge independently).

## Design Notes

- [[design]] — the two surfaces, the translate-to-GraphQL reuse strategy,
  request/response shapes, auth, and the conformance approach

## Decisions

- [[decisions/001-translate-to-graphql-over-direct-ir]] — REST and MCP build
  a GraphQL operation and run it through `execute_full`, instead of
  constructing IR directly
- [[decisions/002-mcp-generic-crud-tools-streamable-http]] — MCP exposes a
  small set of generic CRUD tools over streamable HTTP, not per-table tools
  and not stdio
- [[decisions/005-allowlist-and-jsonrpc-notification-semantics]] — why the
  query allowlist applies to REST/MCP without bypass, why `id`-less JSON-RPC
  requests are no-op notifications, and why a `:param`-match method mismatch
  is a correct `405`
- [[decisions/006-hasura-session-variable-compatibility]] — why `x-hasura-*`
  is accepted as a session-variable compatibility namespace without adding
  Hasura admin-secret or permission-bypass semantics

## Cross-cutting

- **No admin role.** Both surfaces require an explicit role
  (`X-Donat-Role`/JWT) exactly like GraphQL; a roleless trusted request is
  denied. Neither surface introduces a permission bypass or runtime admin
  API. See the BLOCKING RULE in `CLAUDE.md`.
