-- Demo data for the petshop example. A second migration on top of the schema.

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
