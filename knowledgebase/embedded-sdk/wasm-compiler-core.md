---
type: design
domain: embedded-sdk
created: 2026-06-12
---

# Wasm Compiler Core + Native Host Execution Layers

> The leading candidate: one wasm blob holds the conformance-heavy Rust core;
> each language gets a thin native execution layer — hooks become plain
> native functions with zero FFI.

Resolves the contradiction in
[[decisions/005-wasm-compiler-core-over-cgo-or-go-rewrite]]: keeps the Rust
core (work reused by all languages) AND gives every language native hooks
with a pure-language module — no cgo, no second runtime, no FFI hazards.

## Why a full engine can't be wasm'd — and why ours splits cleanly

Wasm/WASI can't host a server: no threads, no tokio, no real sockets to
Postgres. SQLite fits into wasm because it is pure compute delegating I/O to
the host (precedent: **ncruces/go-sqlite3** — SQLite compiled to wasm,
executed under wazero; a fully pure-Go module, `go get` works, ~1.2–2x
slower than the cgo binding — an accepted trade).

Our engine is unusually well-positioned for the same split because of the M4
design: **each operation compiles to a single SQL statement that assembles
the JSON response inside Postgres**. So the host execution layer is thin —
"run statement, hand back JSON blob".

## The split

- **Wasm core (one blob, all languages)**: GraphQL parse/validate, metadata,
  naming, permissions (row filters, column masks, presets, check
  expressions), sqlgen. Input: `(query, variables, session_vars)`. Output: a
  **plan** — list of SQL statements with parameter placeholders, transaction
  flag, hook points, error-mapping rules.
- **Host SDK per language** (Go: net/http + pgx; Node: fastify + pg): HTTP/WS
  with the user's own middleware/auth/metrics; JWT → session vars; PG pool;
  plan execution `begin → pre-hooks → exec → post-hooks → commit`; wrap the
  Postgres-produced JSON into `{"data": ...}`; map errors to Donat shapes.
- **Hooks are plain native functions** — the host owns the execution loop, so
  every FFI problem (trampolines, ownership, panic barriers, two schedulers)
  vanishes by construction.

## Request flow

```text
HTTP → host router → session vars
     → plan cache lookup (query_hash, role)        // hot path: no wasm call
     → miss: wasm.compile(query, vars, session) → plan
     → pgx/pg: txn { pre-hooks → exec → post-hooks }
     → JSON from Postgres → envelope → response
```

## Cost of a wasm call (so this never gets re-debated)

- Boundary crossing: wazero ≈ 100–500ns per call, V8/Node ≈ tens of ns —
  comparable to cgo's ~40ns BUT with none of cgo's scheduler hazards: a
  wazero call is an ordinary function call inside the Go runtime — no OS
  thread pinning, no P-holding, no foreign threads.
- Data transfer: copy query/plan strings in/out of linear memory — µs for
  KB-scale payloads.
- Execution inside wasm: ~1.5–3x native Rust → a full compile
  (parse + permissions + sqlgen) realistically ~0.1–1ms under wazero, less
  in V8. Versus a 0.5–2ms Postgres roundtrip: acceptable even uncached.
- With the plan cache, the hot path never enters wasm at all — only
  first-seen (query, role) pairs pay the compile cost. Net: wasm call cost
  is a non-issue; do not optimize it before measuring.

## Design decisions to make consciously

1. **Wasm instances are single-threaded.** Under concurrency: pool of
   instances (cheap) or mutex + plan cache (likely sufficient — compiles are
   rare). Metadata state lives in the instance; metadata reload = rebuild the
   pool.
2. **Plans must be parameterized** for the cache to work: variables AND
   session vars as statement parameters, not literals. sqlgen already
   parameterizes — verify session vars specifically.
3. **Error mapping is part of the plan contract**: the core emits rules
   (constraint name → Donat error shape, check_violation handling), the
   host applies them — so per-language layers can't drift on error formats.

## Honest costs

1. Execution layer written per language: transactions, retries, error
   mapping, and especially subscriptions (the polling loop is host-side).
   Thin thanks to single-statement JSON, but real — and conformance must be
   run **against each SDK** (mitigated: tests-py harness is HTTP-level and
   implementation-agnostic).
2. The wasm boundary still needs design (string passing, plan format
   versioning) — but single-threaded linear memory is an order of magnitude
   simpler than cgo.
3. Drift risk between host layers — the per-SDK conformance run is the
   guardrail; keep host layers as thin as possible, push every decidable rule
   into the plan format.

## See Also

- [[precedents]] — ncruces/go-sqlite3, the cgo adoption tax this resolves
- [[hooks-and-events]] — hook semantics carry over unchanged
- [[_index|Domain Index]]
