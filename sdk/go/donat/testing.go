package donat

import (
	"context"
	"encoding/json"
	"fmt"
)

// TestEngine builds an engine that can compile and resolve actions but cannot
// reach a database.
//
// The wiring is what is worth testing and what a plain unit test misses: that
// the Go structs agree with the metadata, that the role may see the action, and
// that what the function returns satisfies the declared output type. All of
// that is decided before any SQL runs, so it needs no database — and requiring
// one is why this kind of mistake usually reaches production instead.
//
// A query or a table mutation still needs a real backend; those fail here with
// an explicit error rather than a nil-pointer panic.
//
//	eng, err := donat.TestEngine(ctx, snapshot,
//	    donat.WithFunction("render_invoice_pdf", renderInvoicePDF))
//	body, err := eng.Execute(ctx, query, nil,
//	    map[string]string{"x-donat-role": "user"})
func TestEngine(ctx context.Context, snapshot []byte, opts ...Option) (*Engine, error) {
	cfg := Config{Metadata: snapshot, Backend: noDatabase{}}
	for _, opt := range opts {
		opt(&cfg)
	}
	// A caller that supplied its own backend meant it.
	if cfg.Backend == nil {
		cfg.Backend = noDatabase{}
	}
	return New(ctx, cfg)
}

// noDatabase is a Backend that refuses SQL with a message saying why, so a
// test that accidentally exercises the database learns that rather than
// panicking somewhere in pgx.
type noDatabase struct{}

func (noDatabase) Dialect() string { return "postgres" }

func (noDatabase) RunQuery(context.Context, Plan) (json.RawMessage, error) {
	return nil, fmt.Errorf(
		"donat.TestEngine has no database: this operation reads tables, which needs " +
			"a real backend — pass donat.WithBackend(donat.Postgres(pool))")
}

func (noDatabase) RunMutation(context.Context, Plan) (map[string]json.RawMessage, error) {
	return nil, fmt.Errorf(
		"donat.TestEngine has no database: this operation writes tables, which needs " +
			"a real backend — pass donat.WithBackend(donat.Postgres(pool))")
}

func (noDatabase) MapError(err error, _ map[string]string) []byte {
	return errorBody("unexpected", "$", err.Error())
}
