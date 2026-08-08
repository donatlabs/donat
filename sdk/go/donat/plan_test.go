package donat

import (
	"encoding/json"
	"testing"
)

func TestDecodePlan(t *testing.T) {
	cases := []struct {
		name        string
		raw         string
		wantKind    PlanKind
		wantTxn     bool
		wantAlias   string
		wantSQL     string
		wantHooks   int
		wantErrMap  string // a key that should be in error_map
		wantErrCode string // non-empty when Kind==PlanErrorK
		wantErrMsg  string
	}{
		{
			name: "query plan",
			raw: `{
				"kind": "query",
				"version": 1,
				"transaction": false,
				"statements": [{
					"alias": "data",
					"sql": "SELECT json_build_object('article', (SELECT coalesce(json_agg(\"_t1\".j), '[]'::json) FROM (SELECT json_build_object('id', \"_t0\".\"id\", 'title', \"_t0\".\"title\") AS j FROM \"public\".\"article\" AS \"_t0\" LIMIT 100) AS \"_t1\")) AS root",
					"params": []
				}],
				"hooks": [],
				"error_map": {
					"23502": "constraint-violation:Not-NULL violation. ",
					"23503": "constraint-violation:Foreign key violation. ",
					"23505": "constraint-violation:Uniqueness violation. ",
					"23514": "permission-error-from-payload",
					"default": "data-exception"
				}
			}`,
			wantKind:   PlanQuery,
			wantTxn:    false,
			wantAlias:  "data",
			wantSQL:    "SELECT json_build_object",
			wantHooks:  0,
			wantErrMap: "23505",
		},
		{
			name: "mutation plan with hook and error_map",
			raw: `{
				"kind": "mutation",
				"version": 1,
				"transaction": true,
				"statements": [{
					"alias": "insert_author",
					"sql": "WITH \"ins\" AS (INSERT INTO \"public\".\"author\" (\"name\") VALUES (('Bob')::\"text\") RETURNING *) SELECT json_build_object('affected_rows', (SELECT count(*) FROM \"ins\")) AS root",
					"params": []
				}],
				"hooks": [{
					"phase": "post_commit",
					"trigger": "on_author_change",
					"schema": "public",
					"table": "author",
					"op": "INSERT"
				}],
				"error_map": {
					"23502": "constraint-violation:Not-NULL violation. ",
					"23503": "constraint-violation:Foreign key violation. ",
					"23505": "constraint-violation:Uniqueness violation. ",
					"23514": "permission-error-from-payload",
					"default": "data-exception"
				}
			}`,
			wantKind:   PlanMutation,
			wantTxn:    true,
			wantAlias:  "insert_author",
			wantSQL:    "WITH \"ins\"",
			wantHooks:  1,
			wantErrMap: "default",
		},
		{
			name: "error plan",
			raw: `{
				"kind": "error",
				"version": 1,
				"code": "validation-failed",
				"path": "$.selectionSet.article",
				"message": "field 'article' not found in type: 'query_root'"
			}`,
			wantKind:    PlanErrorK,
			wantErrCode: "validation-failed",
			wantErrMsg:  "field 'article' not found in type: 'query_root'",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			p, err := decodePlan([]byte(tc.raw))
			if err != nil {
				t.Fatalf("decodePlan error: %v", err)
			}
			if p.Kind != tc.wantKind {
				t.Errorf("Kind: got %q, want %q", p.Kind, tc.wantKind)
			}
			if tc.wantKind != PlanErrorK {
				if p.Transaction != tc.wantTxn {
					t.Errorf("Transaction: got %v, want %v", p.Transaction, tc.wantTxn)
				}
				if len(p.Statements) == 0 {
					t.Fatal("expected at least one statement")
				}
				if p.Statements[0].Alias != tc.wantAlias {
					t.Errorf("Alias: got %q, want %q", p.Statements[0].Alias, tc.wantAlias)
				}
				if len(p.Statements[0].SQL) < len(tc.wantSQL) ||
					p.Statements[0].SQL[:len(tc.wantSQL)] != tc.wantSQL {
					t.Errorf("SQL prefix: got %q, want prefix %q", p.Statements[0].SQL, tc.wantSQL)
				}
				if len(p.Hooks) != tc.wantHooks {
					t.Errorf("Hooks count: got %d, want %d", len(p.Hooks), tc.wantHooks)
				}
				if tc.wantHooks > 0 {
					h := p.Hooks[0]
					if h.Phase != "post_commit" {
						t.Errorf("Hook.Phase: got %q, want %q", h.Phase, "post_commit")
					}
					if h.Trigger != "on_author_change" {
						t.Errorf("Hook.Trigger: got %q, want %q", h.Trigger, "on_author_change")
					}
					if h.Schema != "public" {
						t.Errorf("Hook.Schema: got %q, want %q", h.Schema, "public")
					}
					if h.Table != "author" {
						t.Errorf("Hook.Table: got %q, want %q", h.Table, "author")
					}
					if h.Op != "INSERT" {
						t.Errorf("Hook.Op: got %q, want %q", h.Op, "INSERT")
					}
				}
				if tc.wantErrMap != "" {
					if _, ok := p.ErrorMap[tc.wantErrMap]; !ok {
						t.Errorf("ErrorMap missing key %q; got %v", tc.wantErrMap, p.ErrorMap)
					}
				}
			} else {
				if p.Err == nil {
					t.Fatal("expected Err to be set for error plan")
				}
				if p.Err.Code != tc.wantErrCode {
					t.Errorf("Err.Code: got %q, want %q", p.Err.Code, tc.wantErrCode)
				}
				if p.Err.Message != tc.wantErrMsg {
					t.Errorf("Err.Message: got %q, want %q", p.Err.Message, tc.wantErrMsg)
				}
			}
		})
	}
}

// TestDecodePlanVersionGuard ensures a non-error plan with a wrong version is rejected.
func TestDecodePlanVersionGuard(t *testing.T) {
	raw := `{"kind":"query","version":99,"transaction":false,"statements":[],"hooks":[],"error_map":{}}`
	_, err := decodePlan([]byte(raw))
	if err == nil {
		t.Fatal("expected error for version mismatch, got nil")
	}
}

// TestDecodePlanParamsField ensures the params field round-trips as raw JSON.
func TestDecodePlanParamsField(t *testing.T) {
	raw := `{
		"kind": "query",
		"version": 1,
		"transaction": false,
		"statements": [{
			"alias": "data",
			"sql": "SELECT 1",
			"params": [1, "two", null]
		}],
		"hooks": [],
		"error_map": {}
	}`
	p, err := decodePlan([]byte(raw))
	if err != nil {
		t.Fatalf("decodePlan: %v", err)
	}
	if len(p.Statements[0].Params) != 3 {
		t.Errorf("params count: got %d, want 3", len(p.Statements[0].Params))
	}
	// The second param should round-trip as "two".
	var s string
	if err := json.Unmarshal(p.Statements[0].Params[1], &s); err != nil || s != "two" {
		t.Errorf("param[1]: got %s, want \"two\"", string(p.Statements[0].Params[1]))
	}
}
