# Schema migrations (DDL)

Versioned SQL migrations applied by `donat migrate` (refinery). This is
the **only** thing that changes the database schema — the serving binary
never runs DDL and has no `run_sql` endpoint.

## Convention

Files named `V{timestamp}__{description}.sql`, where the timestamp is
`YYYYMMDDHHMMSS` at the moment the migration was written — e.g.
`V20260613222215__create_widget.sql`. Applied in version order, tracked in the
`refinery_schema_history` table; re-running is idempotent.

A timestamp is used rather than a counter for two reasons. Two branches that
each add "the next" migration both pick the same counter and collide on merge,
while two timestamps never do. And two independently versioned sets — this
engine's schema and an application's own — can share one history table, which
a counter makes impossible because both start at `V1`.

Any integer that fits in `BIGINT` is accepted, so a set still on sequential
numbers keeps working. Moving one onto timestamps is safe: `donat migrate`
carries an already applied history onto the new versions, joining on the
migration name, and applies nothing twice. It refuses to guess when a name
does not identify exactly one migration on both sides.

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

- `donat_commands`, `donat_command_claims` and `qualify_command_identity`
  install the Command journal, first-executor claim, and qualified execution
  identity.
- `donat_processes` installs the source-local Process journal and
  activity/inbound ledgers, adds the Command execution-generation UUID used by
  atomic Process outboxes, and owns `donat.check_violation(text)`.
- `process_execution_context` adds the closed caller role/session snapshot used
  by `run_as: caller`, terminal output and safe failure envelopes, and the
  internal `continue` event consumed by deterministic Process state
  transitions.
- `process_wait_history_index` adds the bounded JSONB containment index used to
  classify late signals against retained durable wait history.
- `process_bounded_fanout` adds the finite Process fan-out item journal and its
  durable command-item event kind; connector items continue to use the existing
  activity jobs.
