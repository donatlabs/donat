package donat

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
	"time"
)

// fixtureWithAttachment declares the author's `secret` column a file, plus the
// S3 backend and signing secret a deployment needs to sign URLs.
func fixtureWithAttachment(t *testing.T) []byte {
	t.Helper()
	var cfg map[string]any
	if err := json.Unmarshal(fixtureMetaCatalog(), &cfg); err != nil {
		t.Fatalf("fixture: %v", err)
	}
	md := cfg["metadata"].(map[string]any)
	table := md["sources"].([]any)[0].(map[string]any)["tables"].([]any)[0].(map[string]any)
	table["attachments"] = []any{map[string]any{
		"column": "secret", "backend": "files", "max_bytes": 1024,
	}}
	perm := table["select_permissions"].([]any)[0].(map[string]any)["permission"].(map[string]any)
	perm["columns"] = []any{"id", "name", "secret"}
	md["storage"] = map[string]any{
		"backends": []any{map[string]any{
			"name": "files", "kind": "s3", "bucket": "donat-test",
			"region": "eu-central-1", "endpoint": "http://127.0.0.1:19000",
			"path_style":        true,
			"access_key_id":     map[string]any{"value_from_env": "TEST_STORAGE_KEY"},
			"secret_access_key": map[string]any{"value_from_env": "TEST_STORAGE_SECRET"},
		}},
		"signing": map[string]any{
			"secret": map[string]any{"value_from_env": "TEST_STORAGE_SECRET"},
		},
	}
	out, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("fixture marshal: %v", err)
	}
	return out
}

func storageSecrets() map[string]string {
	return map[string]string{
		"TEST_STORAGE_KEY":    "key",
		"TEST_STORAGE_SECRET": "s3cr3t",
	}
}

// The secrets reach the core from the host, not from the committed snapshot: a
// signing key that travels inside the artifact is a key everyone holding the
// artifact has.
func TestSecretsFromTheHostLetTheCoreSignURLs(t *testing.T) {
	ctx := context.Background()
	eng, err := New(ctx, Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithAttachment(t),
		Secrets:  storageSecrets(),
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	plan, err := eng.compilePlan(ctx, compileInput{
		Query:       "{ author { id secret { url } } }",
		SessionVars: userSession(),
	})
	if err != nil {
		t.Fatalf("compile: %v", err)
	}
	if plan.Kind == PlanErrorK {
		t.Fatalf("planning a file column failed: %+v", plan.Err)
	}
	if !strings.Contains(plan.Statements[0].SQL, "s3_presigned_url") {
		t.Fatalf("the URL must be signed in SQL: %s", plan.Statements[0].SQL)
	}
}

// A deployment declaring an attachment whose secret nobody supplied must fail
// at New, not serve a file column that can never produce a URL.
func TestAMissingStorageSecretFailsAtStartup(t *testing.T) {
	_, err := New(context.Background(), Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithAttachment(t),
	})
	if err == nil {
		t.Fatal("expected New to refuse a deployment whose storage secret is missing")
	}
	if !strings.Contains(err.Error(), "storage") {
		t.Fatalf("the failure must name the storage configuration: %v", err)
	}
}

// The bug this guards: a plan that signs a URL carries a one-shot upload id and
// a day-scoped key. Reusing it would hand two callers the same staging object,
// and would keep serving yesterday's key after the day rolls over.
func TestAPlanThatSignsURLsIsNeverCached(t *testing.T) {
	ctx := context.Background()
	day := 0
	eng, err := New(ctx, Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithAttachment(t),
		Secrets:  storageSecrets(),
		Now: func() time.Time {
			// Each call is a day later, so a cached plan would keep the first
			// day's signature and this test would see two identical statements.
			day++
			return time.Date(2026, 8, day, 12, 0, 0, 0, time.UTC)
		},
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	const query = "{ author { id secret { url } } }"
	first, err := eng.compilePlan(ctx, compileInput{Query: query, SessionVars: userSession()})
	if err != nil {
		t.Fatalf("compile: %v", err)
	}
	second, err := eng.compilePlan(ctx, compileInput{Query: query, SessionVars: userSession()})
	if err != nil {
		t.Fatalf("compile: %v", err)
	}
	if first.Statements[0].SQL == second.Statements[0].SQL {
		t.Fatal("a plan carrying a signature was reused across requests")
	}
}

// The common case must be unaffected: with no attachments there is nothing
// request-specific in a plan, so the cache keeps working.
func TestAPlanWithoutSignaturesIsStillCached(t *testing.T) {
	ctx := context.Background()
	eng, err := New(ctx, Config{Backend: Postgres(nil), Metadata: fixtureMetaCatalog()})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	const query = "{ article { id title } }"
	in := compileInput{Query: query, SessionVars: userSession()}
	if _, err := eng.compilePlan(ctx, in); err != nil {
		t.Fatalf("compile: %v", err)
	}

	cached := 0
	eng.cache.Range(func(_, _ any) bool { cached++; return true })
	if cached != 1 {
		t.Fatalf("a plan with no signature must be cached, found %d entries", cached)
	}
}
