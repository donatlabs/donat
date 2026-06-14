package main

import "os"

// Config holds the app's runtime settings. Everything is read from the
// environment with sane defaults so the binary runs with zero flags.
type Config struct {
	// DatabaseURL is the Postgres DSN. The schema must already be migrated
	// (run `donat migrate --migrations-dir migrations` first; docker-compose
	// does this in a one-shot `migrate` service).
	DatabaseURL string
	// Addr is the HTTP listen address.
	Addr string
	// PoolSize is the number of wasm engine instances kept warm. Each instance
	// is single-threaded; size it to your expected concurrency of plan
	// compiles (the hot path is cached and never enters wasm).
	PoolSize int
}

// LoadConfig reads the configuration from the environment.
func LoadConfig() Config {
	return Config{
		DatabaseURL: getenv("DATABASE_URL", "postgresql://postgres:postgres@127.0.0.1:15432/petshop_golang"),
		Addr:        getenv("ADDR", ":8080"),
		PoolSize:    4,
	}
}

func getenv(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
