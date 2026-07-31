-- `ReconciliationState` declares pending, matched, review_required and
-- resolved, and `reconciliation_state_for_decision` maps a matched decision to
-- `matched`. The original check admitted only the two states that need a human,
-- so an automatically matched reconciliation could never be stored and the
-- Process wedged on `record_exact_match`. The stored states now equal the
-- declared enum.
ALTER TABLE payment_reconciliation
  DROP CONSTRAINT payment_reconciliation_status_check;
ALTER TABLE payment_reconciliation
  ADD CONSTRAINT payment_reconciliation_status_check
  CHECK (status IN ('pending', 'matched', 'review_required', 'resolved'));
