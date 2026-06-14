package donat

import (
	"context"
	"database/sql"
	"encoding/json"
	"strings"
	"testing"

	// Register the modernc pure-Go SQLite driver (CGO_ENABLED=0 compatible).
	// The driver name is "sqlite".
	_ "modernc.org/sqlite"
)

// openSQLiteMemory opens an in-memory SQLite database with a single connection.
//
// Connection pool note: SQLite in-memory databases are per-connection. With
// database/sql's default pooling, multiple connections would each see a
// different (empty) in-memory database. We pin the pool to ONE connection so
// that ATTACH, CREATE TABLE and INSERT are all visible to subsequent queries
// on the same connection.
func openSQLiteMemory(t *testing.T) *sql.DB {
	t.Helper()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("sql.Open sqlite: %v", err)
	}
	// Pin to one connection — in-memory db is connection-local.
	db.SetMaxOpenConns(1)
	t.Cleanup(func() { _ = db.Close() })
	return db
}

// seedSQLiteFixture attaches a named "public" schema and seeds the
// article/author tables to match the fixture catalog in engine_test.go.
//
// Schema qualification: the wasm core renders `FROM "public"."article"`.
// SQLite resolves `"public"."x"` to an ATTACHED database named `public`.
// We ATTACH a second in-memory database (a fresh :memory: file per attached
// name) as the schema named "public" so that the generated SQL resolves
// correctly. SetMaxOpenConns(1) above guarantees the ATTACH persists.
//
//	public.author  (id INTEGER PK, name TEXT, secret TEXT)
//	public.article (id INTEGER PK, title TEXT, author_id INTEGER, published INTEGER)
//
// Seeded rows:
//
//	author:  (1, 'Alice', 'secret1')
//	article: (1, 'First Article', 1, 1)
func seedSQLiteFixture(t *testing.T, db *sql.DB) {
	t.Helper()
	stmts := []string{
		// Attach a fresh in-memory database as the "public" schema.
		// SQLite treats ATTACH ':memory:' as a separate new in-memory file
		// (not the same one as the main db) — that is fine; we only need the
		// tables to live in the "public" schema namespace.
		`ATTACH DATABASE ':memory:' AS "public"`,

		// DDL for public.author
		`CREATE TABLE "public"."author" (
			id      INTEGER PRIMARY KEY,
			name    TEXT NOT NULL,
			secret  TEXT NOT NULL
		)`,

		// DDL for public.article — SQLite does not allow qualified table names
		// ("schema"."table") inside REFERENCES clauses. We omit the FK constraint
		// here; it is not needed for the query proof (no FK enforcement is tested).
		`CREATE TABLE "public"."article" (
			id        INTEGER PRIMARY KEY,
			title     TEXT NOT NULL,
			author_id INTEGER NOT NULL,
			published INTEGER NOT NULL
		)`,

		// Seed one author and one article — deterministic result ordering.
		`INSERT INTO "public"."author"  (id, name, secret)   VALUES (1, 'Alice', 'secret1')`,
		`INSERT INTO "public"."article" (id, title, author_id, published) VALUES (1, 'First Article', 1, 1)`,
	}
	for _, s := range stmts {
		if _, err := db.Exec(s); err != nil {
			t.Fatalf("seedSQLiteFixture: %v\nsql: %s", err, s)
		}
	}
}

// TestSQLiteQuery proves that a second database (SQLite) can serve a GraphQL
// query through the same Engine using only a new Backend — without any change
// to the engine, plan compiler, or executor logic.
//
// It opens an in-memory SQLite database, seeds the article/author fixture,
// builds an Engine with donat.SQL(db, "sqlite"), and runs
// "{ article { id title } }" as role "user". The engine compiles the plan with
// dialect="sqlite" (wasm core renders SQLite json1 SQL), RunQuery scans the
// single text-json column, and Execute wraps it in {"data":...}.
//
// The test asserts:
//  1. The response is valid JSON with a "data" key (no "errors").
//  2. data.article is a non-empty array.
//  3. The seeded row (id:1, title:"First Article") is present.
func TestSQLiteQuery(t *testing.T) {
	ctx := context.Background()

	db := openSQLiteMemory(t)
	seedSQLiteFixture(t, db)

	eng, err := New(ctx, Config{
		Backend:  SQL(db, "sqlite"),
		Metadata: fixtureMetaCatalog(),
	})
	if err != nil {
		t.Fatalf("donat.New (sqlite): %v", err)
	}

	sessionVars := map[string]string{
		"x-donat-role":    "user",
		"x-donat-user-id": "1",
	}

	got, err := eng.Execute(ctx, "{ article { id title } }", nil, sessionVars)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}

	t.Logf("SQLite query response: %s", string(got))

	// Verify the outer envelope shape.
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(got, &raw); err != nil {
		t.Fatalf("unmarshal response: %v\nbody: %s", err, string(got))
	}
	if _, hasErrors := raw["errors"]; hasErrors {
		t.Errorf("unexpected 'errors' key; body: %s", string(got))
	}
	if _, hasData := raw["data"]; !hasData {
		t.Fatalf("missing 'data' key; body: %s", string(got))
	}

	// Decode the article array.
	var envelope struct {
		Data struct {
			Article []struct {
				ID    int    `json:"id"`
				Title string `json:"title"`
			} `json:"article"`
		} `json:"data"`
	}
	if err := json.Unmarshal(got, &envelope); err != nil {
		t.Fatalf("unmarshal article envelope: %v\nbody: %s", err, string(got))
	}
	if len(envelope.Data.Article) == 0 {
		t.Fatalf("expected at least one article; body: %s", string(got))
	}
	a := envelope.Data.Article[0]
	if a.ID != 1 {
		t.Errorf("article.id: got %d, want 1", a.ID)
	}
	if a.Title != "First Article" {
		t.Errorf("article.title: got %q, want %q", a.Title, "First Article")
	}
}

// TestSQLiteMutationDeferred asserts that an insert mutation against the SQLite
// backend returns the documented "not yet supported" error, so that the deferral
// is explicit (tested) rather than silent.
func TestSQLiteMutationDeferred(t *testing.T) {
	ctx := context.Background()

	db := openSQLiteMemory(t)
	seedSQLiteFixture(t, db)

	eng, err := New(ctx, Config{
		Backend:  SQL(db, "sqlite"),
		Metadata: fixtureMetaCatalog(),
	})
	if err != nil {
		t.Fatalf("donat.New (sqlite): %v", err)
	}

	sessionVars := map[string]string{
		"x-donat-role":    "user",
		"x-donat-user-id": "1",
	}

	// An insert mutation on "author" (the "user" role has insert permission in
	// the fixture metadata). The plan kind will be PlanMutation, so Execute will
	// call RunMutation — which for "sqlite" returns the deferred error.
	got, err := eng.Execute(
		ctx,
		`mutation { insert_author(objects: [{ name: "Bob" }]) { affected_rows } }`,
		nil,
		sessionVars,
	)
	if err != nil {
		t.Fatalf("Execute returned Go error (should be nil even for backend errors): %v", err)
	}

	t.Logf("SQLite deferred-mutation response: %s", string(got))

	// The response must be an error body (no "data" key).
	var raw map[string]json.RawMessage
	if err := json.Unmarshal(got, &raw); err != nil {
		t.Fatalf("unmarshal response: %v\nbody: %s", err, string(got))
	}
	if _, hasData := raw["data"]; hasData {
		t.Errorf("unexpected 'data' key in deferred-mutation response; body: %s", string(got))
	}
	if _, hasErrors := raw["errors"]; !hasErrors {
		t.Errorf("expected 'errors' key; body: %s", string(got))
	}

	// Verify the deferred-mutation message is present in the body.
	body := string(got)
	if !strings.Contains(body, "SQLite mutations are not yet supported") {
		t.Errorf("expected deferred-mutation message in body; got: %s", body)
	}
}
