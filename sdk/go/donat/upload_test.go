package donat

import (
	"context"
	"strings"
	"testing"
)

// A function outside the engine gets an uploader that explains itself, not a
// nil dereference.
func TestUploaderOutsideAFunctionExplainsItself(t *testing.T) {
	_, err := UploaderFrom(context.Background()).
		Upload(context.Background(), "public.pet.photo", "x.png", "image/png", []byte("x"))
	if err == nil {
		t.Fatal("expected an uploader outside a request to refuse")
	}
	if !strings.Contains(err.Error(), "no uploader in this context") {
		t.Fatalf("the refusal must say what is missing: %v", err)
	}
}

// The uploader is reachable from inside a function the engine called — that is
// the whole delivery mechanism, so it is worth pinning.
func TestAFunctionReceivesAnUploader(t *testing.T) {
	ctx := context.Background()
	var seen Uploader
	eng, err := TestEngine(ctx, fixtureWithAction(t),
		WithFunction("render_invoice_pdf", func(c context.Context, _ pdfArgs) (pdfOut, error) {
			seen = UploaderFrom(c)
			return pdfOut{URL: "u", Bytes: 1}, nil
		}))
	if err != nil {
		t.Fatalf("TestEngine: %v", err)
	}
	if _, err := eng.Execute(ctx,
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, userSession()); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if _, unavailable := seen.(unavailableUploader); seen == nil || unavailable {
		t.Fatalf("a function must receive a real uploader, got %T", seen)
	}
}

// Storing nothing is a mistake worth naming: the claim gate would certify an
// empty object.
func TestAnEmptyFileIsRefused(t *testing.T) {
	u := engineUploader{}
	if _, err := u.Upload(context.Background(),
		"public.pet.photo", "x.png", "image/png", nil); err == nil ||
		!strings.Contains(err.Error(), "empty file") {
		t.Fatalf("expected an empty file to be refused, got %v", err)
	}
}

// Attachments are Postgres-only, and the generic backend says so rather than
// failing somewhere deeper.
func TestTheGenericBackendRefusesUploads(t *testing.T) {
	b := SQL(nil, "sqlite")
	if _, err := b.ReadUpload(context.Background(), "id"); err == nil ||
		!strings.Contains(err.Error(), "Postgres-only") {
		t.Fatalf("expected a clear refusal, got %v", err)
	}
	if err := b.FinalizeUpload(context.Background(), "id", "key", 1); err == nil ||
		!strings.Contains(err.Error(), "Postgres-only") {
		t.Fatalf("expected a clear refusal, got %v", err)
	}
}
