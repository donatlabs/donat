package donat

import (
	"errors"
	"net/http"
	"testing"
)

// TestSessionFromHeaders_Collection verifies that X-Donat-* headers are
// collected with lowercased keys and that non-X-Donat-* headers are excluded.
func TestSessionFromHeaders_Collection(t *testing.T) {
	h := http.Header{}
	h.Set("X-Donat-Role", "user")
	h.Set("X-Donat-User-Id", "42")
	h.Set("Authorization", "Bearer token") // must be excluded
	h.Set("Content-Type", "application/json")

	vars, err := sessionFromHeaders(h)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if vars["x-donat-role"] != "user" {
		t.Errorf("x-donat-role: got %q, want %q", vars["x-donat-role"], "user")
	}
	if vars["x-donat-user-id"] != "42" {
		t.Errorf("x-donat-user-id: got %q, want %q", vars["x-donat-user-id"], "42")
	}
	if _, ok := vars["authorization"]; ok {
		t.Error("authorization header must not appear in session vars")
	}
	if _, ok := vars["content-type"]; ok {
		t.Error("content-type header must not appear in session vars")
	}
}

// TestSessionFromHeaders_KeysLowercased verifies that the output map keys are
// always lowercase regardless of how the caller sends the header.
func TestSessionFromHeaders_KeysLowercased(t *testing.T) {
	h := http.Header{}
	h["X-DONAT-ROLE"] = []string{"admin"}
	h["X-Donat-Custom-Var"] = []string{"val"}

	vars, err := sessionFromHeaders(h)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if vars["x-donat-role"] != "admin" {
		t.Errorf("key lowercasing: got role %q, want %q", vars["x-donat-role"], "admin")
	}
	if vars["x-donat-custom-var"] != "val" {
		t.Errorf("key lowercasing: got custom %q, want %q", vars["x-donat-custom-var"], "val")
	}
}

// TestSessionFromHeaders_MissingRole verifies that a missing x-donat-role
// returns the exact access-denied denial (code + path + message) that the
// engine returns, per the no-admin rule.
func TestSessionFromHeaders_MissingRole(t *testing.T) {
	h := http.Header{}
	h.Set("X-Donat-User-Id", "7") // has some X-Donat-* but no role

	_, err := sessionFromHeaders(h)
	if err == nil {
		t.Fatal("expected error for missing role, got nil")
	}

	var se *sessionError
	if !errors.As(err, &se) {
		t.Fatalf("error must be *sessionError, got %T: %v", err, err)
	}
	if se.code != "access-denied" {
		t.Errorf("code: got %q, want %q", se.code, "access-denied")
	}
	if se.path != "$" {
		t.Errorf("path: got %q, want %q", se.path, "$")
	}
	const wantMsg = "x-donat-role header is required (this engine has no admin role)"
	if se.message != wantMsg {
		t.Errorf("message: got %q, want %q", se.message, wantMsg)
	}
}

// TestSessionFromHeaders_EmptyRole verifies that an empty x-donat-role value
// also produces the access-denied denial.
func TestSessionFromHeaders_EmptyRole(t *testing.T) {
	h := http.Header{}
	h.Set("X-Donat-Role", "")

	_, err := sessionFromHeaders(h)
	if err == nil {
		t.Fatal("expected error for empty role, got nil")
	}
	var se *sessionError
	if !errors.As(err, &se) {
		t.Fatalf("error must be *sessionError, got %T", err)
	}
	if se.code != "access-denied" {
		t.Errorf("code: got %q, want %q", se.code, "access-denied")
	}
}

// TestSessionFromHeaders_BackendOnlyPermissions verifies that every accepted
// boolean spelling is accepted and that an unrecognised value is rejected.
func TestSessionFromHeaders_BackendOnlyPermissions(t *testing.T) {
	trueSpellings := []string{"true", "True", "TRUE", "t", "T", "yes", "Yes", "YES", "y", "Y"}
	falseSpellings := []string{"false", "False", "FALSE", "f", "F", "no", "No", "NO", "n", "N"}
	allAccepted := append(trueSpellings, falseSpellings...)

	for _, spelling := range allAccepted {
		h := http.Header{}
		h.Set("X-Donat-Role", "user")
		h.Set("X-Donat-Use-Backend-Only-Permissions", spelling)

		vars, err := sessionFromHeaders(h)
		if err != nil {
			t.Errorf("spelling %q: unexpected error: %v", spelling, err)
			continue
		}
		if vars["x-donat-use-backend-only-permissions"] != spelling {
			t.Errorf("spelling %q: header value not preserved in vars", spelling)
		}
	}

	invalidSpellings := []string{"1", "0", "on", "off", "yes!", "maybe", ""}
	for _, bad := range invalidSpellings {
		if bad == "" {
			// An empty value is the "absent" case in net/http (canonical header set to "").
			// net/http.Header.Set("X-Donat-Use-Backend-Only-Permissions", "") stores ""
			// so the key IS present with an empty value. Confirm bad-request.
		}
		h := http.Header{}
		h.Set("X-Donat-Role", "user")
		h.Set("X-Donat-Use-Backend-Only-Permissions", bad)

		_, err := sessionFromHeaders(h)
		if err == nil {
			t.Errorf("invalid spelling %q: expected error, got nil", bad)
			continue
		}
		var se *sessionError
		if !errors.As(err, &se) {
			t.Fatalf("invalid spelling %q: error must be *sessionError, got %T", bad, err)
		}
		if se.code != "bad-request" {
			t.Errorf("invalid spelling %q: code: got %q, want %q", bad, se.code, "bad-request")
		}
	}
}

// TestSessionFromHeaders_AdminSecretExcluded verifies that X-Donat-Admin-Secret
// is never included in the returned session vars.
func TestSessionFromHeaders_AdminSecretExcluded(t *testing.T) {
	h := http.Header{}
	h.Set("X-Donat-Role", "user")
	h.Set("X-Donat-Admin-Secret", "supersecret")

	vars, err := sessionFromHeaders(h)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if _, ok := vars["x-donat-admin-secret"]; ok {
		t.Error("x-donat-admin-secret must not appear in session vars")
	}
}
