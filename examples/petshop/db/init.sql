-- Classic petshop schema + seed data.
--
-- The dist-api engine introspects pg_catalog, so the tables and foreign keys
-- defined here are what drive the generated GraphQL schema. The metadata
-- directory (../metadata) only adds relationships and per-role permissions on
-- top of this physical schema.

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

-- ---------------------------------------------------------------------------
-- Seed data
-- ---------------------------------------------------------------------------

INSERT INTO category (name) VALUES
    ('Dogs'), ('Cats'), ('Birds'), ('Fish');

INSERT INTO pet (name, category_id, price, status, description) VALUES
    ('Rex',     1, 350.00, 'available', 'Friendly Labrador puppy'),
    ('Bella',   1, 420.00, 'available', 'Calm Golden Retriever'),
    ('Whiskers',2,  90.00, 'available', 'Playful tabby kitten'),
    ('Shadow',  2, 120.00, 'pending',   'Quiet black cat'),
    ('Tweety',  3,  35.00, 'available', 'Singing canary'),
    ('Nemo',    4,  15.00, 'sold',      'Clownfish, already sold');

INSERT INTO customer (id, name, email) VALUES
    ('1', 'Alice Buyer', 'alice@example.com'),
    ('2', 'Bob Shopper', 'bob@example.com');

INSERT INTO orders (customer_id, status) VALUES
    ('1', 'placed'),
    ('2', 'shipped');

INSERT INTO order_item (order_id, pet_id, quantity, unit_price) VALUES
    (1, 1, 1, 350.00),
    (1, 5, 2,  35.00),
    (2, 3, 1,  90.00);
