package main

import (
	"context"
	"strconv"
	"strings"
	"sync"
	"testing"
)

// The happy path, and the shape of every other test: ask the service to lend a
// copy, then check the things the YAML promised — a loan exists, the copy is
// no longer on the shelf, and the handler saw it.
func TestBorrowLendsTheCopy(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("The Mythical Man-Month", "Brooks")
	copyID := svc.addCopy(book, "c-1")

	result := commandResult(t, svc.borrow(copyID), "borrow_copy")

	if result["loan_id"] == nil || result["loan_id"] == "" {
		t.Fatalf("borrow returned no loan: %v", result)
	}
	if got := result["copy_id"]; got != copyID {
		t.Fatalf("loan is against copy %v, want %s", got, copyID)
	}
	// The command projects the count it read *before* the write, which is the
	// value the limit rule was actually evaluated against.
	if got := result["open_loans_before"]; !isZero(got) {
		t.Fatalf("first loan saw %v open loans before it, want 0", got)
	}
	if status := svc.copyStatus(copyID); status != "on_loan" {
		t.Fatalf("copy is %q after borrowing, want on_loan", status)
	}
}

// The atomic hold. `borrow_copy` re-states `status: available` in the update
// predicate, so a copy already lent cannot be lent again — and the rejection
// arrives instead of a second loan, not alongside one.
func TestBorrowRefusesACopyAlreadyOnLoan(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("Structure and Interpretation", "Abelson")
	copyID := svc.addCopy(book, "c-1")

	commandResult(t, svc.borrow(copyID), "borrow_copy")
	second := svc.borrow(copyID)

	if errorMessage(second) == "" {
		t.Fatalf("borrowing an already-lent copy succeeded: %v", second)
	}
	if n := svc.openLoanCount(); n != 1 {
		t.Fatalf("library holds %d open loans after a refused borrow, want 1", n)
	}
}

// The limit rule from rules.yaml, which is the reason this is a command and
// not two writes. The member's own `loan_limit` is what it reads.
func TestBorrowRefusesBeyondTheMemberLoanLimit(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 2)
	book := svc.addBook("A Discipline of Programming", "Dijkstra")

	first := svc.addCopy(book, "c-1")
	second := svc.addCopy(book, "c-2")
	third := svc.addCopy(book, "c-3")

	commandResult(t, svc.borrow(first), "borrow_copy")
	commandResult(t, svc.borrow(second), "borrow_copy")

	refused := svc.borrow(third)
	if msg := errorMessage(refused); msg == "" {
		t.Fatalf("a third loan was allowed past a limit of 2: %v", refused)
	} else if !strings.Contains(msg, "maximum number of loans") {
		t.Fatalf("refusal did not carry the rule's message: %q", msg)
	}
	if n := svc.openLoanCount(); n != 2 {
		t.Fatalf("library holds %d open loans, want 2", n)
	}
	// The third copy must still be on the shelf: the whole statement rolled
	// back, including the hold that ran before the rule was evaluated.
	if status := svc.copyStatus(third); status != "available" {
		t.Fatalf("refused copy is %q, want available", status)
	}
}

// A returned loan keeps its row but must stop counting, or borrowing history
// would slowly cost a member their right to borrow.
func TestReturningFreesTheLimitAgain(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 1)
	book := svc.addBook("Notes on Structured Programming", "Dijkstra")
	first := svc.addCopy(book, "c-1")
	second := svc.addCopy(book, "c-2")

	loan := commandResult(t, svc.borrow(first), "borrow_copy")
	if refused := svc.borrow(second); errorMessage(refused) == "" {
		t.Fatalf("a second loan was allowed past a limit of 1: %v", refused)
	}

	returned := commandResult(t, svc.returnCopy(loan["loan_id"].(string)), "return_copy")
	if got := returned["copy_status"]; got != "available" {
		t.Fatalf("returned copy is %v, want available", got)
	}

	commandResult(t, svc.borrow(second), "borrow_copy")
	if n := svc.openLoanCount(); n != 1 {
		t.Fatalf("library holds %d open loans, want 1", n)
	}
}

// Returning is the mirror of borrowing: the loan closes and the copy is
// released in the same statement, so neither can be observed without the other.
func TestReturnClosesTheLoanAndShelvesTheCopy(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("The Pragmatic Programmer", "Hunt")
	copyID := svc.addCopy(book, "c-1")

	loan := commandResult(t, svc.borrow(copyID), "borrow_copy")
	result := commandResult(t, svc.returnCopy(loan["loan_id"].(string)), "return_copy")

	if got := result["copy_id"]; got != copyID {
		t.Fatalf("returned copy %v, want %s", got, copyID)
	}
	if status := svc.copyStatus(copyID); status != "available" {
		t.Fatalf("copy is %q after return, want available", status)
	}
	if n := svc.openLoanCount(); n != 0 {
		t.Fatalf("library holds %d open loans after the only one closed", n)
	}
}

// Returning the same loan twice must be refused: `status: active` is in the
// predicate, so the second attempt matches nothing and `require_affected`
// turns that into a rejection rather than a silent no-op.
func TestReturnRefusesAnAlreadyClosedLoan(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("Refactoring", "Fowler")
	copyID := svc.addCopy(book, "c-1")

	loan := commandResult(t, svc.borrow(copyID), "borrow_copy")
	loanID := loan["loan_id"].(string)
	commandResult(t, svc.returnCopy(loanID), "return_copy")

	if second := svc.returnCopy(loanID); errorMessage(second) == "" {
		t.Fatalf("returning a closed loan succeeded: %v", second)
	}
}

// The extension counter is incremented by the `add_int` rule — the same
// declaration the limit reads — so the counter can never be advanced by one
// path and checked by another.
func TestExtendMovesTheDueDateAndCountsUp(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("Domain-Driven Design", "Evans")
	copyID := svc.addCopy(book, "c-1")

	loan := commandResult(t, svc.borrow(copyID), "borrow_copy")
	loanID := loan["loan_id"].(string)

	first := commandResult(t, svc.extend(loanID, plusDays(21)), "extend_loan")
	if got := first["extensions"]; !isNumber(got, 1) {
		t.Fatalf("first extension left the counter at %v, want 1", got)
	}
	if got := first["due_on"]; got != plusDays(21) {
		t.Fatalf("due date is %v, want %s", got, plusDays(21))
	}

	second := commandResult(t, svc.extend(loanID, plusDays(28)), "extend_loan")
	if got := second["extensions"]; !isNumber(got, 2) {
		t.Fatalf("second extension left the counter at %v, want 2", got)
	}
}

// The extension limit, declared as `maximum: 2` in extend-loan.yaml.
func TestExtendRefusesPastTheExtensionLimit(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("Working Effectively with Legacy Code", "Feathers")
	copyID := svc.addCopy(book, "c-1")

	loan := commandResult(t, svc.borrow(copyID), "borrow_copy")
	loanID := loan["loan_id"].(string)

	commandResult(t, svc.extend(loanID, plusDays(21)), "extend_loan")
	commandResult(t, svc.extend(loanID, plusDays(28)), "extend_loan")

	refused := svc.extend(loanID, plusDays(35))
	if msg := errorMessage(refused); msg == "" {
		t.Fatalf("a third extension was allowed past a limit of 2: %v", refused)
	} else if !strings.Contains(msg, "maximum number of times") {
		t.Fatalf("refusal did not carry the rule's message: %q", msg)
	}
}

// The Go module: a handler registered against the YAML trigger name runs
// in-process after the transaction commits. This is the half of the split that
// is deliberately NOT declarative.
func TestBorrowingFiresTheInProcessHandler(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("Peopleware", "DeMarco")
	copyID := svc.addCopy(book, "c-1")

	if seen := svc.loans.Observed(); len(seen) != 0 {
		t.Fatalf("handler fired before anything was borrowed: %v", seen)
	}

	commandResult(t, svc.borrow(copyID), "borrow_copy")

	seen := svc.loans.Observed()
	if len(seen) == 0 {
		t.Fatal("no handler fired for a committed loan")
	}
	if seen[0].Table != "loan" {
		t.Fatalf("handler saw table %q, want loan", seen[0].Table)
	}
}

// Two members racing for the last copy: exactly one wins. The copy's status is
// the arbiter and it is checked inside the same statement that changes it, so
// there is no window in which both see it available.
func TestConcurrentBorrowersLeaveOneLoan(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 5)
	book := svc.addBook("The Art of Computer Programming", "Knuth")
	copyID := svc.addCopy(book, "c-1")

	const racers = 4
	var wg sync.WaitGroup
	results := make([]map[string]any, racers)
	failures := make([]error, racers)
	for i := 0; i < racers; i++ {
		wg.Add(1)
		go func(i int) {
			defer wg.Done()
			// gqlErr, not gql: a worker must not call t.Fatalf, which would
			// terminate only that goroutine and hang the test.
			results[i], failures[i] = svc.gqlErr(roleMember, `
				mutation ($copy: uuid!, $from: date!, $due: date!) {
				  borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due) { loan_id }
				}`, map[string]any{"copy": copyID, "from": today(), "due": plusDays(14)})
		}(i)
	}
	wg.Wait()

	won := 0
	for i, r := range results {
		if failures[i] != nil {
			t.Fatalf("racer %d could not reach the service: %v", i, failures[i])
		}
		if errorMessage(r) == "" {
			won++
		}
	}
	if won != 1 {
		t.Fatalf("%d of %d concurrent borrowers succeeded, want exactly 1", won, racers)
	}
	if n := svc.openLoanCount(); n != 1 {
		t.Fatalf("library holds %d open loans after the race, want 1", n)
	}
}

// A role the commands do not name must be refused. The engine has no admin
// role, so this is the only kind of authority there is.
func TestLibrarianCannotInvokeMemberCommands(t *testing.T) {
	svc := newService(t)
	svc.addMember("Ada", 3)
	book := svc.addBook("Design Patterns", "Gamma")
	copyID := svc.addCopy(book, "c-1")

	resp := svc.gql(roleLibrarian, `
		mutation ($copy: uuid!, $from: date!, $due: date!) {
		  borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due) { loan_id }
		}`, map[string]any{"copy": copyID, "from": today(), "due": plusDays(14)})

	if errorMessage(resp) == "" {
		t.Fatalf("librarian invoked a member-only command: %v", resp)
	}
}

// A request with no role at all is denied before it reaches the database.
func TestRequestWithNoRoleIsDenied(t *testing.T) {
	svc := newService(t)

	resp := svc.gql("", `{ book { id } }`, nil)
	msg := errorMessage(resp)
	if !strings.Contains(msg, "x-donat-role") {
		t.Fatalf("a roleless request was not denied for the documented reason: %q", msg)
	}
}

// ---------------------------------------------------------------------------
// Small readers used by the assertions above. They read the database directly
// because they are checking state, not exercising the API.
// ---------------------------------------------------------------------------

func (s *service) copyStatus(copyID string) string {
	s.t.Helper()
	var status string
	err := s.pool.QueryRow(context.Background(),
		"SELECT status FROM public.copy WHERE id = $1", copyID).Scan(&status)
	if err != nil {
		s.t.Fatalf("read copy status: %v", err)
	}
	return status
}

func (s *service) openLoanCount() int {
	s.t.Helper()
	var n int
	err := s.pool.QueryRow(context.Background(),
		"SELECT count(*) FROM public.loan WHERE status = 'active'").Scan(&n)
	if err != nil {
		s.t.Fatalf("count open loans: %v", err)
	}
	return n
}

func isZero(v any) bool { return isNumber(v, 0) }

// isNumber compares a JSON number, which decodes as float64 — or as a string
// when the engine is asked to stringify numerics.
func isNumber(v any, want int64) bool {
	switch n := v.(type) {
	case float64:
		return int64(n) == want && n == float64(int64(n))
	case string:
		parsed, err := strconv.ParseInt(n, 10, 64)
		return err == nil && parsed == want
	default:
		return false
	}
}
