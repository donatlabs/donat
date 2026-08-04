---
type: decision
status: accepted
date: 2026-08-01
features:
  - "[[api-surfaces]]"
---

# A compiled snapshot keeps what serving needs, in the form serving needs it

## Context

A compiled snapshot kept four rendered `__schema` documents for every role —
Relay and non-Relay, each in a `backend_request` variant — as
`serde_json::Value` trees, plus a role-independent template document covering
every tracked table with every column. On the Petshop example (133 catalog
tables, 74 tracked, 19 roles) that was the entire resident cost of the process:
150 MB RSS, 132 MB of it heap, against a 16 MB floor for the same engine
serving a small metadata directory.

Almost none of it was needed in that form. The schema builder never reads the
Relay flag — Relay decides how a query is *planned*, not how a schema is
*rendered* — so the Relay documents were verbatim copies. The two
`backend_request` variants differ only in whether a `backend_only` insert
permission is visible, and most metadata, Petshop included, has none. The
template was read only to answer three questions about field merging. And the
documents themselves are read only by an actual introspection query: field
merging uses the template, planning uses the source indexes, MCP and the
RESTified endpoints never introspect at all. Measured against content, the
representation cost roughly thirty times what it held — every node an
`IndexMap`, every type name, field name and JSON key its own allocation.

## Decision

Retain what the request path reads, in the cheapest form that answers it.

- Render a role's document once and share it. Relay slots are clones of the
  same `Arc`; the `backend_request` variant is rendered separately only when
  the metadata actually contains a `backend_only` insert permission.
- Keep documents serialized. Introspection — the one path that materialises a
  whole document — parses on demand; nothing else touches them.
- Replace the template with `ResponseShapeIndex`, which keeps exactly the three
  things field merging asks: whether a named type is an object, interface or
  union, which concrete types it can be, and what one of its fields returns.
  Types that cannot appear as a fragment condition are left out, which is how
  merging already treated them.
- Move rather than copy while composing: types are moved out of each rendered
  source schema, and the collision map indexes a position instead of holding a
  second copy of every type.
- Share compiled commands behind `Arc`. The same command was deep-copied into
  the finalized catalog, back out of it, and into the serving snapshot, each
  copy carrying the command's whole definition AST and two contract catalogs.
- Collapse a role's mutation-owner maps into one: command permission is decided
  by the role alone, and the roots it starts from are session-independent.

`distinct_schema_documents()` reports how many documents a snapshot retains, and
two tests pin it: metadata that cannot differ keeps one document per role
entry, and a `backend_only` insert permission still gets its own.

## Alternatives

| Option | Why Not |
| --- | --- |
| Deduplicate the four documents by content hash after rendering | Removes the copies but not the peak, and the peak is what the allocator holds. It also hashes every document at every boot to discover what the metadata already states. |
| Render variants lazily on first request | Moves a bounded, predictable deployment cost into request latency, and a role's first caller pays for a full render. |
| Cache the parsed document per role alongside the bytes | Buys back the introspection parse with the memory the change just removed, for a path used by developer tooling rather than by traffic. Worth revisiting only if introspection becomes hot. |
| A typed schema IR instead of `serde_json::Value` | Would also remove the parse cost, but touches every reader of a schema document. Storing the bytes captures most of the memory win without that surface. |
| Leave it and document the footprint | The cost scales with tables × roles, which is the direction the product grows — more modules, more roles, eventually multitenancy. |

## Consequences

Petshop's engine drops from 150 MB RSS / 132 MB heap to 60 MB / 43 MB, and boot
to first served request from 1.09 s to 0.66 s, because three quarters of the
rendering no longer happens. The full conformance suite, the workspace tests
and the black-box system suite pass unchanged, and that suite still sees
identical introspection on `/v1/graphql` and `/v1/relay`.

Serving speed is unchanged, measured with both binaries running at once and the
measurement alternating between them (`tests-system/compare-latency.sh`;
sequential runs drift enough to invent a 15% difference in either direction):
query p50 2.92 ms against 2.99 ms and p95 3.61 against 3.66, n=1000 each.

The one real cost is introspection: 3.0 ms to 4.7 ms per `__schema` query,
which is the parse. That path serves GraphiQL and code generation, not traffic
— MCP builds its tools without introspecting, and the RESTified endpoints never
touch a schema document.

What is left is no longer about schemas. Measured by removing one thing at a
time from the Petshop metadata (heap, after this change): a bare 74-table
snapshot with no rules, connectors, commands or processes is 23.8 MB; rules add
5.6 MB for 33 KB of YAML; connectors 1.2 MB; commands and processes about
12 MB. Roles are now free — 19 roles cost the same as 2. The next question is
what makes a tracked table cost ~290 KB, and it needs a heap profiler rather
than another round of reasoning from the source.

A caveat for future edits: the Relay slots being clones is only sound while the
schema builder ignores the Relay flag, and the single mutation-owner map only
while command permission ignores `backend_request`. Anything that changes
either has to split them again — a shared `Arc` would otherwise silently serve
the wrong document.
