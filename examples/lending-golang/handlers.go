package main

import (
	"context"
	"log"
	"sync"

	"github.com/donatlabs/donat/examples/lending-golang/gen"
	"github.com/donatlabs/donat/sdk/go/donat"
)

// ─────────────────────────────────────────────────────────────────────────────
// Event-trigger handlers — THIS is the file you edit.
//
// Each handler is a plain Go function called IN-PROCESS right after the
// command's transaction commits: no webhook, no HTTP round trip, no second
// service. The trigger name passed to donat.On must match an
// `event_triggers[].name` in the YAML metadata (metadata/databases/default/
// tables/public_loan.yaml).
//
// What belongs here and what does not is the whole point of this example:
//
//	belongs here   — sending the "your book is due back on …" mail, pushing a
//	                 row to a search index, emitting a metric, calling another
//	                 service. Work that must not roll the loan back if it fails.
//	does NOT belong — deciding whether the loan was allowed. That is a rule in
//	                 rules.yaml, enforced inside the statement. A check written
//	                 here would run after the write had already committed.
// ─────────────────────────────────────────────────────────────────────────────

// LoanLog records the loans this process observed. A real service would
// notify or integrate; the example keeps an in-memory record so the tests can
// prove the handler ran without reaching for a mock mail server.
type LoanLog struct {
	mu     sync.Mutex
	events []LoanEvent
}

// LoanEvent is one observation of the on_loan_recorded trigger.
type LoanEvent struct {
	Op    donat.Op
	Table string
}

// Observed returns a copy of what the handlers have seen so far.
func (l *LoanLog) Observed() []LoanEvent {
	l.mu.Lock()
	defer l.mu.Unlock()
	return append([]LoanEvent(nil), l.events...)
}

func (l *LoanLog) record(ev LoanEvent) {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.events = append(l.events, ev)
}

// Loans is the process-wide log the handlers write to.
var Loans = &LoanLog{}

// RegisterHandlers wires every event-trigger handler into the registry.
// Add your own with another donat.On(reg, "<trigger name>", <func>) line.
func RegisterHandlers(reg *donat.Registry) {
	donat.On(reg, "on_loan_recorded", onLoanRecorded)
}

// onLoanRecorded fires when a loan is created and when its status changes,
// which is to say once when a copy goes out and once when it comes back.
func onLoanRecorded(_ context.Context, ev donat.Event[gen.Loan]) error {
	Loans.record(LoanEvent{Op: ev.Op, Table: ev.Table.Name})
	log.Printf("[event] on_loan_recorded: op=%s table=%s", ev.Op, ev.Table.Name)
	// Real work goes here: notify the member, update a due-date reminder job,
	// push to a search index. An error returned here is logged and does not
	// roll back the loan — the transaction has already committed.
	return nil
}
