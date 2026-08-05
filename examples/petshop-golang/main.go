// Command petshop-golang is the smallest thing that serves a Donat API from a
// Go binary.
//
// There is no wiring to read, because there is none to write: the behaviour is
// in metadata/, and this file only says which snapshot to serve. Everything a
// program normally spells out — the pool, the mux, the listener, the graceful
// shutdown — is what donat.Main does.
//
//	metadata/      tables, permissions, relationships
//	migrations/    the schema, applied out-of-band (the engine never runs DDL)
//	main.go        this
//
// Deploy-time, exactly as the standalone engine is deployed:
//
//	donat --database-url <url> migrate --migrations-dir <repo>/migrations
//	donat --database-url <url> migrate --migrations-dir migrations
//	donat --database-url <url> dump-core-config --metadata-dir metadata
//	DONAT_DATABASE_URL=<url> go run .
//
// docker-compose.yml wires the same flow: postgres → two one-shot migrates →
// app.
//
// What to add next, and where it goes:
//
//   - logic no declaration can express (rendering a file, calling a library):
//     donat.WithFunction, against an action declared in the metadata
//   - work that runs after a write commits: donat.WithRegistry
//   - your own routes, middleware or auth: build the engine with donat.New and
//     mount eng.Handler() in your own mux
//
// [`examples/lending-golang`](../lending-golang) is the worked version of all
// three.
package main

import (
	_ "embed"

	"github.com/donatlabs/donat/sdk/go/donat"
)

// coreConfig is the {"metadata":…, "catalog":…} snapshot written by
// `donat dump-core-config`. The wasm core compiles it at startup, so metadata
// that does not compile fails the boot rather than the first request that
// touches it. Regenerate after any schema or metadata change — and check it in
// CI, because a stale snapshot is still a valid one:
//
//	donat --database-url <url> dump-core-config --metadata-dir metadata --check
//
//go:embed core-config.json
var coreConfig []byte

func main() {
	donat.Main(donat.WithMetadata(coreConfig))
}
