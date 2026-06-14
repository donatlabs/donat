// Package donat — per-SDK conformance subset (host-layer drift guard).
//
// This file implements a curated SUBSET of the engine's conformance surface,
// exercising the Go SDK host (Engine + Handler) over a real Postgres instance.
// It covers the four representative axes:
//
//  1. Query        — a successful SELECT response through Handler()
//  2. Denial       — a role with no permissions yields a GQL error body
//  3. No-role      — a missing X-Donat-Role header yields access-denied
//  4. Mutation     — a successful INSERT mutation through Handler()
//
// This is NOT a replacement for the full native conformance harness
// (crates/conformance), which drives the Rust engine binary against the
// complete Donat fixture surface. The intent here is a narrowly-scoped
// regression guard for the Go host layer: session resolution, HTTP envelope,
// wasm compile+execute, and Postgres round-trip. Running the full native
// harness against the Go host is a larger follow-up (tracked separately).
//
// All tests in this file require the DONAT_TEST_PG environment variable to
// be set (any non-empty value, or a postgres:// URL) and skip cleanly without
// it. They reuse the testEngine / setupMutationTables helpers defined in
// integration_test.go and executor_mutation_test.go.
package donat

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// TestConformanceSubset is the host-layer drift guard. It stands up
// eng.Handler() behind an httptest.Server and issues real HTTP requests,
// comparing the response bodies to the engine's known-good outputs.
func TestConformanceSubset(t *testing.T) {
	// ── Subtest C1: successful query ──────────────────────────────────────────
	// Uses testEngine (fixtureMetaCatalog + seeded author/article rows).
	t.Run("C1_query_article", func(t *testing.T) {
		eng, _ := testEngine(t) // skips if DONAT_TEST_PG not set

		mux := http.NewServeMux()
		mux.Handle("/v1/graphql", eng.Handler())
		srv := httptest.NewServer(mux)
		defer srv.Close()

		reqBody := `{"query":"query { article { id title } }"}`
		req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/graphql", strings.NewReader(reqBody))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("X-Donat-Role", "user")
		// The fixture metadata filters article by author's user-id; seed has author 1.
		req.Header.Set("X-Donat-User-Id", "1")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("POST /v1/graphql: %v", err)
		}
		defer resp.Body.Close()
		rawBody, _ := io.ReadAll(resp.Body)

		if resp.StatusCode != http.StatusOK {
			t.Errorf("status: got %d, want 200", resp.StatusCode)
		}

		// Decode and compare as structured values (JSON key order may vary).
		var got struct {
			Data struct {
				Article []struct {
					ID    int    `json:"id"`
					Title string `json:"title"`
				} `json:"article"`
			} `json:"data"`
			Errors json.RawMessage `json:"errors"`
		}
		if err := json.Unmarshal(rawBody, &got); err != nil {
			t.Fatalf("unmarshal: %v\nbody: %s", err, rawBody)
		}
		if got.Errors != nil {
			t.Fatalf("unexpected errors; body: %s", rawBody)
		}
		if len(got.Data.Article) != 1 {
			t.Fatalf("expected 1 article, got %d; body: %s", len(got.Data.Article), rawBody)
		}
		a := got.Data.Article[0]
		if a.ID != 1 {
			t.Errorf("article.id: got %d, want 1", a.ID)
		}
		if a.Title != "First Article" {
			t.Errorf("article.title: got %q, want %q", a.Title, "First Article")
		}
		t.Logf("C1 body: %s", rawBody)
	})

	// ── Subtest C2: permission denial (role with no access) ───────────────────
	// "stranger" has no permissions in the fixture metadata; the planner
	// returns a validation-failed error (field not found in query_root).
	// We assert the structural contract: errors array present, no data key,
	// extensions.code non-empty — the exact planner message is already
	// snapshot-tested in crates/wasm-core; a structural assert is sufficient.
	t.Run("C2_permission_denial_stranger_role", func(t *testing.T) {
		eng, _ := testEngine(t) // skips if DONAT_TEST_PG not set

		mux := http.NewServeMux()
		mux.Handle("/v1/graphql", eng.Handler())
		srv := httptest.NewServer(mux)
		defer srv.Close()

		reqBody := `{"query":"query { article { id } }"}`
		req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/graphql", strings.NewReader(reqBody))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("X-Donat-Role", "stranger")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("POST /v1/graphql: %v", err)
		}
		defer resp.Body.Close()
		rawBody, _ := io.ReadAll(resp.Body)

		if resp.StatusCode != http.StatusOK {
			t.Errorf("status: got %d, want 200 (engine always returns HTTP 200)", resp.StatusCode)
		}

		var envelope struct {
			Errors []struct {
				Extensions struct {
					Code string `json:"code"`
					Path string `json:"path"`
				} `json:"extensions"`
				Message string `json:"message"`
			} `json:"errors"`
			Data json.RawMessage `json:"data"`
		}
		if err := json.Unmarshal(rawBody, &envelope); err != nil {
			t.Fatalf("unmarshal: %v\nbody: %s", err, rawBody)
		}
		if len(envelope.Errors) == 0 {
			t.Fatalf("expected errors array for unknown role; body: %s", rawBody)
		}
		if envelope.Data != nil {
			t.Errorf("unexpected 'data' key in error response; body: %s", rawBody)
		}
		if envelope.Errors[0].Extensions.Code == "" {
			t.Errorf("errors[0].extensions.code must be non-empty; body: %s", rawBody)
		}
		t.Logf("C2 body: %s", rawBody)
	})

	// ── Subtest C3: no-role denial (missing X-Donat-Role header) ─────────────
	// The handler must reject the request before any DB access. The exact
	// denial body is part of the engine's API contract (no-admin rule).
	t.Run("C3_no_role_denial", func(t *testing.T) {
		eng, _ := testEngine(t) // skips if DONAT_TEST_PG not set

		mux := http.NewServeMux()
		mux.Handle("/v1/graphql", eng.Handler())
		srv := httptest.NewServer(mux)
		defer srv.Close()

		reqBody := `{"query":"query { article { id } }"}`
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
			t.Errorf("status: got %d, want 200", resp.StatusCode)
		}

		var envelope struct {
			Errors []struct {
				Extensions struct {
					Code string `json:"code"`
					Path string `json:"path"`
				} `json:"extensions"`
				Message string `json:"message"`
			} `json:"errors"`
		}
		if err := json.Unmarshal(rawBody, &envelope); err != nil {
			t.Fatalf("unmarshal: %v\nbody: %s", err, rawBody)
		}
		if len(envelope.Errors) == 0 {
			t.Fatalf("expected errors array for missing role; body: %s", rawBody)
		}
		e0 := envelope.Errors[0]

		// Exact string assertions for the no-admin denial — this is part of the
		// engine's observable API contract.
		const wantCode = "access-denied"
		const wantMsg = "x-donat-role header is required (this engine has no admin role)"
		if e0.Extensions.Code != wantCode {
			t.Errorf("code: got %q, want %q", e0.Extensions.Code, wantCode)
		}
		if e0.Message != wantMsg {
			t.Errorf("message: got %q, want %q", e0.Message, wantMsg)
		}
		if e0.Extensions.Path != "$" {
			t.Errorf("path: got %q, want %q", e0.Extensions.Path, "$")
		}
		t.Logf("C3 body: %s", rawBody)
	})

	// ── Subtest C4: successful mutation ───────────────────────────────────────
	// Uses fixtureMetaCatalogWithTrigger + identity-column tables so that an
	// insert without an explicit id works. This sub-test is self-contained and
	// does not share tables with C1–C3.
	t.Run("C4_mutation_insert_author", func(t *testing.T) {
		pool := testPool(t) // skips if DONAT_TEST_PG not set
		setupMutationTables(t, pool)

		ctx := context.Background()
		eng, err := New(ctx, Config{
			Backend:  Postgres(pool),
			Metadata: fixtureMetaCatalogWithTrigger(),
		})
		if err != nil {
			t.Fatalf("New engine: %v", err)
		}

		mux := http.NewServeMux()
		mux.Handle("/v1/graphql", eng.Handler())
		srv := httptest.NewServer(mux)
		defer srv.Close()

		reqBody := `{"query":"mutation { insert_author(objects:[{name:\"X\"}]) { affected_rows } }"}`
		req, _ := http.NewRequest(http.MethodPost, srv.URL+"/v1/graphql", strings.NewReader(reqBody))
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("X-Donat-Role", "user")
		req.Header.Set("X-Donat-User-Id", "99")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("POST /v1/graphql: %v", err)
		}
		defer resp.Body.Close()
		rawBody, _ := io.ReadAll(resp.Body)

		if resp.StatusCode != http.StatusOK {
			t.Errorf("status: got %d, want 200", resp.StatusCode)
		}

		var got struct {
			Data struct {
				InsertAuthor struct {
					AffectedRows int `json:"affected_rows"`
				} `json:"insert_author"`
			} `json:"data"`
			Errors json.RawMessage `json:"errors"`
		}
		if err := json.Unmarshal(rawBody, &got); err != nil {
			t.Fatalf("unmarshal: %v\nbody: %s", err, rawBody)
		}
		if got.Errors != nil {
			t.Fatalf("unexpected errors; body: %s", rawBody)
		}
		if got.Data.InsertAuthor.AffectedRows != 1 {
			t.Errorf("affected_rows: got %d, want 1; body: %s", got.Data.InsertAuthor.AffectedRows, rawBody)
		}
		t.Logf("C4 body: %s", rawBody)
	})
}
