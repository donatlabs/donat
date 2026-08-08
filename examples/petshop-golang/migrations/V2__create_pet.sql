-- Items for sale, each in a category.

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

INSERT INTO pet (name, category_id, price, status, description) VALUES
    ('Rex',     1, 350.00, 'available', 'Friendly Labrador puppy'),
    ('Bella',   1, 420.00, 'available', 'Calm Golden Retriever'),
    ('Whiskers',2,  90.00, 'available', 'Playful tabby kitten'),
    ('Shadow',  2, 120.00, 'pending',   'Quiet black cat'),
    ('Tweety',  3,  35.00, 'available', 'Singing canary'),
    ('Nemo',    4,  15.00, 'sold',      'Clownfish, already sold');
