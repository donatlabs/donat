# petshop-golang — the minimal embedded host

A Donat API served from a Go binary, in as little Go as it takes. The whole
program:

```go
//go:embed core-config.json
var coreConfig []byte

func main() {
    donat.Main(donat.WithMetadata(coreConfig))
}
```

That is not an abridged listing — it is `main.go`. The behaviour lives in
`metadata/`; the pool, the mux, the listener and the graceful shutdown are what
`donat.Main` does. No cgo: the Rust core is compiled to `wasm32` and driven
through [wazero](https://wazero.io), so the binary is static and the module is
`go get`-able.

Start here when you want a Donat API inside your own process. Go to
[`examples/lending-golang`](../lending-golang) to see the parts you add to it.

## Running it

```bash
docker compose up --build
```

That is postgres → two one-shot migrates → the app. Then:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'X-Donat-Role: staff' \
  -d '{"query":"{ pet(limit: 3) { id name status } }"}'
```

Every request runs as exactly one role, taken from `X-Donat-Role`. A request
without it is denied before it reaches the database — this engine has no admin
role, and there is no way around a permission.

## Running it directly

The engine never runs DDL, so the schema is applied out-of-band, exactly the
way the standalone engine deploys:

```bash
donat --database-url "$URL" migrate --migrations-dir ../../migrations
donat --database-url "$URL" migrate --migrations-dir migrations
donat --database-url "$URL" dump-core-config --metadata-dir metadata
DONAT_DATABASE_URL="$URL" go run .
```

`--help` lists what `main` reads from the environment; `--version` reports the
core ABI the binary carries.

Do not copy the platform's own migrations into this directory. The helper
functions they install are an internal protocol that the GraphQL error decoder
pins by SQLSTATE and envelope, and a local copy drifts silently: a stale
function still exists and still raises, just in a shape the decoder no longer
recognises.

## The snapshot

`core-config.json` is `{metadata, catalog}` — the metadata directory plus the
live catalog, compiled by the core at startup. Metadata that does not compile
fails the boot rather than the first request that touches it.

It is generated and then committed, so it goes stale the moment the metadata or
the schema changes, and the engine cannot notice: a stale snapshot is a
perfectly valid one. Check it in CI:

```bash
donat --database-url "$URL" dump-core-config --metadata-dir metadata --check
```

## What is here

| Path | What it is |
| --- | --- |
| `metadata/` | Tables, per-role permissions, relationships. The behaviour. |
| `migrations/` | This application's own schema and demo seed. The platform's catalog is not copied here; it is applied from the engine's own `migrations/`. |
| `core-config.json` | The compiled snapshot the binary embeds. |
| `main.go` | Which snapshot to serve. |
| `Dockerfile` | Multi-stage, `CGO_ENABLED=0`, distroless final image. |
| `docker-compose.yml` | `db` → two one-shot migrates → `app`. |
| `go.mod` | Module with a `replace` to the in-repo SDK. |

## What you add next

This example deliberately declares nothing it does not implement — no event
triggers without handlers, no actions without functions. Each of the three
extension points is one option away.

**Logic no declaration can express** — rendering a file, calling a library.
Declare an action in the metadata *without* a `handler`, which is what makes it
resolved in this process, and write the body:

```go
donat.Main(
    donat.WithMetadata(coreConfig),
    donat.WithFunction("render_invoice_pdf", renderInvoicePDF),
)
```

The engine refuses to start if a declared action has no function, or if the Go
struct disagrees with the arguments the metadata declares.

**Work that runs after a write commits** — notifying, indexing, emitting a
metric. Declare `event_triggers` on the table and register handlers with
`donat.WithRegistry`. Use it only for work that may fail without undoing the
write; whether the write was *allowed* belongs in the metadata, where it
compiles into the same statement.

**Your own routes, middleware or auth** — build the engine with `donat.New`
instead, and mount `eng.Handler()` in your own mux beside your own routes.

All three are worked through in
[`examples/lending-golang`](../lending-golang); the SDK reference is
[`sdk/go/README.md`](../../sdk/go/README.md).
