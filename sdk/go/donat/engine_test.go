package donat

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"
)

// poolBackend is enough Backend for the pool tests: they never reach the
// database, only the instance pool in front of it.
type poolBackend struct{}

func (poolBackend) Dialect() string { return "postgres" }

func (poolBackend) RunQuery(context.Context, Plan) (json.RawMessage, error) {
	return json.RawMessage("null"), nil
}

func (poolBackend) RunMutation(context.Context, Plan) (map[string]json.RawMessage, error) {
	return map[string]json.RawMessage{}, nil
}

func (poolBackend) ReadUpload(context.Context, string) (UploadRow, error) {
	return UploadRow{}, nil
}

func (poolBackend) FinalizeUpload(context.Context, string, string, int64) error {
	return nil
}

func (poolBackend) MapError(error, map[string]string) []byte {
	return errorBody("unexpected", "$", "poolBackend has no database")
}

// TestEngineRequiresBackend confirms New returns an error when Config.Backend is nil.
func TestEngineRequiresBackend(t *testing.T) {
	ctx := context.Background()
	_, err := New(ctx, Config{Backend: nil, Metadata: []byte("{}")})
	if err == nil {
		t.Fatal("expected error for nil Backend, got nil")
	}
}

// fixtureMetaCatalog returns the JSON payload for core_init that mirrors the
// article/author fixture used in crates/wasm-core/tests/plan_snapshots.rs.
// The Rust `CoreState` serde shape is {"metadata":<Metadata>,"catalog":<Catalog>}.
// Metadata v3 shape mirrors donat_metadata::Metadata; Catalog shape mirrors
// donat_catalog_types::Catalog (BTreeMap keyed by "schema.table").
func fixtureMetaCatalog() []byte {
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
						map[string]any{"name": "id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]any{"name": "name", "pg_type": "text", "nullable": false, "has_default": false},
						map[string]any{"name": "secret", "pg_type": "text", "nullable": false, "has_default": false},
					},
					"relation_kind": "Table",
					"primary_key":   []any{"id"},
					"foreign_keys":  []any{},
				},
				"public.article": map[string]any{
					"schema": "public",
					"name":   "article",
					"columns": []any{
						map[string]any{"name": "id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]any{"name": "title", "pg_type": "text", "nullable": false, "has_default": false},
						map[string]any{"name": "author_id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]any{"name": "published", "pg_type": "bool", "nullable": false, "has_default": false},
					},
					"relation_kind": "Table",
					"primary_key":   []any{"id"},
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
		panic("fixtureMetaCatalog: " + err.Error())
	}
	return b
}

// TestEngineCompilesQuery performs a lower-level boundary test: it creates a
// raw wasmCore instance, seeds it with the article/author fixture, and calls
// compile() for a query against "article" as role "user". This proves the Go
// host can drive the wasm core to a real Plan without a database.
//
// We also verify the compile→decodePlan pipeline for a bad query returns a
// well-formed PlanErrorK plan (proving the error path through the boundary).
func TestEngineCompilesQuery(t *testing.T) {
	ctx := context.Background()

	cfg := fixtureMetaCatalog()

	c, err := newWasmCore(ctx)
	if err != nil {
		t.Fatalf("newWasmCore: %v", err)
	}
	defer c.close(ctx)

	if err := c.initState(ctx, cfg); err != nil {
		t.Fatalf("initState: %v", err)
	}

	// Happy path: a valid query that the "user" role can see.
	input := compileInput{
		Query: "query { article { id title } }",
		SessionVars: map[string]string{
			"x-donat-role":    "user",
			"x-donat-user-id": "7",
		},
	}
	inJSON, err := json.Marshal(input)
	if err != nil {
		t.Fatalf("marshal input: %v", err)
	}
	raw, err := c.compile(ctx, inJSON)
	if err != nil {
		t.Fatalf("compile: %v", err)
	}
	p, err := decodePlan(raw)
	if err != nil {
		t.Fatalf("decodePlan: %v", err)
	}
	if p.Kind != PlanQuery {
		if p.Err != nil {
			t.Errorf("Kind: got %q (err code=%q msg=%q), want %q", p.Kind, p.Err.Code, p.Err.Message, PlanQuery)
		} else {
			t.Errorf("Kind: got %q, want %q", p.Kind, PlanQuery)
		}
	}
	if len(p.Statements) == 0 {
		t.Fatal("expected at least one statement")
	}
	if p.Statements[0].SQL == "" {
		t.Error("SQL must be non-empty")
	}
	// The SQL should reference the article table.
	if !strings.Contains(p.Statements[0].SQL, "article") {
		t.Errorf("SQL does not reference 'article': %s", p.Statements[0].SQL)
	}

	// Error path: a query for a role with no permissions yields PlanErrorK.
	badInput := compileInput{
		Query:       "{ article { id } }",
		SessionVars: map[string]string{"x-donat-role": "stranger"},
	}
	badJSON, _ := json.Marshal(badInput)
	rawErr, err := c.compile(ctx, badJSON)
	if err != nil {
		t.Fatalf("compile (error path): %v", err)
	}
	pe, err := decodePlan(rawErr)
	if err != nil {
		t.Fatalf("decodePlan (error path): %v", err)
	}
	if pe.Kind != PlanErrorK {
		t.Errorf("error path Kind: got %q, want %q", pe.Kind, PlanErrorK)
	}
	if pe.Err == nil || pe.Err.Code == "" {
		t.Error("error path: expected non-empty Err.Code")
	}
}

// TestCompileCacheKey verifies that identical inputs hash to the same key
// and differing roles hash to different keys.
func TestCompileCacheKey(t *testing.T) {
	vars1 := map[string]json.RawMessage{"id": json.RawMessage(`1`)}
	vars2 := map[string]json.RawMessage{"id": json.RawMessage(`1`)}
	sess1 := map[string]string{"x-donat-role": "user", "x-donat-user-id": "7"}
	sess2 := map[string]string{"x-donat-role": "user", "x-donat-user-id": "7"}
	sess3 := map[string]string{"x-donat-role": "admin"}

	key1 := planCacheKey{
		query:    "query { article { id } }",
		role:     sess1["x-donat-role"],
		varsHash: hashJSON(vars1),
		sessHash: hashMap(sess1),
	}
	key2 := planCacheKey{
		query:    "query { article { id } }",
		role:     sess2["x-donat-role"],
		varsHash: hashJSON(vars2),
		sessHash: hashMap(sess2),
	}
	key3 := planCacheKey{
		query:    "query { article { id } }",
		role:     sess3["x-donat-role"],
		varsHash: hashJSON(vars1),
		sessHash: hashMap(sess3),
	}

	if key1 != key2 {
		t.Errorf("identical inputs must produce the same cache key: %+v != %+v", key1, key2)
	}
	if key1 == key3 {
		t.Errorf("different roles must produce different cache keys: %+v == %+v", key1, key3)
	}
}

// A cancelled caller must not take a live one's turn with it.
//
// `release` wakes the last waiter it finds. A caller that gave up but left its
// channel in the queue absorbs that wake, and the caller still waiting blocks
// forever while the instance it was promised sits idle in the pool. With
// PoolSize 1 that is one abandoned request against one real one.
func TestACancelledAcquireDoesNotStrandALiveOne(t *testing.T) {
	ctx := context.Background()
	eng, err := New(ctx, Config{
		Backend:  poolBackend{},
		Metadata: fixtureMetaCatalog(),
		PoolSize: 1,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	// Hold the only instance, so every other caller must wait.
	held, err := eng.acquire(ctx)
	if err != nil {
		t.Fatalf("acquire: %v", err)
	}

	// The caller that will not give up is queued FIRST, because `release`
	// wakes the last waiter it finds: the one that gives up has to be the one
	// holding that position, or nothing is being tested.
	live := make(chan error, 1)
	go func() {
		c, err := eng.acquire(ctx)
		if err == nil {
			eng.release(c)
		}
		live <- err
	}()
	waitUntilWaiters(t, eng, 1)

	giveUp, cancel := context.WithCancel(ctx)
	abandoned := make(chan error, 1)
	go func() { _, err := eng.acquire(giveUp); abandoned <- err }()
	waitUntilWaiters(t, eng, 2)

	cancel()
	if err := <-abandoned; err == nil {
		t.Fatal("the abandoned caller should have reported its cancellation")
	}

	eng.release(held)

	select {
	case err := <-live:
		if err != nil {
			t.Fatalf("the live caller failed: %v", err)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the live caller never got the instance: the cancelled one took its wake")
	}
}

// waitUntilWaiters blocks until exactly n callers are parked, so the test does
// not race the goroutines it just started.
func waitUntilWaiters(t *testing.T, eng *Engine, n int) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		eng.mu.Lock()
		got := len(eng.waiters)
		eng.mu.Unlock()
		if got == n {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("waited for %d parked callers, saw %d", n, got)
		}
		time.Sleep(time.Millisecond)
	}
}
