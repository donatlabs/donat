package donat

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

// fixtureWithAction is the shared fixture plus a handler-less action returning
// a declared object type — the shape a user writing custom logic ends up with.
func fixtureWithAction(t *testing.T) []byte {
	t.Helper()
	var cfg map[string]any
	if err := json.Unmarshal(fixtureMetaCatalog(), &cfg); err != nil {
		t.Fatalf("fixture: %v", err)
	}
	md := cfg["metadata"].(map[string]any)
	md["custom_types"] = map[string]any{
		"objects": []any{map[string]any{
			"name": "InvoicePdf",
			"fields": []any{
				map[string]any{"name": "url", "type": "String!"},
				map[string]any{"name": "bytes", "type": "Int!"},
			},
		}},
	}
	md["actions"] = []any{map[string]any{
		"name": "render_invoice_pdf",
		"definition": map[string]any{
			"type":        "mutation",
			"arguments":   []any{map[string]any{"name": "invoice_id", "type": "String!"}},
			"output_type": "InvoicePdf",
		},
		"permissions": []any{map[string]any{"role": "user"}},
	}}
	out, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("fixture marshal: %v", err)
	}
	return out
}

func userSession() map[string]string {
	return map[string]string{"x-donat-role": "user", "x-donat-user-id": "7"}
}

// The end the whole feature exists for: a GraphQL mutation reaches a plain Go
// function and its return value comes back as a typed GraphQL field, with no
// database involved and no webhook.
func TestAnActionIsResolvedByARegisteredGoFunction(t *testing.T) {
	ctx := context.Background()
	called := 0

	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (pdfOut, error) {
		called++
		return pdfOut{URL: "https://s3/" + a.InvoiceID + ".pdf", Bytes: 12}, nil
	})(&cfg)

	eng, err := New(ctx, cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}

	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if called != 1 {
		t.Fatalf("the function ran %d times, want 1", called)
	}

	var got struct {
		Data struct {
			Render map[string]json.RawMessage `json:"render_invoice_pdf"`
		} `json:"data"`
	}
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatalf("decode %s: %v", body, err)
	}
	if string(got.Data.Render["url"]) != `"https://s3/inv-1.pdf"` {
		t.Fatalf("the function's value did not reach the client: %s", body)
	}
	// `bytes` was returned but not selected. The engine shapes the result to
	// the selection set exactly as it does for a webhook response.
	if _, present := got.Data.Render["bytes"]; present {
		t.Fatalf("an unselected field must not be returned: %s", body)
	}
}

// A function's own failure is the caller's answer — business logic saying no —
// and must not look like the host breaking.
func TestAFunctionErrorBecomesAGraphQLError(t *testing.T) {
	ctx := context.Background()
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (pdfOut, error) {
		return pdfOut{}, errRender
	})(&cfg)

	eng, err := New(ctx, cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, userSession())
	if err != nil {
		t.Fatalf("a refused action is an answer, not a transport failure: %v", err)
	}
	if !strings.Contains(string(body), "invoice has no lines") {
		t.Fatalf("the function's reason must reach the client: %s", body)
	}
}

// The engine does not trust what a Go function returned: a field the metadata
// declares non-null cannot come back empty, or the same declaration would mean
// something different here than on the standalone server.
func TestAResultViolatingTheDeclaredTypeIsRefused(t *testing.T) {
	ctx := context.Background()
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	// url is declared String! but this function leaves it empty as a pointer.
	WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (map[string]any, error) {
		return map[string]any{"url": nil, "bytes": 1}, nil
	})(&cfg)

	eng, err := New(ctx, cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), "url") {
		t.Fatalf("the refusal must name the offending field: %s", body)
	}
}

// A role the action does not name is told the field does not exist, so the
// schema cannot be enumerated through permission errors.
func TestAnActionIsInvisibleToARoleItDoesNotName(t *testing.T) {
	ctx := context.Background()
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (pdfOut, error) {
		t.Fatal("the function must not run for a role that cannot see the action")
		return pdfOut{}, nil
	})(&cfg)

	eng, err := New(ctx, cfg)
	if err != nil {
		t.Fatalf("New: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, map[string]string{"x-donat-role": "counter"})
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), "not found in type") {
		t.Fatalf("a hidden action must read as an unknown field: %s", body)
	}
}

var errRender = &renderError{}

type renderError struct{}

func (*renderError) Error() string { return "invoice has no lines" }
