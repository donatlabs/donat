package donat

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
)

// gqlRequest is the JSON body accepted by the GraphQL endpoint.
// The engine supports the standard GraphQL-over-HTTP request shape.
type gqlRequest struct {
	Query         string                     `json:"query"`
	OperationName *string                    `json:"operationName,omitempty"`
	Variables     map[string]json.RawMessage `json:"variables,omitempty"`
}

// Handler returns an http.Handler that exposes the Engine as a GraphQL
// endpoint. It is designed to be mounted at any path inside the caller's
// own http.ServeMux (composability requirement):
//
//	mux := http.NewServeMux()
//	mux.Handle("/v1/graphql", eng.Handler())
//	mux.HandleFunc("/custom", myCustomHandler)
//
// Request lifecycle:
//
//  1. Resolve session from X-Donat-* headers. Any session error (missing role,
//     invalid backend-only-permissions) is returned as HTTP 200 with a GraphQL
//     error body, matching the engine's own error convention (see
//     crates/server/src/gql.rs:resolve_session — all session errors are
//     wrapped with StatusCode::OK before being sent to the client).
//
//  2. Parse the JSON body ({query, operationName, variables}). A body-parse
//     error returns HTTP 200 with a bad-request GraphQL error body.
//
//  3. Call Execute(ctx, query, variables, sessionVars). The result is already
//     a GraphQL envelope (either {"data":...} or {"errors":[...]}) — write it
//     verbatim with Content-Type: application/json.
//
// The no-admin rule is enforced at step 1: if x-donat-role is absent the
// request is denied BEFORE any database access, so Handler() can be used
// without a database for the denial path (useful in tests and early startup).
func (e *Engine) Handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Step 1: resolve session from headers.
		// This is intentionally done BEFORE reading the body so the
		// no-role denial never touches the database.
		sessionVars, err := sessionFromHeaders(r.Header)
		if err != nil {
			var se *sessionError
			if errors.As(err, &se) {
				writeJSON(w, errorBody(se.code, se.path, se.message))
			} else {
				writeJSON(w, errorBody("internal-error", "$", err.Error()))
			}
			return
		}

		// Step 2: parse the request body.
		body, err := io.ReadAll(r.Body)
		if err != nil {
			writeJSON(w, errorBody("bad-request", "$", "failed to read request body"))
			return
		}
		var req gqlRequest
		if err := json.Unmarshal(body, &req); err != nil {
			writeJSON(w, errorBody("bad-request", "$", "invalid JSON body: "+err.Error()))
			return
		}
		if req.Query == "" {
			writeJSON(w, errorBody("bad-request", "$", "query field is required"))
			return
		}

		// Step 3: compile + execute.
		result, err := e.Execute(r.Context(), req.Query, req.Variables, sessionVars)
		if err != nil {
			// Execute returns a non-nil Go error only for host-level failures
			// (marshal bugs etc.) — treat as internal error, still HTTP 200.
			writeJSON(w, errorBody("internal-error", "$", err.Error()))
			return
		}

		writeJSON(w, result)
	})
}

// writeJSON writes a JSON body with Content-Type: application/json and
// HTTP 200 status. It is used for all responses (success and error) to match
// the engine's HTTP-200-with-errors-body convention.
func writeJSON(w http.ResponseWriter, body []byte) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(body)
}
