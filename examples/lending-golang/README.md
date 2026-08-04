# Lending — a Go application with its business logic in YAML

A small library: members borrow copies, return them, and extend loans. It
exists to show one split, on something small enough to read in a sitting.

|                          | where it lives                       |
| ------------------------ | ------------------------------------ |
| The lending decisions    | `metadata/commands/*.yaml`           |
| The thresholds they read | `metadata/rules.yaml`                |
| What happens afterwards  | `handlers.go`                        |
| What must happen *with* it | `audit.go`                         |

Nothing in the Go code decides whether a loan is allowed. `borrow_copy` checks
that the copy is on the shelf, that the member is under their own limit, and
holds the copy — all inside **one PostgreSQL statement**, so there is no moment
where a copy is held but unborrowed, and no moment where two members both see
the last copy as available. The Go half handles what comes *after* the commit:
notifying, integrating, logging.

Four tables the engine knows about, three commands, three rules, one
handler — and one table the engine has never heard of, which `audit.go` writes
inside the engine's own transaction.

## What each command demonstrates

**`borrow_copy`** — the whole vocabulary in one place: `select_one` with
`require_found` for the copy, `select_many` + `aggregate` to count the member's
open loans, `assert` against a rule for the limit, an `update` whose predicate
re-states `status: available` so the hold is atomic, and an `insert` for the
loan. Steps read each other's output; none of it is visible until all of it
commits.

**`return_copy`** — the mirror. Closing the loan and releasing the copy happen
together, so neither can be observed without the other. `status: active` in the
predicate is what makes a second return a rejection rather than a silent no-op.

**`extend_loan`** — a rule doing arithmetic rather than only gating. The
`extensions` counter is incremented by the same declared rule the limit reads,
so the value that is checked and the value that is written cannot be computed
differently.

## Why a rule and not an `if`

`rules.yaml` says:

```yaml
- name: within_loan_limit
  parameters: { open_loans: bigint!, loan_limit: int! }
  result: bool!
  expression: "open_loans < loan_limit"
```

That expression compiles into the borrow statement. Raising the limit is a
metadata change, not a deploy of new Go — and, more importantly, the check runs
*inside* the transaction that creates the loan. The same check written in Go
would run either before the write, where it races, or after it, where it is too
late.

## When after-the-commit is not good enough

A handler runs after the transaction commits, which is right for notifying
somebody and wrong for a row that must exist if and only if the loan does — a
crash between the commit and the handler would lose it.

`audit.go` shows the other tool. The application opens the transaction, hands
it to the engine with `ExecuteTx`, writes its own row on the same handle, and
decides whether to commit:

```go
tx, _ := pool.Begin(ctx)
defer tx.Rollback(ctx)

body, err := eng.ExecuteTx(ctx, tx, borrowMutation, vars, session)
if err != nil || hasErrors(body) {
    return body, err          // refused: nothing is committed
}
recordAudit(ctx, tx, member, "borrow", copyID)
tx.Commit(ctx)
```

The engine issues no `BEGIN` and no `COMMIT`. If the audit insert fails the
loan rolls back with it, and a refused borrow leaves no audit row claiming one
happened. The price is that post-commit hooks do not fire — the engine has not
committed anything, so it cannot know when to fire them, and side effects after
the caller's own commit are the caller's job.

## Why the handler is Go and not YAML

`handlers.go` runs after the transaction commits, in the same process, with no
webhook and no second service:

```go
donat.On(reg, "on_loan_recorded", func(ctx context.Context, ev donat.Event[gen.Loan]) error {
    // notify the member, push to a search index, emit a metric
    return nil
})
```

This is the half that should be code: it talks to the outside world, it is
allowed to fail without undoing the loan, and it is where an application's
integrations actually live. A check written here would run after the write had
already committed — which is exactly why the *decisions* are not here.

## Running it

The engine never runs DDL, and this example carries no copy of the platform's
DDL either — the platform's deploy-time catalog is applied from the platform's
own migrations, so the helper functions its error decoder pins cannot drift:

```bash
# 1. the platform's catalog, then this library's tables
donat --database-url "$URL" migrate --migrations-dir ../../migrations
donat --database-url "$URL" migrate --migrations-dir migrations

# 2. the snapshot the Go host embeds (metadata + catalog, compiled at boot)
DONAT_GRAPHQL_DATABASE_URL="$URL" donat --database-url "$URL" \
  dump-core-config --metadata-dir metadata --out core-config.json

# 3. serve
DATABASE_URL="$URL" go run .
```

`docker compose up` wires the same three steps.

Borrow something:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'Content-Type: application/json' \
  -H 'X-Donat-Role: member' \
  -H "X-Donat-User-Id: $MEMBER_ID" \
  -d '{"query":"mutation { borrow_copy(copy_id: \"…\", borrowed_on: \"2026-08-04\", due_on: \"2026-08-18\") { loan_id due_on } }"}'
```

There is no admin role. A request with no `X-Donat-Role` is denied, and a
librarian cannot invoke a member's command — the command names its roles, and
the tables grant writes on a separate `command_*_permissions` plane so no raw
CRUD root can bypass a command's guards.

## Tests

```bash
LENDING_TEST_PG="postgresql://…/lending" go test ./...
```

Fifteen cases against a real database, driven through the engine's own handler:
the limit, the atomic hold, double-return, the extension counter, four
concurrent borrowers leaving exactly one loan, the handler firing, and
`ExecuteTx` committing the loan and the audit row together — or neither.
Without `LENDING_TEST_PG` they skip.

The black-box suite in [`tests-system-lending`](../../tests-system-lending)
goes further: it runs the same cases against **both** the standalone Rust
engine and this Go host, from the same YAML, and fails if they disagree.

## Files

| Path | What it is |
| --- | --- |
| `metadata/commands/` | the three lending commands |
| `metadata/rules.yaml` | the thresholds and arithmetic they read |
| `metadata/databases/default/tables/` | table permissions, including the `command_*` plane and the event trigger |
| `migrations/` | this library's tables — no platform DDL |
| `handlers.go` | the Go event modules, called after the commit |
| `audit.go` | `ExecuteTx`: an application write in the engine's transaction |
| `main.go`, `server.go`, `config.go` | wiring, routes, environment |
| `gen/donat_gen.go` | row structs from `donat codegen go`; do not edit |
| `core-config.json` | the compiled snapshot, from `donat dump-core-config` |
