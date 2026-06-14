package donat

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"
)

// wrapData encodes the per-root data map as the "data" value in the GraphQL
// envelope: {"data": <dataJSON>}. It avoids double-encoding by marshalling the
// inner map separately then embedding it as a RawMessage.
func wrapData(data map[string]json.RawMessage) ([]byte, error) {
	dataJSON, err := json.Marshal(data)
	if err != nil {
		return nil, fmt.Errorf("wrapData: marshal inner: %w", err)
	}
	envelope, err := json.Marshal(map[string]json.RawMessage{"data": dataJSON})
	if err != nil {
		return nil, fmt.Errorf("wrapData: marshal envelope: %w", err)
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
// All database and plan-level errors are returned as a GraphQL body (nil Go
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
		data, err := e.backend.RunQuery(ctx, plan)
		if err != nil {
			return e.backend.MapError(err, plan.ErrorMap), nil
		}
		envelope, err := json.Marshal(map[string]json.RawMessage{"data": data})
		if err != nil {
			return nil, fmt.Errorf("Execute: marshal query envelope: %w", err)
		}
		return envelope, nil

	case PlanMutation:
		data, err := e.backend.RunMutation(ctx, plan)
		if err != nil {
			return e.backend.MapError(err, plan.ErrorMap), nil
		}
		// Fire post-commit hooks — the backend committed the transaction before
		// returning, so it is safe to dispatch side effects now.
		e.fireHooks(ctx, plan.Hooks, data, sessionVars)
		return wrapData(data)

	default:
		return nil, fmt.Errorf("Execute: unknown plan kind %q", plan.Kind)
	}
}

// ExecuteTx compiles and executes a GraphQL request within the caller's
// existing transaction. The caller retains full ownership of the transaction:
// commit and rollback are the caller's responsibility.
//
// tx is the driver's transaction handle (e.g. *pgx.Tx); it is passed as any
// so that the Engine is not tied to a specific driver import. The backend
// casts it back to the concrete type it expects. If the backend does not
// implement txRunner, ExecuteTx returns an error body.
//
// Post-commit hooks are NOT fired from ExecuteTx because the host has not yet
// committed. The caller is responsible for any post-commit side effects after
// committing the transaction.
//
// All database and plan-level errors are returned as a GraphQL body (nil Go
// error, matching the HTTP-200 convention). A non-nil Go error indicates a
// host-level failure.
func (e *Engine) ExecuteTx(ctx context.Context, tx any, query string, vars map[string]json.RawMessage, sessionVars map[string]string) ([]byte, error) {
	tr, ok := e.backend.(txRunner)
	if !ok {
		return errorBody("internal-error", "$", "this backend does not support ExecuteTx"), nil
	}

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
		data, err := tr.runQueryTx(ctx, tx, plan)
		if err != nil {
			return e.backend.MapError(err, plan.ErrorMap), nil
		}
		envelope, err := json.Marshal(map[string]json.RawMessage{"data": data})
		if err != nil {
			return nil, fmt.Errorf("ExecuteTx: marshal query envelope: %w", err)
		}
		return envelope, nil

	case PlanMutation:
		// Run inside caller's transaction. No hooks — caller owns commit.
		data, err := tr.runMutationTx(ctx, tx, plan)
		if err != nil {
			return e.backend.MapError(err, plan.ErrorMap), nil
		}
		return wrapData(data)

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
	data, err := e.backend.RunQuery(ctx, plan)
	if err != nil {
		return e.backend.MapError(err, plan.ErrorMap), nil
	}
	envelope, err := json.Marshal(map[string]json.RawMessage{"data": data})
	if err != nil {
		return nil, fmt.Errorf("executeQuery: marshal envelope: %w", err)
	}
	return envelope, nil
}
