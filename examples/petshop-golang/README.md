# petshop-golang — standalone in-memory embedded engine

A self-contained petshop demo that runs the Donat GraphQL engine **in-process**
inside a single Go binary. The Rust engine executes as a WebAssembly module via
[wazero](https://wazero.io) — no cgo, no shared libraries, no Rust binary
needed at runtime. Event triggers fire **registered Go handlers in-process**,
not over an HTTP webhook.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Go process (CGO_ENABLED=0, single static binary)           │
│                                                             │
│  net/http mux                                               │
│    /v1/graphql  ──►  donat.Engine.Handler()                 │
│    /healthz     ──►  your own handler  (composability)      │
│                           │                                 │
│                    wazero (wasm runtime, pure Go)           │
│                    ┌──────────────┐                         │
│                    │  core.wasm   │  ←  Rust engine         │
│                    │  (embedded)  │     compiled to wasm    │
│                    └──────┬───────┘                         │
│                           │ SQL                             │
│                       pgxpool ──► Postgres                  │
│                           │                                 │
│              post-commit hooks (in-process, no webhook)     │
│              ┌─────────────────────────────┐               │
│              │  donat.Registry              │               │
│              │  "on_order_placed" handler   │               │
│              │  "on_pet_status"   handler   │               │
│              └─────────────────────────────┘               │
└─────────────────────────────────────────────────────────────┘
```

**Key difference from the old webhook model:** previously, the engine POSTed
event envelopes to a separate HTTP handler service; the Go process received
them over the network. Here the engine embeds the Rust core as wasm, and after
each mutation commits, the in-process registry dispatches to your Go handlers
directly — no HTTP round-trip, no separate handler service, no retry queue.

The `webhook:` fields that remain in `metadata/**/*.yaml` are placeholders kept
for schema compatibility. They are never contacted; the in-process registry
intercepts every event before any HTTP delivery is attempted.

## What is in this directory

| Path | Purpose |
|---|---|
| `gen/donat_gen.go` | Generated row structs — `donat codegen go` output, do not edit |
| `main.go` | The full app: migrations, seed, registry, engine, mux |
| `core-config.json` | Pre-serialised `{metadata, catalog}` snapshot for `core_init` |
| `metadata/` | Petshop YAML metadata with `event_triggers` declarations |
| `migrations/` | DDL files applied at boot (V0 = donat schema, V1–V5 = petshop) |
| `Dockerfile` | Multi-stage, CGO_ENABLED=0, distroless final image |
| `docker-compose.yml` | `db` (postgres:16) + `app` (this binary); self-migrating |
| `go.mod` | Module with `replace` to the in-repo SDK |

## Quick start — docker-compose

```bash
# From the repository root:
docker-compose -f examples/petshop-golang/docker-compose.yml up --build
```

The app:
1. Connects to Postgres
2. Applies migrations idempotently at boot (donat schema, petshop DDL, seed data)
3. Registers the two Go event handlers
4. Starts the embedded wasm engine
5. Listens on `:8080`

No Rust binary, no `donat migrate`, no separate handler service needed.

## Demo queries

All requests go to `http://localhost:8080/v1/graphql`. The engine enforces the
no-admin rule: `X-Donat-Role` is always required.

### Query pets (staff role sees all rows)

```bash
curl -s -X POST http://localhost:8080/v1/graphql \
  -H "Content-Type: application/json" \
  -H "X-Donat-Role: staff" \
  -d '{"query":"query { pet(limit:3) { id name status price } }"}' | jq .
```

Expected response:

```json
{
  "data": {
    "pet": [
      {"id": 1, "name": "Rex",     "status": "available", "price": 350.00},
      {"id": 2, "name": "Bella",   "status": "available", "price": 420.00},
      {"id": 3, "name": "Whiskers","status": "available", "price":  90.00}
    ]
  }
}
```

### Update a pet status — fires `on_pet_status` handler in-process

```bash
curl -s -X POST http://localhost:8080/v1/graphql \
  -H "Content-Type: application/json" \
  -H "X-Donat-Role: staff" \
  -d '{"query":"mutation { update_pet(where:{id:{_eq:1}}, _set:{status:\"sold\"}) { affected_rows returning { id name status } } }"}' | jq .
```

Check the app logs — you will see the handler fire immediately after the
mutation commits (no webhook, no round-trip):

```
[event] on_pet_status fired: op=UPDATE trigger=on_pet_status table=pet
```

### Insert an order — fires `on_order_placed` handler in-process

Customer role requires `X-Donat-User-Id` (the session variable the metadata
uses as the `customer_id` preset):

```bash
curl -s -X POST http://localhost:8080/v1/graphql \
  -H "Content-Type: application/json" \
  -H "X-Donat-Role: customer" \
  -H "X-Donat-User-Id: 1" \
  -d '{"query":"mutation { insert_orders(objects:[{status:\"placed\"}]) { affected_rows returning { id customer_id status } } }"}' | jq .
```

App log:

```
[event] on_order_placed fired: op=INSERT trigger=on_order_placed table=orders
```

### Update an order status — fires `on_order_placed` UPDATE handler

```bash
curl -s -X POST http://localhost:8080/v1/graphql \
  -H "Content-Type: application/json" \
  -H "X-Donat-Role: staff" \
  -d '{"query":"mutation { update_orders(where:{id:{_eq:1}}, _set:{status:\"shipped\"}) { affected_rows returning { id customer_id status } } }"}' | jq .
```

App log:

```
[event] on_order_placed fired: op=UPDATE trigger=on_order_placed table=orders
```

### Composability — your own route next to the engine

```bash
curl http://localhost:8080/healthz
# {"status":"ok"}
```

`/healthz` is a plain `mux.HandleFunc` registered in `main.go` alongside
`eng.Handler()`. The engine does not own the server — you do.

## How it works

### Engine construction

```go
//go:embed core-config.json
var coreConfig []byte

eng, err := donat.New(ctx, donat.Config{
    Pool:     pool,       // your pgxpool.Pool
    Metadata: coreConfig, // pre-serialised metadata+catalog snapshot
    Registry: reg,        // in-process event handler registry
    PoolSize: 4,          // wasm instance pool
})
```

`core-config.json` was produced by:

```bash
donat dump-core-config \
  --database-url postgresql://... \
  --metadata-dir examples/petshop-golang/metadata \
  --out examples/petshop-golang/core-config.json
```

Note: `--database-url` is a top-level flag (before the subcommand), not a
subcommand flag. Regenerate this file whenever the metadata or schema changes.

### Event handler registration

```go
reg := donat.NewRegistry()

donat.On(reg, "on_pet_status", func(_ context.Context, ev donat.Event[gen.Pet]) error {
    log.Printf("[event] on_pet_status fired: op=%s table=%s", ev.Op, ev.Table.Name)
    return nil
})
```

The trigger name (`"on_pet_status"`) must match the `event_triggers[].name`
in the table YAML. The handler is called synchronously after the mutation's
Postgres transaction commits — in the same Go process, no network.

### SDK v1 data limitation

In the current SDK v1, the in-process hook envelope carries the mutation result
shape (e.g. `{"affected_rows":1,"returning":[...]}`) as `ev.New`, not the
individual captured row. `ev.Table` and `ev.Trigger` are always accurate.
Full old/new row capture (matching the webhook envelope) is a planned v2
follow-up. The webhook model carried proper row data via the PG trigger
(`donat.notify_event()`); the in-process path currently provides the mutation
result. Handlers should use `ev.Table.Name` / `ev.Op` for routing and query
the database for row details if needed.

## Building locally (no Docker)

```bash
# From the repo root:
cd examples/petshop-golang
CGO_ENABLED=0 go build -o /tmp/petshop .

DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/petshop_golang \
  /tmp/petshop
```

The binary is fully static — no libc, no cgo, no runtime dependencies other
than the Postgres connection you point it at.

## Roles and headers

| Role | Header | Capabilities |
|---|---|---|
| `staff` | `X-Donat-Role: staff` | Full read/write on all tables |
| `customer` | `X-Donat-Role: customer` + `X-Donat-User-Id: <id>` | Own orders + available pets |
| `anonymous` | `X-Donat-Role: anonymous` | Available pets only, read-only |

The engine enforces the no-admin rule: `X-Donat-Role` is always required.
Requests without it are denied with an `access-denied` error.
