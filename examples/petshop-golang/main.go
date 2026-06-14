// Command petshop-golang is a boilerplate for embedding the Donat engine
// IN-PROCESS in a Go application via the Go SDK.
//
// Architecture (in-memory, no webhook):
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
//	│                    │  core.wasm   │ ← Rust engine       │
//	│                    │  (embedded)  │   compiled to wasm  │
//	│                    └──────┬───────┘                     │
//	│                           │ SQL                         │
//	│                       pgxpool ──► Postgres              │
//	│                           │                             │
//	│   post-commit hooks, in-process   (handlers.go)         │
//	│   donat.Registry: on_order_placed, on_pet_status        │
//	└─────────────────────────────────────────────────────────┘
//
// Use it as a template. The pieces you edit are split by concern:
//
//	config.go    — environment configuration
//	handlers.go  — your event-trigger handlers (the business logic)
//	server.go    — your HTTP routes, mounted next to the engine
//	main.go      — wiring (this file)
//
// Schema is applied OUT-OF-BAND with `donat migrate` (the project's deploy
// model — the engine never runs DDL):
//
//	donat migrate --migrations-dir migrations --database-url <url>   # once
//	go run .                                                          # serve
//
// docker-compose.yml wires the same flow: postgres → one-shot migrate → app.
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
// produced by `donat dump-core-config`. The wasm engine loads it at startup via
// core_init — no Rust binary is needed at runtime. Regenerate after a schema
// change:
//
//	donat dump-core-config --metadata-dir metadata --database-url <url> --out core-config.json
//
//go:embed core-config.json
var coreConfig []byte

func main() {
	cfg := LoadConfig()
	ctx := context.Background()

	// The engine never opens connections itself; you supply the pool
	// (composability). The schema must already be migrated (`donat migrate`).
	pool, err := pgxpool.New(ctx, cfg.DatabaseURL)
	if err != nil {
		log.Fatalf("pgxpool.New: %v", err)
	}
	defer pool.Close()

	// Register your in-process event-trigger handlers (handlers.go).
	reg := donat.NewRegistry()
	RegisterHandlers(reg)

	// Build the embedded engine over your pool + the serialised config.
	eng, err := donat.New(ctx, donat.Config{
		Pool:     pool,
		Metadata: coreConfig,
		Registry: reg,
		PoolSize: cfg.PoolSize,
	})
	if err != nil {
		log.Fatalf("donat.New: %v", err)
	}

	// Build the HTTP router (server.go) and serve.
	mux := NewMux(eng)
	log.Printf("petshop-golang listening on %s", cfg.Addr)
	log.Printf("  GraphQL:  POST %s/v1/graphql  (header: X-Donat-Role: staff)", cfg.Addr)
	log.Printf("  Healthz:  GET  %s/healthz", cfg.Addr)
	log.Printf("  Handlers: %v (fire in-process, no webhook)", reg.Names())
	log.Fatal(http.ListenAndServe(cfg.Addr, mux))
}
