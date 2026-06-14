package donat

import (
	"context"
	"encoding/json"
	"fmt"
)

// runQuery executes a query plan (exactly one statement) against the pgx pool
// and returns the Donat {"data": <data>} envelope.
//
// Mirrors crates/server/src/state.rs:execute_query_json (Postgres branch):
// query_one → try_get::<Json>(0) → wrap as {"data": <json>}.
//
// On a Postgres error the plan's error_map is used to produce the mapped error
// body, which is returned with a nil error (the error is already serialised as
// a GraphQL body — the caller emits it directly, matching the engine which
// always returns HTTP 200 with a GraphQL body).
//
// Non-DB infrastructure errors (e.g. pool exhausted) are returned as a Go
// error; the caller should map them to a GraphQL body.
func (e *Engine) runQuery(ctx context.Context, plan Plan) ([]byte, error) {
	if len(plan.Statements) == 0 {
		return nil, fmt.Errorf("runQuery: plan has no statements")
	}
	stmt := plan.Statements[0]

	var data json.RawMessage
	err := e.pool.QueryRow(ctx, stmt.SQL).Scan(&data)
	if err != nil {
		// Check if it is a Postgres-level error (SQLSTATE) — map to GraphQL body.
		// pgconn.PgError implements errors.As; pool errors wrap it.
		mapped := mapPGError(err, plan.ErrorMap)
		return mapped, nil
	}

	// Wrap the JSON data as {"data": <data>}.
	// json.Marshal(map[string]json.RawMessage) never double-encodes; data is
	// already JSON bytes from Postgres.
	envelope, err := json.Marshal(map[string]json.RawMessage{"data": data})
	if err != nil {
		return nil, fmt.Errorf("runQuery: marshal envelope: %w", err)
	}
	return envelope, nil
}

// executeQuery is an internal helper for tests and the handler: compile the
// plan for the given query+sessionVars and execute it.
// It handles PlanErrorK by returning the error body directly (nil Go error).
func (e *Engine) executeQuery(ctx context.Context, query string, vars map[string]json.RawMessage, sessionVars map[string]string) ([]byte, error) {
	plan, err := e.compilePlan(ctx, compileInput{
		Query:       query,
		Variables:   vars,
		SessionVars: sessionVars,
	})
	if err != nil {
		return nil, fmt.Errorf("executeQuery: compile: %w", err)
	}
	if plan.Kind == PlanErrorK && plan.Err != nil {
		return errorBody(plan.Err.Code, plan.Err.Path, plan.Err.Message), nil
	}
	return e.runQuery(ctx, plan)
}
