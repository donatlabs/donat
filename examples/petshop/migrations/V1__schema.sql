-- Petshop schema (DDL). Applied by `dist-api migrate` via refinery and
-- tracked in refinery_schema_history. The serving engine never runs DDL; it
-- only introspects the result of these migrations.

CREATE TABLE category (
    id   serial PRIMARY KEY,
    name text NOT NULL UNIQUE
);

CREATE TABLE pet (
    id          serial PRIMARY KEY,
    name        text NOT NULL,
    category_id integer NOT NULL REFERENCES category (id),
    price       numeric(10, 2) NOT NULL DEFAULT 0,
    -- available -> can be ordered, pending -> in someone's cart, sold -> gone
    status      text NOT NULL DEFAULT 'available'
                    CHECK (status IN ('available', 'pending', 'sold')),
    description text
);

-- customer.id is the value carried in the X-Hasura-User-Id session variable.
CREATE TABLE customer (
    id    text PRIMARY KEY,
    name  text NOT NULL,
    email text NOT NULL UNIQUE
);

CREATE TABLE orders (
    id          serial PRIMARY KEY,
    customer_id text NOT NULL REFERENCES customer (id),
    status      text NOT NULL DEFAULT 'placed'
                    CHECK (status IN ('placed', 'approved', 'shipped', 'delivered', 'cancelled')),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE order_item (
    id         serial PRIMARY KEY,
    order_id   integer NOT NULL REFERENCES orders (id),
    pet_id     integer NOT NULL REFERENCES pet (id),
    quantity   integer NOT NULL DEFAULT 1 CHECK (quantity > 0),
    unit_price numeric(10, 2) NOT NULL
);
