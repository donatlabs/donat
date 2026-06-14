package donat

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
)

// sqlBackend is a Backend implemented over the standard database/sql interface.
// It covers any driver whose dialect is handled by the wasm core: "sqlite",
// "mysql" or "postgres" (via a database/sql driver rather than pgx).
//
// SQLite query strategy: the wasm core renders a single SELECT statement that
// returns ONE row / ONE column of TEXT json (a json_object / json_group_array
// expression). RunQuery scans that single text column and returns it verbatim
// as json.RawMessage. This mirrors crates/server/src/state.rs:178-198.
//
// SQLite mutation strategy: the Rust executor (state.rs:execute_sqlite_mutations)
// runs a separate sqlite_mutation_plan (not PlanV1 statements), iterating DML
// rows host-side and folding affected_rows/returning. PlanV1 mutation statements
// are Postgres/MySQL-style and do not map to SQLite's execution model. SQLite
// mutations are therefore deferred; RunMutation returns a clear error when the
// dialect is "sqlite". See Spec 004 follow-ups.
type sqlBackend struct {
	db      *sql.DB
	dialect string // "sqlite" | "mysql" | "postgres"
}

// Compile-time check: sqlBackend satisfies Backend.
var _ Backend = (*sqlBackend)(nil)

// SQL returns a Backend over any database/sql driver. dialect selects the SQL
// the wasm core renders ("sqlite", "mysql", "postgres") and the
// result-assembly strategy used by RunQuery/RunMutation.
//
// The db is caller-owned and must outlive the Engine.
func SQL(db *sql.DB, dialect string) Backend {
	return &sqlBackend{db: db, dialect: dialect}
}

// Dialect returns the SQL flavour this backend requests from the wasm core.
func (b *sqlBackend) Dialect() string { return b.dialect }

// RunQuery executes the single read statement from plan and returns the raw
// JSON data value assembled by the database.
//
// For SQLite the wasm core emits one SELECT that returns one row / one column
// of TEXT json (json_object / json_group_array). We scan that column as a
// string and return it as json.RawMessage — mirroring state.rs:178-198.
//
// For Postgres-over-database/sql the same strategy applies: the generated SQL
// returns a single text/json column that is the fully-assembled data value.
func (b *sqlBackend) RunQuery(ctx context.Context, plan Plan) (json.RawMessage, error) {
	if len(plan.Statements) == 0 {
		return nil, fmt.Errorf("sqlBackend.RunQuery: plan has no statements")
	}
	stmt := plan.Statements[0]
	var text string
	if err := b.db.QueryRowContext(ctx, stmt.SQL).Scan(&text); err != nil {
		if errors.Is(err, sql.ErrNoRows) {
			return json.RawMessage("null"), nil
		}
		return nil, fmt.Errorf("sqlBackend.RunQuery: %w", err)
	}
	return json.RawMessage(text), nil
}

// RunMutation executes a write plan atomically.
//
// For SQLite this is not yet implemented. The Rust host (state.rs) runs
// sqlite_mutation_plan — a richer model that the wasm core does NOT expose
// through PlanV1 statements. PlanV1 mutation statements are Postgres/MySQL-
// style CTEs and do not execute correctly on SQLite. Rather than silently
// misfire, we return a clear error directing to the documented follow-up.
//
// For other dialects a similar not-yet-implemented error is returned; MySQL
// support (companion-SELECT mutation strategy) is a planned follow-up.
func (b *sqlBackend) RunMutation(_ context.Context, _ Plan) (map[string]json.RawMessage, error) {
	switch b.dialect {
	case "sqlite":
		return nil, fmt.Errorf(
			"donat: SQLite mutations are not yet supported by the embedded SDK (queries work); " +
				"see Spec 004 follow-ups — the Rust host uses sqlite_mutation_plan " +
				"which is not exposed through PlanV1 statements",
		)
	default:
		return nil, fmt.Errorf(
			"donat: mutations via the generic database/sql backend are not yet supported "+
				"for dialect %q; use donat.Postgres(pool) for Postgres mutations",
			b.dialect,
		)
	}
}

// MapError maps a driver error to a Donat GraphQL error body.
//
// This implementation covers the common case: any error from the database is
// returned as a "data-exception" body (or the errorMap["default"] directive if
// present). Rich error-code mapping for SQLite/MySQL is a follow-up — queries
// rarely hit the permission-check path (which uses Postgres SQLSTATE 23514).
func (b *sqlBackend) MapError(err error, errorMap map[string]string) []byte {
	if err == nil {
		return errorBody("unexpected", "$", "nil error passed to MapError")
	}
	// Check for an error_map "default" directive to use as the Donat error code.
	if errorMap != nil {
		if directive, ok := errorMap["default"]; ok && directive != "" {
			return errorBody(directive, "$", err.Error())
		}
	}
	return errorBody("data-exception", "$", err.Error())
}
