// Command petshop-golang is a standalone, self-contained petshop demo that
// embeds the Donat engine IN-PROCESS via the Go SDK.
//
// Architecture (in-memory, no webhook):
//
//	┌─────────────────────────────────────────────────────────┐
//	│  Go process (single binary, CGO_ENABLED=0)              │
//	│                                                         │
//	│  net/http mux                                           │
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
//	│              post-commit hooks (in-process)             │
//	│              ┌─────────────────────────┐               │
//	│              │  donat.Registry          │               │
//	│              │  on_order_placed handler │               │
//	│              │  on_pet_status handler   │               │
//	│              └─────────────────────────┘               │
//	└─────────────────────────────────────────────────────────┘
//
// The Rust engine runs as a wasm module inside this Go process (wazero,
// no cgo). GraphQL is served from /v1/graphql. Event triggers fire the
// registered Go handlers IN-PROCESS after the committing transaction —
// there is no HTTP webhook, no network round-trip, no separate handler
// service. The webhook: fields that remain in metadata/**.yaml are
// placeholders kept for schema compatibility; the in-memory registry
// intercepts events before any HTTP delivery is attempted.
package main

import (
	"context"
	"embed"
	"fmt"
	"io/fs"
	"log"
	"net/http"
	"os"
	"sort"
	"strings"

	"github.com/donatlabs/donat/examples/petshop-golang/gen"
	"github.com/donatlabs/donat/sdk/go/donat"
	"github.com/jackc/pgx/v5/pgxpool"
)

// coreConfig is the serialised {"metadata":..., "catalog":...} snapshot
// produced by `donat dump-core-config`. The wasm engine loads it at startup
// via core_init — no Rust binary is needed at runtime.
//
//go:embed core-config.json
var coreConfig []byte

// migrationFiles holds the DDL migrations applied at startup.
//
//go:embed migrations/*.sql
var migrationFiles embed.FS

func main() {
	ctx := context.Background()

	dbURL := os.Getenv("DATABASE_URL")
	if dbURL == "" {
		dbURL = "postgresql://postgres:postgres@127.0.0.1:15432/petshop_golang"
	}

	// Open the pgx connection pool. The engine never opens connections itself;
	// the caller supplies the pool (composability requirement from the SDK).
	pool, err := pgxpool.New(ctx, dbURL)
	if err != nil {
		log.Fatalf("pgxpool.New: %v", err)
	}
	defer pool.Close()

	// Apply DDL migrations idempotently at startup.
	// A real app would use a migration library (golang-migrate, atlas, etc.).
	// For this example a simple sentinel check + ordered execution is sufficient:
	// if the `category` table already exists, skip all migrations.
	if err := applyMigrations(ctx, pool); err != nil {
		log.Fatalf("migrations: %v", err)
	}

	// Seed demo rows so the GraphQL demo returns data immediately.
	// All inserts are guarded with ON CONFLICT DO NOTHING — idempotent.
	if err := seedData(ctx, pool); err != nil {
		log.Fatalf("seed: %v", err)
	}

	// Build the event-trigger registry. Handler names must match the
	// event_triggers[].name values in metadata/*.yaml exactly.
	reg := donat.NewRegistry()

	// on_order_placed: fires on INSERT and on UPDATE of orders.status.
	//
	// SDK v1 note: the in-process hook envelope carries the mutation result
	// (e.g. {"affected_rows":1,"returning":[...]}) as ev.New, not the raw row.
	// This is a known v1 limitation — full old/new row capture is a planned
	// follow-up. The hook IS fired in-process (log line below proves it);
	// the data fields are zero-valued because they don't map to gen.Orders.
	// The original webhook model carried proper row data from the PG trigger.
	donat.On(reg, "on_order_placed", func(_ context.Context, ev donat.Event[gen.Orders]) error {
		switch ev.Op {
		case donat.OpInsert:
			// ev.New.Id == 0 in SDK v1 (envelope carries mutation result, not the row).
			// The hook firing in-process is confirmed by this log line.
			log.Printf("[event] on_order_placed fired: op=INSERT trigger=%s table=%s",
				ev.Trigger.Name, ev.Table.Name)
			if ev.New != nil && ev.New.Id != 0 {
				log.Printf("[event] order #%d placed by customer %s (status=%s)",
					ev.New.Id, ev.New.CustomerId, ev.New.Status)
			}
		case donat.OpUpdate:
			log.Printf("[event] on_order_placed fired: op=UPDATE trigger=%s table=%s",
				ev.Trigger.Name, ev.Table.Name)
			if ev.Old != nil && ev.New != nil && ev.Old.Status != ev.New.Status {
				log.Printf("[event] order #%d: %s -> %s", ev.New.Id, ev.Old.Status, ev.New.Status)
			}
		}
		return nil
	})

	// on_pet_status: fires on UPDATE of pet.status.
	// Same SDK v1 note as above — ev.New carries mutation result, not the row.
	donat.On(reg, "on_pet_status", func(_ context.Context, ev donat.Event[gen.Pet]) error {
		log.Printf("[event] on_pet_status fired: op=%s trigger=%s table=%s",
			ev.Op, ev.Trigger.Name, ev.Table.Name)
		if ev.New != nil && ev.New.Status == "sold" {
			log.Printf("[event] pet %q (#%d) sold for %s", ev.New.Name, ev.New.Id, ev.New.Price.String())
		}
		return nil
	})

	// Construct the embedded engine. core.wasm is embedded inside the SDK
	// package (sdk/go/donat/wasm/core.wasm via //go:embed in wasmcore.go).
	// coreConfig supplies the pre-serialised metadata+catalog snapshot so the
	// engine does not need to introspect the database at startup.
	eng, err := donat.New(ctx, donat.Config{
		Pool:     pool,
		Metadata: coreConfig,
		Registry: reg,
		PoolSize: 4,
	})
	if err != nil {
		log.Fatalf("donat.New: %v", err)
	}

	// Build the HTTP mux. The engine's GraphQL handler is mounted at the
	// standard path; your own routes live alongside it in the same mux.
	// This demonstrates the composability requirement: you own the server.
	mux := http.NewServeMux()

	// GraphQL endpoint — served by the embedded Rust/wasm core via wazero.
	mux.Handle("/v1/graphql", eng.Handler())

	// Your own route — lives next to the engine route in the same mux.
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		fmt.Fprintln(w, `{"status":"ok"}`)
	})

	log.Printf("petshop-golang listening on :8080")
	log.Printf("  GraphQL:  POST http://localhost:8080/v1/graphql  (header: X-Donat-Role: staff)")
	log.Printf("  Healthz:  GET  http://localhost:8080/healthz")
	log.Printf("  Handlers: %v (fire in-process, no webhook)", reg.Names())
	log.Fatal(http.ListenAndServe(":8080", mux))
}

// applyMigrations runs each embedded migration SQL file in sort order.
//
// Idempotency strategy (suitable for this example — a real app would use a
// migration library with version tracking):
//
//   - V0__donat_schema.sql is ALWAYS applied: it uses CREATE … IF NOT EXISTS
//     and CREATE OR REPLACE so running it twice is safe. It creates the `donat`
//     schema + helper functions required by the wasm core.
//   - V1__–V5__*.sql are applied only when the sentinel table `public.category`
//     is absent (fresh database). They use plain CREATE TABLE / INSERT which
//     would fail on a second run, so we skip them if the schema already exists.
func applyMigrations(ctx context.Context, pool *pgxpool.Pool) error {
	// Collect and sort the embedded migration files.
	entries, err := fs.ReadDir(migrationFiles, "migrations")
	if err != nil {
		return fmt.Errorf("readdir migrations: %w", err)
	}
	sort.Slice(entries, func(i, j int) bool {
		return entries[i].Name() < entries[j].Name()
	})

	// Sentinel: does the petshop schema already exist?
	var petshopExists bool
	if err := pool.QueryRow(ctx,
		"SELECT to_regclass('public.category') IS NOT NULL",
	).Scan(&petshopExists); err != nil {
		return fmt.Errorf("sentinel check: %w", err)
	}

	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".sql") {
			continue
		}
		name := e.Name()

		// V0 is idempotent (IF NOT EXISTS / CREATE OR REPLACE) — always apply.
		// V1–V5 are skipped when the petshop schema is already present.
		if name != "V0__donat_schema.sql" && petshopExists {
			continue
		}

		sql, err := migrationFiles.ReadFile("migrations/" + name)
		if err != nil {
			return fmt.Errorf("read %s: %w", name, err)
		}
		if _, err := pool.Exec(ctx, string(sql)); err != nil {
			return fmt.Errorf("apply %s: %w", name, err)
		}
		log.Printf("migrations: applied %s", name)
	}
	if petshopExists {
		log.Println("migrations: petshop schema already present, skipped V1–V5")
	}
	return nil
}

// seedData inserts demo rows so the GraphQL demo returns data immediately.
// All inserts use ON CONFLICT DO NOTHING — safe to call repeatedly.
// The migration files already include seed data, so this function is a no-op
// if the migrations were freshly applied; it becomes meaningful only when the
// database was pre-populated by other means and the seed rows might be absent.
func seedData(ctx context.Context, pool *pgxpool.Pool) error {
	// Check if we have at least one pet already (the migrations seed them).
	var count int
	if err := pool.QueryRow(ctx, "SELECT COUNT(*) FROM pet").Scan(&count); err != nil {
		return fmt.Errorf("pet count: %w", err)
	}
	if count > 0 {
		log.Printf("seed: %d pet(s) already present, skipping", count)
		return nil
	}

	// Seed a minimal dataset so the demo works even with an empty database.
	_, err := pool.Exec(ctx, `
		INSERT INTO category (name) VALUES ('Dogs'), ('Cats') ON CONFLICT DO NOTHING;
		INSERT INTO pet (name, category_id, price, status, description)
		SELECT 'Rex', id, 350.00, 'available', 'Friendly Labrador'
		FROM category WHERE name='Dogs'
		ON CONFLICT DO NOTHING;
		INSERT INTO customer (id, name, email)
		VALUES ('1','Alice Buyer','alice@example.com') ON CONFLICT DO NOTHING;
	`)
	if err != nil {
		return fmt.Errorf("seed insert: %w", err)
	}
	log.Println("seed: demo rows inserted")
	return nil
}
