package donat

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
)

// runQuery executes a query plan (exactly one statement) against the pgx pool
// and returns the Donat {"data": <data>} envelope.
//
// Mirrors crates/server/src/state.rs:execute_query_json (Postgres branch):
// query_one → try_get::<Json>(0) → wrap as {"data": <json>}.
//
// ALL Postgres and infrastructure errors (pool exhausted, network, SQLSTATE)
// are mapped to a GraphQL error body via mapPGError and returned with a nil Go
// error (HTTP-200 convention — the error is already serialised as a GQL body).
// The only case that returns a non-nil Go error is an envelope-marshal failure,
// which would indicate a bug in the host rather than a database or user error.
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

// runMutation executes a mutation plan within a single Postgres transaction.
//
// If tx is nil the function opens its own transaction (owned=true), commits on
// success, and fires post-commit hooks from the plan. When tx is non-nil the
// caller owns the transaction lifecycle; hooks are NOT fired (the caller is
// responsible for post-commit side effects after its own commit).
//
// Mirrors crates/server/src/gql.rs:567-600 (one statement per root, in order,
// all inside one transaction; rollback on the first error).
//
// On a Postgres error the statement's error_map entry is applied and the mapped
// GraphQL body is returned with a nil Go error (HTTP-200 convention). A
// non-nil Go error is returned only for envelope-marshal failures (host bugs).
func (e *Engine) runMutation(ctx context.Context, plan Plan, tx pgx.Tx, sessionVars map[string]string) ([]byte, error) {
	ownTx := tx == nil
	if ownTx {
		var err error
		tx, err = e.pool.Begin(ctx)
		if err != nil {
			// Cannot open a transaction — map to a GraphQL body (nil Go error).
			return mapPGError(err, plan.ErrorMap), nil
		}
	}

	data := make(map[string]json.RawMessage, len(plan.Statements))
	for _, stmt := range plan.Statements {
		var part json.RawMessage
		err := tx.QueryRow(ctx, stmt.SQL).Scan(&part)
		if err != nil {
			if ownTx {
				_ = tx.Rollback(ctx)
			}
			return mapPGError(err, plan.ErrorMap), nil
		}
		data[stmt.Alias] = part
	}

	if ownTx {
		if err := tx.Commit(ctx); err != nil {
			return mapPGError(err, plan.ErrorMap), nil
		}
		// Fire post-commit hooks only for owned transactions (caller-owned
		// transactions must handle post-commit side effects after their own commit).
		e.fireHooks(ctx, plan.Hooks, data, sessionVars)
	}

	// Encode the per-root data map as the "data" value in the GraphQL envelope.
	// json.Marshal on map[string]json.RawMessage produces the inner JSON object;
	// wrapping it in map[string]json.RawMessage{"data": ...} avoids double-encoding.
	dataJSON, err := json.Marshal(data)
	if err != nil {
		return nil, fmt.Errorf("runMutation: marshal data: %w", err)
	}
	envelope, err := json.Marshal(map[string]json.RawMessage{"data": dataJSON})
	if err != nil {
		return nil, fmt.Errorf("runMutation: marshal envelope: %w", err)
	}
	return envelope, nil
}

// fireHooks fires the plan's post-commit hooks against the registry. Called
// only after a successful owned-transaction commit. Hook errors are silently
// dropped (the mutation is already committed); ErrNoHandler is a no-op.
// Only hooks with Phase == "post_commit" are dispatched.
func (e *Engine) fireHooks(ctx context.Context, hooks []Hook, data map[string]json.RawMessage, sessionVars map[string]string) {
	if e.registry == nil || len(hooks) == 0 {
		return
	}
	now := time.Now().UTC()
	// Build session_variables as a JSON object for the envelope.
	sessJSON, _ := json.Marshal(sessionVars)

	for _, h := range hooks {
		if h.Phase != "post_commit" {
			continue
		}

		// Build the event envelope mirroring crates/server/src/events.rs (tick fn).
		// V1: data.new is the aliased statement result if available; data.old is
		// null (INSERT only in v1; full old/new capture is a planned follow-up).
		var newData json.RawMessage = []byte("null")
		if part, ok := data[h.Trigger]; ok {
			newData = part
		} else {
			// Fall back to the first statement's result as the "new" payload.
			for _, v := range data {
				newData = v
				break
			}
		}

		type tableRef struct {
			Schema string `json:"schema"`
			Name   string `json:"name"`
		}
		type trigRef struct {
			Name string `json:"name"`
		}
		type dataField struct {
			Old json.RawMessage `json:"old"`
			New json.RawMessage `json:"new"`
		}
		type eventField struct {
			Op               string          `json:"op"`
			Data             dataField       `json:"data"`
			SessionVariables json.RawMessage `json:"session_variables"`
		}
		type deliveryInfo struct {
			CurrentRetry int `json:"current_retry"`
			MaxRetries   int `json:"max_retries"`
		}
		type envelope struct {
			ID           string       `json:"id"`
			CreatedAt    string       `json:"created_at"`
			Table        tableRef     `json:"table"`
			Trigger      trigRef      `json:"trigger"`
			Event        eventField   `json:"event"`
			DeliveryInfo deliveryInfo `json:"delivery_info"`
		}

		env := envelope{
			ID:        fmt.Sprintf("go-inproc-%d", now.UnixNano()),
			CreatedAt: now.Format(time.RFC3339Nano),
			Table:     tableRef{Schema: h.Schema, Name: h.Table},
			Trigger:   trigRef{Name: h.Trigger},
			Event: eventField{
				Op:               h.Op,
				Data:             dataField{Old: []byte("null"), New: newData},
				SessionVariables: sessJSON,
			},
			DeliveryInfo: deliveryInfo{CurrentRetry: 0, MaxRetries: 0},
		}
		raw, err := json.Marshal(env)
		if err != nil {
			// Marshal failure is a host bug; skip this hook.
			continue
		}
		dispatchErr := e.registry.Dispatch(ctx, h.Trigger, raw)
		if dispatchErr != nil && !errors.Is(dispatchErr, ErrNoHandler) {
			// Hook errors are silently dropped — the mutation is already committed.
			// In v1 we don't propagate hook errors to the caller. A future version
			// may collect them and expose via a side-channel.
			_ = dispatchErr
		}
	}
}

// Execute compiles and executes a GraphQL request (query or mutation).
// It is the primary entry point for one-shot request handling.
//
// For mutations a new transaction is opened, all statements run in order, and
// the transaction is committed before post-commit hooks are fired.
//
// All Postgres and plan-level errors are returned as a GraphQL body (nil Go
// error). A non-nil Go error indicates a host-level failure (marshal etc.).
func (e *Engine) Execute(ctx context.Context, query string, vars map[string]json.RawMessage, sessionVars map[string]string) ([]byte, error) {
	plan, err := e.compilePlan(ctx, compileInput{
		Query:       query,
		Variables:   vars,
		SessionVars: sessionVars,
	})
	if err != nil {
		return nil, fmt.Errorf("Execute: compile: %w", err)
	}
	if plan.Kind == PlanErrorK && plan.Err != nil {
		return errorBody(plan.Err.Code, plan.Err.Path, plan.Err.Message), nil
	}
	switch plan.Kind {
	case PlanQuery:
		return e.runQuery(ctx, plan)
	case PlanMutation:
		return e.runMutation(ctx, plan, nil, sessionVars)
	default:
		return nil, fmt.Errorf("Execute: unknown plan kind %q", plan.Kind)
	}
}

// ExecuteTx compiles and executes a GraphQL request within the caller's
// existing transaction. The caller retains full ownership of the transaction:
// commit and rollback are the caller's responsibility.
//
// Post-commit hooks are NOT fired from ExecuteTx because the host has not yet
// committed. The caller is responsible for any post-commit side effects after
// committing the transaction.
//
// All Postgres and plan-level errors are returned as a GraphQL body (nil Go
// error, matching the HTTP-200 convention). A non-nil Go error indicates a
// host-level failure.
func (e *Engine) ExecuteTx(ctx context.Context, tx pgx.Tx, query string, vars map[string]json.RawMessage, sessionVars map[string]string) ([]byte, error) {
	plan, err := e.compilePlan(ctx, compileInput{
		Query:       query,
		Variables:   vars,
		SessionVars: sessionVars,
	})
	if err != nil {
		return nil, fmt.Errorf("ExecuteTx: compile: %w", err)
	}
	if plan.Kind == PlanErrorK && plan.Err != nil {
		return errorBody(plan.Err.Code, plan.Err.Path, plan.Err.Message), nil
	}
	switch plan.Kind {
	case PlanQuery:
		// Run the query within the provided transaction.
		if len(plan.Statements) == 0 {
			return nil, fmt.Errorf("ExecuteTx: plan has no statements")
		}
		var data json.RawMessage
		if err := tx.QueryRow(ctx, plan.Statements[0].SQL).Scan(&data); err != nil {
			return mapPGError(err, plan.ErrorMap), nil
		}
		envelope, err := json.Marshal(map[string]json.RawMessage{"data": data})
		if err != nil {
			return nil, fmt.Errorf("ExecuteTx: marshal query envelope: %w", err)
		}
		return envelope, nil
	case PlanMutation:
		// Run the mutation within the caller's transaction (no hooks fired).
		return e.runMutation(ctx, plan, tx, sessionVars)
	default:
		return nil, fmt.Errorf("ExecuteTx: unknown plan kind %q", plan.Kind)
	}
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
