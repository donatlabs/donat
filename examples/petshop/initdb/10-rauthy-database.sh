#!/bin/sh
# Give the optional IdP (see auth.env) its own database and role inside the
# Postgres this example already runs, so no second database server is needed.
# Identity data never shares a database with the data plane: password hashes
# must not sit in the database donat serves.
#
# Gated on RAUTHY_DB_PASSWORD, which only auth.env sets. A plain
# `docker compose up` therefore creates nothing — an example used as a starting
# point is not left holding a login nobody asked for.
#
# The postgres image runs this while initializing an empty data directory, so
# enabling the profile on an example that has already been started means
# recreating the volume: `docker compose down -v`.
set -e

if [ -z "$RAUTHY_DB_PASSWORD" ]; then
    echo "identity profile disabled (RAUTHY_DB_PASSWORD unset): no rauthy database created"
    exit 0
fi

psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
     -v password="$RAUTHY_DB_PASSWORD" <<'SQL'
CREATE ROLE rauthy LOGIN PASSWORD :'password';
CREATE DATABASE rauthy OWNER rauthy;
SQL
