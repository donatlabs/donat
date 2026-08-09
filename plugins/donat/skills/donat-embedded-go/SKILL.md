---
name: donat-embedded-go
description: Use when the donat engine runs inside a Go program rather than as a standalone server, or when something cannot be declared and needs an in-process Go function instead of a webhook service.
---

# The engine inside a Go program

The Rust core — parse, permissions, SQL generation — compiles to `wasm32` and
runs through wazero inside your Go binary. No cgo, no second process, one
`go get`. The standalone `donat-server` is unchanged; this is additive.

```go
package main

import "github.com/donatlabs/donat/sdk/go/donat"

func main() { donat.Main() }
```

That is a whole program. The behaviour is metadata; Go is where the parts no
declaration can express live.

Install: `go get github.com/donatlabs/donat/sdk/go@v0.2.0`. Builds with
`CGO_ENABLED=0` and no Rust toolchain.

## Why this changes the "or escalate" rule

Without the Go host, an action or an event trigger delivers to a **URL** —
which means somebody writes and operates a service. With it, both resolve
**in-process**. The escape hatch stops being "build a second system" and
becomes "write a function", which is a different order of cost and a different
thing to maintain.

That is the whole reason this skill exists. Read `declaring-not-coding` for
where the function sits in the order of preference: still after every
declarative option, but well before a separate service.

## Deploy shape

The engine never runs DDL, and the metadata is compiled against the live
catalog into a snapshot before it is served:

```sh
donat --database-url <url> migrate --migrations-dir <repo>/migrations
donat --database-url <url> migrate --migrations-dir migrations
donat --database-url <url> dump-core-config --metadata-dir metadata
DONAT_DATABASE_URL=<url> DONAT_CORE_CONFIG=core-config.json go run .
```

`core-config.json` is a **build output, not source**. Generating it needs a
database and running the app needs one too, so a committed copy saves nobody a
step while going stale in a way the engine cannot detect. Do not check it in.

Metadata is compiled at `core_init`, so metadata that does not compile fails
the **boot** rather than the first request.

## The three extension points

### 1. A function behind a declared action

An action declared **without** `handler:` is resolved by a Go function
registered under its name.

```yaml
# metadata/actions.yaml — no `handler:`, so the body is in this process
actions:
  - name: render_loan_receipt
    definition:
      type: mutation
      arguments:
        - { name: loan_id, type: uuid! }
      output_type: LoanReceipt
    permissions:
      - role: member

custom_types:
  objects:
    - name: LoanReceipt
      fields:
        - { name: file_id, type: uuid! }
        - { name: bytes, type: Int! }
```

```go
donat.Main(donat.WithFunction("render_loan_receipt", renderReceipt))
```

- The return value is validated against the declared `output_type` exactly as a
  webhook body would be — a field declared `String!` that the function leaves
  empty is refused, not returned. One metadata file cannot mean two things.
- **The engine refuses to start** when a handler-less action has no registered
  function. A field that is in the schema and always fails is worse than a
  failed boot. The standalone server refuses the mirror case: an action with no
  handler to call.
- `permissions` still governs who may call it. This is not a way around roles.

Use it for what genuinely cannot be a statement: rendering a document, calling
a library, reaching a system with no declarative connector.

### 2. An event handler after the commit

An event trigger declared in the table metadata, dispatched **in-process** once
the transaction commits.

```yaml
# metadata/databases/default/tables/public_loan.yaml
event_triggers:
  - name: on_loan_recorded
    definition:
      enable_manual: false
      insert: { columns: "*" }
      update: { columns: [status] }     # only a status change fires it
    retry_conf: { num_retries: 3, interval_sec: 5, timeout_sec: 60 }
    webhook: http://in-process/events   # required by the shape, never dialled
```

```go
reg := donat.NewRegistry()
donat.On(reg, "on_loan_recorded", func(ctx context.Context, ev donat.Event[gen.Loan]) error {
    // notify, index, emit a metric, call another service
    return nil
})
```

The trigger name must match `event_triggers[].name`. The `webhook` URL is
required by the metadata shape and never dialled in this model — the Go
executor dispatches from the compiled plan's hooks.

**What belongs here:** work that must not undo the write if it fails — sending
the "due back on…" mail, pushing a row to a search index, a metric, calling
another service.

**What does not:** deciding whether the write was allowed. By the time this
runs, the write has committed. That decision is a permission, a validator or a
command guard, enforced inside the statement.

### 3. `ExecuteTx` — inside your own transaction

For a row that must exist **if and only if** the engine's write exists. A
post-commit handler cannot do this: a crash between the commit and the handler
loses the row.

```go
tx, _ := pool.Begin(ctx)
defer tx.Rollback(ctx)

body, err := eng.ExecuteTx(ctx, tx, borrowMutation, vars, map[string]string{
    "x-donat-role":    "member",
    "x-donat-user-id": memberID,
})
if err != nil { return nil, err }
// a GraphQL error body is a refusal, not a host failure — return without
// committing, so no audit row survives a loan that did not happen
myOwnInsert(ctx, tx)
tx.Commit(ctx)
```

The engine issues no `BEGIN` and no `COMMIT`. **Post-commit hooks do not fire**
from `ExecuteTx` — the engine has not committed anything, so it cannot know
when to. Side effects after your own commit are yours to run.

The application supplies the pool; the engine opens no connections of its own.
That is what makes this possible.

### 4. Your own routes

`eng.Handler()` is an ordinary `http.Handler`. Mount it beside your own routes
in your own mux, with your own middleware:

```go
mux.Handle("/v1/graphql", eng.Handler())
mux.HandleFunc("/healthz", myHealth)
```

## Typed rows and arguments

`donat codegen go` emits the argument and result types of every action beside
the table row types, and the engine checks the Go structs against the metadata
at boot. Without it, a `json` tag that does not match a declared argument
decodes to the zero value and answers 200 — a silent wrong answer. Generate the
types; do not hand-write the structs.

## What the embedded host does not do

Each fails loudly rather than degrading, but they fail at different moments,
and the difference decides what a green deploy actually proves.

**Durable Processes — it originates them, it does not drive them.** A command
*may* start or signal a Process: the effect compiles into the same statement as
the command's own writes, so the journal row and the rows that justify it
commit together. What this host does not do is carry the Process forward —
transitions, timers, activity leases and webhook ingress are a runtime loop
that lives in `donat-server`.

So the shape is not "no Processes here". It is: **run a `donat-server` against
the same database** and it drives what the Go host originates. Without one, a
started Process sits in the journal doing nothing. Publishing the Process
revisions is a **third** `migrate` step — the journal row's foreign key names
the revision, so a command that starts a Process fails on that constraint until
it is published. `examples/lending-golang` is exactly this shape, and
`tests-system-lending` runs an engine beside the Go host to prove the Process
completes.

**Connectors.** Invoked from Process activities, so this host reaches none, and
a Process declaring one fails to compile here. Not a gap in the host: ADR 010
rejects loading any plugin at runtime, so a connector is not where user Go code
plugs in on either host.

**Subscriptions and the REST/MCP surfaces.** GraphQL over HTTP only.

Commands *without* effects — the large majority — plan and execute with the
engine's own SQL, identically on both hosts.

## Testing

This is where the Go host pays for itself twice: the wiring that usually breaks
is testable without a database, and the behaviour is testable through the same
handler production serves.

### Unit — the wiring, no database

`donat.TestEngine` checks what a plain unit test misses: whether the Go structs
agree with the metadata, whether the role may reach the action, whether what
you return satisfies the declared output type. All decided before any SQL runs.

```go
eng, _ := donat.TestEngine(ctx, snapshot,
    donat.WithFunction("render_invoice_pdf", renderInvoicePDF))

body, _ := eng.Execute(ctx, query, nil,
    map[string]string{"x-donat-role": "user"})
```

An operation that does read tables says so rather than panicking; give those
`donat.WithBackend(donat.Postgres(pool))`.

### Integration — through the handler, as a role

Drive the service the way it is driven in production: GraphQL over
`eng.Handler()`, as one of the declared roles.

**Do not reimplement the decision in the test.** A test that computes the
expected answer in Go is testing itself. Ask the service to do something, then
assert what the *YAML* said would happen — the limit from `rules.yaml`, the
atomic hold from the command, the counter from the extension.

Requires Postgres. Take the DSN from an environment variable and **skip the
whole file when it is absent** — a run with no database must not look like a
passing run. That single rule is what keeps a green CI honest.

### System — both hosts, and they must agree

`tests-system-lending` runs the same cases against the embedded host and the
standalone engine and fails if the two disagree. Worth copying whenever an
application runs both, which it does the moment it has a Process.

### The snapshot goes stale silently

`core-config.json` is generated from the metadata and a live catalog. It rots
the moment either changes, and the engine cannot notice, because a stale
snapshot is still a valid one. Gate it in CI:

```bash
donat --database-url "$URL" dump-core-config --metadata-dir metadata --check
```

Non-zero when the committed file is not what would be written now.

### Error codes are part of the contract

A function's error is the caller's answer. The default is `unexpected`, which
is what an unclassified fault is. When you know better, say so:

```go
return Out{}, donat.Errorf("validation-failed", "invoice %s has no lines", id)
```

## Talking about it to a non-technical partner

They do not need to know about wasm. What they need is the boundary:

> Almost everything here is written as rules the system reads — you can change
> a limit or who sees what without a developer. Two kinds of thing still need
> code: making a document, and doing something after a change goes through, like
> sending mail. Those are small, named pieces, and they live in one file.

Then keep the promise: if a requirement lands in Go, say so at the time and say
which file, rather than letting the Go side quietly accumulate the rules.

## Checklist

1. Every handler-less action has a registered function, and vice versa — the
   boot check enforces it, so run the boot.
2. No event handler decides whether a write was allowed.
3. A row that must be atomic with the engine's write uses `ExecuteTx`, not a
   handler.
4. `core-config.json` generated at deploy time, never committed.
5. Types generated with `donat codegen go`, not hand-written.
6. Nothing in the metadata that the embedded host refuses — processes,
   connectors, subscriptions, REST/MCP — unless the target is the standalone
   server.

## Files to read

- [`examples/lending-golang`](https://github.com/donatlabs/donat/tree/main/examples/lending-golang)
  — the worked host, and the shape a new application should start from:
  metadata for the decisions, Go for the side effects, unit and integration
  tests beside them.
- [`examples/lending-golang/main.go`](https://github.com/donatlabs/donat/blob/main/examples/lending-golang/main.go)
  — own pool, own mux, registry, functions, secrets.
- [`examples/lending-golang/handlers.go`](https://github.com/donatlabs/donat/blob/main/examples/lending-golang/handlers.go)
  — event handlers, with the "belongs here / does not belong here" split
  written out.
- [`examples/lending-golang/audit.go`](https://github.com/donatlabs/donat/blob/main/examples/lending-golang/audit.go)
  — `ExecuteTx`, and why a post-commit hook cannot do the same job.
- [`examples/lending-golang/lending_test.go`](https://github.com/donatlabs/donat/blob/main/examples/lending-golang/lending_test.go)
  — integration tests through the handler, and the rule that a test which
  recomputes the answer in Go is testing itself.
- [`examples/petshop-golang/main.go`](https://github.com/donatlabs/donat/blob/main/examples/petshop-golang/main.go)
  — the minimal host: `main` is one line, and the package doc says where each
  kind of addition goes.
- [`sdk/go/README.md`](https://github.com/donatlabs/donat/blob/main/sdk/go/README.md)
  — the extension points, `TestEngine`, error codes, and exactly what this host
  does not do.
- [`tests-system-lending`](https://github.com/donatlabs/donat/tree/main/tests-system-lending)
  — the same cases against both hosts, failing if they disagree.
