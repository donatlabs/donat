---
name: add-table
description: Use when the user runs /donat:add-table, or asks to add a table, entity or per-role permissions to a donat application — produces the migration, the tracking file, relationships, permissions and validators.
argument-hint: <table name> [columns and who may see it]
allowed-tools: [Read, Write, Edit, Glob, Grep, Bash]
---

Add the table `$1` to this donat application. Details: $ARGUMENTS

Load `donat-tables-and-permissions`, `donat-schema-and-migrations` and
`donat-validators`.

1. **Read the neighbours first.** Open two or three existing table files under
   `metadata/databases/*/tables/` and match their conventions — file naming,
   how relationships are declared, how session variables are spelled, whether
   column masks are explicit or `"*"`.

2. **Migration** `V{YYYYMMDDHHMMSS}__create_<table>.sql`. Decide nullability
   deliberately: a nullable column forces every validator touching it to
   declare presence. Add the unique constraints and indexes the row filters
   will need — a filter traversing a relationship that has no index is a
   sequential scan on every request.

3. **Table file** added to `tables.yaml`, with relationships declared before
   permissions, since permissions are written in terms of them.

4. **Permissions, role by role.** For each role state in one sentence what it
   may do, then write it:
   - `select`: `columns` + `filter`
   - `insert`: `check` + `columns` (+ `set` presets)
   - `update`: `columns` + `filter` **and** `check` — they are usually different
   - `delete`: `filter`
   Anything absent is denied. Do not add a role that "can see everything"
   without saying out loud why, and never reach for an admin role — there
   isn't one.

   Where a permission genuinely does not bound rows to the caller, say which of
   `catalogue`, `operator`, `worker` or `command` it is — as an `unbounded:`
   key if the directory sets `unbounded_permissions: declared`, and out loud
   either way. A forgotten bound and a deliberate one are both `filter: {}`.
   Under `tenancy.yaml`, being tenant-scoped is not being caller-bound.

5. **Validators** for value rules that bind one role, ordered so the most
   specific diagnosis comes first, each with its own message. Declare presence
   with `not_null:` or `when_present:` on any nullable column.

6. **Verify**: `donat validate --metadata-dir metadata` against the migrated
   schema, then a query as each role — including one proving another session's
   rows are invisible. Report the actual output, not a claim.
