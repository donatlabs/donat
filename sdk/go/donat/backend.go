package donat

import (
	"context"
	"encoding/json"
)

// Backend is everything the engine needs from a database. Plan compilation,
// permissions, session handling and hook firing are backend-agnostic;
// implement Backend once per database.
// UploadRow is what finishing an upload needs from its row: the core signs
// against the address the bytes were actually written to.
type UploadRow struct {
	Backend    string
	StagingKey string
}

type Backend interface {
	// Dialect is the SQL flavour the wasm core renders ("postgres"|"sqlite"|"mysql").
	Dialect() string
	// RunQuery executes a one-statement read plan and returns the raw JSON `data`
	// value (assembled in-DB for Postgres).
	RunQuery(ctx context.Context, plan Plan) (json.RawMessage, error)
	// RunMutation executes a write plan atomically (all statements in one txn)
	// and returns the per-root alias→value map. Returns a driver error for MapError.
	RunMutation(ctx context.Context, plan Plan) (map[string]json.RawMessage, error)
	// MapError turns a native driver error into a Donat GraphQL error body using
	// the plan's error_map directives.
	MapError(err error, errorMap map[string]string) []byte

	// ReadUpload returns the pending upload's backend and staging key.
	ReadUpload(ctx context.Context, id string) (UploadRow, error)
	// FinalizeUpload records the size the store reported and the address the
	// bytes now live at. It must not match a row that has already been
	// claimed — that would let a claim's certified bytes be replaced.
	FinalizeUpload(ctx context.Context, id, objectKey string, size int64) error
}

// txRunner is an OPTIONAL capability: run a mutation/query inside a caller-owned
// transaction (composability). Backends that support it are reached via
// Engine.ExecuteTx. tx is the driver's transaction handle (e.g. pgx.Tx).
type txRunner interface {
	runMutationTx(ctx context.Context, tx any, plan Plan) (map[string]json.RawMessage, error)
	runQueryTx(ctx context.Context, tx any, plan Plan) (json.RawMessage, error)
}
