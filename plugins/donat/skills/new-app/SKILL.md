---
name: new-app
description: Use when the user runs /donat:new-app, or asks to scaffold, bootstrap or start a new donat application — produces migrations, a metadata directory, a first table with per-role permissions, and a compose stand.
argument-hint: <app name> [what it is for]
allowed-tools: [Read, Write, Edit, Glob, Grep, Bash]
---

Scaffold a new donat application called `$1`. The rest of the arguments, if
any, describe what it is for: $ARGUMENTS

Load the `donat-app-architecture` skill first, then
`donat-schema-and-migrations` and `donat-tables-and-permissions`.

Work in this order and stop for confirmation after step 2:

1. **Establish the domain.** Ask for, or infer from the description, the two or
   three core entities and the roles that will exist. Do not invent a role
   called `admin` — there is no admin role in donat, and a design that needs
   one is a design error to raise now, not to work around later.

2. **Propose the shape before writing it**: the tables with their columns and
   constraints, the roles, and for each role/table pair what it may read and
   write. Show it as a short table and wait for confirmation.

3. **Migrations** under `migrations/`, named `V{YYYYMMDDHHMMSS}__{description}.sql`.
   Put every universal rule here — NOT NULL, foreign keys, unique constraints,
   CHECK constraints true of the whole domain — and nothing role-shaped.

4. **Metadata directory**:
   ```
   metadata/version.yaml               # version: 3
   metadata/databases/databases.yaml
   metadata/databases/default/tables/tables.yaml
   metadata/databases/default/tables/public_<table>.yaml
   ```
   One file per table: tracking, relationships, and per-role
   select/insert/update/delete permissions. Absent means denied — write only
   what each role needs. Use presets plus `columns: []` wherever the caller
   should not be able to name the owning column at all.

5. **Value rules** that bind one role go in `validate` lists with one message
   per condition. Rules that bind every writer stay in the migration.

6. **A compose stand**, copied in shape from
   `examples/petshop-rest/docker-compose.yml`.

7. **Verify**: `donat migrate` then `donat validate --metadata-dir metadata`,
   and report both outputs. Then show one `curl` per role, including one that
   proves the wrong role sees nothing.

Do not add commands, processes or connectors in this pass. A working
permission-checked CRUD surface is the deliverable; anything more is a separate
step once the tables are right.
