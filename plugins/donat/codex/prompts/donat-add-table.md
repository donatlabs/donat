Add a table to this donat application. Arguments: $ARGUMENTS

Read first:
- `~/.codex/donat/skills/donat-tables-and-permissions/SKILL.md`
- `~/.codex/donat/skills/donat-schema-and-migrations/SKILL.md`
- `~/.codex/donat/skills/donat-validators/SKILL.md`

Then:

1. Open two or three existing table files under
   `metadata/databases/*/tables/` and match their conventions exactly.
2. Write the migration. Decide nullability deliberately — a nullable column
   forces every validator touching it to declare presence. Add the unique
   constraints and the indexes the row filters will need.
3. Write the table file and register it in `tables.yaml`. Declare relationships
   before permissions; permissions are written in terms of them.
4. Per role: `select` (`columns` + `filter`), `insert` (`check` + `columns` +
   any `set` presets), `update` (`columns` + `filter` **and** `check` — they
   are usually different), `delete` (`filter`). Absent means denied. No admin
   role exists; do not reach for one.
5. Value rules that bind this role go in `validate`, ordered most-specific
   first, each with its own message. Declare presence with `not_null:` or
   `when_present:` on any nullable column.
6. Run `donat validate --metadata-dir metadata` and paste the output. Then
   query as each role, including one request proving another session's rows are
   invisible.
