# petshop-golang — native Go event-trigger handlers

Handle Donat **event triggers** in a Go program with typed handlers, instead
of writing a webhook receiver by hand. The engine POSTs event envelopes to
*your* HTTP server; the [`sdk/go`](../../sdk/go) SDK decodes each envelope into
a generated row struct and routes it to the handler you registered.

This reuses the schema and metadata from [`../petshop`](../petshop) — only the
event-trigger consumer is new.

## What's here

| File | Purpose |
|---|---|
| `gen/donat_gen.go` | Generated row structs — produced by `donat codegen go`, **do not edit** |
| `main.go` | A plain `net/http` server that decodes envelopes and dispatches to handlers |
| `go.mod` | Depends on the in-repo SDK via a `replace` directive |

## 1. Generate the types (don't hand-write them)

The structs in `gen/` are produced from the live database by the engine — run
this whenever the petshop schema changes:

```bash
# Postgres with the petshop schema applied (see ../petshop for docker-compose):
export DONAT_DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/postgres
donat migrate --migrations-dir ../petshop/migrations

# Generate Go structs for the metadata-tracked tables:
donat codegen go \
  --metadata-dir ../petshop/metadata \
  --out ./gen \
  --package gen
```

This writes `gen/donat_gen.go` (one struct per tracked table) and runs `gofmt`
on it. The pg→Go mapping: `serial/integer → int32`, `numeric → decimal.Decimal`
(lossless), `text → string`, nullable column → pointer, `timestamptz →
time.Time`.

## 2. Declare the event triggers in metadata

Point each trigger's webhook at this server (add to the relevant table YAML
under `../petshop/metadata`, then `donat migrate --metadata-dir ../petshop/metadata`
to reconcile the Postgres triggers):

```yaml
# in databases/default/tables/public_orders.yaml
event_triggers:
  - name: on_order_placed          # must match donat.On(reg, "on_order_placed", ...)
    definition:
      insert: { columns: '*' }
      update: { columns: [status] }
    retry_conf: { num_retries: 3, interval_sec: 10, timeout_sec: 60 }
    webhook: 'http://host.docker.internal:8081/events'

# in databases/default/tables/public_pet.yaml
event_triggers:
  - name: on_pet_status
    definition:
      update: { columns: [status] }
    retry_conf: { num_retries: 3, interval_sec: 10, timeout_sec: 60 }
    webhook: 'http://host.docker.internal:8081/events'
```

## 3. Run the handler server

```bash
go run .          # listens on :8081 (override with ADDR=:9000)
```

Then any insert/update on `orders` / `pet` in the engine is delivered here:

```
order #7 placed by customer 1 (status=placed)
pet "Whiskers" (#3) sold for 90
```

The HTTP status code drives the engine's **at-least-once** retry contract:
`2xx` acks the event, `5xx` makes the engine retry per `retry_conf`. Handlers
must be idempotent.

## How it works

`main.go` owns the transport — it's an ordinary `net/http` server. The SDK does
only two things: decode the envelope (`donat.Event[gen.Orders]`, with
`Old`/`New` typed row pointers) and route by trigger name
(`reg.Dispatch(ctx, name, body)`). Registration is one typed call per trigger:

```go
donat.On(reg, "on_order_placed", func(ctx context.Context, ev donat.Event[gen.Orders]) error {
    // ev.Op, ev.Old (nil on INSERT), ev.New (nil on DELETE), ev.Session
    return nil
})
```

No cgo, no engine runtime embedded: `go build` / `go get` and ship one static
binary (`CGO_ENABLED=0 go build .` works).

> The delivery transport is the webhook receiver you see in `main.go`. A future
> in-process transport (engine embedded via a pure-Go wasm core) will reuse the
> exact same `donat.On(...)` handlers — see `knowledgebase/embedded-sdk`.
