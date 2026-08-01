-- Give the optional IdP (compose profile `auth`) its own database and role
-- inside the Postgres this example already runs, so no second database server
-- is needed. Identity data never shares a database with the data plane:
-- password hashes must not sit in the database donat serves.
--
-- Run by the postgres image's entrypoint while it initializes an empty data
-- directory, so it costs nothing when the `auth` profile is off — an empty,
-- unused database. To enable the profile on an example that has already been
-- started, recreate the volume: `docker compose down -v`.
CREATE ROLE rauthy LOGIN PASSWORD 'rauthy';
CREATE DATABASE rauthy OWNER rauthy;
