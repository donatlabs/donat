package donat

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"os"
)

// Main runs a Donat server whose custom logic is the functions passed here.
//
// It is the whole program for the common case:
//
//	func main() {
//	    donat.Main(donat.WithFunction("render_invoice_pdf", renderInvoicePDF))
//	}
//
// It serves `/v1/graphql` and nothing else. A probe endpoint, a metrics route,
// a shutdown sequence — every deployment wants a different set, and a program
// that wants any of them builds the engine with New and mounts
// `eng.Handler()` in its own mux, keeping every registration it already wrote.
//
// Authentication is not optional and not included: the engine takes a
// request's role from its headers, so anything that reaches the handler is
// already trusted. Use WithMiddleware, or do not expose the port.
//
// Configuration comes from the environment, so the same binary is deployed the
// way the standalone engine is:
//
//	DONAT_DATABASE_URL    required   Postgres connection string
//	DONAT_CORE_CONFIG     required   path to the `donat dump-core-config` snapshot
//	DONAT_PORT            8080       listen port
func Main(opts ...Option) {
	if err := Run(context.Background(), opts...); err != nil {
		fmt.Fprintln(os.Stderr, "donat:", err)
		os.Exit(1)
	}
}

// Run is Main without the process exit, so a caller can decide what a failure
// means. It serves until the listener stops.
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

	// Every request declares its own role in X-Donat-Role, so whatever
	// reaches the handler is trusted to have been authenticated already. On a
	// public address that is an open role-impersonation endpoint, and a
	// deployment that needs otherwise puts its own check in front.
	var handler http.Handler = engine.Handler()
	for i := len(cfg.Middleware) - 1; i >= 0; i-- {
		handler = cfg.Middleware[i](handler)
	}
	mux := http.NewServeMux()
	mux.Handle("/v1/graphql", handler)

	addr := ":" + env("DONAT_PORT", "8080")
	fmt.Fprintf(os.Stderr, "donat: listening on %s\n", addr)
	return http.ListenAndServe(addr, mux)
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

// WithSecrets supplies the values the metadata's storage configuration
// references by `value_from_env`. They are passed here rather than written
// into the committed snapshot, which ships inside the binary.
func WithSecrets(secrets map[string]string) Option {
	return func(c *Config) {
		if c.Secrets == nil {
			c.Secrets = make(map[string]string, len(secrets))
		}
		for k, v := range secrets {
			c.Secrets[k] = v
		}
	}
}

// WithMiddleware wraps the GraphQL handler, outermost first.
//
// The engine resolves a request's role from its X-Donat-* headers and does not
// authenticate it — deciding who may reach the handler at all is the
// deployment's job. Without something here, anyone who can reach the port can
// name their own role.
//
//	donat.Main(donat.WithMiddleware(requireJWT))
func WithMiddleware(mw ...func(http.Handler) http.Handler) Option {
	return func(c *Config) { c.Middleware = append(c.Middleware, mw...) }
}

// WithExternalBaseURL sets the absolute prefix for engine-served URLs, e.g.
// "https://api.example.com". Empty means same-origin.
func WithExternalBaseURL(base string) Option {
	return func(c *Config) { c.ExternalBaseURL = base }
}

func env(name, fallback string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return fallback
}
