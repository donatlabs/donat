package donat

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// postgresBackend is the Backend implementation backed by a pgx connection pool.
// It handles SQL execution, transactions and error mapping for Postgres.
// Hook firing is intentionally NOT here — it belongs to the Engine which owns
// the Registry and the event envelope construction.
type postgresBackend struct {
	pool *pgxpool.Pool
}

// Postgres returns a Backend backed by the supplied pgxpool.Pool.
// The pool is caller-owned and must outlive the Engine.
func Postgres(pool *pgxpool.Pool) Backend {
	return &postgresBackend{pool: pool}
}

// Dialect returns "postgres" — the SQL flavour rendered by the wasm core for
// this backend. The Engine passes this to compileInput so the core emits the
// right SQL dialect.
func (b *postgresBackend) Dialect() string { return "postgres" }

// RunQuery executes a one-statement read plan and returns the raw JSON data
// value assembled by Postgres (via json_build_object / json_agg).
// Mirrors the Postgres branch of crates/server/src/state.rs:execute_query_json.
func (b *postgresBackend) RunQuery(ctx context.Context, plan Plan) (json.RawMessage, error) {
	if len(plan.Statements) == 0 {
		return nil, fmt.Errorf("RunQuery: plan has no statements")
	}
	var data json.RawMessage
	err := b.pool.QueryRow(ctx, plan.Statements[0].SQL).Scan(&data)
	if err != nil {
		return nil, err
	}
	return data, nil
}

// RunMutation executes a write plan atomically inside a self-owned transaction.
// It opens a transaction, runs all statements in order, and commits.
// On any error the transaction is rolled back and the driver error is returned
// (unwrapped) so the Engine can pass it to MapError.
//
// Hooks are NOT fired here — the Engine fires them after RunMutation returns.
// Mirrors crates/server/src/gql.rs:567-600.
func (b *postgresBackend) RunMutation(ctx context.Context, plan Plan) (map[string]json.RawMessage, error) {
	tx, err := b.pool.Begin(ctx)
	if err != nil {
		return nil, err
	}
	data, err := runStmtsInTx(ctx, tx, plan)
	if err != nil {
		_ = tx.Rollback(ctx)
		return nil, err
	}
	if err := tx.Commit(ctx); err != nil {
		return nil, err
	}
	return data, nil
}

// MapError maps a Postgres driver error to the Donat GraphQL error body JSON.
// It delegates to the shared mapPGError helper (errors.go).
func (b *postgresBackend) MapError(err error, errorMap map[string]string) []byte {
	return mapPGError(err, errorMap)
}

// runMutationTx implements txRunner: execute all mutation statements inside the
// caller-provided pgx.Tx. Neither commit nor rollback is issued — that is the
// caller's responsibility. Returns the per-alias data map or a driver error.
func (b *postgresBackend) runMutationTx(ctx context.Context, tx any, plan Plan) (map[string]json.RawMessage, error) {
	pgxTx := tx.(pgx.Tx)
	return runStmtsInTx(ctx, pgxTx, plan)
}

// runQueryTx implements txRunner: execute the single query statement inside the
// caller-provided pgx.Tx and return the raw JSON data value.
func (b *postgresBackend) runQueryTx(ctx context.Context, tx any, plan Plan) (json.RawMessage, error) {
	pgxTx := tx.(pgx.Tx)
	if len(plan.Statements) == 0 {
		return nil, fmt.Errorf("runQueryTx: plan has no statements")
	}
	var data json.RawMessage
	err := pgxTx.QueryRow(ctx, plan.Statements[0].SQL).Scan(&data)
	if err != nil {
		return nil, err
	}
	return data, nil
}

// runStmtsInTx executes all plan statements sequentially in tx, collecting the
// per-alias JSON results. It does NOT commit or roll back. Returns the data map
// or the first driver error encountered.
func runStmtsInTx(ctx context.Context, tx pgx.Tx, plan Plan) (map[string]json.RawMessage, error) {
	data := make(map[string]json.RawMessage, len(plan.Statements))
	for _, stmt := range plan.Statements {
		var part json.RawMessage
		if err := tx.QueryRow(ctx, stmt.SQL).Scan(&part); err != nil {
			return nil, err
		}
		data[stmt.Alias] = part
	}
	return data, nil
}
