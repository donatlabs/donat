# wasm-core Phase 3 — Go Host Execution Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the pure-Go host execution layer that drives the wasm core end-to-end: decode `PlanV1`, run its SQL against a user-supplied `pgx` pool, envelope the result as Donat `{"data":...}`/error bodies, expose the engine as an embeddable library (`http.Handler` + programmatic `Execute`/`ExecuteTx`), and fire native event-trigger hooks in-process — all `CGO_ENABLED=0`.

**Architecture:** The Go `donat` package wraps the Phase 2 wazero core (`wasmCore`). An `Engine` holds a pool of wazero instances (single-threaded each), a user-supplied `*pgxpool.Pool`, the serialized metadata+catalog, and a Spec 003 `*Registry`. A request flows: resolve session from `X-Donat-*` headers → `core_compile` → decode `PlanV1` → executor runs statements (query: one row/one json column; mutation: one txn, statements in order, then fire post-commit hooks) → map pg errors via the plan's `error_map` to the exact Donat error bodies. The engine is composable: the pool is injected, `Handler()` mounts alongside user routes, and `ExecuteTx` runs inside a caller-owned transaction.

**Tech Stack:** Go (pure, `CGO_ENABLED=0`), `github.com/tetratelabs/wazero` v1.9.0, `github.com/jackc/pgx/v5` (+ `pgxpool`), the existing Spec 003 `Event[T]`/`Registry`. Rust side: a small `donat-wasm-core` addition to populate `PlanV1.hooks`.

**Scope:** Phase 3 of Spec 004 — task-breakdown items **#4–#9**. Depends on Phase 2 (PlanV1 + ABI + wazero loader, committed). The standalone `donat-server` stays unchanged; `make test` + `make conformance` stay green (the engine never imports the host layer). Out of scope (later): Node host, full `$n` parameterization, WS/subscriptions beyond one snapshot, sync pre-hooks, MCP/REST on the Go host.

**Governing contract:** The engine's exact behavior is the spec. Cross-check every error body against `crates/server/src/gql.rs:875-964` and the query/mutation execution against `crates/server/src/state.rs:159-177` and `crates/server/src/gql.rs:567-600`. PlanV1 shape is `crates/wasm-core/src/plan.rs`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/wasm-core/src/compile.rs` (modify) | Populate `PlanV1.hooks` from metadata `event_triggers` matching each mutated table+op |
| `crates/wasm-core/tests/plan_snapshots.rs` (modify) | Snapshot asserting a mutation on a table with an event trigger emits the hook |
| `sdk/go/donat/plan.go` (new) | Go mirror of PlanV1 (`Plan`, `Statement`, `Hook`, error body); decode + version check |
| `sdk/go/donat/engine.go` (new) | `Config`, `Engine`, instance pool, `core_init` seeding, `compile()` host call, plan cache |
| `sdk/go/donat/executor.go` (new) | Query + mutation execution against pgx; envelope; post-commit hook firing |
| `sdk/go/donat/errors.go` (new) | pg SQLSTATE → Donat error body via the plan's `error_map` (mirrors `db_error_json`) |
| `sdk/go/donat/session.go` (new) | `X-Donat-*` header → session-vars map (no-admin rule) |
| `sdk/go/donat/handler.go` (new) | `Engine.Handler()` http.Handler; `Execute`/`ExecuteTx` |
| `sdk/go/donat/*_test.go` (new) | unit (plan decode, cache, session, error map) + integration (against conformance Postgres) + composability + event-hook tests |
| `.github/workflows/ci.yml` (modify) | Extend the go-sdk job to run the integration tests against a Postgres service |

---

## Conventions

- **Postgres only** for the host executor v1 (the wasm-core emits Postgres SQL via `operation_to_sql_opts`; SQLite/MySQL host paths are out of scope — the standalone server keeps those).
- **Integration tests** use the conformance Postgres (container `dist-api-pg` on `127.0.0.1:15432`, `postgres/postgres`). Gate them behind an env check (`DONAT_TEST_PG`) so unit tests run without a DB.
- **Fixture parity:** integration tests load the SAME metadata+catalog the wasm-core snapshots use (the article/author fixture, or a small migrated schema) so the Go executor's SQL is exercised end-to-end against a real DB.
- All Go builds/tests: `CGO_ENABLED=0`.

---

### Task 3.0 (Rust): populate PlanV1 hooks from event_triggers

**Files:**
- Modify: `crates/wasm-core/src/compile.rs`
- Modify: `crates/wasm-core/tests/plan_snapshots.rs`

- [ ] **Step 1: Read the metadata event-trigger shape**

Read `crates/metadata/src/types.rs:505-560` (the `TableEntry.event_triggers: Vec<EventTrigger>`, `EventTrigger { name, definition: EventTriggerDefinition { insert/update/delete } }`) and `crates/server/src/events.rs:95-135` (how an op maps to INSERT/UPDATE/DELETE). Determine, for a mutation root targeting `schema.table`, which `event_triggers` fire for the op (Insert→`definition.insert`, Update→`definition.update`, Delete→`definition.delete` present).

- [ ] **Step 2: Write the failing snapshot test**

In `crates/wasm-core/tests/plan_snapshots.rs`, extend `fixture_state()`'s metadata so the `author` table declares an `event_triggers` entry (e.g. `on_author_change` on insert) — mirror the YAML shape as JSON in the inline `metadata()` helper. Add:

```rust
#[test]
fn mutation_emits_event_hook() {
    let state = fixture_state();
    let input = CompileInput {
        query: "mutation { insert_author(objects: [{name: \"Alice\"}]) { affected_rows } }".to_string(),
        operation_name: None,
        variables: Default::default(),
        session_vars: [("x-donat-role".to_string(), "user".to_string())].into_iter().collect(),
        stringify_numerics: false,
    };
    let plan = compile(&state, &input);
    insta::assert_json_snapshot!(plan);
}
```

Run: `cargo test -p donat-wasm-core mutation_emits_event_hook` → the snapshot will show `hooks: []` (current behavior). This is the red baseline.

- [ ] **Step 3: Implement hook population in the mutation arm**

In `compile.rs`, after building the mutation `statements`, derive `hooks` by, for each mutation root, resolving its target `(schema, table, op)` and scanning `state.metadata` for that table's `event_triggers` whose definition includes the op. Emit `Hook { phase: "post_commit".into(), trigger: et.name.clone(), schema, table, op }`. (The op for each `MutationRoot` variant: Insert/InsertOne→"INSERT", Update/UpdateByPk→"UPDATE", Delete/DeleteByPk→"DELETE"; FunctionCall/Typename→no hook. Resolve the target table from the root — read how `MutationRoot` carries its table in `crates/ir/src/lib.rs`.) Replace `hooks: vec![]` in the mutation `PlanBody` with the derived vec. Leave the query path `hooks: vec![]`.

- [ ] **Step 4: Review the snapshot + build**

Run `cargo test -p donat-wasm-core mutation_emits_event_hook`; review the `.snap.new`: it must now contain one `Hook` with `phase:"post_commit"`, `trigger:"on_author_change"`, `schema:"public"`, `table:"author"`, `op:"INSERT"`. Accept after reading. Confirm the OTHER mutation snapshot (a table without triggers) still shows `hooks: []` (no diff there). `cargo build -p donat-wasm-core --target wasm32-unknown-unknown --release` Finishes.

- [ ] **Step 5: Commit + refresh blob**

```bash
cargo build -p donat-wasm-core --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/donat_wasm_core.wasm sdk/go/donat/wasm/core.wasm
git add crates/wasm-core/src/compile.rs crates/wasm-core/tests/ crates/wasm-core/src/snapshots/ sdk/go/donat/wasm/core.wasm
git commit -m "wasm-core: populate PlanV1 hooks from table event_triggers"
```

---

### Task 3.1 (Go): PlanV1 mirror + decode + version guard

**Files:**
- Create: `sdk/go/donat/plan.go`
- Create: `sdk/go/donat/plan_test.go`

- [ ] **Step 1: Failing decode test**

Create `sdk/go/donat/plan_test.go` with a table of PlanV1 JSON samples (a query, a mutation with a hook, an error) decoded via `decodePlan`, asserting `Kind`, `Transaction`, statement SQL/alias, `Hooks`, and `ErrorMap`. Run `CGO_ENABLED=0 go test ./donat/ -run TestDecodePlan` → FAIL (no `decodePlan`).

- [ ] **Step 2: Implement the mirror**

Create `sdk/go/donat/plan.go`:

```go
package donat

import (
	"encoding/json"
	"fmt"
)

// PlanKind is the discriminant of a compiled plan.
type PlanKind string

const (
	PlanQuery    PlanKind = "query"
	PlanMutation PlanKind = "mutation"
	PlanErrorK   PlanKind = "error"
)

// Plan is the Go mirror of the Rust PlanV1 contract (crates/wasm-core/src/plan.rs).
type Plan struct {
	Kind        PlanKind
	Version     uint32
	Transaction bool
	Statements  []Statement
	Hooks       []Hook
	ErrorMap    map[string]string
	Err         *PlanErr // set when Kind == PlanErrorK
}

type Statement struct {
	Alias  string            `json:"alias"`
	SQL    string            `json:"sql"`
	Params []json.RawMessage `json:"params"`
}

type Hook struct {
	Phase   string `json:"phase"`
	Trigger string `json:"trigger"`
	Schema  string `json:"schema"`
	Table   string `json:"table"`
	Op      string `json:"op"`
}

type PlanErr struct {
	Code    string `json:"code"`
	Path    string `json:"path"`
	Message string `json:"message"`
}

// wirePlan matches the serde-tagged JSON: {"kind": "...", ...}.
type wirePlan struct {
	Kind        PlanKind          `json:"kind"`
	Version     uint32            `json:"version"`
	Transaction bool              `json:"transaction"`
	Statements  []Statement       `json:"statements"`
	Hooks       []Hook            `json:"hooks"`
	ErrorMap    map[string]string `json:"error_map"`
	Code        string            `json:"code"`
	Path        string            `json:"path"`
	Message     string            `json:"message"`
}

func decodePlan(raw []byte) (Plan, error) {
	var w wirePlan
	if err := json.Unmarshal(raw, &w); err != nil {
		return Plan{}, fmt.Errorf("decode plan: %w", err)
	}
	if w.Kind != PlanErrorK && w.Version != ABIVersion {
		return Plan{}, fmt.Errorf("plan version %d != supported %d", w.Version, ABIVersion)
	}
	p := Plan{
		Kind: w.Kind, Version: w.Version, Transaction: w.Transaction,
		Statements: w.Statements, Hooks: w.Hooks, ErrorMap: w.ErrorMap,
	}
	if w.Kind == PlanErrorK {
		p.Err = &PlanErr{Code: w.Code, Path: w.Path, Message: w.Message}
	}
	return p, nil
}
```

- [ ] **Step 3: Pass + commit**

`CGO_ENABLED=0 go test ./donat/ -run TestDecodePlan -v` PASS; `gofmt -l`/`go vet` clean.
```bash
git add sdk/go/donat/plan.go sdk/go/donat/plan_test.go
git commit -m "sdk(go): PlanV1 mirror + decode + version guard"
```

---

### Task 3.2 (Go): Engine, instance pool, core_init seeding, compile() + plan cache

**Files:**
- Modify: `sdk/go/donat/wasmcore.go` (add `init`/`compile` calls)
- Create: `sdk/go/donat/engine.go`
- Create: `sdk/go/donat/engine_test.go`
- Modify: `sdk/go/go.mod` (add pgx)

- [ ] **Step 1: Extend the wazero wrapper with init/compile**

Add to `sdk/go/donat/wasmcore.go` methods `initState(ctx, cfgJSON []byte) error` (alloc+write cfg, call `core_init`, expect 0) and `compile(ctx, inputJSON []byte) ([]byte, error)` (alloc+write input, call `core_compile`, decode the packed i64 → ptr/len, read bytes from `mod.Memory().Read`, copy them out, `core_dealloc`). Use `mod.ExportedFunction("core_init")` / `"core_compile"` (add to `newWasmCore`'s required-exports check). Unpack: `ptr := uint32(packed >> 32); n := uint32(packed)`.

- [ ] **Step 2: Engine + pool + cache + Config (composable: pool injected)**

Create `sdk/go/donat/engine.go`:

```go
package donat

import (
	"context"
	"fmt"
	"sync"

	"github.com/jackc/pgx/v5/pgxpool"
)

// Config constructs an Engine. The pool is supplied by the caller — the
// engine never opens connections itself (composability requirement).
type Config struct {
	Pool     *pgxpool.Pool // required: user-owned pgx pool
	Metadata []byte        // serialized {"metadata":..., "catalog":...} for core_init
	Registry *Registry     // optional: Spec 003 native event-trigger handlers
	PoolSize int           // wasm instance pool size (default 4)
}

// Engine is an embeddable Donat GraphQL engine backed by the wasm core.
type Engine struct {
	cfg      Config
	pool     *pgxpool.Pool
	registry *Registry
	mu       sync.Mutex
	insts    []*wasmCore   // idle wazero instances, each seeded by core_init
	cache    sync.Map      // planCacheKey -> Plan
}

func New(ctx context.Context, cfg Config) (*Engine, error) {
	if cfg.Pool == nil {
		return nil, fmt.Errorf("donat.New: Config.Pool is required")
	}
	if cfg.PoolSize == 0 {
		cfg.PoolSize = 4
	}
	e := &Engine{cfg: cfg, pool: cfg.Pool, registry: cfg.Registry}
	// Pre-seed one instance to fail fast on a bad metadata/catalog blob.
	c, err := e.newSeededInstance(ctx)
	if err != nil {
		return nil, err
	}
	e.insts = append(e.insts, c)
	return e, nil
}

func (e *Engine) newSeededInstance(ctx context.Context) (*wasmCore, error) {
	c, err := newWasmCore(ctx)
	if err != nil {
		return nil, err
	}
	if err := c.initState(ctx, e.cfg.Metadata); err != nil {
		_ = c.close(ctx)
		return nil, fmt.Errorf("core_init: %w", err)
	}
	return c, nil
}

func (e *Engine) acquire(ctx context.Context) (*wasmCore, error) {
	e.mu.Lock()
	if n := len(e.insts); n > 0 {
		c := e.insts[n-1]
		e.insts = e.insts[:n-1]
		e.mu.Unlock()
		return c, nil
	}
	e.mu.Unlock()
	return e.newSeededInstance(ctx)
}

func (e *Engine) release(c *wasmCore) {
	e.mu.Lock()
	if len(e.insts) < e.cfg.PoolSize {
		e.insts = append(e.insts, c)
		e.mu.Unlock()
		return
	}
	e.mu.Unlock()
	_ = c.close(context.Background())
}

type planCacheKey struct{ query, role, varsHash, sessHash string }

// compilePlan runs the wasm core (or returns a cached Plan). Coarse key
// because v1 inlines literals (Spec 004 finding): variables+session are
// part of the key.
func (e *Engine) compilePlan(ctx context.Context, in compileInput) (Plan, error) {
	key := planCacheKey{in.Query, in.SessionVars["x-donat-role"], hashJSON(in.Variables), hashMap(in.SessionVars)}
	if v, ok := e.cache.Load(key); ok {
		return v.(Plan), nil
	}
	c, err := e.acquire(ctx)
	if err != nil {
		return Plan{}, err
	}
	defer e.release(c)
	inJSON, err := json.Marshal(in)
	if err != nil {
		return Plan{}, err
	}
	out, err := c.compile(ctx, inJSON)
	if err != nil {
		return Plan{}, err
	}
	p, err := decodePlan(out)
	if err != nil {
		return Plan{}, err
	}
	e.cache.Store(key, p)
	return p, nil
}
```

Add `compileInput` (Go mirror of the Rust `CompileInput`: `Query`, `OperationName`, `Variables map[string]json.RawMessage` or `json.RawMessage`, `SessionVars map[string]string`, `StringifyNumerics`), and `hashJSON`/`hashMap` helpers (sha256 hex) — define them in engine.go. Import `encoding/json`, `crypto/sha256`.

- [ ] **Step 3: Add pgx, build, basic engine test (no DB)**

From `sdk/go/`: `go get github.com/jackc/pgx/v5@latest` (and confirm it stays compatible with `go 1.22` — pin an older pgx/v5 if needed; check its go.mod `go` directive like wazero, and if it requires >1.22 pin the newest pgx/v5 that needs ≤1.22). Write `engine_test.go` `TestEngineRequiresPool` (New with nil pool → error) and a `TestCompileCacheKey` unit. `CGO_ENABLED=0 go test ./donat/ -run 'TestEngine|TestCompileCacheKey'` PASS.

- [ ] **Step 4: Commit**

```bash
git add sdk/go/donat/wasmcore.go sdk/go/donat/engine.go sdk/go/donat/engine_test.go sdk/go/go.mod sdk/go/go.sum
git commit -m "sdk(go): Engine + wazero instance pool + core_init/compile + plan cache"
```

---

### Task 3.3 (Go): query executor + Donat envelope (integration)

**Files:**
- Create: `sdk/go/donat/executor.go`
- Create: `sdk/go/donat/executor_query_test.go`

- [ ] **Step 1: Failing integration test (gated on DONAT_TEST_PG)**

Create `executor_query_test.go`: skip unless `os.Getenv("DONAT_TEST_PG") != ""`. Build an Engine over a pgxpool to the conformance PG, with the article/author fixture metadata+catalog and a migrated `article`/`author` schema (CREATE TABLE + seed in the test setup, or reuse an existing migration). `eng.Execute(ctx, "query { article { id title } }", session "user")` → assert the returned bytes equal `{"data":{"article":[...]}}` matching what the Rust server returns for the same query. Run with `DONAT_TEST_PG=1 CGO_ENABLED=0 go test ./donat/ -run TestQueryExecute` → FAIL (no Execute).

- [ ] **Step 2: Implement the query executor**

Create `sdk/go/donat/executor.go` with `func (e *Engine) runQuery(ctx, plan Plan) ([]byte, error)`: the query plan has one `Statement`; acquire a conn from `e.pool`, `pool.QueryRow(ctx, stmt.SQL).Scan(&data)` into a `json.RawMessage` (the engine's `query_one` + `try_get::<Json>(0)` — one row, one json column, `state.rs:171-176`), wrap as `{"data": <data>}` via `json.Marshal(map...)` or manual `[]byte` assembly. On pg error, return it for the caller to map (Task 3.5). Add `Execute` stub in handler.go later; for this task expose enough to test (a package-internal `executeQuery`).

- [ ] **Step 3: Pass + commit**

`DONAT_TEST_PG=1 CGO_ENABLED=0 go test ./donat/ -run TestQueryExecute -v` PASS; the bytes match the Rust server's output for the same query (document the cross-check). 
```bash
git add sdk/go/donat/executor.go sdk/go/donat/executor_query_test.go
git commit -m "sdk(go): query executor against pgx pool + Donat data envelope"
```

---

### Task 3.4 (Go): error mapping (mirror db_error_json)

**Files:**
- Create: `sdk/go/donat/errors.go`
- Create: `sdk/go/donat/errors_test.go`

- [ ] **Step 1: Failing unit test**

Create `errors_test.go`: feed synthetic `*pgconn.PgError`s (codes 23514 with a JSON payload message, 23505, 23503, 23502, and an unknown code) plus the plan's `error_map` into `mapPGError`, asserting the produced JSON equals the engine bodies from `crates/server/src/gql.rs:884-918`:
- 23514 with payload `{"path":P,"message":M}` → `{"errors":[{"extensions":{"path":P,"code":"permission-error"},"message":M}]}`
- 23505 → code `constraint-violation`, message `Uniqueness violation. <dbmsg>`
- 23503 → `Foreign key violation. <dbmsg>`; 23502 → `Not-NULL violation. <dbmsg>`
- else → code `data-exception`, message `<dbmsg>`
Run `CGO_ENABLED=0 go test ./donat/ -run TestMapPGError` → FAIL.

- [ ] **Step 2: Implement `mapPGError`**

Create `errors.go`. Use `errors.As` to extract `*pgconn.PgError` (`github.com/jackc/pgx/v5/pgconn`). Implement the `db_error_json` logic verbatim: for 23514, `json.Unmarshal([]byte(pgErr.Message))` and if it has string `path`+`message`, emit `permission-error` with them; otherwise fall through to the code table. The `error_map` from the plan provides the `code:prefix` directives (`"constraint-violation:Uniqueness violation. "` → split on first `:` → code + message prefix; `"permission-error-from-payload"` → the 23514 payload path; `"data-exception"` → default). Provide `errorBody(code, path, message string) []byte` matching `error_json` (path defaults to `"$"`). Cross-check the trailing spaces.

- [ ] **Step 3: Pass + commit**

`CGO_ENABLED=0 go test ./donat/ -run TestMapPGError -v` PASS.
```bash
git add sdk/go/donat/errors.go sdk/go/donat/errors_test.go
git commit -m "sdk(go): pg SQLSTATE -> Donat error bodies via plan error_map"
```

---

### Task 3.5 (Go): mutation executor + ExecuteTx + post-commit hooks

**Files:**
- Modify: `sdk/go/donat/executor.go`
- Create: `sdk/go/donat/executor_mutation_test.go`

- [ ] **Step 1: Failing integration tests**

Add `executor_mutation_test.go` (gated on DONAT_TEST_PG): (a) a mutation runs all statements in one txn and returns the combined `{"data":{alias: ...}}`; (b) a check-violation (23514) rolls back and returns the `permission-error` body; (c) `ExecuteTx` with a caller-owned `pgx.Tx` runs an engine mutation AND a user `tx.Exec` and both commit atomically (assert both effects present after commit; assert rollback removes both). Run → FAIL.

- [ ] **Step 2: Implement mutation execution + ExecuteTx**

In `executor.go` add `runMutation(ctx, plan, tx pgx.Tx)`: if `tx` is nil, `e.pool.Begin(ctx)` (own it: commit/rollback); else use the caller's tx (do NOT commit it — the caller owns commit). For each `Statement` in order: `tx.QueryRow(ctx, stmt.SQL).Scan(&part)`, set `data[stmt.Alias] = part`. On error: if we own the tx, rollback; map via `mapPGError` and return the body. On success of an owned tx: `tx.Commit(ctx)`, then **fire hooks** (Step 3). Return `{"data": data}`. (Mirrors `crates/server/src/gql.rs:567-600` — one statement per root, one transaction.)

- [ ] **Step 3: Fire post-commit hooks (Spec 003 Registry.Dispatch)**

After a successful owned-tx commit, for each `Hook` in `plan.Hooks` with `Phase=="post_commit"`, build the Donat event envelope and call `e.registry.Dispatch(ctx, hook.Trigger, envelopeJSON)`. The envelope mirrors `crates/server/src/events.rs` envelope shape: `{id, created_at, table:{schema,name}, trigger:{name}, event:{op, data:{old,new}, session_variables}, delivery_info:{...}}`. v1 simplification (document it): the in-process executor builds `data.new` from the mutation's returned row(s) when available and `data.old` for updates/deletes where the statement RETURNs them; if the single-statement JSON-assembled mutation does not expose per-row old/new, emit the envelope with the available `returning` rows and `session_variables` from the session, and `log` that richer old/new capture is a follow-up (the durable journal in `events.rs` still has the full capture). Only fire when `e.registry != nil` and a handler is registered (`Dispatch` returns `ErrNoHandler` otherwise — ignore that, matching at-least-once/optional-handler semantics). Keep hook errors from failing the (already-committed) mutation: log/collect them per Spec 003 at-least-once intent.

- [ ] **Step 4: Pass + commit**

`DONAT_TEST_PG=1 CGO_ENABLED=0 go test ./donat/ -run 'TestMutation|TestExecuteTx'` PASS.
```bash
git add sdk/go/donat/executor.go sdk/go/donat/executor_mutation_test.go
git commit -m "sdk(go): mutation executor + ExecuteTx (shared txn) + post-commit hook firing"
```

---

### Task 3.6 (Go): Handler(), session resolution, Execute/ExecuteTx, composability

**Files:**
- Create: `sdk/go/donat/session.go`
- Create: `sdk/go/donat/handler.go`
- Create: `sdk/go/donat/handler_test.go`

- [ ] **Step 1: Session resolution (no-admin)**

Create `session.go`: `func sessionFromHeaders(h http.Header) (map[string]string, error)` mirroring `crates/server/src/gql.rs:58-126` — collect `X-Donat-*` headers (lowercased keys), require `x-donat-role` (else the no-admin error). NOTE: the engine also has an admin-secret path (`resolve_session`) — for the embedded library v1, support the plain per-role path only (the BLOCKING no-admin rule still holds: no role → denied). Document that admin-secret/JWT auth is the host app's responsibility (it owns the HTTP middleware), consistent with the composable design.

**Parity note (from the Phase 2 judge):** the Phase 2 `crates/wasm-core/src/compile.rs` `session_from` recognizes only the exact string `"true"` for `x-donat-use-backend-only-permissions`, whereas the engine (`crates/server/src/gql.rs:96-110`) accepts `true/t/yes/y` (case-insensitive) and emits a `bad-request` error on an invalid value. In THIS task reach full parity in BOTH places: implement the same accept-set + invalid→`bad-request` in the Go `sessionFromHeaders`, AND update the Rust `session_from` (a tiny wasm-core change + a refreshed `core.wasm`) so the two agree. Cross-check the exact `bad-request` code/message from `gql.rs` and add a unit test for each accepted spelling + an invalid value.

- [ ] **Step 2: Execute / ExecuteTx / Handler**

Create `handler.go`:
- `func (e *Engine) Execute(ctx, query string, vars map[string]json.RawMessage, sessionVars map[string]string) ([]byte, error)`: compilePlan → if `plan.Kind==PlanErrorK` return the error body (`errorBody(plan.Err...)`); query→runQuery; mutation→runMutation(nil).
- `func (e *Engine) ExecuteTx(ctx, tx pgx.Tx, query, vars, sessionVars) ([]byte, error)`: same but mutation runs in the caller's `tx` (no hooks fired here by default, or fired after the caller commits — document: hooks fire on owned-tx commit; for ExecuteTx the caller controls commit, so hooks are the caller's responsibility or a post-commit callback — keep v1 simple: ExecuteTx does NOT fire hooks, documented).
- `func (e *Engine) Handler() http.Handler`: parse the GraphQL POST body (`{query, variables, operationName}`), `sessionFromHeaders`, call Execute, write the JSON (always HTTP 200 with a GraphQL body, matching the engine — errors are in the body, not the status).

- [ ] **Step 2b: Composability test (mixed mux) + no-admin**

Create `handler_test.go`: (a) mount `eng.Handler()` at `/v1/graphql` in an `http.ServeMux` next to a user `HandleFunc("/custom", ...)`; assert both respond. (b) a request with no `x-donat-role` returns the `"x-donat-role header is required (this engine has no admin role)"` body. (a) can run without a DB if Execute short-circuits on the no-role denial before touching pg; the full query path test is gated on DONAT_TEST_PG.

- [ ] **Step 3: Pass + commit**

`CGO_ENABLED=0 go test ./donat/ -run 'TestHandler|TestSession|TestNoAdmin'` PASS; `go vet`/`gofmt -l` clean.
```bash
git add sdk/go/donat/session.go sdk/go/donat/handler.go sdk/go/donat/handler_test.go
git commit -m "sdk(go): Handler() + session resolution + Execute/ExecuteTx (composable, no-admin)"
```

---

### Task 3.7: per-SDK conformance hook + CI

**Files:**
- Modify: `.github/workflows/ci.yml`
- Create: `sdk/go/donat/conformance_test.go` (or a small runner)

- [ ] **Step 1: Decide the conformance vector**

The native harness (`crates/conformance`) is HTTP-level. The drift guard is: stand up `eng.Handler()` over the conformance Postgres with a representative fixture, and replay a small set of the engine's own request/response expectations against it (queries, a mutation, a permission denial, the no-role denial). Implement `conformance_test.go` (gated on DONAT_TEST_PG) that issues these HTTP requests to an `httptest.Server` wrapping `eng.Handler()` and byte-compares the bodies to the recorded engine outputs. Document this is a SUBSET (the full native harness against the Go host is a larger follow-up); `log`/comment what is and isn't covered (no silent over-claim).

- [ ] **Step 2: CI**

Extend the `go-sdk` job in `.github/workflows/ci.yml`: add a `postgres:16` service, set `DONAT_TEST_PG`, run the migrations/seed, and run `CGO_ENABLED=0 go test ./...`. Keep the existing cgo-free build + vet + gofmt steps. Keep `go-version: "1.22"`.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/ci.yml sdk/go/donat/conformance_test.go
git commit -m "ci+sdk(go): per-SDK conformance subset against the Go host over Postgres"
```

---

### Task 3.8: full regression + judge

- [ ] **Step 1: Engine untouched + green**

`make test && cargo build -p donat-server --bin donat && make conformance` all green; `git diff <phase3-base>..HEAD -- crates/server/` empty. `cargo build -p donat-wasm-core --target wasm32-unknown-unknown --release` Finishes; the embedded `core.wasm` is current (rebuilt in Task 3.0).

- [ ] **Step 2: Go suite green, cgo-free**

`CGO_ENABLED=0 go build ./sdk/go/... && CGO_ENABLED=0 DONAT_TEST_PG=1 go test ./sdk/go/...` green; `go vet ./sdk/go/...` + `gofmt -l sdk/go/` clean.

- [ ] **Step 3: Judge** (hard no-git-state-changes constraint). Continue only after ACCEPT.

---

## Self-Review

**Spec coverage (Spec 004 task-breakdown #4–#9):**
- #4 Go PlanV1 mirror + plan cache → Task 3.1 + Task 3.2 (cache). ✓
- #5 Go executor query path → Task 3.3. ✓
- #6 mutation/txn + ExecuteTx + error mapping → Tasks 3.4, 3.5. ✓
- #7 Handler() + session + composability → Task 3.6. ✓
- #8 native event-trigger hooks → Task 3.0 (Rust: emit hooks) + Task 3.5 Step 3 (fire). ✓
- #9 per-SDK conformance → Task 3.7. ✓
- Acceptance: composability (mixed mux + ExecuteTx shared txn + Execute returns {"data":...}) → Tasks 3.5/3.6; no-admin denial → Task 3.6; CGO_ENABLED=0 green → Task 3.8; engine unchanged + conformance green → Task 3.8. ✓

**Placeholder scan:** Code blocks are concrete Go; the few spots needing live-DB/pgx-version adaptation (pgx version pin like wazero; envelope old/new richness) are called out as explicit decisions with the engine behavior as the contract, not vague TODOs. Event-envelope old/new is honestly scoped (v1 uses returned rows; richer capture flagged as follow-up, durable journal unaffected).

**Type consistency:** `Plan`/`Statement`/`Hook`/`PlanErr` (Task 3.1) are the types consumed by the executor (3.3/3.5), error mapper (3.4), and engine (3.2). `Engine`/`Config`/`compileInput`/`compilePlan`/`acquire`/`release` (3.2) are referenced consistently in 3.3–3.6. `mapPGError`/`errorBody` (3.4) used by 3.5. `Registry`/`Dispatch`/`Event[T]` are the existing Spec 003 names. The Rust `Hook` fields (3.0) match the Go `Hook` JSON tags (3.1).

**Open decisions for the implementer (flagged, grounded):** pin pgx/v5 to a `go 1.22`-compatible version (same check as wazero); confirm `MutationRoot`'s table accessor in `crates/ir/src/lib.rs` for hook target resolution; the exact event envelope field names from `crates/server/src/events.rs`.

**Out of scope (later):** Node host; full `$n` parameterization + `(query_hash, role)` hot path; WS/subscriptions; sync pre-hooks; MCP/REST on the Go host; running the FULL native harness against the Go host (3.7 does a documented subset).
