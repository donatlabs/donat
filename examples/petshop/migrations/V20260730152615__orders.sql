CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE orders (
  internal_id bigserial NOT NULL UNIQUE,
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  customer_id text NOT NULL REFERENCES customer(customer_id),
  order_status text NOT NULL DEFAULT 'pending'
    CHECK (order_status IN ('pending', 'paid', 'cancelled', 'fulfilled')),
  total_minor bigint NOT NULL DEFAULT 0 CHECK (total_minor >= 0),
  currency text NOT NULL DEFAULT 'USD' CHECK (currency ~ '^[A-Z]{3}$'),
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE order_line (
  id bigserial PRIMARY KEY,
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  unit_price_minor bigint NOT NULL CHECK (unit_price_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  line_total_minor bigint NOT NULL
    CHECK (line_total_minor = unit_price_minor * quantity)
);
