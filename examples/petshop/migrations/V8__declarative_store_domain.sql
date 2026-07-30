-- Complete the single-tenant Petshop domain used by the declarative Commands.
-- Business transitions stay in YAML; this migration contains only relational
-- integrity, deterministic command-facing views, and seed-independent defaults.

DROP VIEW order_operations;

ALTER TABLE cart DROP CONSTRAINT cart_status_check;
DROP INDEX cart_one_open_per_customer;
UPDATE cart SET status = 'cart_open' WHERE status = 'open';
ALTER TABLE cart ALTER COLUMN status SET DEFAULT 'cart_open';
ALTER TABLE cart ADD CONSTRAINT cart_status_check
  CHECK (status IN ('cart_open', 'checkout_started', 'checked_out', 'abandoned'));
CREATE UNIQUE INDEX cart_one_open_per_customer
  ON cart(customer_id) WHERE status = 'cart_open';

ALTER TABLE orders DROP CONSTRAINT orders_order_status_check;
ALTER TABLE orders
  ADD COLUMN checkout_quote_id uuid,
  ADD COLUMN subscription_renewal_id uuid,
  ADD COLUMN subtotal_minor bigint NOT NULL DEFAULT 0 CHECK (subtotal_minor >= 0),
  ADD COLUMN discount_minor bigint NOT NULL DEFAULT 0 CHECK (discount_minor >= 0),
  ADD COLUMN shipping_minor bigint NOT NULL DEFAULT 0 CHECK (shipping_minor >= 0),
  ADD COLUMN tax_minor bigint NOT NULL DEFAULT 0 CHECK (tax_minor >= 0),
  ADD COLUMN cancellation_reason text;
ALTER TABLE orders ADD CONSTRAINT orders_order_status_check
  CHECK (order_status IN (
    'pending', 'checkout_started', 'authorized', 'paid', 'fulfilled',
    'cancellation_requested', 'cancelled', 'expired'
  ));

ALTER TABLE order_line DROP CONSTRAINT order_line_pkey;
ALTER TABLE order_line ALTER COLUMN id DROP DEFAULT;
ALTER TABLE order_line
  ALTER COLUMN id TYPE uuid USING gen_random_uuid();
ALTER TABLE order_line ALTER COLUMN id SET DEFAULT gen_random_uuid();
ALTER TABLE order_line ADD PRIMARY KEY (id);
ALTER TABLE order_line ALTER COLUMN line_total_minor DROP NOT NULL;
ALTER TABLE order_line
  ADD COLUMN line_subtotal_minor bigint,
  ADD COLUMN discount_minor bigint NOT NULL DEFAULT 0 CHECK (discount_minor >= 0),
  ADD COLUMN taxable_minor bigint,
  ADD COLUMN tax_code text,
  ADD COLUMN tax_bps integer NOT NULL DEFAULT 0 CHECK (tax_bps BETWEEN 0 AND 10000),
  ADD COLUMN prescription_release_id uuid;
UPDATE order_line
SET line_subtotal_minor = line_total_minor,
    taxable_minor = line_total_minor
WHERE line_subtotal_minor IS NULL OR taxable_minor IS NULL;
ALTER TABLE order_line ADD CONSTRAINT order_line_subtotal_check
  CHECK (line_subtotal_minor = unit_price_minor * quantity);
ALTER TABLE order_line ADD CONSTRAINT order_line_taxable_check
  CHECK (taxable_minor >= 0 AND taxable_minor <= line_subtotal_minor);

ALTER TABLE payment DROP CONSTRAINT payment_status_check;
ALTER TABLE payment
  ADD COLUMN authorization_activity_key text,
  ADD COLUMN captured_minor bigint NOT NULL DEFAULT 0 CHECK (captured_minor >= 0),
  ADD COLUMN refunded_minor bigint NOT NULL DEFAULT 0 CHECK (refunded_minor >= 0),
  ADD COLUMN chargeback_minor bigint NOT NULL DEFAULT 0 CHECK (chargeback_minor >= 0);
ALTER TABLE payment ADD CONSTRAINT payment_status_check
  CHECK (status IN (
    'pending', 'cancellation_requested', 'authorized', 'captured', 'paid',
    'failed', 'refunded', 'chargeback', 'void_in_progress', 'voided'
  ));
CREATE UNIQUE INDEX payment_authorization_activity_key_unique
  ON payment(authorization_activity_key)
  WHERE authorization_activity_key IS NOT NULL;
ALTER TABLE payment ADD CONSTRAINT payment_amount_accounting_check
  CHECK (
    captured_minor <= amount_minor
    AND refunded_minor <= captured_minor
    AND chargeback_minor <= captured_minor
  );

ALTER TABLE payment_event
  ADD COLUMN provider_status text,
  ADD COLUMN provider_reference text,
  ADD COLUMN provider_amount_minor bigint CHECK (provider_amount_minor >= 0),
  ADD COLUMN provider_currency text
    CHECK (provider_currency IS NULL OR provider_currency ~ '^[A-Z]{3}$');

ALTER TABLE refund
  ADD COLUMN return_request_id uuid,
  ADD COLUMN currency text NOT NULL DEFAULT 'USD' CHECK (currency ~ '^[A-Z]{3}$'),
  ADD COLUMN provider_refund_id text;
CREATE UNIQUE INDEX refund_provider_refund_id_unique
  ON refund(provider_refund_id) WHERE provider_refund_id IS NOT NULL;

ALTER TABLE shipment DROP CONSTRAINT shipment_pkey;
ALTER TABLE shipment ALTER COLUMN id DROP DEFAULT;
ALTER TABLE shipment
  ALTER COLUMN id TYPE uuid USING gen_random_uuid();
ALTER TABLE shipment ALTER COLUMN id SET DEFAULT gen_random_uuid();
ALTER TABLE shipment ADD PRIMARY KEY (id);
ALTER TABLE shipment
  ADD COLUMN allocation_id uuid,
  ADD COLUMN stock_location_code text,
  ADD COLUMN shipment_key text,
  ADD COLUMN status text NOT NULL DEFAULT 'packed',
  ADD COLUMN currency text NOT NULL DEFAULT 'USD' CHECK (currency ~ '^[A-Z]{3}$'),
  ADD COLUMN shipped_value_minor bigint NOT NULL DEFAULT 0 CHECK (shipped_value_minor >= 0),
  ADD COLUMN carrier_shipment_reference text,
  ADD COLUMN delivered_at timestamptz;
ALTER TABLE shipment ADD CONSTRAINT shipment_status_check
  CHECK (status IN ('packed', 'label_created', 'label_failed', 'shipped', 'delivered'));
CREATE UNIQUE INDEX shipment_key_unique
  ON shipment(shipment_key) WHERE shipment_key IS NOT NULL;

CREATE VIEW cart_checkout_context AS
SELECT
  cart.id AS cart_id,
  cart.customer_id,
  cart.status,
  'standard'::text AS customer_tier,
  'web'::text AS sales_channel,
  false AS coupon_present,
  COALESCE(address.country_code, 'US') AS destination_country_code
FROM cart
LEFT JOIN LATERAL (
  SELECT customer_address.country_code
  FROM customer_address
  WHERE customer_address.customer_id = cart.customer_id
  ORDER BY customer_address.id
  LIMIT 1
) AS address ON true;

CREATE VIEW cart_price_candidate AS
SELECT
  cart_line.cart_id,
  cart.customer_id,
  'retail'::text AS price_list_code,
  row_number() OVER (
    PARTITION BY cart_line.cart_id
    ORDER BY cart_line.id, cart_line.variant_id
  )::integer AS line_sequence,
  product_variant.id AS variant_id,
  product_variant.sku,
  product.title,
  cart_line.quantity,
  category.slug AS taxable_category,
  product_variant.price_minor AS unit_price_minor,
  product_variant.price_minor * cart_line.quantity AS line_subtotal_minor,
  product_variant.currency
FROM cart_line
JOIN cart ON cart.id = cart_line.cart_id
JOIN product_variant ON product_variant.id = cart_line.variant_id
JOIN product ON product.id = product_variant.product_id
JOIN category ON category.id = product.category_id
WHERE product.status = 'published' AND product_variant.active;

CREATE TABLE checkout_quote (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  cart_id bigint NOT NULL REFERENCES cart(id),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  price_list_code text NOT NULL,
  promotion_code text,
  discount_bps integer NOT NULL CHECK (discount_bps BETWEEN 0 AND 10000),
  shipping_service_code text NOT NULL,
  subtotal_minor bigint NOT NULL CHECK (subtotal_minor >= 0),
  discount_minor bigint NOT NULL CHECK (discount_minor >= 0),
  shipping_minor bigint NOT NULL CHECK (shipping_minor >= 0),
  taxable_minor bigint NOT NULL CHECK (taxable_minor >= 0),
  tax_minor bigint CHECK (tax_minor IS NULL OR tax_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  destination_country_code text NOT NULL CHECK (destination_country_code ~ '^[A-Z]{2}$'),
  tax_quote_id text,
  tax_code text,
  status text NOT NULL CHECK (status IN ('awaiting_tax', 'consumed')),
  created_at timestamptz NOT NULL DEFAULT now(),
  CHECK (discount_minor <= subtotal_minor),
  CHECK (taxable_minor = subtotal_minor - discount_minor),
  UNIQUE (cart_id, id)
);
ALTER TABLE orders
  ADD CONSTRAINT orders_checkout_quote_id_fkey
  FOREIGN KEY (checkout_quote_id) REFERENCES checkout_quote(id);

CREATE TABLE checkout_quote_line (
  checkout_quote_id uuid NOT NULL REFERENCES checkout_quote(id) ON DELETE CASCADE,
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  taxable_category text NOT NULL,
  tax_code text NOT NULL,
  tax_bps integer NOT NULL CHECK (tax_bps BETWEEN 0 AND 10000),
  unit_price_minor bigint NOT NULL CHECK (unit_price_minor >= 0),
  line_subtotal_minor bigint NOT NULL CHECK (line_subtotal_minor >= 0),
  discount_minor bigint NOT NULL CHECK (discount_minor >= 0),
  taxable_minor bigint NOT NULL CHECK (taxable_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  PRIMARY KEY (checkout_quote_id, variant_id),
  CHECK (line_subtotal_minor = unit_price_minor * quantity),
  CHECK (discount_minor <= line_subtotal_minor),
  CHECK (taxable_minor = line_subtotal_minor - discount_minor)
);

CREATE TABLE order_adjustment (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  kind text NOT NULL CHECK (kind IN ('promotion', 'shipping', 'tax', 'price_list')),
  code text,
  provider_reference text,
  amount_minor bigint NOT NULL,
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$')
);
CREATE UNIQUE INDEX order_adjustment_identity_unique
  ON order_adjustment (
    order_id, kind, COALESCE(code, ''), COALESCE(provider_reference, '')
  );

CREATE TABLE payment_authorization (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  provider_authorization_id text NOT NULL UNIQUE,
  authorized_minor bigint NOT NULL CHECK (authorized_minor >= 0),
  captured_minor bigint NOT NULL DEFAULT 0 CHECK (captured_minor >= 0),
  capture_reserved_minor bigint NOT NULL DEFAULT 0 CHECK (capture_reserved_minor >= 0),
  status text NOT NULL CHECK (status IN ('authorized', 'void_in_progress', 'voided')),
  CHECK (captured_minor + capture_reserved_minor <= authorized_minor)
);
CREATE UNIQUE INDEX payment_one_effective_authorization
  ON payment_authorization(payment_id)
  WHERE status IN ('authorized', 'void_in_progress');

CREATE TABLE payment_capture_claim (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  authorization_id uuid NOT NULL REFERENCES payment_authorization(id) ON DELETE CASCADE,
  shipment_id uuid NOT NULL REFERENCES shipment(id),
  amount_minor bigint NOT NULL CHECK (amount_minor > 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  status text NOT NULL CHECK (status IN ('reserved', 'captured', 'terminal_absence')),
  provider_capture_id text,
  provider_absence_id text,
  UNIQUE (authorization_id, shipment_id, amount_minor)
);
CREATE UNIQUE INDEX payment_capture_claim_provider_capture_unique
  ON payment_capture_claim(provider_capture_id) WHERE provider_capture_id IS NOT NULL;
CREATE UNIQUE INDEX payment_capture_claim_provider_absence_unique
  ON payment_capture_claim(provider_absence_id) WHERE provider_absence_id IS NOT NULL;

CREATE TABLE payment_capture (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  authorization_id uuid NOT NULL REFERENCES payment_authorization(id),
  provider_capture_id text NOT NULL UNIQUE,
  amount_minor bigint NOT NULL CHECK (amount_minor > 0),
  status text NOT NULL CHECK (status = 'captured')
);

CREATE TABLE payment_void (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  authorization_id uuid NOT NULL REFERENCES payment_authorization(id),
  provider_void_id text NOT NULL UNIQUE,
  status text NOT NULL CHECK (status = 'voided')
);

CREATE TABLE payment_chargeback (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  provider_chargeback_id text NOT NULL UNIQUE,
  amount_minor bigint NOT NULL CHECK (amount_minor > 0),
  status text NOT NULL CHECK (status = 'chargeback')
);

CREATE TABLE payment_reconciliation (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  provider_event_id text NOT NULL UNIQUE,
  provider_reference text,
  provider_status text NOT NULL,
  provider_amount_minor bigint NOT NULL CHECK (provider_amount_minor >= 0),
  provider_currency text NOT NULL CHECK (provider_currency ~ '^[A-Z]{3}$'),
  decision text,
  status text NOT NULL CHECK (status IN ('review_required', 'resolved'))
);

CREATE TABLE payment_reconciliation_resolution (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  reconciliation_id uuid NOT NULL REFERENCES payment_reconciliation(id) ON DELETE CASCADE,
  resolution_id uuid NOT NULL UNIQUE,
  actor_role text NOT NULL,
  actor_user_id text NOT NULL,
  note text
);

CREATE TABLE payment_fraud_review (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  provider_event_id text NOT NULL,
  fraud_score integer NOT NULL CHECK (fraud_score BETWEEN 0 AND 10000),
  route text NOT NULL,
  status text NOT NULL CHECK (status IN ('review_required', 'approved', 'rejected')),
  UNIQUE (payment_id, provider_event_id)
);

CREATE TABLE payment_fraud_decision (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  fraud_review_id uuid NOT NULL REFERENCES payment_fraud_review(id) ON DELETE CASCADE,
  decision text NOT NULL CHECK (decision IN ('routed', 'approved', 'rejected')),
  decision_id uuid,
  decision_route text,
  actor_role text,
  actor_user_id text,
  note text,
  provider_event_id text
);
CREATE UNIQUE INDEX payment_fraud_decision_id_unique
  ON payment_fraud_decision(decision_id) WHERE decision_id IS NOT NULL;

CREATE TABLE notification_delivery (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  notification_id uuid NOT NULL,
  channel text NOT NULL CHECK (channel IN ('email', 'webhook')),
  status text NOT NULL,
  provider_message_id text,
  occurred_at timestamptz NOT NULL
);
CREATE UNIQUE INDEX notification_delivery_provider_message_unique
  ON notification_delivery(provider_message_id) WHERE provider_message_id IS NOT NULL;

CREATE VIEW order_current_authorization AS
SELECT DISTINCT ON (payment.order_id)
  payment.order_id,
  orders.customer_id,
  payment.id AS payment_id,
  payment_authorization.id AS authorization_id,
  payment.currency
FROM payment
JOIN orders ON orders.id = payment.order_id
JOIN payment_authorization ON payment_authorization.payment_id = payment.id
WHERE payment.status IN ('authorized', 'captured')
  AND payment_authorization.status IN ('authorized', 'void_in_progress')
ORDER BY payment.order_id, payment.internal_id DESC, payment_authorization.id;

CREATE TABLE inventory_level (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  location_code text NOT NULL,
  on_hand_quantity integer NOT NULL CHECK (on_hand_quantity >= 0),
  reserved_quantity integer NOT NULL DEFAULT 0 CHECK (reserved_quantity >= 0),
  UNIQUE (variant_id, location_code),
  CHECK (reserved_quantity <= on_hand_quantity)
);
INSERT INTO inventory_level (variant_id, location_code, on_hand_quantity, reserved_quantity)
SELECT variant_id, 'main', on_hand, reserved FROM inventory_stock;

CREATE VIEW order_inventory_allocation_candidate AS
SELECT
  order_line.order_id,
  order_line.id AS order_line_id,
  row_number() OVER (
    PARTITION BY order_line.order_id
    ORDER BY order_line.id, inventory_level.location_code, inventory_level.id
  )::integer AS line_sequence,
  order_line.variant_id,
  order_line.quantity AS requested_quantity,
  inventory_level.location_code,
  inventory_level.id AS inventory_level_id,
  inventory_level.on_hand_quantity - inventory_level.reserved_quantity AS available_quantity,
  order_line.unit_price_minor,
  order_line.currency
FROM order_line
JOIN inventory_level ON inventory_level.variant_id = order_line.variant_id
WHERE inventory_level.on_hand_quantity > inventory_level.reserved_quantity;

CREATE TABLE inventory_allocation (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  allocation_id uuid NOT NULL,
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  order_line_id uuid NOT NULL REFERENCES order_line(id) ON DELETE CASCADE,
  inventory_level_id uuid NOT NULL REFERENCES inventory_level(id),
  stock_location_code text NOT NULL,
  quantity integer NOT NULL CHECK (quantity > 0),
  unit_price_minor bigint NOT NULL CHECK (unit_price_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  status text NOT NULL CHECK (status IN ('allocated', 'packed')),
  packed_at timestamptz,
  UNIQUE (allocation_id, order_line_id, inventory_level_id)
);

CREATE VIEW inventory_allocation_line AS
SELECT
  allocation_id,
  order_line_id,
  row_number() OVER (
    PARTITION BY allocation_id
    ORDER BY order_line_id, inventory_level_id
  )::integer AS line_sequence,
  order_line.variant_id,
  inventory_allocation.quantity,
  inventory_allocation.unit_price_minor,
  inventory_allocation.quantity * inventory_allocation.unit_price_minor AS line_value_minor,
  inventory_allocation.currency
FROM inventory_allocation
JOIN order_line ON order_line.id = inventory_allocation.order_line_id;

CREATE TABLE inventory_backorder (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  order_line_id uuid NOT NULL REFERENCES order_line(id) ON DELETE CASCADE,
  requested_quantity integer NOT NULL CHECK (requested_quantity > 0),
  backordered_quantity integer NOT NULL CHECK (backordered_quantity > 0),
  status text NOT NULL CHECK (status = 'open'),
  CHECK (backordered_quantity <= requested_quantity),
  UNIQUE (order_id, order_line_id)
);

CREATE TABLE shipment_item (
  shipment_id uuid NOT NULL REFERENCES shipment(id) ON DELETE CASCADE,
  order_line_id uuid NOT NULL REFERENCES order_line(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  unit_price_minor bigint NOT NULL CHECK (unit_price_minor >= 0),
  line_value_minor bigint NOT NULL CHECK (line_value_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  PRIMARY KEY (shipment_id, order_line_id),
  CHECK (line_value_minor = quantity * unit_price_minor)
);

CREATE TABLE shipment_result (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  shipment_id uuid NOT NULL REFERENCES shipment(id) ON DELETE CASCADE,
  provider_event_id text NOT NULL UNIQUE,
  outcome text NOT NULL CHECK (outcome IN (
    'label_created', 'label_failed', 'delivered', 'delivery_failed'
  )),
  tracking_number text,
  carrier_shipment_reference text,
  failure_code text,
  failure_message text,
  occurred_at timestamptz NOT NULL DEFAULT now()
);

CREATE VIEW order_return_context AS
SELECT DISTINCT ON (orders.id)
  orders.id AS order_id,
  orders.customer_id,
  payment.id AS payment_id,
  orders.currency,
  jsonb_build_object(
    'recipient_name', customer.name,
    'address_line_1', customer_address.line1,
    'address_line_2', customer_address.line2,
    'city', customer_address.city,
    'region', '',
    'postal_code', customer_address.postal_code,
    'country_code', customer_address.country_code
  ) AS return_from
FROM orders
JOIN payment ON payment.order_id = orders.id
JOIN customer ON customer.customer_id = orders.customer_id
JOIN customer_address ON customer_address.customer_id = orders.customer_id
WHERE payment.status IN ('captured', 'paid', 'refunded')
ORDER BY orders.id, payment.internal_id DESC, customer_address.id;

CREATE TABLE return_request (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id uuid NOT NULL REFERENCES orders(id),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  replacement_requested boolean NOT NULL DEFAULT false,
  reason text NOT NULL,
  status text NOT NULL CHECK (status IN (
    'requested', 'approved', 'received', 'inspected',
    'refunded', 'rejected', 'exchanged'
  ))
);

CREATE TABLE return_item (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  return_request_id uuid NOT NULL REFERENCES return_request(id) ON DELETE CASCADE,
  order_line_id uuid NOT NULL REFERENCES order_line(id),
  requested_quantity integer NOT NULL CHECK (requested_quantity > 0),
  approved_quantity integer NOT NULL DEFAULT 0 CHECK (approved_quantity >= 0),
  received_quantity integer NOT NULL DEFAULT 0 CHECK (received_quantity >= 0),
  inspected_quantity integer NOT NULL DEFAULT 0 CHECK (inspected_quantity >= 0),
  UNIQUE (return_request_id, order_line_id),
  CHECK (approved_quantity <= requested_quantity),
  CHECK (received_quantity <= approved_quantity),
  CHECK (inspected_quantity <= received_quantity)
);

CREATE TABLE return_event (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  return_request_id uuid NOT NULL REFERENCES return_request(id) ON DELETE CASCADE,
  event_type text NOT NULL CHECK (event_type IN (
    'requested', 'approved', 'received', 'inspected', 'rejected', 'exchanged'
  )),
  actor_role text NOT NULL,
  request_id uuid NOT NULL,
  reason text,
  note text,
  occurred_at timestamptz NOT NULL DEFAULT now(),
  inspection_id uuid,
  exchange_id uuid,
  UNIQUE (return_request_id, request_id)
);

CREATE TABLE return_inspection (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  return_request_id uuid NOT NULL REFERENCES return_request(id) ON DELETE CASCADE,
  inspection_id uuid NOT NULL UNIQUE,
  decision text NOT NULL CHECK (decision IN ('accepted', 'restock', 'exchange', 'rejected')),
  refund_amount_minor bigint NOT NULL CHECK (refund_amount_minor >= 0),
  note text
);

ALTER TABLE refund
  ADD CONSTRAINT refund_return_request_id_fkey
  FOREIGN KEY (return_request_id) REFERENCES return_request(id);

CREATE VIEW return_refund_context AS
SELECT
  return_request.id AS return_id,
  return_request.order_id,
  payment.id AS payment_id,
  payment.currency,
  return_request.status,
  return_request.replacement_requested,
  GREATEST(payment.captured_minor - payment.refunded_minor, 0) AS eligible_refund_minor
FROM return_request
JOIN payment ON payment.order_id = return_request.order_id
WHERE payment.status IN ('captured', 'paid', 'refunded');

CREATE TABLE exchange (
  id uuid PRIMARY KEY,
  return_request_id uuid NOT NULL UNIQUE REFERENCES return_request(id),
  order_id uuid NOT NULL REFERENCES orders(id),
  status text NOT NULL CHECK (status = 'requested')
);

CREATE TABLE exchange_item (
  exchange_id uuid NOT NULL REFERENCES exchange(id) ON DELETE CASCADE,
  return_item_id uuid NOT NULL REFERENCES return_item(id),
  order_line_id uuid NOT NULL REFERENCES order_line(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  PRIMARY KEY (exchange_id, return_item_id)
);

CREATE TABLE subscription (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  unit_price_minor bigint NOT NULL CHECK (unit_price_minor >= 0),
  line_total_minor bigint NOT NULL CHECK (line_total_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  status text NOT NULL CHECK (status IN ('active', 'payment_due', 'paused', 'cancelled')),
  pause_reason text,
  cancellation_reason text,
  CHECK (line_total_minor = unit_price_minor * quantity)
);

CREATE TABLE subscription_renewal (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  subscription_id uuid NOT NULL REFERENCES subscription(id) ON DELETE CASCADE,
  cron_occurrence timestamptz NOT NULL,
  status text NOT NULL CHECK (status IN ('payment_due', 'dunning', 'confirmed')),
  UNIQUE (subscription_id, cron_occurrence)
);
ALTER TABLE orders
  ADD CONSTRAINT orders_subscription_renewal_id_fkey
  FOREIGN KEY (subscription_renewal_id) REFERENCES subscription_renewal(id);

CREATE TABLE subscription_dunning_attempt (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  renewal_id uuid NOT NULL REFERENCES subscription_renewal(id) ON DELETE CASCADE,
  attempt integer NOT NULL CHECK (attempt > 0),
  outcome text NOT NULL,
  provider_event_id text,
  provider_reference text,
  payload jsonb NOT NULL DEFAULT '{}'::jsonb,
  occurred_at timestamptz NOT NULL,
  UNIQUE (renewal_id, attempt)
);
CREATE UNIQUE INDEX subscription_dunning_provider_event_unique
  ON subscription_dunning_attempt(provider_event_id)
  WHERE provider_event_id IS NOT NULL;

CREATE TABLE organization (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  available_credit_minor bigint NOT NULL DEFAULT 0 CHECK (available_credit_minor >= 0),
  consumed_credit_minor bigint NOT NULL DEFAULT 0 CHECK (consumed_credit_minor >= 0)
);
CREATE TABLE organization_membership (
  organization_id uuid NOT NULL REFERENCES organization(id) ON DELETE CASCADE,
  user_id text NOT NULL REFERENCES customer(customer_id),
  PRIMARY KEY (organization_id, user_id)
);
CREATE TABLE quote (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  organization_id uuid NOT NULL REFERENCES organization(id),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  cart_id bigint NOT NULL REFERENCES cart(id),
  status text NOT NULL CHECK (status = 'submitted'),
  total_minor bigint NOT NULL CHECK (total_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  available_credit_minor bigint NOT NULL CHECK (available_credit_minor >= 0)
);
CREATE TABLE quote_line (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  quote_id uuid NOT NULL REFERENCES quote(id) ON DELETE CASCADE,
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  sku text NOT NULL,
  title text NOT NULL,
  quantity integer NOT NULL CHECK (quantity > 0),
  unit_price_minor bigint NOT NULL CHECK (unit_price_minor >= 0),
  line_total_minor bigint NOT NULL CHECK (line_total_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  UNIQUE (quote_id, variant_id),
  CHECK (line_total_minor = unit_price_minor * quantity)
);
CREATE TABLE purchase_approval (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  quote_id uuid NOT NULL UNIQUE REFERENCES quote(id),
  status text NOT NULL CHECK (status IN (
    'submitted', 'awaiting_approver', 'awaiting_finance', 'approved', 'rejected'
  )),
  approved_by_user_id text,
  approved_by_role text,
  rejected_by_user_id text,
  rejected_by_role text,
  rejection_reason text
);
CREATE TABLE credit_usage (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  organization_id uuid NOT NULL REFERENCES organization(id),
  quote_id uuid NOT NULL REFERENCES quote(id),
  approval_id uuid NOT NULL UNIQUE REFERENCES purchase_approval(id),
  amount_minor bigint NOT NULL CHECK (amount_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$')
);

CREATE VIEW order_vendor_split_candidate AS
SELECT
  order_line.order_id,
  order_line.id AS order_line_id,
  row_number() OVER (
    PARTITION BY order_line.order_id ORDER BY order_line.id, product_variant.sku
  )::integer AS line_sequence,
  md5('offer:' || product_variant.sku)::uuid AS offer_id,
  md5('vendor:' || category.slug)::uuid AS vendor_id,
  category.slug AS product_category,
  order_line.line_subtotal_minor AS gross_minor,
  order_line.currency
FROM order_line
JOIN product_variant ON product_variant.id = order_line.variant_id
JOIN product ON product.id = product_variant.product_id
JOIN category ON category.id = product.category_id;

CREATE TABLE vendor_order (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id uuid NOT NULL REFERENCES orders(id),
  order_line_id uuid NOT NULL REFERENCES order_line(id),
  vendor_id uuid NOT NULL,
  offer_id uuid NOT NULL,
  line_sequence integer NOT NULL CHECK (line_sequence > 0),
  product_category text NOT NULL,
  gross_minor bigint NOT NULL CHECK (gross_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  commission_tier text NOT NULL,
  commission_bps integer NOT NULL CHECK (commission_bps BETWEEN 0 AND 10000),
  status text NOT NULL CHECK (status IN ('pending_acceptance', 'accepted')),
  UNIQUE (order_line_id, vendor_id, offer_id)
);
CREATE TABLE vendor_membership (
  vendor_id uuid NOT NULL,
  user_id text NOT NULL REFERENCES customer(customer_id),
  PRIMARY KEY (vendor_id, user_id)
);
CREATE TABLE vendor_order_acceptance (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  vendor_order_id uuid NOT NULL UNIQUE REFERENCES vendor_order(id),
  vendor_id uuid NOT NULL,
  accepted_by_user_id text NOT NULL,
  acceptance_id uuid NOT NULL UNIQUE
);
CREATE TABLE vendor_payout (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payout_cycle_id uuid NOT NULL,
  vendor_id uuid NOT NULL,
  payout_key text NOT NULL UNIQUE,
  vendor_order_count integer NOT NULL CHECK (vendor_order_count > 0),
  gross_minor bigint NOT NULL CHECK (gross_minor >= 0),
  commission_minor bigint NOT NULL CHECK (commission_minor >= 0),
  net_minor bigint NOT NULL CHECK (net_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  provider_payout_id text,
  status text NOT NULL CHECK (status IN ('pending', 'paid', 'failed', 'reconciled')),
  CHECK (net_minor = gross_minor - commission_minor)
);
CREATE UNIQUE INDEX vendor_payout_provider_id_unique
  ON vendor_payout(provider_payout_id) WHERE provider_payout_id IS NOT NULL;
CREATE VIEW vendor_payout_candidate AS
SELECT
  '00000000-0000-0000-0000-000000000001'::uuid AS payout_cycle_id,
  vendor_id,
  ('00000000-0000-0000-0000-000000000001:' || vendor_id::text || ':' || currency)::text AS payout_key,
  count(*)::integer AS vendor_order_count,
  sum(gross_minor)::bigint AS gross_minor,
  sum((gross_minor * commission_bps) / 10000)::bigint AS commission_minor,
  sum(gross_minor - ((gross_minor * commission_bps) / 10000))::bigint AS net_minor,
  currency
FROM vendor_order
WHERE status = 'accepted'
GROUP BY vendor_id, currency;
CREATE TABLE vendor_payout_event (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payout_id uuid NOT NULL REFERENCES vendor_payout(id) ON DELETE CASCADE,
  provider_event_id text NOT NULL UNIQUE,
  provider_payout_id text,
  outcome text NOT NULL CHECK (outcome IN ('paid', 'failed')),
  payload jsonb NOT NULL DEFAULT '{}'::jsonb
);
CREATE TABLE vendor_payout_reconciliation (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  payout_id uuid NOT NULL REFERENCES vendor_payout(id) ON DELETE CASCADE,
  payout_cycle_id uuid NOT NULL,
  reconciliation_id uuid NOT NULL UNIQUE,
  reconciled_by_user_id text NOT NULL,
  note text
);
CREATE TABLE vendor_dispute (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  vendor_order_id uuid NOT NULL REFERENCES vendor_order(id),
  vendor_id uuid NOT NULL,
  dispute_id uuid NOT NULL UNIQUE,
  reason text NOT NULL,
  details jsonb,
  status text NOT NULL CHECK (status = 'open')
);

CREATE TABLE grooming_booking (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  service_resource_id uuid NOT NULL,
  slot_key text NOT NULL,
  starts_at timestamptz NOT NULL,
  hold_expires_at timestamptz NOT NULL,
  status text NOT NULL CHECK (status IN ('held', 'confirmed', 'cancelled', 'expired', 'no_show')),
  cancellation_reason text
);
CREATE UNIQUE INDEX grooming_booking_active_slot_unique
  ON grooming_booking(service_resource_id, slot_key)
  WHERE status IN ('held', 'confirmed');
CREATE TABLE grooming_booking_event (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  booking_id uuid NOT NULL REFERENCES grooming_booking(id) ON DELETE CASCADE,
  event_type text NOT NULL CHECK (event_type IN (
    'held', 'confirmed', 'rescheduled', 'cancelled', 'expired', 'no_show'
  )),
  actor_role text NOT NULL CHECK (actor_role IN ('customer', 'groomer', 'booking_worker')),
  request_id uuid NOT NULL,
  reason text,
  UNIQUE (booking_id, request_id)
);

CREATE VIEW customer_prescription_order_line AS
SELECT
  order_line.id AS order_line_id,
  orders.customer_id
FROM order_line
JOIN orders ON orders.id = order_line.order_id;
CREATE TABLE prescription_request (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  order_line_id uuid NOT NULL REFERENCES order_line(id),
  review_deadline timestamptz NOT NULL,
  status text NOT NULL CHECK (status IN ('submitted', 'approved', 'rejected', 'expired'))
);
CREATE UNIQUE INDEX prescription_request_active_line_unique
  ON prescription_request(order_line_id)
  WHERE status = 'submitted';
CREATE TABLE prescription_review (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  prescription_request_id uuid NOT NULL REFERENCES prescription_request(id) ON DELETE CASCADE,
  decision text NOT NULL CHECK (decision IN ('approved', 'rejected')),
  reviewer_user_id text NOT NULL,
  decision_id uuid NOT NULL UNIQUE,
  private_note text
);
CREATE TABLE prescription_event (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  prescription_request_id uuid NOT NULL REFERENCES prescription_request(id) ON DELETE CASCADE,
  event_type text NOT NULL CHECK (event_type IN ('submitted', 'approved', 'rejected', 'expired')),
  actor_role text NOT NULL,
  request_id uuid NOT NULL,
  UNIQUE (prescription_request_id, request_id)
);

CREATE VIEW order_operations AS
SELECT
  orders.id AS order_id,
  orders.customer_id,
  orders.order_status,
  COALESCE(latest_payment.status, 'pending') AS payment_status,
  COALESCE(latest_shipment.status, 'packed') AS fulfilment_status,
  orders.total_minor,
  orders.currency
FROM orders
LEFT JOIN LATERAL (
  SELECT payment.status
  FROM payment
  WHERE payment.order_id = orders.id
  ORDER BY payment.internal_id DESC
  LIMIT 1
) AS latest_payment ON true
LEFT JOIN LATERAL (
  SELECT shipment.status
  FROM shipment
  WHERE shipment.order_id = orders.id
  ORDER BY shipment.id DESC
  LIMIT 1
) AS latest_shipment ON true;
