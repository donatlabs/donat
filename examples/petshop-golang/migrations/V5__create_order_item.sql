-- Line items linking an order to the pets being bought.

CREATE TABLE order_item (
    id         serial PRIMARY KEY,
    order_id   integer NOT NULL REFERENCES orders (id),
    pet_id     integer NOT NULL REFERENCES pet (id),
    quantity   integer NOT NULL DEFAULT 1 CHECK (quantity > 0),
    unit_price numeric(10, 2) NOT NULL
);

INSERT INTO order_item (order_id, pet_id, quantity, unit_price) VALUES
    (1, 1, 1, 350.00),
    (1, 5, 2,  35.00),
    (2, 3, 1,  90.00);
