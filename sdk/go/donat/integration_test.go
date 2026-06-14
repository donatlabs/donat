package donat

import (
	"context"
	"encoding/json"
	"os"
	"testing"

	"github.com/jackc/pgx/v5/pgxpool"
)

// pgTestDSN returns the Postgres DSN to use for integration tests.
// If DONAT_TEST_PG is a URL it is used directly; otherwise the default
// local container DSN is used.
func pgTestDSN() string {
	env := os.Getenv("DONAT_TEST_PG")
	if env != "" && (len(env) > 4 && env[:4] == "post") {
		return env
	}
	return "postgresql://postgres:postgres@127.0.0.1:15432/postgres"
}

// testPool connects a pgxpool.Pool to the integration test Postgres and
// registers t.Cleanup to close it.
func testPool(t *testing.T) *pgxpool.Pool {
	t.Helper()
	if os.Getenv("DONAT_TEST_PG") == "" {
		t.Skip("set DONAT_TEST_PG=1 to run integration tests")
	}
	ctx := context.Background()
	pool, err := pgxpool.New(ctx, pgTestDSN())
	if err != nil {
		t.Fatalf("pgxpool.New: %v", err)
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		t.Fatalf("pool.Ping: %v", err)
	}
	t.Cleanup(pool.Close)
	return pool
}

// testEngine creates an Engine backed by a real Postgres pool and the
// article/author fixture metadata+catalog. It seeds the public.author and
// public.article tables (dropping them on cleanup) and returns the Engine
// together with the pool for any direct SQL operations the test may need.
//
// Table schema matches the fixture catalog in engine_test.go exactly:
//
//	public.author  (id int4 PK, name text, secret text)
//	public.article (id int4 PK, title text, author_id int4 FK→author.id, published bool)
//
// The seeded rows are:
//
//	author:  (1, 'Alice', 'secret1')
//	article: (1, 'First Article', 1, true)
//
// A single article row makes result ordering deterministic (no ORDER BY in the
// generated SQL).
func testEngine(t *testing.T) (*Engine, *pgxpool.Pool) {
	t.Helper()
	pool := testPool(t)
	ctx := context.Background()

	// Create the tables in public schema, clean up after the test.
	ddl := []string{
		`CREATE TABLE IF NOT EXISTS public.author (
			id       int4 PRIMARY KEY,
			name     text NOT NULL,
			secret   text NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS public.article (
			id        int4 PRIMARY KEY,
			title     text NOT NULL,
			author_id int4 NOT NULL REFERENCES public.author(id),
			published bool NOT NULL
		)`,
		// Seed exactly one author and one article for deterministic results.
		`INSERT INTO public.author  (id, name, secret)   VALUES (1, 'Alice', 'secret1')
		 ON CONFLICT (id) DO UPDATE SET name='Alice', secret='secret1'`,
		`INSERT INTO public.article (id, title, author_id, published)
		 VALUES (1, 'First Article', 1, true)
		 ON CONFLICT (id) DO UPDATE SET title='First Article', author_id=1, published=true`,
	}
	for _, q := range ddl {
		if _, err := pool.Exec(ctx, q); err != nil {
			t.Fatalf("setup DDL: %v\nsql: %s", err, q)
		}
	}
	t.Cleanup(func() {
		cleanup := []string{
			`DROP TABLE IF EXISTS public.article`,
			`DROP TABLE IF EXISTS public.author`,
		}
		for _, q := range cleanup {
			if _, err := pool.Exec(ctx, q); err != nil {
				t.Logf("cleanup warning: %v (sql: %s)", err, q)
			}
		}
	})

	eng, err := New(ctx, Config{
		Pool:     pool,
		Metadata: fixtureMetaCatalog(),
	})
	if err != nil {
		t.Fatalf("New engine: %v", err)
	}
	return eng, pool
}

// TestQueryExecute exercises the full query path end-to-end:
// wasm compile → pgx execution → {"data":...} envelope.
//
// The query "query { article { id title } }" as role "user" (no row filter on
// article in the fixture metadata) should return ALL article rows — we seed
// exactly one to keep ordering deterministic.
func TestQueryExecute(t *testing.T) {
	ctx := context.Background()
	eng, _ := testEngine(t)

	sessionVars := map[string]string{
		"x-donat-role":    "user",
		"x-donat-user-id": "1",
	}
	got, err := eng.executeQuery(ctx, "query { article { id title } }", nil, sessionVars)
	if err != nil {
		t.Fatalf("executeQuery: %v", err)
	}

	// Decode to verify structure — we don't byte-compare because JSON key
	// ordering from json_build_object is Postgres-controlled (it follows
	// insertion order matching the SELECT list, so id then title here).
	var envelope struct {
		Data struct {
			Article []struct {
				ID    int    `json:"id"`
				Title string `json:"title"`
			} `json:"article"`
		} `json:"data"`
	}
	if err := json.Unmarshal(got, &envelope); err != nil {
		t.Fatalf("unmarshal response: %v\nbody: %s", err, string(got))
	}
	if len(envelope.Data.Article) != 1 {
		t.Fatalf("expected 1 article, got %d; body: %s", len(envelope.Data.Article), string(got))
	}
	a := envelope.Data.Article[0]
	if a.ID != 1 {
		t.Errorf("article.id: got %d, want 1", a.ID)
	}
	if a.Title != "First Article" {
		t.Errorf("article.title: got %q, want %q", a.Title, "First Article")
	}

	// Also verify the outer shape: the raw bytes must start with {"data":
	// (not {"errors":) — confirming the envelope wrapper.
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(got, &raw); err != nil {
		t.Fatalf("raw unmarshal: %v", err)
	}
	if _, hasData := raw["data"]; !hasData {
		t.Errorf("response missing 'data' key; body: %s", string(got))
	}
	if _, hasErrors := raw["errors"]; hasErrors {
		t.Errorf("unexpected 'errors' key in success response; body: %s", string(got))
	}

	t.Logf("query response: %s", string(got))
}

// TestQueryExecuteError verifies that a plan-level error (wrong role) returns
// a {"errors":[...]} body and a nil Go error (GraphQL error-in-body, not
// transport error), which mirrors the engine's HTTP-200 error convention.
func TestQueryExecuteError(t *testing.T) {
	ctx := context.Background()
	eng, _ := testEngine(t)

	sessionVars := map[string]string{
		"x-donat-role": "stranger", // no permissions in fixture metadata
	}
	got, err := eng.executeQuery(ctx, "query { article { id } }", nil, sessionVars)
	if err != nil {
		t.Fatalf("executeQuery returned Go error (should be nil even for GQL errors): %v", err)
	}
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(got, &raw); err != nil {
		t.Fatalf("unmarshal: %v\nbody: %s", err, string(got))
	}
	if _, hasErrors := raw["errors"]; !hasErrors {
		t.Errorf("expected 'errors' key for unknown role; body: %s", string(got))
	}
	if _, hasData := raw["data"]; hasData {
		t.Errorf("unexpected 'data' key in error response; body: %s", string(got))
	}
	t.Logf("error response: %s", string(got))
}
