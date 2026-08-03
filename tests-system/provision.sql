-- Stand provisioning — NOT part of any test.
--
-- The suite is black-box: it only ever uses the store's public API. But the
-- example seeds a demo-sized warehouse (a handful of units), and two of its
-- inventory representations cannot be replenished from outside at all:
--
--   * inventory_stock  — staff can restock it over GraphQL, so the suite does.
--   * inventory_level  — the per-location stock the fulfilment module allocates
--     from. It is tracked in metadata but carries no permission for any role,
--     and no Command receives goods into a location, so nothing outside the
--     database can ever put units back on that shelf.
--
-- Provisioning the warehouse belongs to standing the environment up, next to
-- the migrations, exactly as a real deployment would load its opening stock.
-- Everything a test asserts still goes through the API.

UPDATE inventory_stock
   SET on_hand = GREATEST(on_hand, 100000);

UPDATE inventory_level
   SET on_hand_quantity = GREATEST(on_hand_quantity, 100000)
 WHERE location_code = 'main';

-- Reference data the store cannot be told about from outside either: no role
-- may create an organization, a membership or a subscription, and no Command
-- does it as a side effect. A B2B customer and a subscriber are facts about
-- the business that exist before anyone calls the API, so the stand states
-- them here and the scenarios exercise the flows that act on them.

INSERT INTO organization (id, currency, available_credit_minor)
VALUES ('00000000-0000-0000-0000-0000000000c1', 'USD', 100000000)
ON CONFLICT (id) DO UPDATE
   SET available_credit_minor = GREATEST(organization.available_credit_minor, 100000000);

INSERT INTO organization_membership (organization_id, user_id)
VALUES ('00000000-0000-0000-0000-0000000000c1', 'customer-1')
ON CONFLICT DO NOTHING;

INSERT INTO subscription (
  id, customer_id, variant_id, quantity, unit_price_minor, line_total_minor, currency, status
)
VALUES (
  '00000000-0000-0000-0000-0000000000d1', 'customer-1', 2, 1, 2499, 2499, 'USD', 'active'
)
ON CONFLICT (id) DO UPDATE SET status = 'active';

-- Marketplace hygiene. `vendor_payout_candidate` reports every vendor_order
-- marked `accepted`, against one payout cycle id fixed in the view — and
-- `vendor_order.status` has no settled state to move to (`pending_acceptance`
-- and `accepted` are the only ones the check constraint allows). A paid
-- candidate therefore stays a candidate, and a second payout run re-creates a
-- payout_key the first one already inserted, which the runtime turns into an
-- endlessly retried transition that wedges every Process (see the README).
-- Sending the leftovers back to `pending_acceptance` takes them out of the
-- candidate set so a reused stand can run the cycle again.
UPDATE vendor_order
   SET status = 'pending_acceptance'
 WHERE status = 'accepted';

-- And the payouts themselves: the cycle can only be run once per stand, so a
-- stand that is being reused starts it over.
DELETE FROM vendor_payout_reconciliation;
DELETE FROM vendor_payout_event;
DELETE FROM vendor_payout;

-- File uploads. A session may hold ten unclaimed uploads at a time, and the
-- collector reclaims abandoned ones a day later — so a suite that deliberately
-- leaves uploads unfinished exhausts the allowance on a reused stand long
-- before the collector runs. Clearing the pending rows is the stand's
-- equivalent of waiting that day out.
DELETE FROM donat.file_uploads WHERE state <> 'claimed';
