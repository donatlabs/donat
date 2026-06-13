---
type: research
domain: embedded-sdk
created: 2026-06-12
---

# Deep-Research Findings (June 2026)

> 5 search angles → 20 sources → 99 claims extracted → top 25 adversarially
> verified (3 votes each): **25 confirmed, 0 refuted**. Raw output:
> [research-report.json](research-report.json).

## Confirmed findings

1. **Trampoline + handle registry is THE pattern.** mattn/go-sqlite3:
   mutex-protected map keyed by a 1-byte `C.malloc`'d opaque pointer, one
   `//export` trampoline per hook category. grocksdb: thread-safe
   copy-on-write list addressed by integer index. C never holds a Go pointer
   in either. `runtime/cgo.Handle` (Go 1.17) is the stdlib sanctioning of
   exactly this round-trip. Caveat: Handle is sync.Map-backed (~0.5µs/op
   under contention) — hot paths may need a grocksdb-style indexed registry.
2. **cgo pointer rules make handle indirection mandatory** and dictate
   copy-or-C-allocate for any payload the engine retains; Go pointers
   containing Go pointers panic at runtime; strings/slices/channels are
   unpinnable (narrow `runtime.Pinner` exception for backing arrays).
3. **Precedent exists for every needed hook semantic.** Sync + abortable:
   SQLite UDF errors abort the statement in-flight. Row-mutating: RocksDB
   compaction filter `Filter() (remove bool, newVal []byte)`. Pre/post chain
   with modify/abort and explicit continuation: PocketBase's `e.Next()`
   interceptor chain; `OnRecordAfterCreateSuccess` fires only after commit.
4. **Callbacks arrive on foreign engine threads, concurrently**; RocksDB
   documents thread-safety as the callback author's problem. Decide and
   document the concurrency contract explicitly.
5. **Performance**: see [[performance]] — ~40ns/call modern; P-holding
   scheduler hazard; CockroachDB 6.5x collapse + Go-side batching fix; Go
   team's formal decline of nonblocking cgo (structural cost).

## Honest coverage gaps

- Research questions about Donat/Supabase/PostGraphile delivery semantics,
  DuckDB/libSQL/wasmtime-go/wazero FFI designs, and the wasm/embedded-JS/CDC
  alternatives comparison did NOT survive verification — under-evidenced.
  Only PocketBase represents "backend engine with native hooks".
- The 6.5x CockroachDB figure predates Go 1.8 scheduler improvements; the
  mechanism is confirmed current (open runtime issues), the magnitude on
  modern Go is unmeasured.

## Open questions for implementation time

1. Measured cost of the **reverse direction** (Rust/tokio threads → Go via
   //export, needm/dropm machinery); does a pre-attached fixed thread pool
   amortize it? → microbenchmark before sizing the hook pool.
2. Delivery-semantics precedent for in-process journal + pull consumer
   (libSQL embedded replicas, Badger value-log subscribers, NATS JetStream
   embedded)?
3. wasm (wazero host functions, extism) and embedded-JS (goja, v8go) vs cgo
   reverse callbacks: per-call overhead and operational safety; at what hook
   rate does each win?
4. Do Go 1.22 `#cgo noescape/nocallback` + `runtime.Pinner` zero-copy change
   the batching calculus for small transactions?

## Key sources

- https://github.com/mattn/go-sqlite3/blob/master/callback.go
- https://github.com/linxGnu/grocksdb/blob/master/compaction_filter.go
- https://pkg.go.dev/runtime/cgo · https://golang.design/research/cgo-handle/
- https://www.cockroachlabs.com/blog/the-cost-and-complexity-of-cgo/
- https://github.com/golang/go/issues/19574 · /16051 · /14592
- https://shane.ai/posts/cgo-performance-in-go1.21/
- https://pocketbase.io/docs/go-event-hooks/

## See Also

- [[ffi-boundary]] — the design these findings validate
- [[_index|Domain Index]]
