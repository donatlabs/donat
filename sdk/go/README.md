# Donat for Go

Embed the Donat engine **inside** a Go application. The Rust core — GraphQL
parse, permissions, SQL generation — is compiled to `wasm32` and driven through
[wazero](https://wazero.io), so there is no cgo and no separate engine process.
Your program owns HTTP and the database pool; the core only compiles plans.

```go
eng, err := donat.New(ctx, donat.Config{
    Backend:  donat.Postgres(pool),   // your pool, your lifecycle
    Metadata: coreConfig,             // from `donat dump-core-config`
    Registry: reg,                    // your in-process handlers
})
mux.Handle("/v1/graphql", eng.Handler())
```

`CGO_ENABLED=0` throughout: the SDK is `go get`-able and builds a static
binary.

## Where your code plugs in

There are four extension points, and it is worth being clear about which one
each job belongs to — most of the design mistakes here are picking the wrong
one.

### 1. Event handlers — after the commit

A plain Go function called in-process once the transaction has committed. No
webhook, no second service. The trigger name must match an
`event_triggers[].name` in the YAML metadata.

```go
reg := donat.NewRegistry()
donat.On(reg, "on_loan_recorded", func(ctx context.Context, ev donat.Event[gen.Loan]) error {
    // notify, index, emit a metric, call another service
    return nil
})
```

Use it for work that is allowed to fail without undoing the write. Do **not**
use it to decide whether the write was allowed — by the time it runs, the write
has committed. Those decisions belong in the metadata, where they compile into
the same statement.

Worked example: [`examples/lending-golang/handlers.go`](../../examples/lending-golang/handlers.go).

### 2. `ExecuteTx` — with the commit

When your row must exist if and only if the engine's does, own the transaction
and hand it to the engine:

```go
tx, _ := pool.Begin(ctx)
defer tx.Rollback(ctx)

body, err := eng.ExecuteTx(ctx, tx, mutation, vars, session)
if err != nil || hasErrors(body) {
    return body, err          // refused: nothing is committed
}
myOwnInsert(ctx, tx)
tx.Commit(ctx)
```

The engine issues no `BEGIN` and no `COMMIT`. Post-commit hooks do **not** fire
from `ExecuteTx` — the engine has not committed anything, so it cannot know
when to fire them; side effects after your commit are yours to run.

Worked example: [`examples/lending-golang/audit.go`](../../examples/lending-golang/audit.go).

### 3. Your own routes

`eng.Handler()` is an ordinary `http.Handler`. Mount it in your mux beside your
own routes, with your own middleware and your own auth in front of it.

Worked example: [`examples/lending-golang/server.go`](../../examples/lending-golang/server.go).

### 4. A backend

`donat.Backend` is everything the engine needs from a database. Two
implementations ship:

| Constructor | Status |
| --- | --- |
| `donat.Postgres(pool)` | queries and mutations, plus `ExecuteTx` |
| `donat.SQL(db, dialect)` | **queries only.** `RunMutation` returns an explicit error for every dialect — SQLite mutations need `sqlite_mutation_plan`, which PlanV1 does not carry yet, and MySQL is a planned follow-up |

Implementing the interface yourself is supported, but reach for it only if you
have a database neither covers. `sdk/go/donat/backend_sql.go` is the shorter of
the two to read as a model.

## Sessions and roles

This engine has **no admin role**. Every request runs as one explicit role,
resolved from `X-Donat-*` headers, and a request with no `X-Donat-Role` is
denied before it reaches the database. There is no permission-bypass path and
no admin-over-HTTP surface.

`X-Donat-Admin-Secret` is API-level auth, never a role: the SDK excludes it
from the session variables and does not check it. Deciding who may reach the
handler at all — a secret, a JWT, a mesh identity — belongs to your own
middleware, in front of `eng.Handler()`. What the engine guarantees is that
whatever gets through still runs as exactly one declared role.

## Deploying

The engine never runs DDL. Apply the platform's own migrations, then your
application's, with the `donat` CLI at deploy time, and generate the snapshot
the host embeds:

```bash
donat --database-url "$URL" migrate --migrations-dir <repo>/migrations
donat --database-url "$URL" migrate --migrations-dir migrations
DONAT_GRAPHQL_DATABASE_URL="$URL" donat --database-url "$URL" \
  dump-core-config --metadata-dir metadata --out core-config.json
donat --database-url "$URL" codegen go --metadata-dir metadata --out gen --package gen
```

Do not copy the platform's DDL into your own migrations. The helper functions
it installs are an internal protocol — the GraphQL error decoder pins both
their SQLSTATE and their envelope — and a local copy drifts silently: a stale
function still exists and still raises, just in a shape the decoder no longer
recognises.

The snapshot is compiled at `core_init`, so metadata that does not compile
fails the boot rather than the first request that touches it.

## What the embedded host does not do

Stated plainly, because these fail at boot rather than degrading quietly:

- **Durable Processes.** A command declaring `effects:` that start or signal a
  Process is **refused when the snapshot compiles**. A Process needs a journal,
  a transition queue and leases, which live host-side in `donat-server` and
  have no counterpart here.
- **Connectors.** Connectors are invoked from Process activities
  (`crates/server/src/processes/activity.rs`), so they are unreachable for the
  same reason. Their design is a deploy-time one besides — see
  [ADR 010](../../knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md),
  which rejects loading any plugin at runtime — so a connector is not a place
  user Go code plugs in on either host.
- **File attachments.** Storage is not wired into the wasm core, so
  upload/download URL minting is unavailable.
- **Subscriptions and the REST/MCP surfaces.** GraphQL over HTTP only.

Commands *without* effects — the large majority — plan and execute with the
engine's own SQL. [`tests-system-lending`](../../tests-system-lending) runs the
same cases against both this host and the standalone engine and fails if they
disagree.

## Regenerating the core

The wasm blob is committed so `go get` needs no Rust toolchain, which means
nothing notices when a crate below it changes:

```bash
make wasm-core      # rebuilds sdk/go/donat/wasm/core.wasm
make go-test
```
