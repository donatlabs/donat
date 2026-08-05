package donat

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

// A function registered under a name the metadata never declares is dead code
// that reads like working code — usually a rename on one side, or a typo.
func TestAFunctionWithNoDeclaredActionFailsAtStartup(t *testing.T) {
	cfg := Config{Backend: Postgres(nil), Metadata: fixtureWithAction(t)}
	WithFunction("render_invoice_pdf", func(_ context.Context, _ pdfArgs) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)
	WithFunction("renderInvoicePdf", func(_ context.Context, _ pdfArgs) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	_, err := New(context.Background(), cfg)
	if err == nil {
		t.Fatal("expected a function with no matching action to fail the boot")
	}
	if !strings.Contains(err.Error(), "renderInvoicePdf") {
		t.Fatalf("the failure must name the stray function: %v", err)
	}
	if !strings.Contains(err.Error(), "never be called") {
		t.Fatalf("the failure must say what the consequence is: %v", err)
	}
}

// Every refusal used to be `unexpected`, which tells a client nothing it can
// act on. A function that knows better picks the code.
func TestAFunctionErrorChoosesItsCode(t *testing.T) {
	ctx := context.Background()
	eng, err := TestEngine(ctx, fixtureWithAction(t),
		WithFunction("render_invoice_pdf", func(_ context.Context, _ pdfArgs) (pdfOut, error) {
			return pdfOut{}, Errorf("validation-failed", "invoice has no lines")
		}))
	if err != nil {
		t.Fatalf("TestEngine: %v", err)
	}

	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`, nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	var got struct {
		Errors []struct {
			Message    string `json:"message"`
			Extensions struct {
				Code string `json:"code"`
			} `json:"extensions"`
		} `json:"errors"`
	}
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatalf("decode %s: %v", body, err)
	}
	if len(got.Errors) != 1 {
		t.Fatalf("expected one error: %s", body)
	}
	if got.Errors[0].Extensions.Code != "validation-failed" {
		t.Fatalf("the function's code must reach the client: %s", body)
	}
	if got.Errors[0].Message != "invoice has no lines" {
		t.Fatalf("the function's message must reach the client: %s", body)
	}
}

// An ordinary error is still `unexpected`, which is what an unclassified fault
// is — the code is opt-in, not a new obligation.
func TestAPlainErrorStaysUnexpected(t *testing.T) {
	ctx := context.Background()
	eng, err := TestEngine(ctx, fixtureWithAction(t),
		WithFunction("render_invoice_pdf", func(_ context.Context, _ pdfArgs) (pdfOut, error) {
			return pdfOut{}, errRender
		}))
	if err != nil {
		t.Fatalf("TestEngine: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`, nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), `"code":"unexpected"`) {
		t.Fatalf("an unclassified failure stays unexpected: %s", body)
	}
}

// The wiring is testable without a database, which is the point: the shape
// check, the role check and the output check all run before any SQL would.
func TestTestEngineResolvesAnActionWithNoDatabase(t *testing.T) {
	ctx := context.Background()
	eng, err := TestEngine(ctx, fixtureWithAction(t),
		WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (pdfOut, error) {
			return pdfOut{URL: "https://s3/" + a.InvoiceID + ".pdf", Bytes: 1}, nil
		}))
	if err != nil {
		t.Fatalf("TestEngine: %v", err)
	}
	body, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`, nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), "inv-1.pdf") {
		t.Fatalf("the action must resolve without a database: %s", body)
	}
}

// An operation that does need SQL says so, instead of panicking inside pgx.
func TestTestEngineExplainsThatItHasNoDatabase(t *testing.T) {
	ctx := context.Background()
	eng, err := TestEngine(ctx, fixtureWithAction(t),
		WithFunction("render_invoice_pdf", func(_ context.Context, _ pdfArgs) (pdfOut, error) {
			return pdfOut{}, nil
		}))
	if err != nil {
		t.Fatalf("TestEngine: %v", err)
	}
	body, err := eng.Execute(ctx, "{ article { id } }", nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), "no database") {
		t.Fatalf("a table read must explain what is missing: %s", body)
	}
}

// An operator whose snapshot and binary disagree needs to know what this build
// speaks.
func TestVersionReportsTheABIAndCoreSize(t *testing.T) {
	v := Version()
	if !strings.Contains(v, "ABI 1") || !strings.Contains(v, "core.wasm") {
		t.Fatalf("unexpected version string: %q", v)
	}
}
