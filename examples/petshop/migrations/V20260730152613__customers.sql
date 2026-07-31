CREATE TABLE customer (
  id bigserial PRIMARY KEY,
  customer_id text NOT NULL UNIQUE,
  name text NOT NULL,
  email text NOT NULL UNIQUE
);

CREATE TABLE customer_address (
  id bigserial PRIMARY KEY,
  customer_id text NOT NULL REFERENCES customer(customer_id),
  label text NOT NULL,
  line1 text NOT NULL,
  line2 text,
  city text NOT NULL,
  postal_code text NOT NULL,
  country_code text NOT NULL CHECK (country_code ~ '^[A-Z]{2}$')
);

INSERT INTO customer (customer_id, name, email) VALUES
  ('customer-1', 'Alice Buyer', 'alice@example.com'),
  ('customer-2', 'Bob Buyer', 'bob@example.com');

INSERT INTO customer_address (customer_id, label, line1, city, postal_code, country_code) VALUES
  ('customer-1', 'Home', '1 Pet Lane', 'Pawsville', '10001', 'US'),
  ('customer-2', 'Home', '2 Cat Street', 'Whiskerton', '10002', 'US');
