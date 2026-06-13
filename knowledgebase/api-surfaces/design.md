---
type: design
status: accepted
date: 2026-06-13
features:
  - "[[api-surfaces]]"
---

# REST & MCP API Surfaces — Design

## Goal & scope

Serve the engine's per-role data plane over two transports in addition to
GraphQL:

1. **REST** — Donat v2 **RESTified endpoints**: metadata (`rest_endpoints`)
   maps an HTTP method + URL template to a *saved* GraphQL operation stored
   in `query_collections`. This is the authentic v2 surface (not generic
   auto-CRUD), so exported v2 metadata that declares REST endpoints loads
   without conversion.
2. **MCP** — a Model Context Protocol server over **streamable HTTP** at
   `/mcp`, exposing a small set of **generic CRUD tools** (`query`,
   `insert`, `update`, `delete`) plus discovery (`list_tables`,
   `describe_table`), for LLM clients.

Both run inside the existing axum server process and share its state, pools,
auth, and the engine metadata/catalog.

## Key decision: translate to GraphQL, reuse the pipeline

Neither surface re-implements permissions, filtering, type checking, or SQL
generation. Each request is turned into a **GraphQL operation** and executed
through the existing `gql::execute_full(state, session, body, ...)`:

- **REST**: the endpoint's saved query is the operation; URL path params,
  query string, and JSON body supply the GraphQL **variables**.
- **MCP**: each tool call is rendered into an equivalent GraphQL query or
  mutation (e.g. `insert` → `insert_<table>(objects: ...) { returning {...} }`)
  and executed.

Consequence: one execution path, one set of error shapes, the M4
single-statement invariant, remote joins, allowlist, and per-role
permissions all apply automatically — and conformance parity with GraphQL is
structural, not re-tested behaviour. Rationale and the rejected
"build IR directly" alternative:
[[decisions/001-translate-to-graphql-over-direct-ir]].

## REST surface

**Metadata.** A top-level `rest_endpoints` list (v2 shape):

```yaml
- name: get_pet_by_id
  url: pet/:id                 # ':id' is a path variable
  methods: [GET]
  definition:
    query:
      collection_name: pet_queries
      query_name: PetById      # resolves to a CollectionQuery in query_collections
  comment: optional
```

**Routing.** Served under `/api/rest/<url>`. At boot we resolve each
endpoint's saved query text from `query_collections` and validate it parses;
a route table maps (method, url-template) → resolved operation. URL templates
support `:param` path segments.

**Variable binding.** GraphQL variables are filled by name with precedence
**path > query string > JSON body** (Donat v2 behaviour). A declared GraphQL
variable with no supplied value falls back to its default; a required
variable left unbound is an error.

**Response.** The GraphQL `data` object is returned directly as the JSON body
(not wrapped in `{ "data": ... }`). GraphQL errors map to the Donat v2 REST
status/shape. Method mismatch → 405; unknown endpoint → 404.

**Auth.** `resolve_session` exactly as GraphQL: role required.

## MCP surface

**Transport.** Streamable HTTP at `/mcp` (JSON-RPC 2.0 over POST, with the
optional SSE response stream), in-process, sharing the axum server and its
auth. Chosen over stdio because the engine is a server, not a desktop
sidecar — same process, same `X-Donat-Role`/JWT auth. See
[[decisions/002-mcp-generic-crud-tools-streamable-http]].

**Tools (generic, table-parameterized):**

| Tool | Maps to GraphQL |
|---|---|
| `list_tables` | metadata/catalog enumeration for the caller's role |
| `describe_table` | columns/relationships the role may see |
| `query` | `<table>(where, order_by, limit, offset) { columns }` |
| `insert` | `insert_<table>(objects) { returning }` |
| `update` | `update_<table>(where, _set) { returning }` |
| `delete` | `delete_<table>(where) { returning }` |

Generic over a `table` argument rather than one tool per table: far less
generated surface, and the role's permissions still gate every call (a table
the role can't touch simply errors through the normal permission path).

**Auth.** The MCP HTTP request carries the same headers; `resolve_session`
applies. No tool can bypass a role.

## Conformance

TDD-first, same harness. Two extensions to `crates/conformance`:

1. **REST**: fixtures gain an HTTP `method` and a templated `url` plus
   optional query/body; the harness issues the real method and compares the
   (unwrapped) JSON body + status. New fixtures under `fixtures/rest/`, driver
   `tests/rest_endpoints.rs`.
2. **MCP**: fixtures express a JSON-RPC call to `/mcp` (`tools/list`,
   `tools/call`) and compare the JSON-RPC result. New fixtures under
   `fixtures/mcp/`, driver `tests/mcp_tools.rs`.

Each engine-behaviour change still starts from a failing conformance case;
the full conformance crate must stay green after rebuilding the `donat`
binary.

## Examples

The `examples/petshop` project gains `query_collections.yaml` +
`rest_endpoints.yaml` (e.g. `GET pet/:id`, `GET pets`, `POST pet`) and README
notes showing REST calls and how to point an MCP client at `/mcp`.

## Risks

- **R1 — variable binding fidelity.** Path/query/body precedence and
  type coercion (query strings are stringly-typed) must match v2. Covered by
  conformance fixtures mined from v2 behaviour.
- **R2 — MCP spec churn.** Pin a protocol version; prefer a maintained Rust
  MCP SDK (`rmcp`) over hand-rolling, but keep the tool layer SDK-agnostic.
- **R3 — server coupling (multi-backend R5).** Both surfaces touch
  `crates/server`; keep them on this branch and merge independently of the
  multi-backend execution-dispatch work.
