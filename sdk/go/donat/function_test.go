package donat

import (
	"context"
	"encoding/json"
	"strings"
	"testing"
)

type pdfArgs struct {
	InvoiceID string `json:"invoice_id"`
}

type pdfOut struct {
	URL   string `json:"url"`
	Bytes int    `json:"bytes"`
}

func snapshotWithActions(actions string) []byte {
	return []byte(`{"metadata":{"version":3,"actions":` + actions + `},"catalog":{}}`)
}

// The registration a user writes must be reachable by the action's name and
// must decode the arguments into their own struct — that is the whole point of
// the generic signature.
func TestWithFunctionBindsArgumentsToTheDeclaredStruct(t *testing.T) {
	cfg := Config{}
	WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (pdfOut, error) {
		return pdfOut{URL: "https://s3/" + a.InvoiceID + ".pdf", Bytes: 7}, nil
	})(&cfg)

	out, ok, err := cfg.Functions.Call(
		context.Background(), "render_invoice_pdf",
		json.RawMessage(`{"invoice_id":"inv-1"}`),
	)
	if err != nil || !ok {
		t.Fatalf("Call: ok=%v err=%v", ok, err)
	}
	got, ok := out.(pdfOut)
	if !ok {
		t.Fatalf("expected pdfOut, got %T", out)
	}
	if got.URL != "https://s3/inv-1.pdf" {
		t.Fatalf("arguments did not reach the function: %+v", got)
	}
}

// A name nobody registered must be distinguishable from a function that ran
// and failed, because the two have different causes and different fixes.
func TestCallReportsAnUnregisteredNameSeparatelyFromAFailure(t *testing.T) {
	f := NewFunctions()
	if _, ok, err := f.Call(context.Background(), "absent", nil); ok || err != nil {
		t.Fatalf("expected not-registered, got ok=%v err=%v", ok, err)
	}
}

// An action declared without a handler is this host's to implement. Starting
// without the function would put a field in the schema that always fails.
func TestNewRefusesAnInProcessActionWithNoFunction(t *testing.T) {
	err := checkFunctionsCoverActions(
		snapshotWithActions(`[{"name":"render_invoice_pdf","definition":{}}]`),
		NewFunctions(),
	)
	if err == nil {
		t.Fatal("expected a refusal for an unimplemented in-process action")
	}
	if !strings.Contains(err.Error(), "render_invoice_pdf") {
		t.Fatalf("the refusal must name the action: %v", err)
	}
	if !strings.Contains(err.Error(), "WithFunction") {
		t.Fatalf("the refusal must say how to fix it: %v", err)
	}
}

// An action with a handler is a webhook: not this host's to implement, and its
// absence from the registry is not an error.
func TestAnActionWithAHandlerNeedsNoFunction(t *testing.T) {
	err := checkFunctionsCoverActions(
		snapshotWithActions(`[{"name":"send_email","definition":{"handler":"https://x.test/h"}}]`),
		NewFunctions(),
	)
	if err != nil {
		t.Fatalf("a webhook action must not require a local function: %v", err)
	}
}

// The registered case is the one that must start.
func TestARegisteredInProcessActionStarts(t *testing.T) {
	cfg := Config{}
	WithFunction("render_invoice_pdf", func(_ context.Context, a pdfArgs) (pdfOut, error) {
		return pdfOut{}, nil
	})(&cfg)

	err := checkFunctionsCoverActions(
		snapshotWithActions(`[{"name":"render_invoice_pdf","definition":{}}]`),
		cfg.Functions,
	)
	if err != nil {
		t.Fatalf("a registered action must start: %v", err)
	}
}
