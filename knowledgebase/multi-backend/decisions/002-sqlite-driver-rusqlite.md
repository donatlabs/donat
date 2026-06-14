---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[multi-backend]]"
---

# SQLite driver: rusqlite (bundled), not sqlx

## Context

The first non-Postgres backend is SQLite (cheapest to run in CI, closest to
the PG JSON-assembly model). We need a Rust SQLite driver for two jobs:
catalog introspection (once, at boot) and query execution (per request, on the
async hot path). The Postgres backend uses `tokio-postgres` (async) with
`deadpool-postgres` pooling — there is no existing SQLite driver in the tree.

## Decision

Use **`rusqlite`** with the `bundled` feature.

- `bundled` compiles SQLite from source into the binary — no system SQLite
  dependency, so CI (and release artifacts) need nothing installed. This keeps
  the "SQLite runs in-process, every push, no service container" property that
  made SQLite the first backend.
- `rusqlite` is synchronous. Introspection runs at boot, so sync is fine there.
  Execution is on the async request path, so SQLite queries will run inside
  `tokio::task::spawn_blocking` (or a small blocking pool) when the execution
  slice lands — SQLite calls are fast and local, so the blocking-pool cost is
  negligible.

## Alternatives

| Option | Why Not |
|--------|---------|
| **sqlx (sqlite)** | Async-native (no spawn_blocking), but a heavy dependency tree the project otherwise avoids — it uses `tokio-postgres`, not sqlx, for Postgres. Adding sqlx solely for SQLite pulls in a large surface for little gain; SQLite calls are local and short, so async I/O buys little. |
| **libsqlite3-sys directly** | Too low-level; rusqlite is the safe idiomatic wrapper over it. |
| **System SQLite (non-bundled rusqlite)** | Requires SQLite installed on every build/CI/release host; breaks the zero-setup CI property. |

## Consequences

**We get:** zero-setup SQLite in CI and releases (bundled), an idiomatic safe
API, and a clean separation (introspection sync at boot; execution via
`spawn_blocking`).

**We pay:** a `spawn_blocking` hop per SQLite query (negligible for local
SQLite), and a heterogeneous execution layer — Postgres stays async-native via
`tokio-postgres`, SQLite is sync-behind-blocking. The `Backend`/execution
abstraction (a later slice) must accommodate both shapes; it already must, since
pools/clients differ per backend.
