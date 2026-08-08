---
name: donat-app-architecture
description: Use when starting work on a donat application, deciding whether a rule belongs in a SQL migration or in YAML metadata, or reading an unfamiliar metadata directory. Start here before the other donat skills.
---

# Building an application on donat

donat is one Rust binary plus Postgres. You do not write a backend; you declare
one. A request arrives, resolves against a per-role permission, and is answered
by SQL the engine generated. There is no place to put application code inside
the request path, and that is the point.

## The one decision that orders everything else

Every requirement lands in exactly one of two layers:

| Layer | Owns | Changed by |
|---|---|---|
| **SQL migrations** | Tables, columns, types, foreign keys, unique constraints, `CHECK`, indexes — anything that binds *every* writer | `donat migrate`, versioned files, reviewed like code |
| **YAML metadata** | Which tables are visible, to which roles, under which row filters, with which validators, plus rules, commands, processes, connectors and the REST/MCP surfaces | Edited in the metadata directory, loaded at boot |

The test for which layer a rule belongs to: **does it bind every writer, or one
role?** `quantity > 0` is true for a shopper, a wholesale command and a data
fix, so it is a database `CHECK`. `quantity <= 20` is a shopper's basket limit
and must not bind the wholesale command, so it is a per-role `validate` entry.

Getting this wrong is the most common mistake in a donat application. A
constraint in the wrong layer either leaks (a role-specific rule in the
database blocks a legitimate writer) or fails to hold (a universal rule in
metadata is bypassed by any other role).

## Metadata directory layout

This is the shape `--metadata-dir` expects. Files compose through the
`!include` tag, which is why the tree can be split by domain without the engine
knowing about the split.

```
metadata/
  version.yaml               # version: 3
  databases/
    databases.yaml           # sources, connection env vars, tables: "!include ..."
    default/tables/
      tables.yaml            # a list of "!include public_<table>.yaml"
      public_<table>.yaml    # tracking + relationships + per-role permissions
  rules.yaml                 # types (enums/objects), rules, decision tables
  commands.yaml              # a list of "!include commands/<domain>/<name>.yaml"
  commands/<domain>/*.yaml
  flows.yaml                 # a list of "!include flows/<name>.yaml"
  flows/*.yaml               # durable processes
  connectors.yaml            # a list of "!include connectors/<name>.yaml"
  connectors/*.yaml
  query_collections.yaml     # saved GraphQL operations
  rest_endpoints.yaml        # URLs bound to saved operations
  mcp.yaml                   # which tables/queries are agent-visible
  storage.yaml               # object store, signing, GC, upload limits
```

`!include` values are written as quoted strings — `"!include tables.yaml"` —
not as bare YAML tags. Copy the spelling from an existing file.

## The declarative surfaces, in the order you build them

Build bottom-up. Each layer is useful on its own, and a layer that is not
needed is simply absent.

1. **Migrations** — the tables exist. See `donat-schema-and-migrations`.
2. **Tables and permissions** — the tables are visible to named roles under row
   filters. At this point GraphQL, REST and MCP already work. See
   `donat-tables-and-permissions` and `donat-validators`.
3. **Rules** — typed enums and expressions, so guards are named and reusable.
   See `donat-rules`.
4. **Commands** — a domain operation as ordered steps in one transaction, with
   guards and an idempotency key. See `donat-commands`.
5. **Processes** — a long-running flow with waits, timers, signals and
   compensation, durable across restarts. See `donat-processes`.
6. **Connectors** — an external HTTP provider as a declared contract with
   bounds, retries and idempotency evidence. See `donat-connectors`.

Files and API surfaces sit beside all of this: `donat-file-attachments` and
`donat-api-surfaces`.

## The deploy pipeline

```sh
donat migrate  --migrations-dir migrations    # DDL, and Process revisions
donat validate --metadata-dir metadata        # metadata vs the real schema
donat serve                                   # reads both, runs neither
```

`validate` exits non-zero on any inconsistency — a permission naming a column
that does not exist, a validator whose expression cannot type-check, a command
step writing an untracked table. **A metadata error is a deploy failure, not a
request failure.** That is deliberate: the engine refuses to serve rather than
discovering the problem on a customer's request.

Run `validate` in CI against the migrated schema. It is the closest thing this
architecture has to a compiler.

## Rules that are never negotiable

**There is no admin role.** Not disabled, not gated behind a flag — the
permission-bypass role does not exist. Every data access, including the ones a
Process makes on your behalf, resolves through an explicit per-role permission.
If a design needs "something that can see everything", the answer is an
ordinary role with an explicit permission on the tables it needs, and a review
of why it needs them.

`X-Donat-Admin-Secret` marks a request as *trusted*, which only means it is
allowed to assert a role via `X-Donat-Role`. It is transport authentication,
never a permission. A trusted request with no role is rejected, or falls back
to `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` if one is configured.

**There is no runtime configuration API.** No `run_sql`, no metadata mutation
over HTTP. If a change is not in a migration or in the metadata directory, it
does not happen. Anything proposing otherwise is a design error.

**Nothing that a permission decides may be re-decided in a client.** A row
filter that a UI also enforces is fine; a row filter that *only* a UI enforces
is a hole. The API is the security boundary because there is no other one.

## Reference implementation

[`examples/petshop`](https://github.com/donatlabs/donat/tree/main/examples/petshop) is the worked application: 11 durable processes, 73
commands, 60 rules and 10 decision tables over 41 declared types, 5 connectors,
file attachments, REST endpoints and MCP tools. It runs under
`docker compose up` and is exercised end to end over HTTP in
[`crates/conformance`](https://github.com/donatlabs/donat/tree/main/crates/conformance).

When a pattern here is ambiguous, read the petshop file it points at. The
example is the specification; this skill is the map.

> One stale note: [`examples/petshop/README.md`](https://github.com/donatlabs/donat/tree/main/examples/petshop/README.md) still says its declarative YAML
> "is not currently runnable". That caveat predates the runtime — `commands`,
> `rules`, `processes` and `connectors` each have conformance suites, and
> [`crates/conformance/tests/petshop_process.rs`](https://github.com/donatlabs/donat/tree/main/crates/conformance/tests/petshop_process.rs) drives the petshop flows end to
> end. Trust the tests.
