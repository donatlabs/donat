package donat

import (
	"errors"
	"fmt"
	"testing"

	"github.com/jackc/pgx/v5/pgconn"
)

// standardErrorMap is the error_map emitted by the wasm-core (from plan_test.go).
var standardErrorMap = map[string]string{
	"23502":   "constraint-violation:Not-NULL violation. ",
	"23503":   "constraint-violation:Foreign key violation. ",
	"23505":   "constraint-violation:Uniqueness violation. ",
	"23514":   "permission-error-from-payload",
	"default": "data-exception",
}

// TestMapPGError verifies each SQLSTATE branch against the exact bodies
// produced by crates/server/src/gql.rs:db_error_json.
func TestMapPGError(t *testing.T) {
	cases := []struct {
		name string
		err  error
		want string // exact JSON
	}{
		{
			name: "23514 with valid JSON payload",
			err: &pgconn.PgError{
				Code:    "23514",
				Message: `{"path":"$.selectionSet.insert_author.args.objects[0].name","message":"check constraint of an insert/update permission has failed"}`,
			},
			// The payload has path+message → permission-error with those values.
			want: `{"errors":[{"extensions":{"path":"$.selectionSet.insert_author.args.objects[0].name","code":"permission-error"},"message":"check constraint of an insert/update permission has failed"}]}`,
		},
		{
			name: "23514 with non-JSON message falls through to bare permission-error",
			err: &pgconn.PgError{
				Code:    "23514",
				Message: "plain check violation message",
			},
			want: `{"errors":[{"extensions":{"path":"$","code":"permission-error"},"message":"plain check violation message"}]}`,
		},
		{
			name: "23505 uniqueness violation",
			err: &pgconn.PgError{
				Code:    "23505",
				Message: "duplicate key value violates unique constraint \"author_pkey\"",
			},
			// Cross-check gql.rs:907: "Uniqueness violation. " + db.message()
			want: `{"errors":[{"extensions":{"path":"$","code":"constraint-violation"},"message":"Uniqueness violation. duplicate key value violates unique constraint \"author_pkey\""}]}`,
		},
		{
			name: "23503 foreign key violation",
			err: &pgconn.PgError{
				Code:    "23503",
				Message: "insert or update on table \"article\" violates foreign key constraint \"article_author_id_fkey\"",
			},
			// Cross-check gql.rs:908: "Foreign key violation. " + db.message()
			want: `{"errors":[{"extensions":{"path":"$","code":"constraint-violation"},"message":"Foreign key violation. insert or update on table \"article\" violates foreign key constraint \"article_author_id_fkey\""}]}`,
		},
		{
			name: "23502 not-null violation",
			err: &pgconn.PgError{
				Code:    "23502",
				Message: "null value in column \"name\" of relation \"author\" violates not-null constraint",
			},
			// Cross-check gql.rs:909: "Not-NULL violation. " + db.message()
			want: `{"errors":[{"extensions":{"path":"$","code":"constraint-violation"},"message":"Not-NULL violation. null value in column \"name\" of relation \"author\" violates not-null constraint"}]}`,
		},
		{
			name: "unknown SQLSTATE uses default directive",
			err: &pgconn.PgError{
				Code:    "42601",
				Message: "syntax error at or near \"FROM\"",
			},
			// "default" → "data-exception" (bare code), message is pgErr.Message
			want: `{"errors":[{"extensions":{"path":"$","code":"data-exception"},"message":"syntax error at or near \"FROM\""}]}`,
		},
		{
			name: "non-PgError wrapping",
			err:  fmt.Errorf("connection refused"),
			// errors.As fails → "unexpected" code
			want: `{"errors":[{"extensions":{"path":"$","code":"unexpected"},"message":"connection refused"}]}`,
		},
		{
			name: "non-PgError via wrapping with errors.As",
			err:  fmt.Errorf("wrapped: %w", errors.New("inner error")),
			want: `{"errors":[{"extensions":{"path":"$","code":"unexpected"},"message":"wrapped: inner error"}]}`,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := mapPGError(tc.err, standardErrorMap)
			if string(got) != tc.want {
				t.Errorf("\ngot:  %s\nwant: %s", string(got), tc.want)
			}
		})
	}
}

// TestErrorBody verifies the shape of the basic error body helper.
func TestErrorBody(t *testing.T) {
	// With explicit path.
	got := errorBody("permission-error", "$.foo", "msg")
	want := `{"errors":[{"extensions":{"path":"$.foo","code":"permission-error"},"message":"msg"}]}`
	if string(got) != want {
		t.Errorf("\ngot:  %s\nwant: %s", string(got), want)
	}

	// Empty path defaults to "$".
	got2 := errorBody("data-exception", "", "some error")
	want2 := `{"errors":[{"extensions":{"path":"$","code":"data-exception"},"message":"some error"}]}`
	if string(got2) != want2 {
		t.Errorf("\ngot:  %s\nwant: %s", string(got2), want2)
	}
}

// TestMapPGErrorWithNilMap verifies built-in fallbacks work when no errorMap is provided.
func TestMapPGErrorWithNilMap(t *testing.T) {
	err := &pgconn.PgError{
		Code:    "23505",
		Message: "duplicate key",
	}
	got := mapPGError(err, nil)
	want := `{"errors":[{"extensions":{"path":"$","code":"constraint-violation"},"message":"Uniqueness violation. duplicate key"}]}`
	if string(got) != want {
		t.Errorf("\ngot:  %s\nwant: %s", string(got), want)
	}
}
