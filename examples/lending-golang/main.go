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
	"fmt"
	"log"
	"net/http"
	"os"

	"github.com/donatlabs/donat/sdk/go/donat"
	"github.com/jackc/pgx/v5/pgxpool"
)

func main() {
	ctx := context.Background()
	databaseURL := env("DONAT_DATABASE_URL", "postgresql://postgres:postgres@127.0.0.1:15432/lending")
	addr := ":" + env("DONAT_PORT", "8080")

	coreConfig, err := loadCoreConfig()
	if err != nil {
		log.Fatalf("%v", err)
	}

	// The engine opens no connections of its own; the application supplies the
	// pool, which is what makes ExecuteTx possible (audit.go).
	pool, err := pgxpool.New(ctx, databaseURL)
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
	})
	if err != nil {
		log.Fatalf("donat.New: %v", err)
	}

	// The engine's handler is an ordinary http.Handler, mounted beside the
	// application's own routes in the application's own mux.
	mux := http.NewServeMux()
	mux.Handle("/v1/graphql", eng.Handler())
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprintln(w, `{"status":"ok"}`)
	})

	log.Printf("lending-golang on %s; handlers: %v", addr, reg.Names())
	log.Fatal(http.ListenAndServe(addr, mux))
}

// loadCoreConfig reads the `donat dump-core-config` snapshot: the metadata
// directory compiled against the live catalog. It is a build output rather
// than source — producing it needs a migrated database — so it is generated at
// deploy time and read here instead of embedded.
func loadCoreConfig() ([]byte, error) {
	path := env("DONAT_CORE_CONFIG", "core-config.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading the core config %s: %w", path, err)
	}
	return raw, nil
}

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}
