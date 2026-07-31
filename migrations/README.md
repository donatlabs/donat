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

When Process metadata is present, deploy each selected Postgres source
explicitly:

```sh
donat migrate \
  --migrations-dir migrations \
  --metadata-dir metadata \
  --source default
```

After applying versioned DDL, this command reconciles immutable Process
definition revisions for that source. Reconciliation is deploy-time state,
not serving-time schema repair: `serve` only verifies the migrated helper and
the deployed active/live-retired revisions with read-only catalog queries.

## Bundled runtime migrations

- V3–V5 install the Command journal, first-executor claim, and qualified
  execution identity.
- V6 installs the source-local Process journal and activity/inbound ledgers,
  adds the Command execution-generation UUID used by atomic Process outboxes,
  and owns `donat.check_violation(text)`.
- V7 adds the closed caller role/session snapshot used by `run_as: caller`,
  terminal output and safe failure envelopes, and the internal `continue`
  event consumed by deterministic Process state transitions.
- V8 adds the bounded JSONB containment index used to classify late signals
  against retained durable wait history.
- V9 adds the finite Process fan-out item journal and its durable command-item
  event kind; connector items continue to use the existing activity jobs.
