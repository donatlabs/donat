---
type: decision
status: accepted
date: 2026-06-13
features:
  - "[[multi-backend]]"
---

# In-process backend trait, not an out-of-process NDC-style protocol

## Context

We want to support data sources on databases other than Postgres (SQLite,
MySQL/MariaDB, SQL Server) over the same GraphQL surface, with **minimal
latency** as a stated requirement, and with **every backend exercised in the
conformance harness**.

The reference for "modular backends" is Hasura v3, which modularized by
extracting the entire data layer into **out-of-process NDC (Native Data
Connectors)**: the engine plans GraphQL into an NDC IR (`QueryRequest`), sends
it over HTTP to a connector (a separate service built on `ndc-sdk-rs`), and
gets back rowsets; connectors advertise a capabilities document; cross-source
relationships are stitched in the engine. Hasura's own connectors
(`ndc-postgres`, `ndc-sdk-rs`) are Apache-2.0.

This engine is a **v2-surface** engine whose core performance property (M4) is
*one SQL statement per operation, with the response JSON assembled inside the
database* — no N+1, no in-process result stitching.

## Decision

Make the backend boundary an **in-process Rust trait** (`Backend` +
`Dialect` + `Capabilities`), dispatched by `SourceKind`, with each backend
compiled into the engine. We **borrow NDC's good ideas** — a per-backend
capabilities document, the IR as a backend-neutral contract, the "connector"
shape (introspect + capabilities + dialect) — **without** its out-of-process
HTTP protocol.

Performance is the deciding factor. An out-of-process protocol adds, per
request, a network hop, IR/rowset (de)serialization, and engine-side join
stitching, and it abandons the one-statement-in-DB invariant. The in-process
trait keeps a single native statement per source (JSON assembled in the
database) and zero IPC overhead — directly serving the latency requirement.

## Alternatives

| Option | Why Not |
|--------|---------|
| **NDC out-of-process protocol** (adopt `ndc-spec`, possibly reuse Hasura connectors) | Per-request network hop + IR/rowset serialization; abandons the M4 one-statement invariant (rowsets + engine-side stitching); turns the engine into a v3-style executor (OpenDD/v3 execution model) — a far larger pivot than a dialect trait, and contrary to the latency goal. Connectors target the NDC IR, not our v2 IR, so they are not drop-in regardless. Kept as a possible *future* extension, not the foundation. |
| **Dynamic plugins (.so/FFI or wasm)** | Rust has no stable ABI; wasm complicates native DB-driver access and async execution. Justified only for a third-party backend ecosystem, which is not a goal — we own the SQL family. |
| **Minimal: `Dialect` enum threaded through current `sqlgen`, IR leaks left in place** | Faster to a first SQLite, but `match dialect` scatters through codegen, the IR stays Postgres-coupled, and there is no capability model — so "every backend in tests" (which needs honest feature degradation) is unachievable cleanly, and the Nth backend hurts. |

## Consequences

**We get:** the lowest-latency design (one native statement per source, no
IPC); the M4 invariant preserved; a single deployable binary; a clean
capability model that systematically fixes the IR's Postgres leaks (jsonb /
PostGIS / upsert become advertised capabilities, not assumptions); a
conformance matrix that maps naturally onto in-process per-backend execution.

**We pay:** backends must be Rust and compiled into the engine (no third-party
drop-in connectors); we write and maintain each SQL dialect ourselves (we may
*derive* from Apache-2.0 `ndc-*` connectors as a reference, license permitting
per connector); and a large up-front refactor — de-leaking the IR and
splitting `sqlgen` into a dialect-driven assembler — before the first
non-Postgres backend lands.
