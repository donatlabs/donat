-- Reconciliation compares a provider's answer against what this store already
-- recorded, so it is only meaningful for a payment that actually reached the
-- provider. Exposing those rows as one view lets the entry-point Command read
-- the expectations instead of accepting operator-supplied ones, and the
-- required domains publish the non-null contract the Process input declares.
CREATE VIEW payment_reconciliation_candidate AS
SELECT
  id::petshop_required_uuid AS payment_id,
  status::petshop_required_text AS status,
  amount_minor::petshop_required_int8 AS amount_minor,
  currency::petshop_required_text AS currency,
  provider_reference::petshop_required_text AS provider_reference
FROM payment
WHERE provider_reference IS NOT NULL;
