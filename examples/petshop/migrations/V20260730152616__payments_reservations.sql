CREATE TABLE inventory_reservation (
  id bigserial PRIMARY KEY,
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  variant_id bigint NOT NULL REFERENCES product_variant(id),
  quantity integer NOT NULL CHECK (quantity > 0),
  status text NOT NULL DEFAULT 'reserved'
    CHECK (status IN ('reserved', 'released', 'consumed')),
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (order_id, variant_id)
);

CREATE TABLE payment (
  internal_id bigserial NOT NULL UNIQUE,
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  order_id uuid NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
  status text NOT NULL DEFAULT 'pending'
    CHECK (status IN ('pending', 'authorized', 'paid', 'failed', 'refunded')),
  amount_minor bigint NOT NULL CHECK (amount_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  provider_reference text,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE payment_event (
  id bigserial PRIMARY KEY,
  payment_id uuid NOT NULL REFERENCES payment(id) ON DELETE CASCADE,
  event_type text NOT NULL,
  provider_event_id text NOT NULL UNIQUE,
  payload jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now()
);
