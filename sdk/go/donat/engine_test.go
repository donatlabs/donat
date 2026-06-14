package donat

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

// TestEngineRequiresPool confirms New returns an error when Config.Pool is nil.
func TestEngineRequiresPool(t *testing.T) {
	ctx := context.Background()
	_, err := New(ctx, Config{Pool: nil, Metadata: []byte("{}")})
	if err == nil {
		t.Fatal("expected error for nil Pool, got nil")
	}
}

// fixtureMetaCatalog returns the JSON payload for core_init that mirrors the
// article/author fixture used in crates/wasm-core/tests/plan_snapshots.rs.
// The Rust `CoreState` serde shape is {"metadata":<Metadata>,"catalog":<Catalog>}.
// Metadata v3 shape mirrors donat_metadata::Metadata; Catalog shape mirrors
// donat_catalog_types::Catalog (BTreeMap keyed by "schema.table").
func fixtureMetaCatalog() []byte {
	v := map[string]interface{}{
		"metadata": map[string]interface{}{
			"version": 3,
			"sources": []interface{}{
				map[string]interface{}{
					"name":          "default",
					"kind":          "postgres",
					"configuration": map[string]interface{}{"connection_info": map[string]interface{}{"database_url": "postgres://unused"}},
					"tables": []interface{}{
						map[string]interface{}{
							"table": map[string]interface{}{"schema": "public", "name": "author"},
							"array_relationships": []interface{}{
								map[string]interface{}{
									"name": "articles",
									"using": map[string]interface{}{
										"foreign_key_constraint_on": map[string]interface{}{
											"table":  map[string]interface{}{"schema": "public", "name": "article"},
											"column": "author_id",
										},
									},
								},
							},
							"insert_permissions": []interface{}{
								map[string]interface{}{"role": "user", "permission": map[string]interface{}{"check": map[string]interface{}{}, "columns": []interface{}{"name"}}},
							},
							"select_permissions": []interface{}{
								map[string]interface{}{
									"role": "user",
									"permission": map[string]interface{}{
										"columns": []interface{}{"id", "name"},
										"filter":  map[string]interface{}{"id": map[string]interface{}{"_eq": "X-Donat-User-Id"}},
									},
								},
							},
							"update_permissions": []interface{}{
								map[string]interface{}{"role": "user", "permission": map[string]interface{}{"columns": []interface{}{"name"}, "filter": map[string]interface{}{}}},
							},
						},
						map[string]interface{}{
							"table": map[string]interface{}{"schema": "public", "name": "article"},
							"object_relationships": []interface{}{
								map[string]interface{}{
									"name":  "author",
									"using": map[string]interface{}{"foreign_key_constraint_on": "author_id"},
								},
							},
							"select_permissions": []interface{}{
								map[string]interface{}{
									"role": "user",
									"permission": map[string]interface{}{
										"columns":            "*",
										"filter":             map[string]interface{}{},
										"limit":              100,
										"allow_aggregations": true,
									},
								},
							},
						},
					},
				},
			},
			"inherited_roles": []interface{}{},
		},
		"catalog": map[string]interface{}{
			"tables": map[string]interface{}{
				"public.author": map[string]interface{}{
					"schema": "public",
					"name":   "author",
					"columns": []interface{}{
						map[string]interface{}{"name": "id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]interface{}{"name": "name", "pg_type": "text", "nullable": false, "has_default": false},
						map[string]interface{}{"name": "secret", "pg_type": "text", "nullable": false, "has_default": false},
					},
					"primary_key":  []interface{}{"id"},
					"foreign_keys": []interface{}{},
				},
				"public.article": map[string]interface{}{
					"schema": "public",
					"name":   "article",
					"columns": []interface{}{
						map[string]interface{}{"name": "id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]interface{}{"name": "title", "pg_type": "text", "nullable": false, "has_default": false},
						map[string]interface{}{"name": "author_id", "pg_type": "int4", "nullable": false, "has_default": false},
						map[string]interface{}{"name": "published", "pg_type": "bool", "nullable": false, "has_default": false},
					},
					"primary_key": []interface{}{"id"},
					"foreign_keys": []interface{}{
						map[string]interface{}{
							"constraint_name":   "article_author_id_fkey",
							"column_mapping":    map[string]interface{}{"author_id": "id"},
							"referenced_schema": "public",
							"referenced_table":  "author",
						},
					},
				},
			},
			"functions": map[string]interface{}{},
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
