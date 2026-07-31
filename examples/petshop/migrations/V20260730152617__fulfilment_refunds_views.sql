CREATE TABLE shipment (
  id bigserial PRIMARY KEY,
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  fulfilment_status text NOT NULL DEFAULT 'unfulfilled'
    CHECK (fulfilment_status IN ('unfulfilled', 'packing', 'shipped', 'delivered')),
  tracking_number text,
  shipped_at timestamptz
);

CREATE TABLE refund (
  id bigserial PRIMARY KEY,
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  amount_minor bigint NOT NULL CHECK (amount_minor >= 0),
  status text NOT NULL DEFAULT 'requested'
    CHECK (status IN ('requested', 'approved', 'refunded', 'failed')),
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE VIEW cart_pricing AS
SELECT
  cart.id AS cart_id,
  cart.customer_id,
  cart_line.variant_id,
  product_variant.sku,
  product.title,
  cart_line.quantity,
  product_variant.price_minor AS unit_price_minor,
  product_variant.currency,
  product_variant.price_minor * cart_line.quantity AS line_total_minor,
  inventory_stock.available_quantity
FROM cart
JOIN cart_line ON cart_line.cart_id = cart.id
JOIN product_variant ON product_variant.id = cart_line.variant_id
JOIN product ON product.id = product_variant.product_id
JOIN inventory_stock ON inventory_stock.variant_id = product_variant.id;

CREATE VIEW order_operations AS
SELECT
  orders.id AS order_id,
  orders.customer_id,
  orders.order_status,
  COALESCE(latest_payment.status, 'pending') AS payment_status,
  COALESCE(latest_shipment.fulfilment_status, 'unfulfilled') AS fulfilment_status,
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
  SELECT shipment.fulfilment_status
  FROM shipment
  WHERE shipment.order_id = orders.id
  ORDER BY shipment.id DESC
  LIMIT 1
) AS latest_shipment ON true;
