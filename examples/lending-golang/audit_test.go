package main

import (
	"context"
	"testing"
)

// The property ExecuteTx exists for: the engine's write and the application's
// write are one transaction, so a committed loan always has its audit row.
func TestBorrowWithAuditCommitsBothTogether(t *testing.T) {
	svc := newService(t)
	member := svc.addMember("Ada", 3)
	book := svc.addBook("The Little Prover", "Friedman")
	copyID := svc.addCopy(book, "c-1")

	body, err := BorrowWithAudit(context.Background(), svc.engine, svc.pool,
		member, copyID, today(), plusDays(14))
	if err != nil {
		t.Fatalf("BorrowWithAudit: %v", err)
	}
	if hasErrors(body) {
		t.Fatalf("borrow was refused: %s", body)
	}

	if status := svc.copyStatus(copyID); status != "on_loan" {
		t.Fatalf("copy is %q, want on_loan", status)
	}
	if n := svc.auditCount(copyID); n != 1 {
		t.Fatalf("subject has %d audit rows, want 1", n)
	}
}

// The half that matters more: when the command is refused, the audit row must
// not survive. An audit trail that records attempts as if they were loans is
// worse than none.
func TestRefusedBorrowWritesNoAuditRow(t *testing.T) {
	svc := newService(t)
	member := svc.addMember("Ada", 3)
	book := svc.addBook("The Little Typer", "Friedman")
	copyID := svc.addCopy(book, "c-1")

	// Lend it first, so the second attempt is refused by the atomic hold.
	commandResult(t, svc.borrow(copyID), "borrow_copy")

	body, err := BorrowWithAudit(context.Background(), svc.engine, svc.pool,
		member, copyID, today(), plusDays(14))
	if err != nil {
		t.Fatalf("BorrowWithAudit returned a host error, want a refusal body: %v", err)
	}
	if !hasErrors(body) {
		t.Fatalf("borrowing an already-lent copy succeeded: %s", body)
	}
	if n := svc.auditCount(copyID); n != 0 {
		t.Fatalf("a refused borrow left %d audit rows behind", n)
	}
	if n := svc.openLoanCount(); n != 1 {
		t.Fatalf("library holds %d open loans, want 1", n)
	}
}

// A loan the rule refuses must leave nothing behind either — the rejection
// arrives from inside the statement, so the audit insert is never reached.
func TestBorrowOverTheLimitWritesNoAuditRow(t *testing.T) {
	svc := newService(t)
	member := svc.addMember("Ada", 1)
	book := svc.addBook("The Little Schemer", "Friedman")
	first := svc.addCopy(book, "c-1")
	second := svc.addCopy(book, "c-2")

	commandResult(t, svc.borrow(first), "borrow_copy")

	body, err := BorrowWithAudit(context.Background(), svc.engine, svc.pool,
		member, second, today(), plusDays(14))
	if err != nil {
		t.Fatalf("BorrowWithAudit: %v", err)
	}
	if !hasErrors(body) {
		t.Fatalf("a second loan was allowed past a limit of 1: %s", body)
	}
	if n := svc.auditCount(second); n != 0 {
		t.Fatalf("a refused borrow left %d audit rows behind", n)
	}
	if status := svc.copyStatus(second); status != "available" {
		t.Fatalf("refused copy is %q, want available", status)
	}
}

func (s *service) auditCount(subject string) int {
	s.t.Helper()
	var n int
	err := s.pool.QueryRow(context.Background(),
		"SELECT count(*) FROM public.audit_entry WHERE subject = $1", subject).Scan(&n)
	if err != nil {
		s.t.Fatalf("count audit rows: %v", err)
	}
	return n
}
