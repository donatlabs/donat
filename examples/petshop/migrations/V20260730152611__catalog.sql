CREATE TABLE category (
  id bigserial PRIMARY KEY,
  slug text NOT NULL UNIQUE,
  name text NOT NULL
);

CREATE TABLE product (
  id bigserial PRIMARY KEY,
  category_id bigint NOT NULL REFERENCES category(id),
  slug text NOT NULL UNIQUE,
  title text NOT NULL,
  description text,
  status text NOT NULL DEFAULT 'draft'
    CHECK (status IN ('draft', 'published', 'archived'))
);

INSERT INTO category (slug, name) VALUES
  ('dogs', 'Dogs'),
  ('cats', 'Cats'),
  ('reptiles', 'Reptiles');

INSERT INTO product (category_id, slug, title, description, status) VALUES
  (1, 'dog-kibble', 'Dog Kibble', 'Balanced chicken and salmon recipes.', 'published'),
  (2, 'cat-scratcher', 'Cat Scratcher', 'A sturdy scratch post for indoor cats.', 'published'),
  (3, 'turtle-heat-lamp', 'Turtle Heat Lamp', 'A draft product that is not in the public catalogue.', 'draft');
