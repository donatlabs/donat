-- Shoppers. customer.id is the value carried in the X-Donat-User-Id session
-- variable, so it is a text key rather than a serial.

CREATE TABLE customer (
    id    text PRIMARY KEY,
    name  text NOT NULL,
    email text NOT NULL UNIQUE
);

INSERT INTO customer (id, name, email) VALUES
    ('1', 'Alice Buyer', 'alice@example.com'),
    ('2', 'Bob Shopper', 'bob@example.com');
