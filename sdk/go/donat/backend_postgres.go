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

// Compile-time checks that the postgres backend satisfies both the core
// Backend interface and the optional txRunner (ExecuteTx) capability.
var (
	_ Backend  = (*postgresBackend)(nil)
	_ txRunner = (*postgresBackend)(nil)
)

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
	pgxTx, ok := tx.(pgx.Tx)
	if !ok {
		return nil, fmt.Errorf("postgres backend: ExecuteTx requires a pgx.Tx, got %T", tx)
	}
	return runStmtsInTx(ctx, pgxTx, plan)
}

// runQueryTx implements txRunner: execute the single query statement inside the
// caller-provided pgx.Tx and return the raw JSON data value.
func (b *postgresBackend) runQueryTx(ctx context.Context, tx any, plan Plan) (json.RawMessage, error) {
	pgxTx, ok := tx.(pgx.Tx)
	if !ok {
		return nil, fmt.Errorf("postgres backend: ExecuteTx requires a pgx.Tx, got %T", tx)
	}
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
		part, err := scanStatement(ctx, tx, stmt)
		if err != nil {
			return nil, err
		}
		data[stmt.Alias] = part
	}
	return data, nil
}

// scanStatement reads one statement's row in whatever shape the plan declared.
//
// The shape is not guessable from the SQL, and it is not cosmetic: an
// idempotent command returns its durable execution generation beside the
// result, so a host that scanned column 0 would fail with a column-count
// mismatch — an error about the row shape, telling an operator nothing about
// the command that produced it.
func scanStatement(ctx context.Context, tx pgx.Tx, stmt Statement) (json.RawMessage, error) {
	switch stmt.Result {
	case ResultCommandExecution:
		rows, err := tx.Query(ctx, stmt.SQL)
		if err != nil {
			return nil, err
		}
		defer rows.Close()
		if !rows.Next() {
			if err := rows.Err(); err != nil {
				return nil, err
			}
			return nil, fmt.Errorf("command %q returned no execution row", stmt.Alias)
		}
		// By name, not by position: the engine reads `root` the same way, and
		// the column order is the renderer's business, not the host's.
		values, err := rows.Values()
		if err != nil {
			return nil, err
		}
		var root json.RawMessage
		for i, field := range rows.FieldDescriptions() {
			if field.Name != "root" {
				continue
			}
			if values[i] == nil {
				return json.RawMessage("null"), nil
			}
			raw, err := json.Marshal(values[i])
			if err != nil {
				return nil, fmt.Errorf("command %q result: %w", stmt.Alias, err)
			}
			root = raw
		}
		if root == nil {
			return nil, fmt.Errorf("command %q returned no `root` column", stmt.Alias)
		}
		rows.Close()
		return root, rows.Err()
	default:
		var part json.RawMessage
		if err := tx.QueryRow(ctx, stmt.SQL).Scan(&part); err != nil {
			return nil, err
		}
		return part, nil
	}
}

// postgresFromURL opens a pool for Main, which has no pool of its own to be
// given. A program that owns its pool passes WithBackend(donat.Postgres(pool))
// and this is never reached.
func postgresFromURL(ctx context.Context, url string) (Backend, func(), error) {
	pool, err := pgxpool.New(ctx, url)
	if err != nil {
		return nil, nil, fmt.Errorf("connecting to the database: %w", err)
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, nil, fmt.Errorf("connecting to the database: %w", err)
	}
	return Postgres(pool), pool.Close, nil
}

// ReadUpload returns what finishing a pending upload needs from its row.
func (b *postgresBackend) ReadUpload(ctx context.Context, id string) (UploadRow, error) {
	var row UploadRow
	err := b.pool.QueryRow(ctx,
		"SELECT backend, object_key FROM donat.file_uploads WHERE id = $1 AND state = 'pending'",
		id,
	).Scan(&row.Backend, &row.StagingKey)
	if err != nil {
		return UploadRow{}, err
	}
	return row, nil
}

// FinalizeUpload records the size the store reported and the address the bytes
// now live at.
//
// The `state = 'pending'` guard is what stops a finished upload being pointed
// somewhere else: once a claim has certified the bytes, moving the row would
// let them be replaced under a row that already references them.
func (b *postgresBackend) FinalizeUpload(ctx context.Context, id, objectKey string, size int64) error {
	tag, err := b.pool.Exec(ctx,
		"UPDATE donat.file_uploads SET byte_size = $2, object_key = $3 "+
			"WHERE id = $1 AND state = 'pending'",
		id, size, objectKey,
	)
	if err != nil {
		return err
	}
	if tag.RowsAffected() == 0 {
		return fmt.Errorf("the upload was claimed before it was finalized")
	}
	return nil
}
