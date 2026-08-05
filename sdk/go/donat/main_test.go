package donat

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// The same metadata must work on either host. An action naming a webhook is
// not "the standalone server's kind" — the embedded host calls it too, with the
// payload that server sends, so one handler serves both unchanged.
func TestAWebhookActionIsCalledByTheEmbeddedHost(t *testing.T) {
	ctx := context.Background()
	var gotPayload map[string]json.RawMessage

	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		_ = json.Unmarshal(body, &gotPayload)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"url":"https://s3/from-webhook.pdf","bytes":3}`))
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
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, userSession())
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if !strings.Contains(string(body), "from-webhook.pdf") {
		t.Fatalf("the webhook's value did not reach the client: %s", body)
	}
	// The payload shape is the contract an existing handler was written
	// against, so it has to be the one the standalone server sends.
	if string(gotPayload["action"]) != `{"name":"render_invoice_pdf"}` {
		t.Fatalf("unexpected action envelope: %s", gotPayload["action"])
	}
	if !strings.Contains(string(gotPayload["session_variables"]), `"x-donat-role":"user"`) {
		t.Fatalf("session variables must reach the handler: %s", gotPayload["session_variables"])
	}
}

// A webhook action is not this host's to implement, so its absence from the
// function registry must not stop the engine starting.
func TestAWebhookActionDoesNotRequireAFunction(t *testing.T) {
	ctx := context.Background()
	if _, err := New(ctx, Config{
		Backend:  Postgres(nil),
		Metadata: fixtureWithWebhookAction(t, "https://unused.test/h"),
	}); err != nil {
		t.Fatalf("a webhook action must not demand a local function: %v", err)
	}
}

// A handler that refuses answers the caller: it is the handler saying no, not
// the host failing.
func TestAFailingWebhookBecomesAGraphQLError(t *testing.T) {
	ctx := context.Background()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
		_, _ = w.Write([]byte(`{"message":"invoice has no lines"}`))
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
		`mutation { render_invoice_pdf(invoice_id: "inv-1") { url } }`,
		nil, userSession())
	if err != nil {
		t.Fatalf("a refused action is an answer, not a transport failure: %v", err)
	}
	if !strings.Contains(string(body), "invoice has no lines") {
		t.Fatalf("the handler's reason must reach the client: %s", body)
	}
}

// Run must refuse a snapshot whose in-process action has no function, rather
// than listen and serve a field that always fails.
func TestRunRefusesAnUnimplementedInProcessAction(t *testing.T) {
	err := Run(context.Background(),
		WithBackend(Postgres(nil)),
		WithMetadata(fixtureWithAction(t)),
	)
	if err == nil {
		t.Fatal("expected Run to refuse an unimplemented in-process action")
	}
	if !strings.Contains(err.Error(), "render_invoice_pdf") {
		t.Fatalf("the refusal must name the action: %v", err)
	}
}

// Missing configuration is a startup error naming the variable, not a panic.
func TestRunReportsMissingConfiguration(t *testing.T) {
	t.Setenv("DONAT_CORE_CONFIG", "")
	err := Run(context.Background(), WithBackend(Postgres(nil)))
	if err == nil || !strings.Contains(err.Error(), "DONAT_CORE_CONFIG") {
		t.Fatalf("expected a named configuration error, got %v", err)
	}
}

func fixtureWithWebhookAction(t *testing.T, url string) []byte {
	t.Helper()
	var cfg map[string]any
	if err := json.Unmarshal(fixtureWithAction(t), &cfg); err != nil {
		t.Fatalf("fixture: %v", err)
	}
	md := cfg["metadata"].(map[string]any)
	action := md["actions"].([]any)[0].(map[string]any)
	action["definition"].(map[string]any)["handler"] = url
	out, err := json.Marshal(cfg)
	if err != nil {
		t.Fatalf("fixture marshal: %v", err)
	}
	return out
}
