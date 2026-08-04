CREATE TABLE product_variant (
  id bigserial PRIMARY KEY,
  product_id bigint NOT NULL REFERENCES product(id),
  sku text NOT NULL UNIQUE,
  title text NOT NULL,
  price_minor bigint NOT NULL CHECK (price_minor >= 0),
  currency text NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
  active boolean NOT NULL DEFAULT true
);

CREATE TABLE inventory_stock (
  variant_id bigint PRIMARY KEY REFERENCES product_variant(id),
  on_hand integer NOT NULL CHECK (on_hand >= 0),
  reserved integer NOT NULL DEFAULT 0 CHECK (reserved >= 0),
  available_quantity integer
    GENERATED ALWAYS AS (on_hand - reserved) STORED,
  CHECK (reserved <= on_hand)
);

INSERT INTO product_variant (product_id, sku, title, price_minor, currency, active) VALUES
  (1, 'DOG-CHICKEN-1KG', 'Chicken 1 kg', 1999, 'USD', true),
  (1, 'DOG-SALMON-1KG', 'Salmon 1 kg', 2499, 'USD', true),
  (2, 'CAT-SCRATCH-POST', 'Scratch post', 1599, 'USD', true),
  (3, 'TURTLE-HEAT-50W', '50 W heat lamp', 2999, 'USD', false);

INSERT INTO inventory_stock (variant_id, on_hand, reserved) VALUES
  (1, 1, 0),
  (2, 5, 2),
  (3, 5, 0),
  (4, 2, 0);
