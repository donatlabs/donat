#!/bin/sh
# Give the IdP its own database and role inside the Postgres this example
# already runs, so no second database server is needed. Identity data never
# shares a database with the data plane: password hashes must not sit in the
# database donat serves.
#
# Still gated on RAUTHY_DB_PASSWORD so a deployment that replaces this IdP with
# its own can unset the variable and be left with no unused role.
#
# The postgres image runs this while initializing an empty data directory, so
# an example that was started before the IdP existed needs its volume
# recreated: `docker compose down -v`.
set -e

if [ -z "$RAUTHY_DB_PASSWORD" ]; then
    echo "RAUTHY_DB_PASSWORD unset: no rauthy database created"
    exit 0
fi

psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
     -v password="$RAUTHY_DB_PASSWORD" <<'SQL'
CREATE ROLE rauthy LOGIN PASSWORD :'password';
CREATE DATABASE rauthy OWNER rauthy;
SQL
