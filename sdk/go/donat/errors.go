package donat

import (
	"encoding/json"
	"errors"
	"strings"

	"github.com/jackc/pgx/v5/pgconn"
)

// errorBody serialises a single Donat GraphQL error body.
// It mirrors crates/server/src/gql.rs:error_json.
// path defaults to "$" when the empty string is passed.
func errorBody(code, path, message string) []byte {
	if path == "" {
		path = "$"
	}
	type extensions struct {
		Path string `json:"path"`
		Code string `json:"code"`
	}
	type entry struct {
		Extensions extensions `json:"extensions"`
		Message    string     `json:"message"`
	}
	type body struct {
		Errors []entry `json:"errors"`
	}
	b, _ := json.Marshal(body{
		Errors: []entry{{
			Extensions: extensions{Path: path, Code: code},
			Message:    message,
		}},
	})
	return b
}

// mapPGError maps a Postgres error (or any error) to the Donat error body JSON,
// mirroring crates/server/src/gql.rs:db_error_json.
//
// errorMap is the plan's error_map field, which encodes directives as:
//   - "permission-error-from-payload"  – for SQLSTATE 23514: try to decode the
//     message as a JSON payload {path, message} and emit a permission-error body.
//   - "code:prefix"                    – split on the first ':'; emit {code, "$", prefix+pgErr.Message}.
//   - "bare-code"                      – no colon; emit {bare-code, "$", pgErr.Message}.
//
// Lookup order: errorMap[sqlstate] → errorMap["default"] → built-in fallback.
func mapPGError(err error, errorMap map[string]string) []byte {
	var pgErr *pgconn.PgError
	if !errors.As(err, &pgErr) {
		return errorBody("unexpected", "$", err.Error())
	}

	// Look up the directive for this SQLSTATE, falling back to "default".
	directive := ""
	if errorMap != nil {
		if d, ok := errorMap[pgErr.Code]; ok {
			directive = d
		} else if d, ok := errorMap["default"]; ok {
			directive = d
		}
	}
	// If no directive found from the map, use built-in defaults (mirrors the
	// static match in db_error_json as a fallback for an absent error_map).
	if directive == "" {
		switch pgErr.Code {
		case "23514":
			directive = "permission-error-from-payload"
		case "23505":
			directive = "constraint-violation:Uniqueness violation. "
		case "23503":
			directive = "constraint-violation:Foreign key violation. "
		case "23502":
			directive = "constraint-violation:Not-NULL violation. "
		default:
			directive = "data-exception"
		}
	}

	// Interpret directive.
	if directive == "permission-error-from-payload" {
		// SQLSTATE 23514: our check_violation() raises this with a JSON payload
		// carrying the GraphQL error path. If we can decode it, use the path and
		// message from the payload; otherwise fall through to the default handler.
		var payload map[string]json.RawMessage
		if jsonErr := json.Unmarshal([]byte(pgErr.Message), &payload); jsonErr == nil {
			var path, message string
			if err := json.Unmarshal(payload["path"], &path); err == nil {
				if err := json.Unmarshal(payload["message"], &message); err == nil && path != "" && message != "" {
					return errorBody("permission-error", path, message)
				}
			}
		}
		// Fall through: emit bare permission-error with the raw message.
		return errorBody("permission-error", "$", pgErr.Message)
	}

	// Split "code:prefix" on the first colon.
	if code, prefix, ok := strings.Cut(directive, ":"); ok {
		return errorBody(code, "$", prefix+pgErr.Message)
	}

	// Bare code.
	return errorBody(directive, "$", pgErr.Message)
}
