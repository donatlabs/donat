// Integration tests for the lending example, driven the way the service is
// driven: GraphQL over the engine's handler, as one of the library's roles.
//
// Nothing here reimplements a lending decision. Each test asks the service to
// do something and then asserts what the YAML said would happen — the limit in
// rules.yaml, the atomic hold in borrow-copy.yaml, the extension counter in
// extend-loan.yaml. A test that computed the expected answer in Go would be
// testing itself.
//
// Requires Postgres: set LENDING_TEST_PG to a DSN whose database already has
// the platform's migrations and this example's applied. Without it the whole
// file skips — a run with no database must not look like a passing run.
package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/donatlabs/donat/sdk/go/donat"
	"github.com/jackc/pgx/v5/pgxpool"
)

const (
	roleMember    = "member"
	roleLibrarian = "librarian"
)

// service is one running lending service plus the handles a test needs to
// drive it.
type service struct {
	t       *testing.T
	handler http.Handler
	pool    *pgxpool.Pool
	loans   *LoanLog
	// memberID is the identity the member role acts as. Set by addMember, so
	// a test that borrows always borrows as somebody the library knows.
	memberID string
}

func newService(t *testing.T) *service {
	t.Helper()
	dsn := os.Getenv("LENDING_TEST_PG")
	if dsn == "" {
		t.Skip("set LENDING_TEST_PG to a migrated database to run the lending tests")
	}

	ctx := context.Background()
	pool, err := pgxpool.New(ctx, dsn)
	if err != nil {
		t.Fatalf("pgxpool.New: %v", err)
	}
	t.Cleanup(pool.Close)

	// Each test gets its own log, so an assertion about handlers firing cannot
	// be satisfied by an earlier test's events.
	loans := &LoanLog{}
	reg := donat.NewRegistry()
	donat.On(reg, "on_loan_recorded", func(_ context.Context, ev donat.Event[loanRow]) error {
		loans.record(LoanEvent{Op: ev.Op, Table: ev.Table.Name})
		return nil
	})

	eng, err := donat.New(ctx, donat.Config{
		Backend:  donat.Postgres(pool),
		Metadata: coreConfig,
		Registry: reg,
		PoolSize: 2,
	})
	if err != nil {
		t.Fatalf("donat.New: %v", err)
	}

	svc := &service{t: t, handler: eng.Handler(), pool: pool, loans: loans}
	svc.reset()
	return svc
}

// loanRow is the handler payload shape. It mirrors gen.Loan but is declared
// here so the test does not depend on the generated file being regenerated.
type loanRow struct {
	ID     string `json:"id"`
	Status string `json:"status"`
}

// reset empties the library between tests. It talks to the database directly
// on purpose: this is fixture setup, not behaviour under test.
func (s *service) reset() {
	s.t.Helper()
	ctx := context.Background()
	for _, stmt := range []string{
		"DELETE FROM public.loan",
		"DELETE FROM public.copy",
		"DELETE FROM public.book",
		"DELETE FROM public.member",
	} {
		if _, err := s.pool.Exec(ctx, stmt); err != nil {
			s.t.Fatalf("reset %q: %v", stmt, err)
		}
	}
}

// gql posts one operation as `role` and returns the decoded response.
func (s *service) gql(role, query string, vars map[string]any) map[string]any {
	s.t.Helper()
	payload := map[string]any{"query": query}
	if len(vars) > 0 {
		payload["variables"] = vars
	}
	body, err := json.Marshal(payload)
	if err != nil {
		s.t.Fatalf("marshal request: %v", err)
	}
	req := httptest.NewRequest(http.MethodPost, "/v1/graphql", strings.NewReader(string(body)))
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("X-Donat-Role", role)
	if role == roleMember {
		req.Header.Set("X-Donat-User-Id", s.memberID)
	}
	rec := httptest.NewRecorder()
	s.handler.ServeHTTP(rec, req)

	var out map[string]any
	if err := json.Unmarshal(rec.Body.Bytes(), &out); err != nil {
		s.t.Fatalf("decode response (status %d): %v\nbody: %s", rec.Code, err, rec.Body.String())
	}
	return out
}

func errorsOf(resp map[string]any) []any {
	if errs, ok := resp["errors"].([]any); ok {
		return errs
	}
	return nil
}

func dataOf(t *testing.T, resp map[string]any) map[string]any {
	t.Helper()
	if errs := errorsOf(resp); errs != nil {
		t.Fatalf("unexpected GraphQL errors: %v", errs)
	}
	data, ok := resp["data"].(map[string]any)
	if !ok {
		t.Fatalf("response carried no data object: %v", resp)
	}
	return data
}

func errorMessage(resp map[string]any) string {
	errs := errorsOf(resp)
	if len(errs) == 0 {
		return ""
	}
	first, ok := errs[0].(map[string]any)
	if !ok {
		return ""
	}
	msg, _ := first["message"].(string)
	return msg
}

func today() string { return time.Now().UTC().Format("2006-01-02") }

func plusDays(n int) string {
	return time.Now().UTC().AddDate(0, 0, n).Format("2006-01-02")
}

// ---------------------------------------------------------------------------
// Fixture builders — librarian-side CRUD, which is ordinary and uncommanded.
// ---------------------------------------------------------------------------

func (s *service) addMember(name string, limit int) string {
	s.t.Helper()
	resp := s.gql(roleLibrarian, `
		mutation ($name: String!, $limit: Int!) {
		  insert_member(objects: [{ name: $name, loan_limit: $limit }]) {
		    returning { id }
		  }
		}`, map[string]any{"name": name, "limit": limit})
	id := firstReturnedID(s.t, dataOf(s.t, resp), "insert_member")
	s.memberID = id
	return id
}

func (s *service) addBook(title, author string) string {
	s.t.Helper()
	resp := s.gql(roleLibrarian, `
		mutation ($title: String!, $author: String!) {
		  insert_book(objects: [{ title: $title, author: $author }]) {
		    returning { id }
		  }
		}`, map[string]any{"title": title, "author": author})
	return firstReturnedID(s.t, dataOf(s.t, resp), "insert_book")
}

func (s *service) addCopy(bookID, label string) string {
	s.t.Helper()
	resp := s.gql(roleLibrarian, `
		mutation ($book: uuid!, $label: String!) {
		  insert_copy(objects: [{ book_id: $book, label: $label, status: "available" }]) {
		    returning { id }
		  }
		}`, map[string]any{"book": bookID, "label": label})
	return firstReturnedID(s.t, dataOf(s.t, resp), "insert_copy")
}

func firstReturnedID(t *testing.T, data map[string]any, root string) string {
	t.Helper()
	node, ok := data[root].(map[string]any)
	if !ok {
		t.Fatalf("%s returned no object: %v", root, data)
	}
	rows, ok := node["returning"].([]any)
	if !ok || len(rows) == 0 {
		t.Fatalf("%s returned no rows: %v", root, node)
	}
	row, _ := rows[0].(map[string]any)
	id, _ := row["id"].(string)
	if id == "" {
		t.Fatalf("%s returned no id: %v", root, row)
	}
	return id
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

func (s *service) borrow(copyID string) map[string]any {
	s.t.Helper()
	return s.gql(roleMember, `
		mutation ($copy: uuid!, $from: date!, $due: date!) {
		  borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due) {
		    loan_id
		    copy_id
		    due_on
		    open_loans_before
		  }
		}`, map[string]any{"copy": copyID, "from": today(), "due": plusDays(14)})
}

func (s *service) returnCopy(loanID string) map[string]any {
	s.t.Helper()
	return s.gql(roleMember, `
		mutation ($loan: uuid!, $on: date!) {
		  return_copy(loan_id: $loan, returned_on: $on) {
		    loan_id
		    copy_id
		    copy_status
		  }
		}`, map[string]any{"loan": loanID, "on": today()})
}

func (s *service) extend(loanID string, due string) map[string]any {
	s.t.Helper()
	return s.gql(roleMember, `
		mutation ($loan: uuid!, $due: date!) {
		  extend_loan(loan_id: $loan, new_due_on: $due) {
		    loan_id
		    due_on
		    extensions
		  }
		}`, map[string]any{"loan": loanID, "due": due})
}

func commandResult(t *testing.T, resp map[string]any, root string) map[string]any {
	t.Helper()
	data := dataOf(t, resp)
	node, ok := data[root].(map[string]any)
	if !ok {
		t.Fatalf("%s returned no object: %v", root, data)
	}
	return node
}
