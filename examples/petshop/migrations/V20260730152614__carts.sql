CREATE TABLE cart (
  id bigserial PRIMARY KEY,
  customer_id text NOT NULL REFERENCES customer(customer_id),
  status text NOT NULL DEFAULT 'open'
    CHECK (status IN ('open', 'checked_out', 'abandoned')),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX cart_one_open_per_customer
ON cart(customer_id) WHERE status = 'open';

CREATE TABLE cart_line (
  id bigserial PRIMARY KEY,
  cart_id bigint NOT NULL REFERENCES cart(id) ON DELETE CASCADE,
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  UNIQUE (cart_id, variant_id)
);
