# Schema migrations (DDL)

Versioned SQL migrations applied by `donat migrate` (refinery). This is
the **only** thing that changes the database schema — the serving binary
never runs DDL and has no `run_sql` endpoint.

## Convention

Files named `V{n}__{description}.sql`, e.g. `V1__create_widget.sql`,
`V2__add_author_fk.sql`. Applied in version order, tracked in the
`refinery_schema_history` table; re-running is idempotent.

```sh
donat migrate --migrations-dir migrations   # apply pending DDL
donat validate --metadata-dir metadata      # check YAML metadata vs DB
```

Deploy order: `migrate` (schema) → `validate` (metadata vs schema) →
`serve` (boots from the migrated DB + the YAML metadata, read-only).

Metadata (table tracking, permissions, relationships, remote schemas,
allowlists, inherited roles) is NOT migrated — it is desired-state YAML
loaded at boot via `--metadata-dir`; `validate` fails the deploy if it is
inconsistent with the schema.
