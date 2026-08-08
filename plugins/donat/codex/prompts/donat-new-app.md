Scaffold a new donat application. Arguments: $ARGUMENTS

Read these first:
- `~/.codex/donat/skills/donat-app-architecture/SKILL.md`
- `~/.codex/donat/skills/donat-schema-and-migrations/SKILL.md`
- `~/.codex/donat/skills/donat-tables-and-permissions/SKILL.md`

Then:

1. Establish the core entities and the roles. Do not invent an `admin` role —
   donat has none, and a design that needs one is a problem to raise now.
2. Propose the tables, constraints and the role × table access matrix as a
   short table. **Stop and wait for confirmation.**
3. Write migrations under `migrations/V{YYYYMMDDHHMMSS}__{description}.sql`.
   Everything universal goes here; nothing role-shaped.
4. Write `metadata/`: `version.yaml`, `databases/databases.yaml`,
   `databases/default/tables/tables.yaml`, and one file per table with
   tracking, relationships and per-role permissions. Absent means denied.
   Prefer a preset plus `columns: []` over trusting the caller with an
   ownership column.
5. Put per-role value rules in `validate` lists, one message per condition.
6. Add a compose stand modelled on `examples/petshop-rest/docker-compose.yml`.
7. Run `donat migrate`, then `donat validate --metadata-dir metadata`, and
   paste both outputs. Show one request per role, including one proving the
   wrong role sees nothing.

No commands, processes or connectors in this pass.
