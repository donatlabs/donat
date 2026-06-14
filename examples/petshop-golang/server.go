package main

import (
	"fmt"
	"net/http"

	"github.com/donatlabs/donat/sdk/go/donat"
)

// NewMux builds the HTTP router. The engine's GraphQL handler is mounted next
// to your own routes in the same mux — you own the server, its middleware and
// its auth. Add your routes here.
func NewMux(eng *donat.Engine) *http.ServeMux {
	mux := http.NewServeMux()

	// GraphQL — served by the embedded Rust core (wasm via wazero). Resolves
	// the per-role session from X-Donat-* headers; a request with no
	// X-Donat-Role is denied (this engine has no admin role).
	mux.Handle("/v1/graphql", eng.Handler())

	// Your own routes live alongside the engine route (composability).
	mux.HandleFunc("/healthz", healthz)

	return mux
}

func healthz(w http.ResponseWriter, _ *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	fmt.Fprintln(w, `{"status":"ok"}`)
}
