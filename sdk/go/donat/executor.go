package donat

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
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

// hooksExcludingReplays drops the hooks belonging to command roots that were
// replayed rather than executed.
//
// A hook names the root it came from, so the match is by alias. With nothing
// replayed the slice is returned untouched, which is every plan a backend that
// does not report replays produces.
func hooksExcludingReplays(hooks []Hook, replays map[string]bool) []Hook {
	if len(replays) == 0 || len(hooks) == 0 {
		return hooks
	}
	kept := make([]Hook, 0, len(hooks))
	for _, h := range hooks {
		if !replays[h.Alias] {
			kept = append(kept, h)
		}
	}
	return kept
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
		// The payload is the result of the statement this hook came from,
		// found by the alias the core recorded. Looking it up by trigger name
		// used to miss — the map is keyed by response alias — and the fallback
		// then took whichever entry ranged first, so a handler could be handed
		// another root's rows. Absent is `null`, never someone else's data.
		newData := json.RawMessage("null")
		if part, ok := data[h.Alias]; ok {
			newData = part
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
			// The write has already committed, so the error cannot be returned
			// to the caller — but it must not vanish either. A handler whose
			// payload shape does not match the row never runs, and a swallowed
			// error makes that indistinguishable from a handler with nothing
			// to do.
			e.reportHookError(h.Trigger, dispatchErr)
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
	return e.ExecuteOperation(ctx, query, nil, vars, sessionVars)
}

// ExecuteOperation is Execute for a document carrying more than one operation,
// where `operationName` says which to run — the same field the GraphQL request
// body has. Without it such a document is ambiguous and the core refuses it,
// so a client that works against the standalone server would fail here.
func (e *Engine) ExecuteOperation(ctx context.Context, query string, operationName *string, vars map[string]json.RawMessage, sessionVars map[string]string) ([]byte, error) {
	plan, err := e.compilePlan(ctx, compileInput{
		Query:         query,
		OperationName: operationName,
		Variables:     vars,
		SessionVars:   sessionVars,
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
		return queryEnvelope(plan, data)

	case PlanAction:
		return e.runAction(ctx, plan, query, vars, sessionVars)

	case PlanMutation:
		var (
			data    map[string]json.RawMessage
			replays map[string]bool
			err     error
		)
		if reporter, ok := e.backend.(replayReporter); ok {
			data, replays, err = reporter.runMutationReportingReplays(ctx, plan)
		} else {
			data, err = e.backend.RunMutation(ctx, plan)
		}
		if err != nil {
			return e.backend.MapError(err, plan.ErrorMap), nil
		}
		// Fire post-commit hooks — the backend committed the transaction before
		// returning, so it is safe to dispatch side effects now. A replayed
		// command is skipped: it projected its stored result and ran no DML, so
		// on the native engine no trigger fired either.
		e.fireHooks(ctx, hooksExcludingReplays(plan.Hooks, replays), data, sessionVars)
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
		return queryEnvelope(plan, data)

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
	return queryEnvelope(plan, data)
}

// queryEnvelope wraps a read's result in the GraphQL envelope, in the order
// the client asked for.
//
// Every read path goes through here rather than marshalling the statement
// result itself. Two of the three once did that, and both silently dropped a
// root `__typename` — the planner answers it, so it never reaches SQL and
// cannot come back in the statement result. A host that moved a read inside
// its own transaction got a different response body than the same read outside
// one, which is precisely the difference the embedded SDK exists not to have.
func queryEnvelope(plan Plan, data json.RawMessage) ([]byte, error) {
	assembled, err := assembleResponse(plan.Response, data)
	if err != nil {
		return nil, fmt.Errorf("assembling the query response: %w", err)
	}
	envelope, err := json.Marshal(map[string]json.RawMessage{"data": assembled})
	if err != nil {
		return nil, fmt.Errorf("marshalling the query envelope: %w", err)
	}
	return envelope, nil
}

// assembleResponse builds the top-level object in the order the client asked
// for, mirroring crates/server/src/gql.rs.
//
// A root `__typename` is answered by the planner and never reaches SQL, so a
// host that returned the statement result unchanged dropped it — and a query
// selecting only `__typename` produced no statement at all, which then read as
// an error. With no slots the result passes through untouched, which is every
// plan the core built before it emitted them.
func assembleResponse(slots []ResponseSlot, data json.RawMessage) (json.RawMessage, error) {
	if len(slots) == 0 {
		return data, nil
	}
	values := map[string]json.RawMessage{}
	if len(data) > 0 && string(data) != "null" {
		if err := json.Unmarshal(data, &values); err != nil {
			return nil, fmt.Errorf("decoding the statement result: %w", err)
		}
	}

	// json.Marshal of a map sorts its keys, and the client's field order is
	// part of the response, so the object is built by hand.
	var out bytes.Buffer
	out.WriteByte('{')
	for i, slot := range slots {
		if i > 0 {
			out.WriteByte(',')
		}
		key, err := json.Marshal(slot.Key)
		if err != nil {
			return nil, fmt.Errorf("encoding response key %q: %w", slot.Key, err)
		}
		out.Write(key)
		out.WriteByte(':')

		switch slot.Kind {
		case "local_typename":
			value, err := json.Marshal(slot.Value)
			if err != nil {
				return nil, fmt.Errorf("encoding __typename for %q: %w", slot.Key, err)
			}
			out.Write(value)
		default:
			if v, ok := values[slot.Key]; ok {
				out.Write(v)
			} else {
				out.WriteString("null")
			}
		}
	}
	out.WriteByte('}')
	return out.Bytes(), nil
}

// reportHookError surfaces a post-commit handler failure through the
// configured callback, or to the standard logger when none is set.
func (e *Engine) reportHookError(trigger string, err error) {
	if e.cfg.OnHookError != nil {
		e.cfg.OnHookError(trigger, err)
		return
	}
	log.Printf("donat: event handler %q failed: %v", trigger, err)
}
