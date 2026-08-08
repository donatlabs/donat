# Pluggable `Backend` in the Go Host — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the embedded Go host **database-agnostic** by introducing a `Backend` interface, so adding a database is one small implementation rather than a rewrite of the executor. Wire the SQL dialect through `core_compile` (the wasm core already renders any dialect via sqlgen), refactor the existing pgx path behind `Backend`, and prove the abstraction with a `database/sql` backend running SQLite.

**Architecture:** The wasm core (plan compilation, permissions, session, hooks) is dialect-parameterized: `core_compile` takes a `dialect` and calls `operation_to_sql_with(roots, dialect)` / `mutation_to_sql_with`. On the Go side, everything that varies per database is hidden behind one interface — `Dialect()` (which SQL to render), `RunQuery`/`RunMutation` (execution + `data` assembly, which differs: Postgres assembles JSON in-DB; SQLite/MySQL fold it host-side), and `MapError` (native driver error → Donat body). The `Engine` programs against `Backend`; `donat.Postgres(pool)` is the native fast path, `donat.SQL(db, dialect)` covers any `database/sql` driver.

**Tech Stack:** Rust (`donat-wasm-core`, `donat-sqlgen`/`donat-backend` dialects), Go (`github.com/jackc/pgx/v5`, `database/sql`, `modernc.org/sqlite` — pure Go, no cgo), wazero. All `CGO_ENABLED=0`.

**Scope:** The Go-host backend abstraction + dialect-aware core + Postgres backend (the refactor that locks the abstraction) + a `database/sql` backend with SQLite as the proof. MySQL is left as a follow-up (same `database/sql` backend; only its mutation strategy — companion SELECT — differs). The standalone `donat-server` is UNCHANGED; `make test` + `make conformance` stay green.

**Grounding (verified):**
- sqlgen: `operation_to_sql_with(roots, donat_backend::AnyDialect)` (lib.rs:32), `mutation_to_sql_with(root, AnyDialect)` (lib.rs:818); `donat_backend::{AnyDialect, Dialect, PostgresDialect, SqliteDialect, MySqlDialect}` (backend lib.rs:14).
- wasm core today hardcodes Postgres: `compile.rs:113` `operation_to_sql_opts`, `:137` `mutation_to_sql_opts`.
- Engine's SQLite execution to mirror: `crates/server/src/state.rs` query path reads column 0 as a **text** JSON string then `serde_json::from_str` (`:178-198`); `execute_sqlite_mutations` runs one DML per root and folds the response in Rust (`:242+`).
- Go host today: pgx-only (`sdk/go/donat/executor.go`: `pool.QueryRow`, `pgx.Tx`).

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/wasm-core/src/compile.rs` (modify) | `CompileInput.dialect`; map it to `AnyDialect`; call `operation_to_sql_with`/`mutation_to_sql_with` |
| `crates/wasm-core/tests/plan_snapshots.rs` (modify) | snapshot proving a `sqlite` dialect renders SQLite SQL (distinct from Postgres) |
| `sdk/go/donat/backend.go` (new) | the `Backend` interface (+ optional `txRunner` capability) + shared types |
| `sdk/go/donat/backend_postgres.go` (new) | `Postgres(pool)` — the native pgx backend (moves the current executor logic here) |
| `sdk/go/donat/backend_sql.go` (new) | `SQL(db *sql.DB, dialect string)` — generic `database/sql` backend (Postgres/SQLite/MySQL strategies) |
| `sdk/go/donat/engine.go` (modify) | `Config.Backend`; `compilePlan` passes `backend.Dialect()`; drop the hardcoded `Pool` |
| `sdk/go/donat/executor.go` (modify) | executor delegates to `e.backend.RunQuery/RunMutation/MapError`; `ExecuteTx` via the `txRunner` capability |
| `sdk/go/donat/*_test.go` (modify/new) | update Postgres tests to `donat.Postgres(pool)`; new SQLite backend tests |
| `examples/petshop-golang/main.go` (modify) | `donat.New(Config{Backend: donat.Postgres(pool), …})` |

---

### Task 1 (Rust): thread the dialect through `core_compile`

**Files:** `crates/wasm-core/src/compile.rs`, `crates/wasm-core/tests/plan_snapshots.rs`

- [ ] **Step 1: Add `dialect` to `CompileInput`**

In `compile.rs`, add to `CompileInput`:
```rust
    #[serde(default)]
    pub dialect: Option<String>, // "postgres" (default) | "sqlite" | "mysql"
```
Add a resolver:
```rust
fn dialect_of(name: Option<&str>) -> donat_backend::AnyDialect {
    use donat_backend::*;
    match name {
        Some("sqlite") => AnyDialect::Sqlite(SqliteDialect),
        Some("mysql")  => AnyDialect::Mysql(MySqlDialect),
        _ => AnyDialect::Postgres(PostgresDialect),
    }
}
```
(Confirm the exact `AnyDialect` variant constructors by reading `crates/backend/src/dialect.rs`.) Add `donat-backend` to `crates/wasm-core/Cargo.toml` deps if not already present (sqlgen re-exports it, but depend on it directly for the enum).

- [ ] **Step 2: Use the dialect in `compile()`**

Replace the query render `donat_sqlgen::operation_to_sql_opts(&roots, input.stringify_numerics)` with:
```rust
let dialect = dialect_of(input.dialect.as_deref());
let sql = donat_sqlgen::operation_to_sql_with(&roots, dialect);
```
and the mutation render `mutation_to_sql_opts(&root, …)` with `mutation_to_sql_with(&root, dialect)`. NOTE: `operation_to_sql_with` may not take `stringify_numerics` — check its signature (lib.rs:32). If the dialect path doesn't thread `stringify_numerics`, keep the Postgres default behavior for `stringify=false` (the current default) and note that `stringify_numerics` + dialect is a follow-up if they're mutually exclusive in the current API. Preserve the EXISTING Postgres output byte-for-byte when `dialect` is absent/`"postgres"` (the existing snapshots MUST NOT change).

- [ ] **Step 3: Existing snapshots unchanged + new SQLite snapshot**

Run `cargo test -p donat-wasm-core` — the existing `query_plan_v1`/`mutation_plan_v1`/etc. snapshots MUST be unchanged (Postgres default preserved). If any change, STOP — the default dialect path drifted.

Add `#[test] fn query_plan_v1_sqlite()` (same query/role as `query_plan_v1` but `dialect: Some("sqlite")`) and snapshot it; confirm the `sql` differs from the Postgres snapshot in the dialect-specific way (e.g. quoting/json funcs). Review and accept.

- [ ] **Step 4: Build wasm + refresh blob + commit**

```bash
cargo build -p donat-wasm-core --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/donat_wasm_core.wasm sdk/go/donat/wasm/core.wasm
cargo test -p donat-wasm-core   # all green, no unexpected snapshot diffs
git add crates/wasm-core/ sdk/go/donat/wasm/core.wasm
git commit -m "wasm-core: thread SQL dialect through core_compile (default postgres, +sqlite/mysql)"
```

---

### Task 2 (Go): define the `Backend` interface + refactor the Postgres path behind it

**Files:** `sdk/go/donat/backend.go` (new), `sdk/go/donat/backend_postgres.go` (new), `sdk/go/donat/engine.go`, `sdk/go/donat/executor.go`

- [ ] **Step 1: The interface**

Create `sdk/go/donat/backend.go`:
```go
package donat

import (
	"context"
	"encoding/json"
)

// Backend is everything the engine needs from a database. The engine's plan
// compilation, permissions, session handling and hook firing are
// backend-agnostic; implement Backend once per database.
type Backend interface {
	// Dialect is the SQL flavor the wasm core must render for this backend.
	// One of "postgres", "sqlite", "mysql".
	Dialect() string

	// RunQuery executes a read plan (one statement) and returns the raw JSON
	// `data` value — assembled in-DB (Postgres) or folded host-side (SQLite/MySQL).
	RunQuery(ctx context.Context, plan Plan) (json.RawMessage, error)

	// RunMutation executes a write plan atomically (all statements in one
	// transaction) and returns the per-root alias→value map. On success the
	// engine fires post-commit hooks; on a DB error the backend should return
	// it for MapError.
	RunMutation(ctx context.Context, plan Plan) (map[string]json.RawMessage, error)

	// MapError turns a native driver error into a Donat GraphQL error body,
	// applying the plan's error_map directives.
	MapError(err error, errorMap map[string]string) []byte
}

// txRunner is an OPTIONAL capability: a backend that can run a mutation inside
// a caller-owned transaction (composability — engine writes + user writes share
// one atomic txn). Backends that support it are reached via Engine.ExecuteTx.
type txRunner interface {
	// runMutationTx runs the plan's statements in the caller's transaction
	// (no commit/rollback — the caller owns the lifecycle) and returns data.
	runMutationTx(ctx context.Context, tx any, plan Plan) (map[string]json.RawMessage, error)
}
```

- [ ] **Step 2: Move the pgx logic into a Postgres backend**

Create `sdk/go/donat/backend_postgres.go`. Move the current `runQuery`/`runMutation`/`mapPGError` bodies (from `executor.go`/`errors.go`) into methods on a `postgresBackend{pool *pgxpool.Pool}`:
```go
type postgresBackend struct{ pool *pgxpool.Pool }

// Postgres returns a Backend backed by a pgx pool (the native fast path).
func Postgres(pool *pgxpool.Pool) Backend { return &postgresBackend{pool: pool} }

func (b *postgresBackend) Dialect() string { return "postgres" }
func (b *postgresBackend) RunQuery(ctx, plan) (json.RawMessage, error) { /* pool.QueryRow(stmt.SQL).Scan(&json.RawMessage) */ }
func (b *postgresBackend) RunMutation(ctx, plan) (map[string]json.RawMessage, error) { /* Begin → stmts → Commit; on error Rollback + return err */ }
func (b *postgresBackend) MapError(err, m) []byte { /* the existing mapPGError */ }
// txRunner: run the plan in the caller's pgx.Tx (cast tx.(pgx.Tx)).
func (b *postgresBackend) runMutationTx(ctx, tx any, plan) (map[string]json.RawMessage, error) { pgxTx := tx.(pgx.Tx); /* run stmts, no commit */ }
```
Keep `mapPGError`/`errorBody` (errors.go) — the Postgres backend's `MapError` calls `mapPGError`. The hook-firing stays in the Engine (post-commit), NOT in the backend — `RunMutation` returns data, the Engine fires hooks (it already owns the registry + envelope).

IMPORTANT: `RunMutation` (owned-tx) must NOT fire hooks — that stays the Engine's job after `RunMutation` returns success. So move ONLY the SQL execution into the backend; the Engine's `runMutation` becomes: `data, err := e.backend.RunMutation(ctx, plan); if err==nil { fireHooks(...) }`.

- [ ] **Step 3: Engine programs against Backend**

In `engine.go`: change `Config{ Pool *pgxpool.Pool, … }` → `Config{ Backend Backend, … }`. `New` errors if `Backend == nil`. Drop the `pool` field; store `backend Backend`. In `compilePlan`, set `in.Dialect = e.backend.Dialect()` on the `compileInput` (add the `Dialect string` field with json tag `dialect` to `compileInput`).

In `executor.go`: `Execute` → compile → PlanErrorK→errorBody; query→`e.backend.RunQuery`→wrap `{"data":…}`; mutation→`e.backend.RunMutation`→ on success fire hooks → wrap. On a backend error, `e.backend.MapError(err, plan.ErrorMap)` → body. `ExecuteTx(ctx, tx any, …)`: require `e.backend.(txRunner)` (error if unsupported) → `runMutationTx(ctx, tx, plan)`; queries in a tx similarly. (Change `ExecuteTx`'s `tx pgx.Tx` param to `tx any` so non-pgx backends can pass their own tx; the Postgres backend casts it back. Document this.)

- [ ] **Step 4: Update all existing tests + the example to `donat.Postgres(pool)`**

Update every `donat.New(ctx, Config{Pool: pool, …})` → `Config{Backend: donat.Postgres(pool), …}` across `sdk/go/donat/*_test.go` and `examples/petshop-golang/main.go`. `ExecuteTx` tests pass `tx` (still a `pgx.Tx`, now via `any`).

- [ ] **Step 5: Full green + commit**

```bash
CGO_ENABLED=0 go test ./sdk/go/donat/ -count=1                 # no-DB tests green
DONAT_TEST_PG=1 CGO_ENABLED=0 go test ./sdk/go/donat/ -count=1 # all 33 green
go vet ./sdk/go/... && gofmt -l sdk/go/donat/                  # clean
cd examples/petshop-golang && CGO_ENABLED=0 go build -o /tmp/p .  # example builds
git add sdk/go/donat/ examples/petshop-golang/main.go
git commit -m "sdk(go): pluggable Backend interface; Postgres backend behind it"
```

The Postgres path is behavior-identical (same SQL via the default dialect, same pgx execution, same error bodies) — the existing suite proves it.

---

### Task 3 (Go): generic `database/sql` backend + SQLite proof

**Files:** `sdk/go/donat/backend_sql.go` (new), `sdk/go/donat/backend_sql_test.go` (new), `sdk/go/go.mod`

- [ ] **Step 1: Add a pure-Go SQLite driver (dev/test + optional runtime)**

From `sdk/go/`: `go get modernc.org/sqlite` (pure Go, cgo-free; confirm its go.mod `go` directive is ≤ 1.22 — pin an older version if it requires newer, like we did for wazero/pgx). Keep go.mod at `go 1.22`, no toolchain>1.22.

- [ ] **Step 2: The generic backend**

Create `sdk/go/donat/backend_sql.go`:
```go
type sqlBackend struct {
	db      *sql.DB
	dialect string // "sqlite" | "mysql" | "postgres"
}

// SQL returns a Backend over any database/sql driver. dialect selects the SQL
// the wasm core renders and the result-assembly strategy.
func SQL(db *sql.DB, dialect string) Backend { return &sqlBackend{db: db, dialect: dialect} }

func (b *sqlBackend) Dialect() string { return b.dialect }
```
- **RunQuery (SQLite):** the SQLite query plan is ONE statement returning ONE column of **text** JSON (mirror `state.rs:178-198`). `row := b.db.QueryRowContext(ctx, plan.Statements[0].SQL); var text string; row.Scan(&text); return json.RawMessage(text)`. (For "postgres" via database/sql the column is json/text too — Scan into a string works.)
- **RunMutation (SQLite):** mirror `execute_sqlite_mutations` (`state.rs:242+`) — SQLite can't put DML in a CTE, so the wasm core's `mutation_to_sql_with(root, Sqlite)` emits a strategy the host folds: run each root's statement(s) in one `sql.Tx`, assemble the `data[alias]` map, rollback on a check-violation (SQLSTATE/❲sqlite error❳ for the 23514-equivalent). READ `state.rs` `execute_sqlite_mutations` carefully and reproduce its control flow. If the SQLite mutation SQL shape from `mutation_to_sql_with` doesn't cleanly map to a single `QueryRow`, replicate exactly what the Rust executor does. **If SQLite mutations prove too involved for this task, implement RunQuery (read path) fully + return a clear `"sqlite mutations: not yet implemented"` error from RunMutation, and note it as a follow-up** — the READ path is enough to prove the abstraction; say so honestly.
- **MapError (SQLite):** map the sqlite error for the check-violation to the `permission-error` body; everything else → `data-exception`/`unexpected`. Use the plan's `error_map` the same way `mapPGError` does, but read modernc/sqlite's error type.

- [ ] **Step 3: SQLite proof test**

Create `backend_sql_test.go` (NO external DB — in-memory SQLite `sql.Open("sqlite", ":memory:")`): create the article/author tables in SQLite, build an Engine with `donat.New(Config{Backend: donat.SQL(db, "sqlite"), Metadata: <article/author core-config for sqlite>})`, run `{ article { id title } }` as role `user`, assert the `{"data":{"article":[…]}}` envelope. This proves a SECOND database works through the SAME engine with only a new Backend. (The metadata+catalog JSON can reuse the article/author fixture; the catalog is dialect-independent.) If mutations are deferred (Step 2), test the query path + assert the deferred-mutation error is clean.

- [ ] **Step 4: Green + commit**

```bash
CGO_ENABLED=0 go test ./sdk/go/donat/ -count=1 -run 'TestSQL|TestSQLite'   # green
CGO_ENABLED=0 go test ./sdk/go/donat/ -count=1                            # whole suite green, no DB
DONAT_TEST_PG=1 CGO_ENABLED=0 go test ./sdk/go/donat/ -count=1            # PG suite green
go vet ./sdk/go/... && gofmt -l sdk/go/donat/
git add sdk/go/donat/backend_sql.go sdk/go/donat/backend_sql_test.go sdk/go/go.mod sdk/go/go.sum
git commit -m "sdk(go): generic database/sql backend + SQLite (proves the Backend abstraction)"
```

---

### Task 4: regression + judge

- [ ] **Step 1: Engine untouched + green** — `make test && cargo build -p donat-server --bin donat && make conformance` green; `git diff <base>..HEAD -- crates/server/` empty (only `crates/wasm-core` + `sdk/go` + the example changed). `cargo build -p donat-wasm-core --target wasm32-unknown-unknown --release` Finishes; blob current.
- [ ] **Step 2: Go cgo-free** — `CGO_ENABLED=0 go build ./sdk/go/...`; full suite green (PG + SQLite); vet+gofmt clean; the petshop example builds + runs (smoke).
- [ ] **Step 3: Judge** (hard no-git-state-changes). Continue only after ACCEPT.

---

## Self-Review

**Goal coverage:**
- "interface so adding a DB is small" → `Backend` (Task 2). ✓
- "many databases" → generic `database/sql` backend (Task 3) covers SQLite/MySQL/etc. via a dialect string. ✓
- dialect through the core → Task 1. ✓
- Postgres native fast path preserved → `donat.Postgres` (Task 2), existing 33 tests green. ✓
- proof with a second DB → SQLite (Task 3). ✓
- composability/`ExecuteTx` preserved → `txRunner` capability (Task 2 Step 3). ✓

**Placeholder scan:** The SQLite mutation strategy is the one genuinely-hard spot; it is explicitly scoped with a documented fallback (read-path proves the abstraction; mutation parity is a clear follow-up if the RETURNING-fold is too involved) rather than a vague TODO. The Rust `AnyDialect` constructors + `state.rs` SQLite control flow are flagged as verbatim cross-checks.

**Type consistency:** `Backend` (`Dialect`/`RunQuery`/`RunMutation`/`MapError`) + `txRunner` are the types the Engine (Task 2 Step 3) and both backends (postgres, sql) implement; `compileInput.Dialect` (json `dialect`) matches the Rust `CompileInput.dialect` (Task 1). `Config.Backend` replaces `Config.Pool` consistently across tests + example (Task 2 Step 4).

**Out of scope (follow-ups):** MySQL backend (same generic backend; companion-SELECT mutation strategy); SQLite/MySQL through the petshop example + docker-compose; full `stringify_numerics` × dialect; the universal `*sql.Tx` ExecuteTx for non-pgx backends (the capability hook exists; wiring a `database/sql` txRunner is incremental).
