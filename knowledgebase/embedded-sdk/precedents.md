---
type: design
domain: embedded-sdk
created: 2026-06-12
---

# Precedents: Who Embeds Engines with Native Hooks

> The FFI mechanics are a beaten path (embedded-DB world); the product shape
> "GraphQL engine as an embeddable library" is nearly unoccupied.

## The embedded-DB tradition (mechanics are proven)

- **SQLite** — the canonical case: update/commit hooks, UDFs, bindings in
  every language, decades in production.
- **RocksDB/LevelDB** — compaction filters and merge operators as
  host-language callbacks.
- **DuckDB** — host-language UDFs (vectorized: a chunk of values per call —
  the batching argument).
- **Realm, Couchbase Lite, ObjectBox, libSQL** — "C/C++/Rust core + thin
  per-language SDKs with callbacks" is the industry standard for mobile/edge
  databases.

## GraphQL/API engines — almost nobody

Donat, PostGraphile, Supabase all live in the server+webhooks model. The
combination "Donat-compatible engine with PocketBase DX" is essentially
unoccupied — a differentiator, not a beaten path.

## Two closest analogs, both instructive

- **PocketBase** — closest by DX: backend engine as a Go library, native
  hooks with modify/abort via an `e.Next()` interceptor chain (good API
  model to copy); `OnRecordAfterCreateSuccess` fires only after commit.
  Hugely popular precisely for this. But pure Go — zero FFI.
- **Prisma** — closest by architecture: Rust query engine embedded in Node
  via N-API (deliberately moved from a sidecar binary to embedding for
  latency). Then publicly retreated (2024–2025): rewriting the engine from
  Rust to TypeScript, citing boundary pain — serialization, debugging,
  shipping native binaries for every platform. Not a verdict on our idea
  (Prisma crosses the boundary per query; we'd cross only for hooks), but
  the most relevant industry scar.
- Counter-direction: **CockroachDB** dumped RocksDB for pure-Go Pebble
  largely because of cgo pain; pure-Go-over-bindings is a real ecosystem
  current (Badger, bbolt, wazero).

## The cgo adoption tax (the critical argument)

cgo is not "broken" — it taxes **every user at every build**: cross-compile
breaks, musl/alpine pain, slow CI, no plain `go get`. The Go ecosystem's
revealed preference: people pick `modernc.org/sqlite` (transpiled, slower!)
over mattn/go-sqlite3 just to avoid cgo; franz-go/segmentio took mindshare
from confluent-kafka-go (librdkafka). A Rust core behind cgo injects maximum
friction into the flagship SDK for exactly the audience the embedded idea
targets. This contradiction is the strongest argument against the
"full Rust engine via cgo" variant — and what
[[wasm-compiler-core]] resolves.

## Node-specific precedent

Embedding a Go runtime inside a Node process is effectively a non-pattern:
**esbuild** (written in Go, used from Node by millions) deliberately ships a
child-process binary speaking stdio instead of embedding; its wasm build is
~10x slower. The native-Node-module ecosystem (swc, Prisma engine, Parcel,
lightningcss) uses C/C++/Rust via N-API; Go is absent as a class. See
[[decisions/003-never-embed-go-runtime-in-node]].

## See Also

- [[decisions/005-wasm-compiler-core-over-cgo-or-go-rewrite]] — the
  core-language decision these precedents feed
- [[_index|Domain Index]]
