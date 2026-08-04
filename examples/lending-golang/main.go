// Command lending-golang is a small library-lending service whose business
// logic is declared in YAML and whose side effects are written in Go.
//
// The split it exists to demonstrate:
//
//	metadata/commands/*.yaml   the lending decisions — availability, the
//	                           borrowing limit, the atomic hold, the extension
//	                           counter. Each compiles to ONE PostgreSQL
//	                           statement, so no invariant has a window.
//	metadata/rules.yaml        the arithmetic and thresholds those decisions
//	                           read, as expressions rather than Go.
//	handlers.go                what happens AFTER a loan commits — notifying,
//	                           logging, integrating. Ordinary Go, called
//	                           in-process, no webhook and no second service.
//
// Nothing in this directory reimplements borrowing. `borrowCopy` in the Go
// tests is a GraphQL call; the rule that refuses a fourth loan lives in
// rules.yaml and is enforced inside the database statement.
//
//	┌─────────────────────────────────────────────────────────┐
//	│  Go process (single binary, CGO_ENABLED=0)              │
//	│                                                         │
//	│  net/http mux            (server.go)                    │
//	│    /v1/graphql  ──►  donat.Engine.Handler()             │
//	│    /healthz     ──►  your own handler (composability)   │
//	│                           │                             │
//	│                    wazero (wasm runtime)                │
//	│                    ┌──────────────┐                     │
//	│                    │  core.wasm   │ ← Rust planner      │
//	│                    │  (embedded)  │   compiled to wasm  │
//	│                    └──────┬───────┘                     │
//	│                           │ one SQL statement           │
//	│                       pgxpool ──► Postgres              │
//	│                           │                             │
//	│   post-commit hooks, in-process   (handlers.go)         │
//	└─────────────────────────────────────────────────────────┘
//
// Schema is applied out-of-band with the platform's own tooling — the engine
// never runs DDL, and this example carries no copy of the platform's DDL:
//
//	donat --database-url <url> migrate --migrations-dir <repo>/migrations
//	donat --database-url <url> migrate --migrations-dir migrations
//	go run .
//
// docker-compose.yml wires the same flow: postgres → two one-shot migrates → app.
package main

import (
	"context"
	_ "embed"
	"log"
	"net/http"

	"github.com/donatlabs/donat/sdk/go/donat"
	"github.com/jackc/pgx/v5/pgxpool"
)

// coreConfig is the serialised {"metadata":..., "catalog":...} snapshot
// produced by `donat dump-core-config`. The wasm core loads it at startup via
// core_init, which is also where the declarative metadata is compiled — a
// command that does not compile fails the boot rather than the first request.
// Regenerate after a schema or metadata change:
//
//	donat --database-url <url> dump-core-config --metadata-dir metadata --out core-config.json
//
//go:embed core-config.json
var coreConfig []byte

func main() {
	cfg := LoadConfig()
	ctx := context.Background()

	// The engine never opens connections itself; you supply the pool.
	pool, err := pgxpool.New(ctx, cfg.DatabaseURL)
	if err != nil {
		log.Fatalf("pgxpool.New: %v", err)
	}
	defer pool.Close()

	reg := donat.NewRegistry()
	RegisterHandlers(reg)

	eng, err := donat.New(ctx, donat.Config{
		Backend:  donat.Postgres(pool),
		Metadata: coreConfig,
		Registry: reg,
		PoolSize: cfg.PoolSize,
	})
	if err != nil {
		log.Fatalf("donat.New: %v", err)
	}

	mux := NewMux(eng)
	log.Printf("lending-golang listening on %s", cfg.Addr)
	log.Printf("  GraphQL:  POST %s/v1/graphql  (header: X-Donat-Role: member)", cfg.Addr)
	log.Printf("  Healthz:  GET  %s/healthz", cfg.Addr)
	log.Printf("  Handlers: %v (fire in-process, no webhook)", reg.Names())
	log.Fatal(http.ListenAndServe(cfg.Addr, mux))
}
