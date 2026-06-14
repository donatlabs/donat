package donat

import (
	"context"
	"encoding/json"
	"sync"
	"testing"

	"github.com/jackc/pgx/v5/pgxpool"
)

// fixtureMetaCatalogWithTrigger returns the fixture metadata+catalog JSON with
// an additional event trigger "on_author_change" (INSERT) on public.author.
// It mirrors the Rust metadata_with_author_trigger() in
// crates/wasm-core/tests/plan_snapshots.rs so that the compiled mutation plan
// carries the hook needed by TestEventHookFires.
func fixtureMetaCatalogWithTrigger() []byte {
	v := map[string]any{
		"metadata": map[string]any{
			"version": 3,
			"sources": []any{
				map[string]any{
					"name":          "default",
					"kind":          "postgres",
					"configuration": map[string]any{"connection_info": map[string]any{"database_url": "postgres://unused"}},
					"tables": []any{
						map[string]any{
							"table": map[string]any{"schema": "public", "name": "author"},
							"array_relationships": []any{
								map[string]any{
									"name": "articles",
									"using": map[string]any{
										"foreign_key_constraint_on": map[string]any{
											"table":  map[string]any{"schema": "public", "name": "article"},
											"column": "author_id",
										},
									},
								},
							},
							"insert_permissions": []any{
								map[string]any{"role": "user", "permission": map[string]any{"check": map[string]any{}, "columns": []any{"name"}}},
							},
							"select_permissions": []any{
								map[string]any{
									"role": "user",
									"permission": map[string]any{
										"columns": []any{"id", "name"},
										"filter":  map[string]any{"id": map[string]any{"_eq": "X-Donat-User-Id"}},
									},
								},
							},
							"update_permissions": []any{
								map[string]any{"role": "user", "permission": map[string]any{"columns": []any{"name"}, "filter": map[string]any{}}},
							},
							// The event trigger that drives TestEventHookFires.
							"event_triggers": []any{
								map[string]any{
									"name": "on_author_change",
									"definition": map[string]any{
										"insert": map[string]any{"columns": "*"},
									},
									"webhook": "http://unused",
								},
							},
						},
						map[string]any{
							"table": map[string]any{"schema": "public", "name": "article"},
							"object_relationships": []any{
								map[string]any{
									"name":  "author",
									"using": map[string]any{"foreign_key_constraint_on": "author_id"},
								},
							},
							"select_permissions": []any{
								map[string]any{
									"role": "user",
									"permission": map[string]any{
										"columns":            "*",
										"filter":             map[string]any{},
										"limit":              100,
										"allow_aggregations": true,
									},
								},
							},
						},
					},
				},
			},
			"inherited_roles": []any{},
		},
		"catalog": map[string]any{
			"tables": map[string]any{
				"public.author": map[string]any{
					"schema": "public",
					"name":   "author",
					"columns": []any{
						map[string]any{"name": "id", "pg_type": "int4", "nullable": false, "has_default": true},
						map[string]any{"name": "name", "pg_type": "text", "nullable": false, "has_default": false},
						map[string]any{"name": "secret", "pg_type": "text", "nullable": false, "has_default": true},
					},
					"primary_key":  []any{"id"},
					"foreign_keys": []any{},
				},
				"public.article": map[string]any{
					"schema": "public",
					"name":   "article",
					"columns": []any{
						map[string]any{"name": "id", "pg_type": "int4", "nullable": false, "has_default": true},
						map[string]any{"name": "title", "pg_type": "text", "nullable": false, "has_default": false},
						map[string]any{"name": "author_id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]any{"name": "published", "pg_type": "bool", "nullable": false, "has_default": true},
					},
					"primary_key": []any{"id"},
					"foreign_keys": []any{
						map[string]any{
							"constraint_name":   "article_author_id_fkey",
							"column_mapping":    map[string]any{"author_id": "id"},
							"referenced_schema": "public",
							"referenced_table":  "author",
						},
					},
				},
			},
			"functions": map[string]any{},
		},
	}
	b, err := json.Marshal(v)
	if err != nil {
		panic("fixtureMetaCatalogWithTrigger: " + err.Error())
	}
	return b
}

// setupMutationTables creates public.author and public.article with identity
// columns so that insert mutations that only supply "name" work without
// providing an explicit id. Tables are dropped in t.Cleanup.
//
// This is separate from testEngine (which uses an explicit-id schema) so that
// the mutation tests are self-contained and don't perturb the query tests.
func setupMutationTables(t *testing.T, pool *pgxpool.Pool) {
	t.Helper()
	ctx := context.Background()
	ddl := []string{
		`CREATE TABLE IF NOT EXISTS public.author (
			id      int4 PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
			name    text NOT NULL,
			secret  text NOT NULL DEFAULT ''
		)`,
		`CREATE TABLE IF NOT EXISTS public.article (
			id        int4 PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
			title     text NOT NULL,
			author_id int4 NOT NULL REFERENCES public.author(id),
			published bool NOT NULL DEFAULT false
		)`,
	}
	for _, q := range ddl {
		if _, err := pool.Exec(ctx, q); err != nil {
			t.Fatalf("setupMutationTables DDL: %v\nsql: %s", err, q)
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
}

// TestMutationExecute verifies that an insert mutation runs in a transaction
// and returns the correct {"data":{"insert_author":{"affected_rows":1}}} body.
// The inserted row is verified by a follow-up SELECT.
func TestMutationExecute(t *testing.T) {
	ctx := context.Background()
	pool := testPool(t)
	setupMutationTables(t, pool)

	eng, err := New(ctx, Config{
		Backend:  Postgres(pool),
		Metadata: fixtureMetaCatalogWithTrigger(),
	})
	if err != nil {
		t.Fatalf("New engine: %v", err)
	}

	sessionVars := map[string]string{
		"x-donat-role":    "user",
		"x-donat-user-id": "99",
	}
	mutation := `mutation { insert_author(objects:[{name:"Bob"}]) { affected_rows } }`

	got, err := eng.Execute(ctx, mutation, nil, sessionVars)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}

	// Parse response.
	var resp struct {
		Data struct {
			InsertAuthor struct {
				AffectedRows int `json:"affected_rows"`
			} `json:"insert_author"`
		} `json:"data"`
		Errors json.RawMessage `json:"errors"`
	}
	if err := json.Unmarshal(got, &resp); err != nil {
		t.Fatalf("unmarshal response: %v\nbody: %s", err, string(got))
	}
	if resp.Errors != nil {
		t.Fatalf("unexpected errors; body: %s", string(got))
	}
	if resp.Data.InsertAuthor.AffectedRows != 1 {
		t.Errorf("affected_rows: got %d, want 1; body: %s", resp.Data.InsertAuthor.AffectedRows, string(got))
	}

	// Confirm the row actually exists in Postgres.
	var name string
	if err := pool.QueryRow(ctx, `SELECT name FROM public.author WHERE name = 'Bob'`).Scan(&name); err != nil {
		t.Fatalf("SELECT after insert: %v (body: %s)", err, string(got))
	}
	if name != "Bob" {
		t.Errorf("inserted row name: got %q, want %q", name, "Bob")
	}
	t.Logf("mutation response: %s", string(got))
}

// TestExecuteTxSharedTransaction proves that ExecuteTx shares a real Postgres
// transaction with the caller: both the engine write and a direct caller write
// commit atomically on Commit, and both are absent on Rollback.
func TestExecuteTxSharedTransaction(t *testing.T) {
	ctx := context.Background()
	pool := testPool(t)
	setupMutationTables(t, pool)

	eng, err := New(ctx, Config{
		Backend:  Postgres(pool),
		Metadata: fixtureMetaCatalogWithTrigger(),
	})
	if err != nil {
		t.Fatalf("New engine: %v", err)
	}

	sessionVars := map[string]string{
		"x-donat-role":    "user",
		"x-donat-user-id": "99",
	}
	mutation := `mutation { insert_author(objects:[{name:"TxCommit"}]) { affected_rows } }`

	t.Run("commit_both_rows_present", func(t *testing.T) {
		tx, err := pool.Begin(ctx)
		if err != nil {
			t.Fatalf("Begin: %v", err)
		}

		// Engine insert (via ExecuteTx).
		body, err := eng.ExecuteTx(ctx, tx, mutation, nil, sessionVars)
		if err != nil {
			_ = tx.Rollback(ctx)
			t.Fatalf("ExecuteTx: %v", err)
		}
		// Verify no GQL errors.
		var rr map[string]json.RawMessage
		if err := json.Unmarshal(body, &rr); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatalf("unmarshal: %v\nbody: %s", err, string(body))
		}
		if _, hasErr := rr["errors"]; hasErr {
			_ = tx.Rollback(ctx)
			t.Fatalf("unexpected GQL errors; body: %s", string(body))
		}

		// Caller's own insert (within the same transaction).
		if _, err := tx.Exec(ctx, `INSERT INTO public.author (name) VALUES ('TxDirect')`); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatalf("Exec direct insert: %v", err)
		}

		if err := tx.Commit(ctx); err != nil {
			t.Fatalf("Commit: %v", err)
		}

		// Both rows must be present after commit.
		var n int
		if err := pool.QueryRow(ctx, `SELECT count(*) FROM public.author WHERE name IN ('TxCommit','TxDirect')`).Scan(&n); err != nil {
			t.Fatalf("SELECT count: %v", err)
		}
		if n != 2 {
			t.Errorf("after commit: expected 2 rows, got %d", n)
		}
	})

	t.Run("rollback_neither_row_present", func(t *testing.T) {
		tx, err := pool.Begin(ctx)
		if err != nil {
			t.Fatalf("Begin: %v", err)
		}

		mutation2 := `mutation { insert_author(objects:[{name:"TxRollCommit"}]) { affected_rows } }`
		if _, err := eng.ExecuteTx(ctx, tx, mutation2, nil, sessionVars); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatalf("ExecuteTx: %v", err)
		}

		if _, err := tx.Exec(ctx, `INSERT INTO public.author (name) VALUES ('TxRollDirect')`); err != nil {
			_ = tx.Rollback(ctx)
			t.Fatalf("Exec direct insert: %v", err)
		}

		if err := tx.Rollback(ctx); err != nil {
			t.Fatalf("Rollback: %v", err)
		}

		// Neither row must exist after rollback.
		var n int
		if err := pool.QueryRow(ctx, `SELECT count(*) FROM public.author WHERE name IN ('TxRollCommit','TxRollDirect')`).Scan(&n); err != nil {
			t.Fatalf("SELECT count: %v", err)
		}
		if n != 0 {
			t.Errorf("after rollback: expected 0 rows, got %d", n)
		}
	})
}

// TestEventHookFires registers a typed handler for "on_author_change" in a
// Registry, builds an Engine with the trigger-carrying metadata, runs an
// insert mutation, and asserts the handler received an Event with the correct
// Op/Table/Trigger fields.
func TestEventHookFires(t *testing.T) {
	ctx := context.Background()
	pool := testPool(t)
	setupMutationTables(t, pool)

	// Register the handler.
	reg := NewRegistry()
	var (
		mu      sync.Mutex
		gotOp   string
		gotTbl  string
		gotTrig string
		fired   bool
	)
	On(reg, "on_author_change", func(ctx context.Context, e Event[json.RawMessage]) error {
		mu.Lock()
		defer mu.Unlock()
		gotOp = string(e.Op)
		gotTbl = e.Table.Name
		gotTrig = e.Trigger.Name
		fired = true
		return nil
	})

	eng, err := New(ctx, Config{
		Backend:  Postgres(pool),
		Metadata: fixtureMetaCatalogWithTrigger(),
		Registry: reg,
	})
	if err != nil {
		t.Fatalf("New engine with trigger metadata: %v", err)
	}

	sessionVars := map[string]string{
		"x-donat-role":    "user",
		"x-donat-user-id": "99",
	}
	mutation := `mutation { insert_author(objects:[{name:"HookAuthor"}]) { affected_rows } }`

	body, err := eng.Execute(ctx, mutation, nil, sessionVars)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	// Confirm no GQL errors.
	var rr map[string]json.RawMessage
	if err := json.Unmarshal(body, &rr); err != nil {
		t.Fatalf("unmarshal body: %v\n%s", err, string(body))
	}
	if _, hasErr := rr["errors"]; hasErr {
		t.Fatalf("unexpected GQL errors; body: %s", string(body))
	}

	// Verify the handler fired with correct fields.
	mu.Lock()
	defer mu.Unlock()
	if !fired {
		t.Fatal("event hook handler was not invoked")
	}
	if gotOp != "INSERT" {
		t.Errorf("hook Op: got %q, want %q", gotOp, "INSERT")
	}
	if gotTbl != "author" {
		t.Errorf("hook Table.Name: got %q, want %q", gotTbl, "author")
	}
	if gotTrig != "on_author_change" {
		t.Errorf("hook Trigger.Name: got %q, want %q", gotTrig, "on_author_change")
	}
	t.Logf("hook: op=%s table=%s trigger=%s", gotOp, gotTbl, gotTrig)
}
