# Spec 004 — wasm-core + native Go host split

Status: proposed (2026-06-14). Confirms and operationalises ADR
[[knowledgebase/embedded-sdk/decisions/005-wasm-compiler-core-over-cgo-or-go-rewrite]]
(currently *proposed*). This spec is the "confirm at implementation time"
step: it specifies the catalog split, the wasm core build, the plan-format
contract, and the composable Go host execution layer, with a phased,
TDD-friendly breakdown.

## Summary

Split the engine into (1) a **wasm core** — one `wasm32` blob holding the
conformance-heavy Rust logic (GraphQL parse/validate, metadata, naming,
permissions, sqlgen) that emits a serializable **plan**, and (2) a **native
Go host** (pure Go, no cgo, wazero runtime) that owns HTTP/WS, the pgx pool,
plan execution, the JSON envelope, error mapping, and native event-trigger
hooks. The host is an **embeddable library** (an `http.Handler` plus a
programmatic API), not a process: users mount engine routes alongside their
own and share a pgx pool and even a single transaction with engine queries.

## Background

The embedded-SDK direction ("Donat v2 compatibility with PocketBase DX")
forces a core-language bet. The settled resolution (ADR-005,
[[knowledgebase/embedded-sdk/wasm-compiler-core]]) is the wasm-core split: it
keeps the Rust core (reused by every future language) yet gives every host
language native hooks with zero FFI hazards, and — critically — preserves the
hard user goal of a **pure-Go, `go get`-able, `CGO_ENABLED=0` static build**
(precedent: `ncruces/go-sqlite3`, SQLite-in-wasm under wazero;
[[knowledgebase/embedded-sdk/precedents]]).

This is feasible specifically because of the M4 invariant — *each operation
compiles to a single SQL statement that assembles the JSON response inside
Postgres* (`crates/sqlgen/src/lib.rs:1-11`). The host layer is therefore thin:
"run statement, return the JSON blob". This spec builds directly on Spec 003
(the transport-agnostic event core: `Event[T]`, `Registry`, `Dispatch`) and
Spec 002 (the durable journal); it does **not** re-specify those — event
hooks here become plain native Go calls invoked by the host's plan executor.

### Verified feasibility (established facts, with commands)

Toolchain: `rustc 1.96.0`; `wasm32-unknown-unknown` and `wasm32-wasip1`
targets installed (`rustup target list --installed | grep wasm`).

1. **The pure logic crates compile to wasm today.** Verified `Finished` for
   each:
   ```bash
   cargo build -p donat-ir       --target wasm32-unknown-unknown
   cargo build -p donat-backend  --target wasm32-unknown-unknown
   cargo build -p donat-metadata --target wasm32-unknown-unknown
   cargo build -p donat-sqlgen   --target wasm32-unknown-unknown
   ```
2. **`donat-schema` does NOT compile to wasm.** The *only* blocker is its
   dependency on `donat-catalog`, which bundles I/O drivers
   (`crates/catalog/Cargo.toml:10-12`): `tokio-postgres` (full tokio — "Only
   features sync,macros,io-util,rt,time are supported on wasm"), `rusqlite`
   with `bundled` (C `libsqlite3-sys`), and `mysql`. On `wasm32-wasip1`
   `getrandom` is fine; the drivers are the blockers.
3. **`donat-catalog` already separates pure types from drivers.** Pure serde
   types `Catalog`, `FunctionInfo`, `FunctionArg`, `TableInfo`, `ColumnInfo`,
   `ForeignKey` (`crates/catalog/src/lib.rs:13-85`) vs the I/O entry points
   `introspect(&tokio_postgres::Client)` (`:163`), `sqlite_introspect(&rusqlite::Connection)`
   (`:299`), `mysql_introspect(&mut mysql::Conn, ...)` (`:402`).
4. **`schema` consumes only the pure types** — `Catalog`, `TableInfo`,
   `ColumnInfo`, `FunctionInfo` (`crates/schema/src/plan.rs:5,188,607,663`;
   `crates/schema/src/introspection.rs:557`). No `schema` code calls any
   driver function.

## Goal / Non-Goals

### Goals
- Carve the engine into a wasm core (pure compute) and a native host
  (I/O + execution), with a **versioned plan-format contract** between them.
- Ship a pure-Go, cgo-free, `go get`-able host that loads the wasm blob via
  **wazero** and runs under `CGO_ENABLED=0`.
- Make the host **composable / embeddable**: an `http.Handler` mounted next
  to user routes; the **pgx pool is supplied by the user**; a programmatic
  `Execute` for non-HTTP use; and `ExecuteTx` that runs engine queries inside
  a **user-supplied transaction** so user Go code and engine queries share one
  atomic txn.
- Reuse the Spec 003 `Event[T]`/`Registry`/`Dispatch` seam: post-hooks are
  plain in-process native Go calls; the durable journal (ADR 007 /
  `crates/server/src/events.rs`) stays for capture and at-least-once.
- Keep the existing Rust `donat-server` binary fully unchanged and green
  throughout — it remains the standalone deployment option.

### Non-Goals
- **Not** a Go rewrite of the engine. The conformance-heavy logic stays in
  Rust and is shared across every future host language.
- **Not** cgo. No C ABI, no `dist-ffi`, no prebuilt-`.so` matrix; the
  [[knowledgebase/embedded-sdk/ffi-boundary]] cgo design is explicitly *not*
  the chosen path.
- **Not** replacing `donat-server`. The Rust axum binary, the conformance
  harness, `migrate`/`validate`, MCP/REST surfaces — all untouched here.
- **Not** the Node host (deferred; ADR-003 — Node will bind the Rust core via
  napi-rs or run the same wasm core, decided in a later spec).
- **Not** new GraphQL/metadata features. This is an architectural pivot that
  must produce byte-identical behavior to the existing engine.

## Governing Fixtures

There are no *new* Donat-surface fixtures: this spec changes **how** the
existing engine runs, not **what** it answers. The guardrail is that the Go
host must answer the **existing** native conformance suite
(`crates/conformance`) identically. The conformance harness is HTTP-level and
implementation-agnostic ([[knowledgebase/embedded-sdk/wasm-compiler-core]]
§"Honest costs"; ADR-005 Consequences), which is exactly what makes per-SDK
conformance the drift guard.

Two existing behaviors are load-bearing for the plan contract and must be
reproduced exactly by the host:

| Behavior | Source of truth | What the host must reproduce |
|---|---|---|
| Error mapping (Postgres SQLSTATE → Donat code/path/message) | `crates/server/src/gql.rs:871-964` | `23514`→`permission-error` (path+message from the `donat.check_violation()` JSON payload), `23505/23503/23502`→`constraint-violation` with the exact message prefixes, default→`data-exception`, decode/pool→`unexpected` |
| Single-statement JSON assembly | `crates/sqlgen/src/lib.rs:15-60` | run one statement, return the Postgres-produced `json` value verbatim under `{"data": ...}` |
| Mutation transaction semantics | `crates/server/src/gql.rs` (mutation branch ~`:589`) | mutation root fields execute sequentially in **one** transaction (`Plan::Mutation`, `crates/schema/src/plan.rs:86-90`) |

The decisive error shapes the host must emit (from `gql.rs`):

```jsonc
// SQLSTATE 23514 with donat.check_violation() JSON payload {path,message}:
{"errors":[{"extensions":{"path":"<path>","code":"permission-error"},"message":"<message>"}]}
// 23505 unique:
{"errors":[{"extensions":{"path":"$","code":"constraint-violation"},"message":"Uniqueness violation. <db msg>"}]}
```

## Critical design finding: plans are NOT parameterized yet

ADR-005 §Consequences and [[wasm-compiler-core]] §"Design decisions" assume
"sqlgen already parameterizes — verify session vars specifically." **Verified:
it does not.** Today:

- sqlgen renders **literals inline** with quote-escaping; parameterized
  execution is an explicit planned refactor (`crates/sqlgen/src/lib.rs:8-11`).
- Both GraphQL **variables** and **session variables** are substituted by the
  planner into the IR before sqlgen runs — `Scalar::Json` is the only literal
  the IR carries and "Session variables are substituted by the planner before
  the IR is final, so sqlgen only ever sees literals"
  (`crates/ir/src/lib.rs:451-456`, `:106`).

Consequence for this spec: the **v1 plan format carries a fully-rendered SQL
string with values already inlined** (matching today's engine byte-for-byte).
The plan cache key is therefore `(query_text, role, variables, session_vars)`
— coarser than the ideal `(query_hash, role)` in
[[wasm-compiler-core]] §"Request flow". Full parameterization (variables and
session vars as `$1..$n` so the hot path is `(query_hash, role)`) is a
**separate follow-up refactor in sqlgen/IR**, tracked as an open question
below, not a precondition for the split. This keeps Phase boundaries honest
and the early phases low-risk.

## Requirements

### Functional
- [ ] `donat-catalog-types` (or feature-gated `donat-catalog`) exposes the
      pure types and compiles to `wasm32-unknown-unknown`; `donat-schema`
      depends only on the pure types and compiles to wasm.
- [ ] A `donat-wasm-core` crate compiles to a `wasm32` `cdylib` exporting a
      minimal `compile(input_json) -> plan_json` ABI over linear memory.
- [ ] The plan format is a **versioned, serializable** contract (a Rust type
      with `serde`, mirrored by a Go struct) covering: ordered SQL
      statements, transaction flag, post-hook points, and error-mapping rules.
- [ ] A Go host package loads the blob via wazero, maintains an instance
      pool, and offers: an `http.Handler`; `Execute(ctx, query, vars) ([]byte, error)`;
      `ExecuteTx(ctx, tx, query, vars) ([]byte, error)` over a caller's pgx tx.
- [ ] The user supplies the pgx pool; the engine never opens or owns it.
- [ ] Event-trigger post-hooks dispatch through the existing Spec 003
      `Registry.Dispatch` in-process from the host's plan executor.
- [ ] The Go host passes the existing native conformance suite.

### Non-functional
- [ ] Pure Go host: `CGO_ENABLED=0 go build ./...` and `go vet`/`gofmt` clean
      in CI (extends the existing Go SDK CI job, commit `0c7c7a3`).
- [ ] Single SQL statement per operation preserved (M4 invariant) — the host
      executes exactly the statements the plan lists, no row-by-row Rust/Go
      post-processing.
- [ ] SQL-injection safety unchanged: the wasm core reuses sqlgen's existing
      quoting helpers; the host treats plan SQL as opaque and never
      re-templates user input into it.
- [ ] **No admin role** (BLOCKING): the host enforces the same role rules — a
      trusted request with no `X-Donat-Role` is denied ("x-donat-role header
      is required"); no permission bypass is introduced anywhere in the host.
- [ ] The Rust `donat-server` binary and `make test` + `make conformance`
      stay green at every phase boundary.

## Technical Design

### Affected Crates / Files

| File | Change | Description |
|---|---|---|
| `crates/catalog-types/` (new) OR `crates/catalog/Cargo.toml` + `lib.rs` | Create / Modify | Pure serde types in a wasm-safe crate or behind a default `introspect` feature that gates `tokio-postgres`/`rusqlite`/`mysql` + the three `*introspect` fns |
| `crates/schema/Cargo.toml` | Modify | Depend on the pure-types crate (or `donat-catalog` with `default-features = false`) |
| `crates/wasm-core/` (new) | Create | `cdylib` for `wasm32`; depends on `donat-metadata`, `donat-schema`, `donat-sqlgen`, `donat-ir`, `donat-backend`, `donat-catalog-types`; exports the `compile` ABI; defines the `Plan` contract type |
| `crates/wasm-core/src/plan.rs` (new) | Create | Versioned `PlanV1` serde type (the contract artifact) |
| `sdk/go/donat/` | Modify/Extend | Host package: wazero loader + instance pool, `Engine`, `http.Handler`, `Execute`, `ExecuteTx`; Go mirror of `PlanV1`; wires `Registry.Dispatch` |
| `sdk/go/donat/event.go`, `registry.go` (Spec 003) | Reuse | Post-hooks invoked by the executor; not re-specified |
| `crates/server/*` | Unchanged | Standalone binary stays as-is (regression guard) |

### Phase 1 — Catalog split (task #1, low-risk, Rust-only)

Goal: make the pure catalog types wasm-compilable **without** changing any
behavior of the standalone server. Two acceptable shapes — pick one in the
spike, prefer (A) for a clean dependency graph:

**(A) New `donat-catalog-types` crate.** Move the six serde structs
(`crates/catalog/src/lib.rs:13-85`) into `donat-catalog-types` (deps: `serde`
only — already wasm-clean). `donat-catalog` re-exports them and keeps the
drivers + `introspect`/`sqlite_introspect`/`mysql_introspect`. `donat-schema`
depends on `donat-catalog-types`. The host-side server keeps depending on
`donat-catalog` for introspection.

**(B) Feature-gate.** Add a default feature `introspect` to `donat-catalog`
that gates `tokio-postgres`, `rusqlite/bundled`, `mysql`, and the three
driver fns. `schema`/`wasm-core` use `donat-catalog = { default-features =
false }`. Riskier: `cargo` feature unification can re-enable the drivers when
the server and wasm-core are in the same build graph — so the wasm-core build
must be a **separate `cargo build --target wasm32-...` invocation**, which it
is anyway.

The pure types already derive `Serialize`/`Deserialize`, so a **serialized
`Catalog` snapshot** crosses into wasm unchanged. Introspection stays
host-side (boot/deploy time): the server already builds `catalogs:
HashMap<String, Catalog>` at boot (`crates/server/src/state.rs:89-91, :593`),
and `migrate`/codegen already introspect deploy-time. The host serializes that
snapshot and hands it to the wasm core.

Verification at the end of Phase 1:
```bash
cargo build -p donat-schema --target wasm32-unknown-unknown   # must Finish
make test && cargo build -p donat-server --bin donat && make conformance  # stay green
```

### Phase 2 — wasm-core crate + plan-format contract

A `cdylib` crate built for `wasm32-unknown-unknown` (no WASI needed — pure
compute; `getrandom` is not required by the logic crates). Reuse the exact
existing pipeline, just behind a string boundary:

```rust
// Pattern reused from crates/server/src/gql.rs:500 + crates/server/src/gql.rs:658
// (planner -> roots -> sqlgen), lifted into the wasm core verbatim:
let planner = donat_schema::Planner::new(&metadata, &catalog);     // plan.rs:215
let plan = planner.plan(&doc, op_name, &variables, &session)?;     // plan.rs:694
let sql  = donat_sqlgen::operation_to_sql_opts(&roots, stringify); // sqlgen lib.rs:21
```

**ABI (string/memory passing).** wasm32 cannot pass structs; only linear
memory + i32 offsets. Minimal exported surface (wazero-friendly):

```text
core_abi_version() -> i32
core_alloc(len: i32) -> ptr          // host writes input bytes here
core_dealloc(ptr: i32, len: i32)
core_init(cfg_ptr, cfg_len) -> i32   // load serialized metadata + Catalog snapshot into the instance
core_compile(in_ptr, in_len) -> i64  // packed (out_ptr<<32 | out_len); host reads then core_dealloc
```

`core_init` carries metadata + the serialized `Catalog` snapshot; metadata
state lives in the instance ([[wasm-compiler-core]] §"Design decisions" #1) —
metadata reload rebuilds the instance pool. All payloads are JSON byte
buffers (format may evolve without breaking the numeric ABI).

**`core_compile` input** (JSON):
```jsonc
{ "query": "<graphql>", "operation_name": null,
  "variables": { ... }, "session_vars": { "x-donat-role": "user", ... },
  "stringify_numerics": false }
```

**`core_compile` output — the PlanV1 contract** (the central artifact):
```jsonc
{
  "version": 1,
  "kind": "query" | "mutation",          // mutation => wrap in one txn
  "transaction": false,                  // true for mutations (plan.rs:86-90)
  "statements": [                        // ordered; one per root for mutations
    { "sql": "SELECT json_build_object(...) AS root",
      "params": [] }                     // v1: empty (literals inlined; see finding above)
  ],
  "hooks": [                             // post-hooks the executor must fire (Spec 003)
    { "phase": "post_commit", "trigger": "<event_trigger_name>",
      "schema": "public", "table": "order", "op": "INSERT" }
  ],
  "error_map": {                         // host applies these -> Donat shapes (gql.rs:871-964)
    "23514": "permission-error-from-payload",
    "23505": "constraint-violation:Uniqueness violation. ",
    "23503": "constraint-violation:Foreign key violation. ",
    "23502": "constraint-violation:Not-NULL violation. ",
    "default": "data-exception"
  },
  "envelope": "json"                     // result column is the final `data` value
}
```
Or `{ "version": 1, "error": { "code": "...", "path": "...", "message": "..." } }`
for planner/validation failures (`PlanError` → the existing
`error_json`/`access-denied`/`bad-request` shapes, e.g. `gql.rs:1017,1064`),
so even compile-time errors are emitted identically.

**Versioning.** `version` is mandatory; the Go mirror rejects an unknown
major and the wazero round-trip test asserts `core_abi_version()` matches the
Go-side expected constant. The plan type lives in
`crates/wasm-core/src/plan.rs` with a `#[serde(deny_unknown_fields)]`-free,
additive evolution policy (new optional fields only within a major).

### Phase 3 — Go host execution layer (the large phase)

Pure Go, wazero runtime. Mirror `PlanV1` as a Go struct; load the embedded
`.wasm` blob (`//go:embed core.wasm`); maintain an **instance pool** because
wasm instances are single-threaded ([[wasm-compiler-core]] §"Design
decisions" #1) — a `sync.Pool` of wazero instances, each seeded by `core_init`
with the current metadata+catalog snapshot; metadata reload drains and
rebuilds the pool. A **plan cache** keyed by
`(query, role, variables-hash, session-hash)` (coarse, per the finding above)
keeps the hot path out of wasm.

**Composable host API (first-class requirement).** The engine is a library,
not a process:

```go
// User owns the pool and passes it in; engine never opens connections itself.
eng, err := donat.New(donat.Config{
    WASM:     coreBlob,          // //go:embed
    Pool:     pgxPool,           // *pgxpool.Pool supplied by the user
    Metadata: metaBytes,         // serialized metadata + Catalog snapshot
    Registry: reg,               // Spec 003 *donat.Registry of native hooks
})

// 1. Mount alongside the user's own routes in one mux:
mux := http.NewServeMux()
mux.Handle("/v1/graphql", eng.Handler())   // engine route
mux.HandleFunc("/my/custom", myHandler)     // user route, same process

// 2. Programmatic, non-HTTP:
data, err := eng.Execute(ctx, query, vars)  // returns the {"data":...} bytes

// 3. Share ONE transaction between user Go code and engine queries:
tx, _ := pgxPool.Begin(ctx)
_, _ = eng.ExecuteTx(ctx, tx, query, vars)  // engine runs inside the caller's tx
myOwnWrite(ctx, tx)                          // atomic with the engine query
_ = tx.Commit(ctx)
```

`Handler()` resolves the session from `X-Donat-*` headers (same rules as
`crates/server/src/gql.rs:63`, including the no-admin denial), calls
`compile`, then runs the executor.

**Executor** (`begin → exec → post-hooks → commit`, mirroring `gql.rs`):
1. `kind=="query"`: acquire a conn, run the single statement, read the `json`
   column, wrap as `{"data": <json>}`.
2. `kind=="mutation"` (or `ExecuteTx`): in the (caller's or a fresh) txn, run
   statements in order; on success, fire `hooks` (post-commit) by calling
   `Registry.Dispatch(ctx, trigger, envelopeJSON)` — plain in-process Go,
   no webhook (Spec 003 seam; durable journal capture from ADR 007 still
   happens via the deploy-time triggers, delivery becomes in-process).
3. On a pg error, apply `plan.error_map` to produce the exact Donat error
   body (the `query_error_json`/`db_error_json` table at `gql.rs:875-918`).

`ExecuteTx` taking an external `pgx.Tx` is what lets user Go writes and engine
queries commit atomically — the composability requirement.

### Metadata / API Changes

None to the Donat surface. New developer-facing surface only: the Go
`donat.Config`/`Engine` API and the `PlanV1` contract. No new `/v1/query`
metadata ops, no admin surface (BLOCKING rule honored).

## Phased Task Breakdown (TDD-ordered, each independently testable)

1. **Catalog split** (Rust). Extract pure types (shape A or B). *Test:*
   `cargo build -p donat-schema --target wasm32-unknown-unknown` Finishes;
   `make test` + `make conformance` stay green (server unchanged). Low-risk.
2. **wasm-core crate skeleton + ABI.** `cdylib`, `core_abi_version/alloc/
   dealloc`, `core_init` loading metadata+catalog. *Test:* `cargo build
   -p donat-wasm-core --target wasm32-unknown-unknown` Finishes; a wazero Go
   round-trip test calls `core_abi_version`. Low-risk.
3. **`core_compile` + PlanV1.** Wire `Planner::plan` + `operation_to_sql_opts`
   behind the string ABI; define `PlanV1` serde type. *Test:* Rust insta
   snapshot over PlanV1 for a representative query/mutation/permission-error,
   reusing existing fixture metadata; assert SQL equals the current engine's.
4. **Go PlanV1 mirror + plan cache.** Decode PlanV1; cache key. *Test:* Go
   unit tests on decode + cache hit/miss.
5. **Go executor (query path).** Acquire conn from the user pool, run one
   statement, envelope. *Test:* Go integration test against a real Postgres
   (the conformance PG) for a SELECT; byte-compare to the Rust server.
6. **Go executor (mutation/txn) + `ExecuteTx` + error mapping.** *Test:*
   constraint/permission cases reproduce the `gql.rs` error bodies exactly;
   `ExecuteTx` shares a txn (insert via engine + user write commit/rollback
   atomically).
7. **`Handler()` + session resolution + composability.** Mount alongside a
   user route; no-admin denial. *Test:* Go HTTP test asserting the
   `x-donat-role`-required denial and a mixed mux.
8. **Native event-trigger hooks.** Fire `Registry.Dispatch` post-commit from
   the executor (Spec 003). *Test:* Go test registers `On[T]`, runs a
   mutation, asserts the handler saw the `Event[T]`.
9. **Per-SDK conformance run.** Point the native harness at the Go host.
   *Test:* the existing suite is green against the Go host (or a documented
   known-diff list); wire into CI.

Phases 1–2 are low-risk (compile-only milestones). Phase 3 fixes the
contract. Phases 5–9 are the large host-execution work and where most effort
and risk live.

## Acceptance Criteria

- [ ] `cargo build -p donat-schema --target wasm32-unknown-unknown` →
      `Finished` (Phase 1).
- [ ] `cargo build -p donat-wasm-core --target wasm32-unknown-unknown` →
      `Finished`; `core.wasm` produced (Phase 2–3).
- [ ] `make test && cargo build -p donat-server --bin donat && make
      conformance` green at **every** phase boundary (server unchanged).
- [ ] Rust insta snapshots in `donat-wasm-core` for PlanV1 over a query, a
      multi-root mutation, and a permission-error compile — reviewed, never
      blind-accepted.
- [ ] `CGO_ENABLED=0 go build ./sdk/go/...` and `go vet`/`gofmt -l` clean;
      `go test ./sdk/go/...` green including a wazero round-trip test.
- [ ] The native conformance suite passes against the **Go host** (Phase 9)
      with a documented known-diff list if any.
- [ ] Composability proven by tests: engine route mounted in a user mux;
      `ExecuteTx` commits a user write and an engine mutation in one txn;
      `Execute` returns `{"data":...}` with no HTTP.
- [ ] No-admin enforced: a trusted request without `X-Donat-Role` is denied
      with "x-donat-role header is required".

## Out of Scope

- Node host / napi-rs (ADR-003) — later spec.
- Full SQL parameterization (variables + session vars as `$1..$n`) and the
  `(query_hash, role)` hot-path cache — a separate sqlgen/IR refactor (see
  open questions); v1 inlines literals to stay byte-identical.
- Subscriptions/WS delivery beyond the existing one-snapshot behavior — the
  host-side polling loop is acknowledged but specified in a follow-up.
- Sync `pre_insert`/in-txn `post_insert` hooks — the registry seam reserves
  them (Spec 003 §Forward-compatibility); not implemented here.
- Replacing or deprecating `donat-server`.
- MCP/REST surfaces on the Go host.

## Testing Strategy

- **Unit / insta (Rust):** PlanV1 snapshots in `donat-wasm-core`; the catalog
  split keeps existing `donat-schema`/`donat-sqlgen` snapshots unchanged
  (proof the split is behavior-preserving). `cargo insta review` every diff.
- **Go:** wazero round-trip (`core_abi_version`, alloc/compile/dealloc);
  PlanV1 decode + plan-cache; executor integration against the conformance
  Postgres; `ExecuteTx` atomicity; `Handler()` session/no-admin; event-hook
  dispatch. All `CGO_ENABLED=0`.
- **Conformance:** the native harness against the Go host (Phase 9) is the
  drift guard — keep host logic thin, push every decidable rule into PlanV1.
- **Benchmark (ADR-005 confirmation item):** a wazero **compile-latency**
  micro-benchmark on real fixture metadata, asserting the ~0.1–1ms ballpark
  from [[wasm-compiler-core]] §"Cost of a wasm call" before optimizing.

## Risks / Open Questions

1. **Plan-format versioning.** Additive-only within a major; `core_abi_version`
   gate. Risk: a behavior that doesn't fit the declarative `error_map`/`hooks`
   shape — mitigation: start from the exact `gql.rs` cases, extend the
   contract before adding host-side special-casing.
2. **Parameterization debt.** v1 inlines literals (finding above), so the
   plan cache key is coarse and the wasm core is entered more often than the
   ideal. Confirm the wazero compile latency makes this acceptable
   (benchmark); schedule the sqlgen/IR `$n` refactor as the follow-up that
   unlocks the `(query_hash, role)` hot path.
3. **Error-mapping fidelity.** The `23514` path/message come from the
   `donat.check_violation()` JSON payload at runtime (`gql.rs:888-904`) — the
   host, not the plan, reads the DB error body; the plan only says "this code
   maps to permission-error-from-payload." Per-SDK conformance is the check.
4. **Subscriptions host-side.** The polling loop is host work
   ([[wasm-compiler-core]] §"Honest costs" #1); deferred but flagged so it
   isn't discovered late.
5. **wazero compile-latency on real metadata** — one of ADR-005's three
   confirmation items; must be measured, not assumed.
6. **Feature-unification (if shape B).** `cargo` may re-enable drivers in a
   shared graph; the wasm build being a separate target invocation avoids it,
   but shape A (separate types crate) is cleaner and preferred.

## Estimated Complexity

**XL (> 5 days, new subsystem).** Phases 1–2 are S each (compile milestones,
Rust-only). Phase 3 is M (contract + snapshots). Phases 5–9 (the Go execution
layer, error fidelity, composable API, event hooks, per-SDK conformance) are
the bulk — cross-language, cross-crate, with a new serialized contract and a
new runtime (wazero) — comfortably XL overall.

## References

- ADRs: [[knowledgebase/embedded-sdk/decisions/005-wasm-compiler-core-over-cgo-or-go-rewrite]]
  (proposed → this spec confirms),
  [[knowledgebase/embedded-sdk/decisions/007-event-triggers-yaml-and-deploy-time-ddl]],
  [[knowledgebase/embedded-sdk/decisions/003-never-embed-go-runtime-in-node]],
  [[knowledgebase/embedded-sdk/decisions/002-keep-durable-journal-alongside-in-memory-hooks]]
- Design notes: [[knowledgebase/embedded-sdk/wasm-compiler-core]],
  [[knowledgebase/embedded-sdk/performance]],
  [[knowledgebase/embedded-sdk/ffi-boundary]] (the rejected cgo path),
  [[knowledgebase/embedded-sdk/hooks-and-events]],
  [[knowledgebase/embedded-sdk/precedents]]
- Specs: `specs/003-go-event-trigger-sdk-codegen.md` (the reused
  `Event[T]`/`Registry`/`Dispatch` core), `specs/002-event-triggers.md`
- Code: `crates/catalog/src/lib.rs:13-85` (pure types) / `:163,:299,:402`
  (drivers); `crates/catalog/Cargo.toml:10-12`;
  `crates/schema/src/plan.rs:5,215,694` (Planner) / `:86-90` (Plan enum);
  `crates/sqlgen/src/lib.rs:8-11,15-60`;
  `crates/ir/src/lib.rs:106,451-456` (session/literal substitution);
  `crates/server/src/gql.rs:63,500,658,589,871-964` (session, plan, sql,
  txn, error mapping); `crates/server/src/state.rs:89-91,593`
  (boot-time catalog snapshot); `crates/server/src/events.rs` (journal);
  `sdk/go/donat/{event,registry}.go` (Spec 003 seam)
