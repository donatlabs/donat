---
type: design
domain: embedded-sdk
created: 2026-06-12
---

# Hooks and Events: Taxonomy, Semantics, Guarantees

> Three distinct mechanisms with three distinct contracts; in-memory hooks
> must NOT silently replace the durable journal.

## 1. Durable event triggers (Hasura v2 compatible)

- Capture: PG triggers write to an event log table **in the same transaction**
  as the mutation → nothing is lost on crash.
- Delivery: poller (`FOR UPDATE SKIP LOCKED`) → HTTP webhook, at-least-once,
  retries; handlers must be idempotent.
- Fire on **any table change**, including raw SQL inserts bypassing GraphQL.
- Needed for tests-py conformance regardless of the embedded idea.
- Build order: implement HTTP delivery first → extract a `DeliveryTransport`
  trait → add in-process transport later. The transport must support both
  **pull** (Go consumer) and **push** (Node event loop) modes.
- Fast-path optimization (in-process only): after commit, push the event to an
  in-process queue immediately; the journal poller becomes a crash/nack
  fallback. p50 commit→handler drops to single-digit ms while keeping
  at-least-once.

## 2. Sync hooks (in-memory, no journal)

- **`pre_insert`** — before SQL generation. Can enrich/modify rows or reject
  (client gets the error). Nothing to lose by design; the safest and most
  useful hook.
- **`post_insert` in-transaction** — sees inserted rows (defaults, generated
  ids), can roll back the whole mutation. Requires wrapping the operation in
  an explicit transaction (`begin → insert..returning → hook → commit`),
  which deviates from the single-statement execution model — only wrap when
  a hook is registered. The transaction and a pool connection stay busy while
  the handler runs: handlers must be fast, no external calls by convention.
- **post-commit in-memory** — honest **at-most-once**. Process dies between
  commit and call → event gone. Fine for cache invalidation, metrics,
  notifications; not for money. For durability, use mechanism 1
  (see [[decisions/002-keep-durable-journal-alongside-in-memory-hooks]]).
- Hooks see only GraphQL-path writes; raw SQL bypasses them (unlike PG-trigger
  based events). Decide consciously per use case.

## Handler rules (Go specifics — goroutines welcome, with rules)

1. The engine sees only what the handler returns. If goroutines influence the
   decision (parallel validation via errgroup), wait for them before
   returning. Fire-and-forget goroutines are fine but can't affect anything.
2. Detached goroutines spawned from pre/in-txn hooks don't know the
   transaction's fate — it may still roll back. "After success" side effects
   belong in post-commit hooks only.
3. The SDK deserializes the payload into Go-owned structs **before** invoking
   user code (the FFI buffer is freed on trampoline return). Users then can't
   create use-after-free even from long-lived goroutines.
4. Lifecycle: hook calls are foreign threads inside the Go runtime; their
   count is bounded by the Rust-side hook pool (explicit config, e.g. 16–64).
   Shutdown order: `dist_stop` drains in-flight hooks → SDK waits on a
   WaitGroup over trampoline calls → process may exit. The ctx passed to
   handlers must cancel on engine stop and on request timeout.
5. Concurrency contract (RocksDB precedent, see [[research-findings]]):
   callbacks arrive on engine threads, potentially concurrently. Document
   "handlers must be concurrent-safe" rather than serializing in the engine.

## API sketch (Go)

```go
eng := donat.Start(cfg)
donat.PreInsert(eng, "public", "order", func(ctx context.Context, rows []Order) ([]Order, error) { ... })
donat.OnInsert(eng, "public", "article", func(ctx context.Context, ev donat.Event[Article]) error { ... })
eng.Run()
```

Same handler signatures must work over the webhook transport (sidecar mode) —
transport interchangeability is the design acceptance criterion.

## See Also

- [[ffi-boundary]] — how hooks cross the boundary in the cgo variant
- [[wasm-compiler-core]] — variant where hooks are plain native calls
- [[_index|Domain Index]]
