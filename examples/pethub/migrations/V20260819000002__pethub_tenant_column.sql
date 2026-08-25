-- Pethub: the store domain, made multitenant without editing a line of it.
--
-- Everything here is DDL. The Petshop *metadata* is composed unchanged
-- (`extends.yaml`); what a platform owes its domain is the column, and it owes
-- it once — one `donat migrate` and every tenant has it, which is what the
-- row_key binding buys and what a schema-per-tenant binding would turn into a
-- fan-out with a partial-failure story.
--
-- The column is `text` rather than one of Petshop's `petshop_required_*`
-- domains on purpose: the tenancy check compares a tenant key's type against
-- the registry's identifier, and a domain on one side of that comparison would
-- be refused for not matching the other.

-- ---------------------------------------------------------------- tables

ALTER TABLE public."cart" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."cart" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."cart" (tenant_id);
ALTER TABLE public."cart_line" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."cart_line" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."cart_line" (tenant_id);
ALTER TABLE public."category" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."category" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."category" (tenant_id);
ALTER TABLE public."checkout_quote" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."checkout_quote" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."checkout_quote" (tenant_id);
ALTER TABLE public."checkout_quote_line" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."checkout_quote_line" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."checkout_quote_line" (tenant_id);
ALTER TABLE public."credit_usage" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."credit_usage" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."credit_usage" (tenant_id);
ALTER TABLE public."customer" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."customer" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."customer" (tenant_id);
ALTER TABLE public."customer_address" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."customer_address" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."customer_address" (tenant_id);
ALTER TABLE public."exchange" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."exchange" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."exchange" (tenant_id);
ALTER TABLE public."exchange_item" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."exchange_item" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."exchange_item" (tenant_id);
ALTER TABLE public."grooming_booking" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."grooming_booking" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."grooming_booking" (tenant_id);
ALTER TABLE public."grooming_booking_event" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."grooming_booking_event" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."grooming_booking_event" (tenant_id);
ALTER TABLE public."inventory_allocation" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."inventory_allocation" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."inventory_allocation" (tenant_id);
ALTER TABLE public."inventory_backorder" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."inventory_backorder" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."inventory_backorder" (tenant_id);
ALTER TABLE public."inventory_level" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."inventory_level" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."inventory_level" (tenant_id);
ALTER TABLE public."inventory_reservation" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."inventory_reservation" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."inventory_reservation" (tenant_id);
ALTER TABLE public."inventory_stock" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."inventory_stock" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."inventory_stock" (tenant_id);
ALTER TABLE public."notification_delivery" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."notification_delivery" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."notification_delivery" (tenant_id);
ALTER TABLE public."order_adjustment" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."order_adjustment" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."order_adjustment" (tenant_id);
ALTER TABLE public."order_line" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."order_line" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."order_line" (tenant_id);
ALTER TABLE public."orders" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."orders" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."orders" (tenant_id);
ALTER TABLE public."organization" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."organization" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."organization" (tenant_id);
ALTER TABLE public."organization_membership" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."organization_membership" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."organization_membership" (tenant_id);
ALTER TABLE public."payment" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment" (tenant_id);
ALTER TABLE public."payment_authorization" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_authorization" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_authorization" (tenant_id);
ALTER TABLE public."payment_capture" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_capture" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_capture" (tenant_id);
ALTER TABLE public."payment_capture_claim" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_capture_claim" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_capture_claim" (tenant_id);
ALTER TABLE public."payment_chargeback" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_chargeback" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_chargeback" (tenant_id);
ALTER TABLE public."payment_event" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_event" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_event" (tenant_id);
ALTER TABLE public."payment_fraud_decision" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_fraud_decision" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_fraud_decision" (tenant_id);
ALTER TABLE public."payment_fraud_review" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_fraud_review" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_fraud_review" (tenant_id);
ALTER TABLE public."payment_reconciliation" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_reconciliation" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_reconciliation" (tenant_id);
ALTER TABLE public."payment_reconciliation_resolution" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_reconciliation_resolution" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_reconciliation_resolution" (tenant_id);
ALTER TABLE public."payment_void" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."payment_void" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."payment_void" (tenant_id);
ALTER TABLE public."prescription_event" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."prescription_event" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."prescription_event" (tenant_id);
ALTER TABLE public."prescription_request" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."prescription_request" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."prescription_request" (tenant_id);
ALTER TABLE public."prescription_review" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."prescription_review" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."prescription_review" (tenant_id);
ALTER TABLE public."product" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."product" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."product" (tenant_id);
ALTER TABLE public."product_variant" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."product_variant" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."product_variant" (tenant_id);
ALTER TABLE public."purchase_approval" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."purchase_approval" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."purchase_approval" (tenant_id);
ALTER TABLE public."quote" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."quote" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."quote" (tenant_id);
ALTER TABLE public."quote_line" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."quote_line" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."quote_line" (tenant_id);
ALTER TABLE public."refund" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."refund" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."refund" (tenant_id);
ALTER TABLE public."return_event" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."return_event" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."return_event" (tenant_id);
ALTER TABLE public."return_inspection" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."return_inspection" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."return_inspection" (tenant_id);
ALTER TABLE public."return_item" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."return_item" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."return_item" (tenant_id);
ALTER TABLE public."return_request" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."return_request" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."return_request" (tenant_id);
ALTER TABLE public."shipment" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."shipment" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."shipment" (tenant_id);
ALTER TABLE public."shipment_item" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."shipment_item" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."shipment_item" (tenant_id);
ALTER TABLE public."shipment_result" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."shipment_result" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."shipment_result" (tenant_id);
ALTER TABLE public."subscription" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."subscription" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."subscription" (tenant_id);
ALTER TABLE public."subscription_dunning_attempt" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."subscription_dunning_attempt" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."subscription_dunning_attempt" (tenant_id);
ALTER TABLE public."subscription_renewal" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."subscription_renewal" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."subscription_renewal" (tenant_id);
ALTER TABLE public."vendor_dispute" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_dispute" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_dispute" (tenant_id);
ALTER TABLE public."vendor_membership" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_membership" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_membership" (tenant_id);
ALTER TABLE public."vendor_order" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_order" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_order" (tenant_id);
ALTER TABLE public."vendor_order_acceptance" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_order_acceptance" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_order_acceptance" (tenant_id);
ALTER TABLE public."vendor_payout" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_payout" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_payout" (tenant_id);
ALTER TABLE public."vendor_payout_event" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_payout_event" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_payout_event" (tenant_id);
ALTER TABLE public."vendor_payout_reconciliation" ADD COLUMN tenant_id text NOT NULL DEFAULT '';
ALTER TABLE public."vendor_payout_reconciliation" ALTER COLUMN tenant_id DROP DEFAULT;
CREATE INDEX ON public."vendor_payout_reconciliation" (tenant_id);

-- ----------------------------------------------------------------- views
--
-- A view is a table to the engine: it is tracked, so it is scoped, so it has
-- to carry the column. Each is redefined from its own stored definition with
-- the tenant of its driving table appended — never cast to a domain, for the
-- reason above. `CREATE OR REPLACE` keeps the existing columns in place and
-- adds this one at the end, so nothing that reads these views sees a change.

CREATE OR REPLACE VIEW public."cart_checkout_context" AS SELECT cart.id::petshop_required_int8 AS cart_id,
    cart.customer_id::petshop_required_text AS customer_id,
    cart.status::petshop_required_text AS status,
    'standard'::text::petshop_required_text AS customer_tier,
    'web'::text::petshop_required_text AS sales_channel,
    false::petshop_required_bool AS coupon_present,
    COALESCE(address.country_code, 'US'::text)::petshop_required_text AS destination_country_code
,
    cart.tenant_id AS tenant_id
   FROM cart
     LEFT JOIN LATERAL ( SELECT customer_address.country_code
           FROM customer_address
          WHERE customer_address.customer_id = cart.customer_id
          ORDER BY customer_address.id
         LIMIT 1) address ON true;

CREATE OR REPLACE VIEW public."cart_price_candidate" AS SELECT cart_line.cart_id::petshop_required_int8 AS cart_id,
    cart.customer_id::petshop_required_text AS customer_id,
    'retail_usd'::text::petshop_required_text AS price_list_code,
    row_number() OVER (PARTITION BY cart_line.cart_id ORDER BY cart_line.id, cart_line.variant_id)::integer::petshop_required_int4 AS line_sequence,
    product_variant.id::petshop_required_int8 AS variant_id,
    product_variant.sku::petshop_required_text AS sku,
    product.title::petshop_required_text AS title,
    cart_line.quantity::petshop_required_int4 AS quantity,
    category.slug::petshop_required_text AS taxable_category,
    product_variant.price_minor::petshop_required_int8 AS unit_price_minor,
    (product_variant.price_minor * cart_line.quantity)::petshop_required_int8 AS line_subtotal_minor,
    product_variant.currency::petshop_required_text AS currency
,
    cart_line.tenant_id AS tenant_id
   FROM cart_line
     JOIN cart ON cart.id = cart_line.cart_id
     JOIN product_variant ON product_variant.id = cart_line.variant_id
     JOIN product ON product.id = product_variant.product_id
     JOIN category ON category.id = product.category_id
  WHERE product.status = 'published'::text AND product_variant.active;

CREATE OR REPLACE VIEW public."cart_pricing" AS SELECT cart.id::petshop_required_int8 AS cart_id,
    cart.customer_id::petshop_required_text AS customer_id,
    cart_line.variant_id::petshop_required_int8 AS variant_id,
    product_variant.sku::petshop_required_text AS sku,
    product.title::petshop_required_text AS title,
    cart_line.quantity::petshop_required_int4 AS quantity,
    product_variant.price_minor::petshop_required_int8 AS unit_price_minor,
    product_variant.currency::petshop_required_text AS currency,
    (product_variant.price_minor * cart_line.quantity)::petshop_required_int8 AS line_total_minor,
    inventory_stock.available_quantity::petshop_required_int4 AS available_quantity
,
    cart.tenant_id AS tenant_id
   FROM cart
     JOIN cart_line ON cart_line.cart_id = cart.id
     JOIN product_variant ON product_variant.id = cart_line.variant_id
     JOIN product ON product.id = product_variant.product_id
     JOIN inventory_stock ON inventory_stock.variant_id = product_variant.id;

CREATE OR REPLACE VIEW public."customer_prescription_order_line" AS SELECT order_line.id::petshop_required_uuid AS order_line_id,
    orders.customer_id::petshop_required_text AS customer_id
,
    order_line.tenant_id AS tenant_id
   FROM order_line
     JOIN orders ON orders.id = order_line.order_id;

CREATE OR REPLACE VIEW public."inventory_allocation_group" AS SELECT allocation_id::petshop_required_uuid AS allocation_id,
    order_id::petshop_required_uuid AS order_id,
    stock_location_code::petshop_required_text AS stock_location_code,
    status::petshop_required_text AS status,
    sum(quantity)::petshop_required_int8 AS quantity,
    currency::petshop_required_text AS currency
,
    inventory_allocation.tenant_id AS tenant_id
   FROM inventory_allocation
  GROUP BY allocation_id, order_id, stock_location_code, status, currency, inventory_allocation.tenant_id;

CREATE OR REPLACE VIEW public."inventory_allocation_line" AS SELECT inventory_allocation.allocation_id::petshop_required_uuid AS allocation_id,
    inventory_allocation.order_line_id::petshop_required_uuid AS order_line_id,
    row_number() OVER (PARTITION BY inventory_allocation.allocation_id ORDER BY inventory_allocation.order_line_id, inventory_allocation.inventory_level_id)::integer::petshop_required_int4 AS line_sequence,
    order_line.variant_id::petshop_required_int8 AS variant_id,
    inventory_allocation.quantity::petshop_required_int4 AS quantity,
    inventory_allocation.unit_price_minor::petshop_required_int8 AS unit_price_minor,
    (inventory_allocation.quantity * inventory_allocation.unit_price_minor)::petshop_required_int8 AS line_value_minor,
    inventory_allocation.currency::petshop_required_text AS currency
,
    inventory_allocation.tenant_id AS tenant_id
   FROM inventory_allocation
     JOIN order_line ON order_line.id = inventory_allocation.order_line_id;

CREATE OR REPLACE VIEW public."order_current_authorization" AS SELECT DISTINCT ON (payment.order_id) payment.order_id::petshop_required_uuid AS order_id,
    orders.customer_id::petshop_required_text AS customer_id,
    payment.id::petshop_required_uuid AS payment_id,
    payment_authorization.id::petshop_required_uuid AS authorization_id,
    payment.currency::petshop_required_text AS currency
,
    payment.tenant_id AS tenant_id
   FROM payment
     JOIN orders ON orders.id = payment.order_id
     JOIN payment_authorization ON payment_authorization.payment_id = payment.id
  WHERE (payment.status = ANY (ARRAY['authorized'::text, 'captured'::text])) AND (payment_authorization.status = ANY (ARRAY['authorized'::text, 'void_in_progress'::text]))
  ORDER BY payment.order_id, payment.internal_id DESC, payment_authorization.id;

CREATE OR REPLACE VIEW public."order_inventory_allocation_candidate" AS SELECT order_line.order_id::petshop_required_uuid AS order_id,
    order_line.id::petshop_required_uuid AS order_line_id,
    row_number() OVER (PARTITION BY order_line.order_id ORDER BY order_line.id, inventory_level.location_code, inventory_level.id)::integer::petshop_required_int4 AS line_sequence,
    order_line.variant_id::petshop_required_int8 AS variant_id,
    order_line.quantity::petshop_required_int4 AS requested_quantity,
    inventory_level.location_code::petshop_required_text AS location_code,
    inventory_level.id::petshop_required_uuid AS inventory_level_id,
    (inventory_level.on_hand_quantity - inventory_level.reserved_quantity)::petshop_required_int4 AS available_quantity,
    order_line.unit_price_minor::petshop_required_int8 AS unit_price_minor,
    order_line.currency::petshop_required_text AS currency
,
    order_line.tenant_id AS tenant_id
   FROM order_line
     JOIN inventory_level ON inventory_level.variant_id = order_line.variant_id
  WHERE inventory_level.on_hand_quantity > inventory_level.reserved_quantity;

CREATE OR REPLACE VIEW public."order_operations" AS SELECT orders.id AS order_id,
    orders.customer_id,
    orders.order_status,
    COALESCE(latest_payment.status, 'pending'::text) AS payment_status,
    COALESCE(latest_shipment.status, 'packed'::text) AS fulfilment_status,
    orders.total_minor,
    orders.currency
,
    orders.tenant_id AS tenant_id
   FROM orders
     LEFT JOIN LATERAL ( SELECT payment.status
           FROM payment
          WHERE payment.order_id = orders.id
          ORDER BY payment.internal_id DESC
         LIMIT 1) latest_payment ON true
     LEFT JOIN LATERAL ( SELECT shipment.status
           FROM shipment
          WHERE shipment.order_id = orders.id
          ORDER BY shipment.id DESC
         LIMIT 1) latest_shipment ON true;

CREATE OR REPLACE VIEW public."order_return_context" AS SELECT DISTINCT ON (orders.id) orders.id::petshop_required_uuid AS order_id,
    orders.customer_id::petshop_required_text AS customer_id,
    payment.id::petshop_required_uuid AS payment_id,
    orders.currency::petshop_required_text AS currency,
    jsonb_build_object('recipient_name', customer.name, 'address_line_1', customer_address.line1, 'address_line_2', COALESCE(customer_address.line2, ''::text), 'city', customer_address.city, 'region', '', 'postal_code', customer_address.postal_code, 'country_code', customer_address.country_code)::petshop_required_jsonb AS return_from,
    customer.name::petshop_required_text AS recipient_name,
    customer_address.line1::petshop_required_text AS address_line_1,
    COALESCE(customer_address.line2, ''::text)::petshop_required_text AS address_line_2,
    customer_address.city::petshop_required_text AS city,
    ''::text::petshop_required_text AS region,
    customer_address.postal_code::petshop_required_text AS postal_code,
    customer_address.country_code::petshop_required_text AS country_code
,
    orders.tenant_id AS tenant_id
   FROM orders
     JOIN payment ON payment.order_id = orders.id
     JOIN customer ON customer.customer_id = orders.customer_id
     JOIN customer_address ON customer_address.customer_id = orders.customer_id
  WHERE payment.status = ANY (ARRAY['captured'::text, 'paid'::text, 'refunded'::text])
  ORDER BY orders.id, payment.internal_id DESC, customer_address.id;

CREATE OR REPLACE VIEW public."order_vendor_split_candidate" AS SELECT order_line.order_id::petshop_required_uuid AS order_id,
    order_line.id::petshop_required_uuid AS order_line_id,
    row_number() OVER (PARTITION BY order_line.order_id ORDER BY order_line.id, product_variant.sku)::integer::petshop_required_int4 AS line_sequence,
    md5('offer:'::text || product_variant.sku)::uuid::petshop_required_uuid AS offer_id,
    md5('vendor:'::text || category.slug)::uuid::petshop_required_uuid AS vendor_id,
    category.slug::petshop_required_text AS product_category,
    order_line.line_subtotal_minor::petshop_required_int8 AS gross_minor,
    order_line.currency::petshop_required_text AS currency
,
    order_line.tenant_id AS tenant_id
   FROM order_line
     JOIN product_variant ON product_variant.id = order_line.variant_id
     JOIN product ON product.id = product_variant.product_id
     JOIN category ON category.id = product.category_id;

CREATE OR REPLACE VIEW public."payment_reconciliation_candidate" AS SELECT id::petshop_required_uuid AS payment_id,
    status::petshop_required_text AS status,
    amount_minor::petshop_required_int8 AS amount_minor,
    currency::petshop_required_text AS currency,
    provider_reference::petshop_required_text AS provider_reference
,
    payment.tenant_id AS tenant_id
   FROM payment
  WHERE provider_reference IS NOT NULL;

CREATE OR REPLACE VIEW public."return_refund_context" AS SELECT return_request.id::petshop_required_uuid AS return_id,
    return_request.order_id::petshop_required_uuid AS order_id,
    payment.id::petshop_required_uuid AS payment_id,
    payment.currency::petshop_required_text AS currency,
    return_request.status::petshop_required_text AS status,
    return_request.replacement_requested::petshop_required_bool AS replacement_requested,
    GREATEST(payment.captured_minor - payment.refunded_minor, 0::bigint)::petshop_required_int8 AS eligible_refund_minor
,
    return_request.tenant_id AS tenant_id
   FROM return_request
     JOIN payment ON payment.order_id = return_request.order_id
  WHERE payment.status = ANY (ARRAY['captured'::text, 'paid'::text, 'refunded'::text]);

CREATE OR REPLACE VIEW public."vendor_payout_candidate" AS SELECT '00000000-0000-0000-0000-000000000001'::uuid::petshop_required_uuid AS payout_cycle_id,
    vendor_id::petshop_required_uuid AS vendor_id,
    ((('00000000-0000-0000-0000-000000000001:'::text || vendor_id::text) || ':'::text) || currency)::petshop_required_text AS payout_key,
    count(*)::integer::petshop_required_int4 AS vendor_order_count,
    sum(gross_minor)::bigint::petshop_required_int8 AS gross_minor,
    sum(gross_minor * commission_bps / 10000)::bigint::petshop_required_int8 AS commission_minor,
    sum(gross_minor - gross_minor * commission_bps / 10000)::bigint::petshop_required_int8 AS net_minor,
    currency::petshop_required_text AS currency
,
    vendor_order.tenant_id AS tenant_id
   FROM vendor_order
  WHERE status = 'accepted'::text
  GROUP BY vendor_id, currency, vendor_order.tenant_id;


-- ------------------------------------------------------- natural keys
--
-- A unique constraint over a *natural* key — a string a person chose — becomes
-- a collision between stores the moment there are two of them: the second
-- merchant to want the slug `dog-food` is refused, and the refusal tells them
-- somebody else has it. Both are wrong, and the second is a disclosure.
--
-- Surrogate keys are left alone deliberately. `UNIQUE (cart_id, variant_id)`
-- is already per tenant because a cart id is, and a uuid or a bigserial does
-- not repeat across stores. Scoping those would add a column to an index for
-- no property gained.
--
-- Provider-supplied identifiers (`provider_event_id` and its neighbours) are
-- also left alone, and that is a consequence of a deferral rather than a
-- judgement: one deployment holds one payment account, so its identifiers are
-- unique across the stores that share it. Per-tenant connector credentials
-- would change that, and would have to change these with them.
ALTER TABLE public.category DROP CONSTRAINT category_slug_key;
ALTER TABLE public.category ADD CONSTRAINT category_tenant_slug_key UNIQUE (tenant_id, slug);

ALTER TABLE public.product DROP CONSTRAINT product_slug_key;
ALTER TABLE public.product ADD CONSTRAINT product_tenant_slug_key UNIQUE (tenant_id, slug);

ALTER TABLE public.product_variant DROP CONSTRAINT product_variant_sku_key;
ALTER TABLE public.product_variant ADD CONSTRAINT product_variant_tenant_sku_key UNIQUE (tenant_id, sku);

-- --------------------------------------------- a customer belongs to a store
--
-- `customer.customer_id` is the identity a person signs in with, and the same
-- person shops at two stores. Kept globally unique it would make the second
-- store's sign-up fail because the first store's customer got there first —
-- and the failure would say so, which is a disclosure as well as a bug.
--
-- Scoping it means every reference to it becomes composite, and that is worth
-- more than the constraint it replaces: `(tenant_id, customer_id)` makes a
-- cart in one store unable to name a customer in another *in the database*,
-- underneath the predicate rather than beside it. The predicate is what a
-- request is bounded by; this is what remains true if one is ever missing.

ALTER TABLE public."cart" DROP CONSTRAINT "cart_customer_id_fkey";
ALTER TABLE public."checkout_quote" DROP CONSTRAINT "checkout_quote_customer_id_fkey";
ALTER TABLE public."customer_address" DROP CONSTRAINT "customer_address_customer_id_fkey";
ALTER TABLE public."grooming_booking" DROP CONSTRAINT "grooming_booking_customer_id_fkey";
ALTER TABLE public."orders" DROP CONSTRAINT "orders_customer_id_fkey";
ALTER TABLE public."organization_membership" DROP CONSTRAINT "organization_membership_user_id_fkey";
ALTER TABLE public."prescription_request" DROP CONSTRAINT "prescription_request_customer_id_fkey";
ALTER TABLE public."quote" DROP CONSTRAINT "quote_customer_id_fkey";
ALTER TABLE public."return_request" DROP CONSTRAINT "return_request_customer_id_fkey";
ALTER TABLE public."subscription" DROP CONSTRAINT "subscription_customer_id_fkey";
ALTER TABLE public."vendor_membership" DROP CONSTRAINT "vendor_membership_user_id_fkey";

ALTER TABLE public.customer DROP CONSTRAINT customer_customer_id_key;
ALTER TABLE public.customer ADD CONSTRAINT customer_tenant_customer_id_key UNIQUE (tenant_id, customer_id);
ALTER TABLE public.customer DROP CONSTRAINT customer_email_key;
ALTER TABLE public.customer ADD CONSTRAINT customer_tenant_email_key UNIQUE (tenant_id, email);

ALTER TABLE public."cart" ADD CONSTRAINT "cart_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."checkout_quote" ADD CONSTRAINT "checkout_quote_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."customer_address" ADD CONSTRAINT "customer_address_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."grooming_booking" ADD CONSTRAINT "grooming_booking_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."orders" ADD CONSTRAINT "orders_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."organization_membership" ADD CONSTRAINT "organization_membership_user_id_fkey" FOREIGN KEY (tenant_id, "user_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."prescription_request" ADD CONSTRAINT "prescription_request_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."quote" ADD CONSTRAINT "quote_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."return_request" ADD CONSTRAINT "return_request_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."subscription" ADD CONSTRAINT "subscription_customer_id_fkey" FOREIGN KEY (tenant_id, "customer_id") REFERENCES public.customer (tenant_id, customer_id);
ALTER TABLE public."vendor_membership" ADD CONSTRAINT "vendor_membership_user_id_fkey" FOREIGN KEY (tenant_id, "user_id") REFERENCES public.customer (tenant_id, customer_id);

-- ------------------------------------- and so does everything keyed by them
--
-- A unique index over a column that is only unique *within* a store has to say
-- so too, or the store boundary leaks back in through the constraint. This one
-- is the whole reason the rule is worth stating: `customer_id` is now unique
-- per store, so one person is legitimately a customer of two — and an index on
-- `cart(customer_id)` alone means opening a cart in the first store stops them
-- opening one in the second. The refusal even names the constraint, so it
-- reports that somebody, somewhere, already holds that id.
--
-- Scoping a *column* is not finished until everything keyed by it is scoped as
-- well. The engine cannot check this: `donat validate` reads columns, not the
-- reach of an index.
DROP INDEX IF EXISTS cart_one_open_per_customer;
CREATE UNIQUE INDEX cart_one_open_per_customer
  ON public.cart (tenant_id, customer_id) WHERE status = 'cart_open';
