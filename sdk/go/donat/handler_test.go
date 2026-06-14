package donat

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/jackc/pgx/v5/pgxpool"
)

// noopPool returns a pgxpool.Pool that is connected to a syntactically valid
// but unreachable DSN. pgxpool.New is lazy (no actual TCP connection until
// the first query), so construction succeeds. Used for tests that exercise
// the handler's session-denial path, which never reaches the database.
func noopPool(t *testing.T) *pgxpool.Pool {
	t.Helper()
	ctx := context.Background()
	pool, err := pgxpool.New(ctx, "postgresql://unused:unused@127.0.0.1:1/unused?connect_timeout=1")
	if err != nil {
		t.Fatalf("noopPool: pgxpool.New: %v", err)
	}
	t.Cleanup(pool.Close)
	return pool
}

// denialEngine builds an Engine suitable for handler tests that exercise the
// session-denial path. The pool is never actually used — session resolution
// short-circuits before Execute touches the database.
func denialEngine(t *testing.T) *Engine {
	t.Helper()
	ctx := context.Background()
	pool := noopPool(t)
	eng, err := New(ctx, Config{
		Backend:  Postgres(pool),
		Metadata: fixtureMetaCatalog(),
	})
	if err != nil {
		t.Fatalf("denialEngine: New: %v", err)
	}
	return eng
}

// TestHandlerComposability verifies that Handler() can be mounted alongside
// user-defined routes in the same http.ServeMux without interference:
//
//   - A request to the user's own route is served by the user's handler.
//   - A POST to /v1/graphql without X-Donat-Role returns the exact no-admin
//     denial body (HTTP 200, GraphQL error in body) WITHOUT hitting the database.
//
// This test does NOT require a real Postgres instance.
func TestHandlerComposability(t *testing.T) {
	eng := denialEngine(t)

	// Build a mixed mux: engine route + user route side by side.
	mux := http.NewServeMux()
	mux.Handle("/v1/graphql", eng.Handler())
	mux.HandleFunc("/custom", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		w.WriteHeader(http.StatusOK)
		_, _ = io.WriteString(w, "hello from custom")
	})

	srv := httptest.NewServer(mux)
	defer srv.Close()

	t.Run("user_route_reachable", func(t *testing.T) {
		resp, err := http.Get(srv.URL + "/custom")
		if err != nil {
			t.Fatalf("GET /custom: %v", err)
		}
		defer resp.Body.Close()
		body, _ := io.ReadAll(resp.Body)
		if resp.StatusCode != http.StatusOK {
			t.Errorf("status: got %d, want 200", resp.StatusCode)
		}
		if string(body) != "hello from custom" {
			t.Errorf("body: got %q, want %q", string(body), "hello from custom")
		}
	})

	t.Run("engine_route_no_role_denial", func(t *testing.T) {
		// POST to /v1/graphql with NO X-Donat-Role header.
		// Must return HTTP 200 with the exact denial body — no DB required.
		reqBody := `{"query":"{ article { id } }"}`
		req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/graphql", strings.NewReader(reqBody))
		req.Header.Set("Content-Type", "application/json")
		// Intentionally no X-Donat-Role header.

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("POST /v1/graphql: %v", err)
		}
		defer resp.Body.Close()
		rawBody, _ := io.ReadAll(resp.Body)

		if resp.StatusCode != http.StatusOK {
			t.Errorf("status: got %d, want 200 (engine uses HTTP-200-with-errors)", resp.StatusCode)
		}
		ct := resp.Header.Get("Content-Type")
		if !strings.HasPrefix(ct, "application/json") {
			t.Errorf("Content-Type: got %q, want application/json", ct)
		}

		// Decode and assert the exact denial shape.
		var envelope struct {
			Errors []struct {
				Extensions struct {
					Path string `json:"path"`
					Code string `json:"code"`
				} `json:"extensions"`
				Message string `json:"message"`
			} `json:"errors"`
		}
		if err := json.Unmarshal(rawBody, &envelope); err != nil {
			t.Fatalf("unmarshal denial body: %v\nbody: %s", err, string(rawBody))
		}
		if len(envelope.Errors) == 0 {
			t.Fatalf("expected errors array in denial body; got: %s", string(rawBody))
		}
		e := envelope.Errors[0]
		if e.Extensions.Code != "access-denied" {
			t.Errorf("code: got %q, want %q", e.Extensions.Code, "access-denied")
		}
		if e.Extensions.Path != "$" {
			t.Errorf("path: got %q, want %q", e.Extensions.Path, "$")
		}
		const wantMsg = "x-donat-role header is required (this engine has no admin role)"
		if e.Message != wantMsg {
			t.Errorf("message: got %q, want %q", e.Message, wantMsg)
		}

		t.Logf("denial body: %s", string(rawBody))
	})
}

// TestHandlerBadBackendOnlyPermissions verifies that an invalid value for
// X-Donat-Use-Backend-Only-Permissions returns the bad-request error body
// (HTTP 200) without touching the database.
func TestHandlerBadBackendOnlyPermissions(t *testing.T) {
	eng := denialEngine(t)
	mux := http.NewServeMux()
	mux.Handle("/v1/graphql", eng.Handler())
	srv := httptest.NewServer(mux)
	defer srv.Close()

	req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/graphql",
		strings.NewReader(`{"query":"{ article { id } }"}`))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Donat-Role", "user")
	req.Header.Set("X-Donat-Use-Backend-Only-Permissions", "maybe") // invalid

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	defer resp.Body.Close()
	rawBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusOK {
		t.Errorf("status: got %d, want 200", resp.StatusCode)
	}

	var envelope struct {
		Errors []struct {
			Extensions struct {
				Code string `json:"code"`
			} `json:"extensions"`
		} `json:"errors"`
	}
	if err := json.Unmarshal(rawBody, &envelope); err != nil {
		t.Fatalf("unmarshal: %v\nbody: %s", err, string(rawBody))
	}
	if len(envelope.Errors) == 0 {
		t.Fatalf("expected errors array; got: %s", string(rawBody))
	}
	if envelope.Errors[0].Extensions.Code != "bad-request" {
		t.Errorf("code: got %q, want %q", envelope.Errors[0].Extensions.Code, "bad-request")
	}
	t.Logf("bad-permissions body: %s", string(rawBody))
}

// TestHandlerIntegration exercises the full Handler() path end-to-end:
// session resolution → wasm compile → Postgres execute → response.
// Requires DONAT_TEST_PG to be set.
func TestHandlerIntegration(t *testing.T) {
	eng, _ := testEngine(t) // skips if DONAT_TEST_PG not set

	mux := http.NewServeMux()
	mux.Handle("/v1/graphql", eng.Handler())
	srv := httptest.NewServer(mux)
	defer srv.Close()

	reqBody := `{"query":"query { article { id title } }"}`
	req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/graphql", strings.NewReader(reqBody))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Donat-Role", "user")
	req.Header.Set("X-Donat-User-Id", "1")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("POST: %v", err)
	}
	defer resp.Body.Close()
	rawBody, _ := io.ReadAll(resp.Body)

	if resp.StatusCode != http.StatusOK {
		t.Errorf("status: got %d, want 200", resp.StatusCode)
	}

	var envelope struct {
		Data struct {
			Article []struct {
				ID    int    `json:"id"`
				Title string `json:"title"`
			} `json:"article"`
		} `json:"data"`
		Errors []json.RawMessage `json:"errors"`
	}
	if err := json.Unmarshal(rawBody, &envelope); err != nil {
		t.Fatalf("unmarshal: %v\nbody: %s", err, string(rawBody))
	}
	if len(envelope.Errors) > 0 {
		t.Fatalf("unexpected errors: %s", string(rawBody))
	}
	if len(envelope.Data.Article) == 0 {
		t.Fatalf("expected at least 1 article; body: %s", string(rawBody))
	}
	if envelope.Data.Article[0].ID != 1 {
		t.Errorf("article[0].id: got %d, want 1", envelope.Data.Article[0].ID)
	}
	t.Logf("handler integration response: %s", string(rawBody))
}
