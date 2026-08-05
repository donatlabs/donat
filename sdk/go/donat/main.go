package donat

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"
)

// Main runs a Donat server whose custom logic is the functions passed here.
//
// It is the whole program for the common case:
//
//	func main() {
//	    donat.Main(donat.WithFunction("render_invoice_pdf", renderInvoicePDF))
//	}
//
// Configuration comes from the environment, so the same binary is deployed the
// way the standalone engine is:
//
//	DONAT_DATABASE_URL    required   Postgres connection string
//	DONAT_CORE_CONFIG     required   path to the `donat dump-core-config` snapshot
//	DONAT_PORT            8080       listen port
//
// Main exits the process on failure, because that is what a `main` wants. A
// program that needs to own its lifecycle — its own mux, its own pool, its own
// shutdown — builds the same Config with New instead and keeps every
// registration it already wrote.
func Main(opts ...Option) {
	if err := Run(context.Background(), opts...); err != nil {
		fmt.Fprintln(os.Stderr, "donat:", err)
		os.Exit(1)
	}
}

// Run is Main without the exit: it serves until ctx is cancelled or the process
// is signalled, then shuts down and returns. Tests use this.
func Run(ctx context.Context, opts ...Option) error {
	cfg := Config{}
	for _, opt := range opts {
		opt(&cfg)
	}

	if cfg.Metadata == nil {
		path := env("DONAT_CORE_CONFIG", "")
		if path == "" {
			return errors.New(
				"DONAT_CORE_CONFIG is required: the snapshot written by " +
					"`donat dump-core-config`, or pass WithMetadata")
		}
		snapshot, err := os.ReadFile(path)
		if err != nil {
			return fmt.Errorf("reading %s: %w", path, err)
		}
		cfg.Metadata = snapshot
	}

	if cfg.Backend == nil {
		url := env("DONAT_DATABASE_URL", "")
		if url == "" {
			return errors.New("DONAT_DATABASE_URL is required, or pass WithBackend")
		}
		backend, closeBackend, err := postgresFromURL(ctx, url)
		if err != nil {
			return err
		}
		defer closeBackend()
		cfg.Backend = backend
	}

	// New refuses a snapshot whose in-process actions have no function, so a
	// missing registration fails here rather than on the request that needs it.
	engine, err := New(ctx, cfg)
	if err != nil {
		return err
	}

	mux := http.NewServeMux()
	mux.Handle("/v1/graphql", engine.Handler())
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusOK)
	})

	addr := ":" + env("DONAT_PORT", "8080")
	server := &http.Server{Addr: addr, Handler: mux}

	ctx, stop := signal.NotifyContext(ctx, os.Interrupt, syscall.SIGTERM)
	defer stop()

	errc := make(chan error, 1)
	go func() {
		fmt.Fprintf(os.Stderr, "donat: listening on %s\n", addr)
		if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			errc <- err
			return
		}
		errc <- nil
	}()

	select {
	case err := <-errc:
		return err
	case <-ctx.Done():
		// In-flight requests finish; a handler that outlives the grace period
		// is abandoned rather than allowed to hold the shutdown open.
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		return server.Shutdown(shutdownCtx)
	}
}

// WithBackend supplies the database backend, instead of Main opening one from
// DONAT_DATABASE_URL.
func WithBackend(b Backend) Option {
	return func(c *Config) { c.Backend = b }
}

// WithMetadata supplies the core snapshot bytes, instead of Main reading
// DONAT_CORE_CONFIG.
func WithMetadata(snapshot []byte) Option {
	return func(c *Config) { c.Metadata = snapshot }
}

// WithRegistry supplies event-trigger handlers, which fire after a commit.
func WithRegistry(r *Registry) Option {
	return func(c *Config) { c.Registry = r }
}

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}
