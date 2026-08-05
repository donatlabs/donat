package donat

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"
)

// runAction resolves an action operation by calling the registered functions
// and then asking the core to shape what they returned.
//
// The shaping is not optional. A function's return value is ordinary Go data
// with no relation to the action's declared `output_type`, and the standalone
// server runs the same check over a webhook body. Skipping it here would let
// one metadata file answer differently depending on which host is serving —
// a field declared `String!` returning null on this one and erroring on that
// one.
func (e *Engine) runAction(
	ctx context.Context,
	plan Plan,
	query string,
	vars map[string]json.RawMessage,
	sessionVars map[string]string,
) ([]byte, error) {
	results := make(map[string]json.RawMessage, len(plan.Items))

	for _, item := range plan.Items {
		// A typename is answered by the core and is already in the plan;
		// shaping puts it back in the response.
		if item.Kind != "call" {
			continue
		}
		// An action with a handler is a webhook, and the same metadata has to
		// work here and on the standalone server. Calling it keeps the two
		// hosts interchangeable rather than making "which host" a property of
		// the declaration.
		if item.Handler != nil {
			raw, err := e.callWebhook(ctx, item)
			if err != nil {
				return errorBody("unexpected", "$", err.Error()), nil
			}
			results[item.Alias] = raw
			continue
		}

		out, ok, err := e.functions().Call(ctx, item.Name, item.Input)
		if !ok {
			// New refuses to start in this state, so reaching it means the
			// registry changed under a running engine.
			return errorBody(
				"unexpected", "$",
				fmt.Sprintf("no function is registered for action %q", item.Name),
			), nil
		}
		if err != nil {
			// The function's own failure is the caller's answer, not a host
			// fault: it is the business logic saying no. A FunctionError picks
			// the code; anything else is `unexpected`, which is what an
			// unclassified fault is.
			code := "unexpected"
			var fe *FunctionError
			if errors.As(err, &fe) && fe.Code != "" {
				code = fe.Code
			}
			return errorBody(code, "$", err.Error()), nil
		}
		raw, err := json.Marshal(out)
		if err != nil {
			return nil, fmt.Errorf("action %q: encoding result: %w", item.Name, err)
		}
		results[item.Alias] = raw
	}

	return e.shapeAction(ctx, query, vars, sessionVars, results)
}

// shapeAction asks the core to validate and project the collected results.
func (e *Engine) shapeAction(
	ctx context.Context,
	query string,
	vars map[string]json.RawMessage,
	sessionVars map[string]string,
	results map[string]json.RawMessage,
) ([]byte, error) {
	payload, err := json.Marshal(shapeInput{
		Query:       query,
		Variables:   vars,
		SessionVars: sessionVars,
		Results:     results,
	})
	if err != nil {
		return nil, fmt.Errorf("shape action: marshal: %w", err)
	}
	c, err := e.acquire(ctx)
	if err != nil {
		return nil, err
	}
	defer e.release(c)

	raw, err := c.shapeAction(ctx, payload)
	if err != nil {
		return nil, fmt.Errorf("shape action: %w", err)
	}
	var shaped shapeResult
	if err := json.Unmarshal(raw, &shaped); err != nil {
		return nil, fmt.Errorf("shape action: decode: %w", err)
	}
	if shaped.Kind == "error" {
		return errorBody(shaped.Code, shaped.Path, shaped.Message), nil
	}
	return json.Marshal(map[string]json.RawMessage{"data": shaped.Data})
}

// shapeInput mirrors crates/wasm-core/src/compile.rs ShapeInput.
type shapeInput struct {
	Query       string                     `json:"query"`
	Variables   map[string]json.RawMessage `json:"variables,omitempty"`
	SessionVars map[string]string          `json:"session_vars"`
	Results     map[string]json.RawMessage `json:"results"`
}

// shapeResult mirrors crates/wasm-core/src/compile.rs ShapeResult.
type shapeResult struct {
	Kind    string          `json:"kind"`
	Data    json.RawMessage `json:"data"`
	Code    string          `json:"code"`
	Path    string          `json:"path"`
	Message string          `json:"message"`
}

// functions returns the configured set, never nil, so a call site does not
// have to distinguish "no functions" from "this one is missing".
func (e *Engine) functions() *Functions {
	if e.cfg.Functions == nil {
		return NewFunctions()
	}
	return e.cfg.Functions
}

// callWebhook resolves an action that names an HTTP handler, with the payload
// the standalone server sends so one handler serves both hosts unchanged.
func (e *Engine) callWebhook(ctx context.Context, item ActionItem) (json.RawMessage, error) {
	payload, err := json.Marshal(map[string]any{
		"action":            map[string]string{"name": item.Name},
		"input":             item.Input,
		"session_variables": item.SessionVariables,
	})
	if err != nil {
		return nil, fmt.Errorf("action %q: encoding the webhook payload: %w", item.Name, err)
	}
	if item.Timeout != nil {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, time.Duration(*item.Timeout)*time.Second)
		defer cancel()
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, *item.Handler, bytes.NewReader(payload))
	if err != nil {
		return nil, fmt.Errorf("action %q: %w", item.Name, err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := e.httpClient().Do(req)
	if err != nil {
		return nil, fmt.Errorf("http exception when calling webhook: %w", err)
	}
	defer resp.Body.Close() //nolint:errcheck
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("action %q: reading the webhook response: %w", item.Name, err)
	}
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		// The handler's own message is the caller's answer, the way the
		// standalone server surfaces it.
		var e struct {
			Message string `json:"message"`
		}
		_ = json.Unmarshal(body, &e)
		if e.Message == "" {
			e.Message = "webhook returned an error"
		}
		return nil, fmt.Errorf("%s", e.Message)
	}
	return body, nil
}

// httpClient is the client webhook actions use. A caller that needs its own
// timeouts, proxy or transport sets Config.HTTPClient.
func (e *Engine) httpClient() *http.Client {
	if e.cfg.HTTPClient != nil {
		return e.cfg.HTTPClient
	}
	return http.DefaultClient
}
