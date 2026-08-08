package donat

import (
	"context"
	"strings"
	"testing"
)

// The declaration says invoice_id; the struct says invoiceId. Before this
// check, the function ran with an empty string and the request answered 200 —
// the failure surfaced as missing data somewhere far away.
func TestATagThatDisagreesWithTheDeclarationFailsAtStartup(t *testing.T) {
	type args struct {
		InvoiceID string `json:"invoiceId"`
	}
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, _ args) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	_, err := New(context.Background(), cfg)
	if err == nil {
		t.Fatal("expected a mismatched json tag to fail the boot")
	}
	if !strings.Contains(err.Error(), "invoice_id") {
		t.Fatalf("the failure must name the declared argument: %v", err)
	}
	if !strings.Contains(err.Error(), "arrive empty") {
		t.Fatalf("the failure must say what would go wrong: %v", err)
	}
}

// The reverse drift: the struct wants something the metadata never declares,
// so it can only ever be zero.
func TestAStructFieldWithNoDeclaredArgumentFailsAtStartup(t *testing.T) {
	type args struct {
		InvoiceID string `json:"invoice_id"`
		Currency  string `json:"currency"`
	}
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, _ args) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	_, err := New(context.Background(), cfg)
	if err == nil {
		t.Fatal("expected an undeclared struct field to fail the boot")
	}
	if !strings.Contains(err.Error(), "currency") {
		t.Fatalf("the failure must name the extra field: %v", err)
	}
}

// The matching case must start.
func TestAMatchingStructStarts(t *testing.T) {
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, _ pdfArgs) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	if _, err := New(context.Background(), cfg); err != nil {
		t.Fatalf("a struct matching its declaration must start: %v", err)
	}
}

// Taking a map is an explicit choice to decode whatever arrives, so it opts
// out of the check rather than failing it.
func TestAMapArgumentOptsOutOfTheShapeCheck(t *testing.T) {
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, _ map[string]any) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	if _, err := New(context.Background(), cfg); err != nil {
		t.Fatalf("a map argument must be allowed: %v", err)
	}
}

// A field the author excluded from JSON is not part of the shape.
func TestAnExcludedFieldIsNotPartOfTheShape(t *testing.T) {
	type args struct {
		InvoiceID string `json:"invoice_id"`
		internal  string //nolint:unused
		Cache     string `json:"-"`
	}
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, _ args) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	if _, err := New(context.Background(), cfg); err != nil {
		t.Fatalf("unexported and json:\"-\" fields must not count: %v", err)
	}
}
