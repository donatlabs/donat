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

// replayReporter is an OPTIONAL capability: run a mutation and say which of
// its command roots were replayed from the idempotency journal rather than
// executed.
//
// It matters for post-commit hooks. On the native engine an event is a
// Postgres trigger, and a replay re-projects the stored result without
// re-running any DML — so no trigger fires. An embedded host fires from the
// plan, which cannot know, so without this it would deliver an event for a
// write that did not happen this time. A backend that does not implement it
// keeps the old behaviour.
type replayReporter interface {
	runMutationReportingReplays(
		ctx context.Context,
		plan Plan,
	) (map[string]json.RawMessage, map[string]bool, error)
}

// txRunner is an OPTIONAL capability: run a mutation/query inside a caller-owned
// transaction (composability). Backends that support it are reached via
// Engine.ExecuteTx. tx is the driver's transaction handle (e.g. pgx.Tx).
type txRunner interface {
	runMutationTx(ctx context.Context, tx any, plan Plan) (map[string]json.RawMessage, error)
	runQueryTx(ctx context.Context, tx any, plan Plan) (json.RawMessage, error)
}
