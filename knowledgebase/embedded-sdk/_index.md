# Embedded SDK & Native Hooks

> Run the engine inside a host-language app (Go first, Node.js later) and
> register native functions as data hooks — "PocketBase DX with Donat v2
> compatibility".

**Status: idea, deferred.** Resume after core conformance (websockets,
introspection, subscriptions — see PLAN.md). Captured June 2026 so the
reasoning never has to be reconstructed.

## Design Notes

- [[embedding-options]] — Embedding a Rust engine into a Go process: cgo costs, two-runtimes problem, the "engine keeps its own listener" shape, sidecar fallback
- [[hooks-and-events]] — Hook taxonomy: durable event triggers vs sync in-txn hooks vs post-commit; delivery guarantees; goroutine rules; Go API sketch
- [[ffi-boundary]] — The cgo boundary design: tiny versioned C ABI, single batched trampoline, pull-based event consumer, memory ownership, Node via napi-rs
- [[performance]] — Numbers: cgo ~40ns but scheduler P-holding is the real hazard, event-journal ceiling ~5–20k ev/s, Node single-thread ceiling
- [[precedents]] — Who does this in the wild: SQLite/RocksDB tradition, PocketBase, Prisma's retreat, CockroachDB's escape from cgo, the cgo adoption tax
- [[wasm-compiler-core]] — **Leading candidate**: wasm "compiler core" + native per-language execution layers — native hooks with zero FFI
- [[research-findings]] — Deep-research results: 25/25 adversarially verified claims, sources, coverage gaps, open questions ([raw JSON](research-report.json))

## Decisions

- [[decisions/001-webhooks-first-transport-agnostic-sdk]] — build webhook event triggers first; SDK handler API identical across webhook and embedded transports
- [[decisions/002-keep-durable-journal-alongside-in-memory-hooks]] — in-memory hooks complement the journal, never replace it; at-most-once stated honestly
- [[decisions/003-never-embed-go-runtime-in-node]] — Node in-process only via a Rust core (napi-rs); a Go core means Node = sidecar forever
- [[decisions/004-defer-embedded-sdk-until-conformance]] — sequencing: conformance → webhooks → embedded
- [[decisions/005-wasm-compiler-core-over-cgo-or-go-rewrite]] — (proposed) the core-language resolution

## Architecture Overview (leading candidate)

```text
HTTP → host router (net/http + pgx | fastify + pg) → session vars
     → plan cache lookup (query_hash, role)        // hot path: no wasm call
     → miss: wasm.compile(query, vars, session)    // Rust core: parse,
     │                                             // permissions, sqlgen
     → plan: [SQL + params, txn flag, hook points, error-mapping rules]
     → host txn { pre-hooks (native fns) → exec → post-hooks } 
     → JSON already assembled by Postgres (M4) → envelope → response

post-commit events: journal (at-least-once) → host-native handlers
```
