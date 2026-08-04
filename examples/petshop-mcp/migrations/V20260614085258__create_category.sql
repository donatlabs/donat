-- Catalogue sections. First table, no dependencies.

CREATE TABLE category (
    id   serial PRIMARY KEY,
    name text NOT NULL UNIQUE
);

INSERT INTO category (name) VALUES
    ('Dogs'), ('Cats'), ('Birds'), ('Fish');
