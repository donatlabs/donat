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

**The default deliverable is a Go application that embeds the engine**, in the
shape of `examples/lending-golang`: metadata for everything declarable, Go for
the parts no declaration can express, unit and integration tests, and a compose
stand. Load `donat-embedded-go` alongside the others.

Say so rather than asking — but say the trade in the same breath, because it is
an architecture decision and it is theirs:

> This comes out as one Go program with the engine inside it — one binary, no
> separate service to run, and the parts that can't be declared are small named
> functions in one file. If we end up needing long-running flows with timers,
> we also run the engine's own server beside it against the same database; I'll
> flag it when we get there.

Work in this order and stop for confirmation after step 2:

1. **Establish the domain.** Ask for, or infer from the description, the two or
   three core entities and the roles that will exist. Do not invent a role
   called `admin` — there is no admin role in donat, and a design that needs
   one is a problem to raise now, not to work around later.

2. **Propose the shape before writing it**: the tables with their columns and
   constraints, the roles, and for each role/table pair what it may read and
   write. Show it as a short table and wait for confirmation. In analytics mode
   this is the access matrix — put it up already filled in and ask what is
   wrong.

3. **Migrations** under `migrations/`, named `V{YYYYMMDDHHMMSS}__{description}.sql`.
   Put every universal rule here — NOT NULL, foreign keys, unique constraints,
   CHECK constraints true of the whole domain — and nothing role-shaped.

4. **Metadata directory**: `version.yaml`, `databases/`, and one file per table
   with tracking, relationships and per-role permissions. Absent means denied.
   Prefer a preset plus `columns: []` over trusting the caller with an
   ownership column. Per-role value rules go in `validate` lists, one message
   per condition.

5. **The Go host.** `main.go` — often three lines — plus a file per extension
   point that is actually needed: a function behind a declared action, an event
   handler, `ExecuteTx` where a row must be atomic with the engine's write.
   Declare nothing you do not implement: the engine refuses to boot when a
   handler-less action has no registered function.

   Generate the types with `donat codegen go`. Hand-written structs whose json
   tags drift from the metadata decode to the zero value and answer 200.

6. **Tests, both kinds**, from the start rather than after:
   - **unit** with `donat.TestEngine` — no database; proves the structs, the
     role and the output type agree with the metadata;
   - **integration** through `eng.Handler()` as each role, against Postgres,
     **skipping the whole file when the DSN is absent** so a run with no
     database cannot look like a passing run.

   Assert what the metadata said would happen, never a value recomputed in Go.
   At least one test per role must prove a **refusal** — another session's rows
   come back empty, not denied.

7. **A compose stand**: postgres → engine migrations → app migrations →
   `dump-core-config` → the app. Mount the engine's `migrations/` from a
   pinned engine version; never a floating tag beside a copied schema.

8. **Verify and report**: `donat migrate`, `donat validate --metadata-dir
   metadata`, `go test ./...`, and the stand up. Report the actual output — in
   tech mode pasted, in analytics mode as scenarios, including the refusals.

Do not add commands, processes or connectors in this pass. A permission-checked
API with a Go host and tests around it is the deliverable; anything more is a
separate step once the tables are right.
