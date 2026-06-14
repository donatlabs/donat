package donat

import (
	"fmt"
	"net/http"
	"strings"
)

// sessionError is a structured Donat GraphQL error produced during session
// resolution. It implements the error interface so the handler can test for it
// with errors.As and render it via errorBody.
type sessionError struct {
	code    string
	path    string
	message string
}

func (e *sessionError) Error() string {
	return fmt.Sprintf("session error [%s] %s: %s", e.code, e.path, e.message)
}

// sessionFromHeaders mirrors crates/server/src/gql.rs:session_from_headers
// (trusted branch, no admin role). It:
//
//  1. Collects every X-Donat-* header (except X-Donat-Admin-Secret) into a
//     map with LOWERCASED keys and the original value.
//  2. Requires x-donat-role to be non-empty; absent or empty → access-denied.
//  3. Validates x-donat-use-backend-only-permissions when present; an
//     unrecognised value → bad-request (matching gql.rs exactly).
//
// JWT / admin-secret auth is the responsibility of the host application's HTTP
// middleware. The SDK operates only in the "trusted" branch (role is explicit).
//
// Returned error is always *sessionError; check with errors.As.
func sessionFromHeaders(h http.Header) (map[string]string, error) {
	vars := make(map[string]string)
	for rawKey, vals := range h {
		key := strings.ToLower(rawKey)
		if !strings.HasPrefix(key, "x-donat-") {
			continue
		}
		if key == "x-donat-admin-secret" {
			continue
		}
		if len(vals) == 0 {
			continue
		}
		vars[key] = vals[0]
	}

	// Require x-donat-role (no admin role: explicit role is mandatory).
	role, ok := vars["x-donat-role"]
	if !ok || role == "" {
		return nil, &sessionError{
			code:    "access-denied",
			path:    "$",
			message: "x-donat-role header is required (this engine has no admin role)",
		}
	}

	// Validate x-donat-use-backend-only-permissions when present.
	// Mirrors gql.rs:96-109 exactly (case-insensitive accept-set).
	if raw, present := vars["x-donat-use-backend-only-permissions"]; present {
		switch strings.ToLower(raw) {
		case "true", "t", "yes", "y", "false", "f", "no", "n":
			// valid — pass through
		default:
			return nil, &sessionError{
				code:    "bad-request",
				path:    "$",
				message: "x-donat-use-backend-only-permissions:  Not a valid boolean text. True values are [\"true\",\"t\",\"yes\",\"y\"] and  False values are [\"false\",\"f\",\"no\",\"n\"]. All values are case insensitive",
			}
		}
	}

	return vars, nil
}
