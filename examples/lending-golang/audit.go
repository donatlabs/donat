package main

import (
	"context"
	"encoding/json"
	"fmt"

	"github.com/donatlabs/donat/sdk/go/donat"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"
)

// ─────────────────────────────────────────────────────────────────────────────
// Borrowing with an application-owned audit row, in ONE transaction.
//
// This is the other half of composability, and it is not the same thing as an
// event handler. A handler in handlers.go runs AFTER the commit: if it fails,
// the loan still happened, which is exactly right for notifying somebody. But
// a row that must exist if and only if the loan exists cannot be written
// there — a crash between the commit and the handler would lose it.
//
// `ExecuteTx` is for that case. The application opens the transaction, hands
// it to the engine for the command, writes its own row on the same handle, and
// decides whether to commit. The engine issues no BEGIN and no COMMIT: if the
// audit insert fails, the loan rolls back with it.
//
// The cost of owning the transaction is that post-commit hooks do not fire —
// the engine has not committed anything, so it cannot know when to fire them.
// The caller is responsible for side effects after its own commit.
// ─────────────────────────────────────────────────────────────────────────────

// BorrowWithAudit lends a copy and records who did it, atomically.
//
// It returns the GraphQL response body. A rejected borrow — an unavailable
// copy, a member over their limit — comes back as a GraphQL error body with a
// nil Go error, and this function rolls back rather than committing an audit
// row for a loan that did not happen.
func BorrowWithAudit(
	ctx context.Context,
	eng *donat.Engine,
	pool *pgxpool.Pool,
	memberID, copyID, borrowedOn, dueOn string,
) ([]byte, error) {
	tx, err := pool.Begin(ctx)
	if err != nil {
		return nil, fmt.Errorf("begin: %w", err)
	}
	// Rollback is a no-op once the transaction has committed, so this is safe
	// on every path and is what covers an early return.
	defer tx.Rollback(ctx) //nolint:errcheck

	vars := map[string]json.RawMessage{
		"copy": mustJSON(copyID),
		"from": mustJSON(borrowedOn),
		"due":  mustJSON(dueOn),
	}
	body, err := eng.ExecuteTx(ctx, tx, borrowMutation, vars, map[string]string{
		"x-donat-role":    "member",
		"x-donat-user-id": memberID,
	})
	if err != nil {
		return nil, fmt.Errorf("borrow: %w", err)
	}
	// A GraphQL error body is a refusal, not a host failure. Returning without
	// committing is what keeps the audit row and the loan in step.
	if hasErrors(body) {
		return body, nil
	}

	if err := recordAudit(ctx, tx, memberID, "borrow", copyID); err != nil {
		return nil, fmt.Errorf("audit: %w", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return nil, fmt.Errorf("commit: %w", err)
	}
	return body, nil
}

const borrowMutation = `
mutation ($copy: uuid!, $from: date!, $due: date!) {
  borrow_copy(copy_id: $copy, borrowed_on: $from, due_on: $due) {
    loan_id
    copy_id
    due_on
  }
}`

func recordAudit(ctx context.Context, tx pgx.Tx, actor, action, subject string) error {
	_, err := tx.Exec(ctx,
		"INSERT INTO public.audit_entry (actor, action, subject) VALUES ($1, $2, $3)",
		actor, action, subject)
	return err
}

// hasErrors reports whether a GraphQL response body carries a rejection.
func hasErrors(body []byte) bool {
	var envelope struct {
		Errors []json.RawMessage `json:"errors"`
	}
	if err := json.Unmarshal(body, &envelope); err != nil {
		// An unreadable body is not a successful borrow.
		return true
	}
	return len(envelope.Errors) > 0
}

func mustJSON(v any) json.RawMessage {
	raw, err := json.Marshal(v)
	if err != nil {
		// The inputs here are strings from the caller; a failure would be a
		// programming error, not a runtime condition.
		panic(fmt.Sprintf("marshal %v: %v", v, err))
	}
	return raw
}
