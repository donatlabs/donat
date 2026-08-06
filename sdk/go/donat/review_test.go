package donat

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// A handler's own error code has to survive the trip. The standalone server
// honours the body's `code` and `extensions`; this host dropped both, so the
// same handler answered `validation-failed` there and `unexpected` here.
func TestAWebhookRefusalKeepsItsCode(t *testing.T) {
	ctx := context.Background()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
		_, _ = w.Write([]byte(`{"message":"invoice has no lines","code":"validation-failed"}`))
	}))
	defer srv.Close()

	eng, err := New(ctx, Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithWebhookAction(t, srv.URL),
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`, nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), `"code":"validation-failed"`) {
		t.Fatalf("the handler's code must reach the client: %s", body)
	}
}

// An `extensions` object is used verbatim, the way the server uses it.
func TestAWebhookRefusalKeepsItsExtensions(t *testing.T) {
	ctx := context.Background()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusForbidden)
		_, _ = w.Write([]byte(
			`{"message":"nope","extensions":{"code":"access-denied","path":"$.x","hint":"k"}}`))
	}))
	defer srv.Close()

	eng, err := New(ctx, Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithWebhookAction(t, srv.URL),
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`, nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	for _, want := range []string{`"code":"access-denied"`, `"hint":"k"`, `"path":"$.x"`} {
		if !strings.Contains(string(body), want) {
			t.Fatalf("extensions must pass through verbatim, missing %s: %s", want, body)
		}
	}
}

// A body with neither code nor extensions is still `unexpected`.
func TestAnUnclassifiedWebhookRefusalIsUnexpected(t *testing.T) {
	ctx := context.Background()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"message":"boom"}`))
	}))
	defer srv.Close()

	eng, err := New(ctx, Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithWebhookAction(t, srv.URL),
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`, nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), `"code":"unexpected"`) {
		t.Fatalf("an unclassified refusal stays unexpected: %s", body)
	}
}

// A hook carries the alias of the statement it came from, so a handler cannot
// be handed another root's rows.
func TestAHookNamesTheStatementItCameFrom(t *testing.T) {
	raw := []byte(`{"kind":"mutation","version":1,"transaction":true,
		"statements":[{"alias":"insert_author","sql":"","params":[]}],
		"hooks":[{"phase":"post_commit","alias":"insert_author","trigger":"on_author",
		          "schema":"public","table":"author","op":"INSERT"}],
		"response":[],"error_map":{}}`)
	plan, err := decodePlan(raw)
	if err != nil {
		t.Fatalf("decodePlan: %v", err)
	}
	if plan.Hooks[0].Alias != "insert_author" {
		t.Fatalf("a hook must carry its statement alias: %+v", plan.Hooks[0])
	}
}

// A root `__typename` is answered by the planner and never reaches SQL, so the
// host has to put it back — and a query selecting only `__typename` produces
// no statement at all.
func TestRootTypenameIsAssembledFromThePlan(t *testing.T) {
	slots := []ResponseSlot{
		{Kind: "local_typename", Key: "__typename", Value: "query_root"},
		{Kind: "source_field", Key: "article"},
	}
	out, err := assembleResponse(slots, json.RawMessage(`{"article":[{"id":1}]}`))
	if err != nil {
		t.Fatalf("assembleResponse: %v", err)
	}
	// The client's field order is part of the response.
	want := `{"__typename":"query_root","article":[{"id":1}]}`
	if string(out) != want {
		t.Fatalf("got %s, want %s", out, want)
	}
}

// A typename-only query has no statement result at all.
func TestATypenameOnlyQueryNeedsNoStatementResult(t *testing.T) {
	slots := []ResponseSlot{{Kind: "local_typename", Key: "__typename", Value: "query_root"}}
	out, err := assembleResponse(slots, nil)
	if err != nil {
		t.Fatalf("assembleResponse: %v", err)
	}
	if string(out) != `{"__typename":"query_root"}` {
		t.Fatalf("got %s", out)
	}
}

// Two operations in one document share its text, so the name is part of the
// plan's identity — otherwise the second call is served the first one's plan.
func TestTheOperationNameIsPartOfTheCacheKey(t *testing.T) {
	ctx := context.Background()
	eng, err := New(ctx, Config{Backend: Postgres(nil), Metadata: fixtureMetaCatalog()})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	const doc = `query A { article { id } } query B { article { title } }`
	nameA, nameB := "A", "B"

	planA, err := eng.compilePlan(ctx, compileInput{
		Query: doc, OperationName: &nameA, SessionVars: userSession()})
	if err != nil {
		t.Fatalf("compile A: %v", err)
	}
	planB, err := eng.compilePlan(ctx, compileInput{
		Query: doc, OperationName: &nameB, SessionVars: userSession()})
	if err != nil {
		t.Fatalf("compile B: %v", err)
	}
	if planA.Statements[0].SQL == planB.Statements[0].SQL {
		t.Fatal("two named operations shared one cached plan")
	}
}

// The cache is bounded: a caller sending distinct variables cannot grow the
// process without limit.
func TestThePlanCacheIsBounded(t *testing.T) {
	ctx := context.Background()
	eng, err := New(ctx, Config{
		Backend: Postgres(nil), Metadata: fixtureMetaCatalog(), PlanCacheSize: 4,
	})
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	for i := 0; i < 40; i++ {
		vars := map[string]json.RawMessage{"v": json.RawMessage(itoa(i))}
		if _, err := eng.compilePlan(ctx, compileInput{
			Query: "{ article { id } }", Variables: vars, SessionVars: userSession(),
		}); err != nil {
			t.Fatalf("compile %d: %v", i, err)
		}
	}
	n := 0
	eng.cache.Range(func(_, _ any) bool { n++; return true })
	if n > 4 {
		t.Fatalf("the cache holds %d entries, over its limit of 4", n)
	}
}

// A dialect the core does not know falls back to Postgres, so accepting the
// driver's own spelling would render the wrong SQL and fail far from here.
func TestAnUnknownDialectIsRefusedAtConstruction(t *testing.T) {
	defer func() {
		r := recover()
		if r == nil {
			t.Fatal("expected an unknown dialect to be refused")
		}
		if !strings.Contains(itos(r), "sqlite3") {
			t.Fatalf("the refusal must name what was passed: %v", r)
		}
	}()
	SQL(nil, "sqlite3")
}

// Main authenticates nothing, so a deployment must be able to put a check in
// front of the handler.
func TestMiddlewareWrapsTheHandler(t *testing.T) {
	seen := false
	cfg := Config{}
	WithMiddleware(func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			seen = true
			next.ServeHTTP(w, r)
		})
	})(&cfg)

	if len(cfg.Middleware) != 1 {
		t.Fatalf("expected one middleware, got %d", len(cfg.Middleware))
	}
	var h http.Handler = http.HandlerFunc(func(http.ResponseWriter, *http.Request) {})
	h = cfg.Middleware[0](h)
	h.ServeHTTP(httptest.NewRecorder(), httptest.NewRequest("POST", "/v1/graphql", nil))
	if !seen {
		t.Fatal("the middleware did not run")
	}
}

func itoa(i int) string { return string(rune('0' + i%10)) }
func itos(v any) string { s, _ := v.(string); return s }
