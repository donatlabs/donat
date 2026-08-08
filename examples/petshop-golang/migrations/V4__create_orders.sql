-- A customer's order with a fulfilment status.

CREATE TABLE orders (
    id          serial PRIMARY KEY,
    customer_id text NOT NULL REFERENCES customer (id),
    status      text NOT NULL DEFAULT 'placed'
                    CHECK (status IN ('placed', 'approved', 'shipped', 'delivered', 'cancelled')),
    created_at  timestamptz NOT NULL DEFAULT now()
);

INSERT INTO orders (customer_id, status) VALUES
    ('1', 'placed'),
    ('2', 'shipped');
