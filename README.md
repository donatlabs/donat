# dist-api

A GraphQL engine over Postgres, compatible with the Hasura v2 surface
(metadata format, API shape). The conformance contract is enforced by a
native Rust test harness (`crates/conformance`) executing Hasura-derived
fixtures — no admin role by design: all data access goes through explicit
role permissions. Rust workspace; see [PLAN.md](PLAN.md) for the
architecture and milestones.

## Layout

| Path | Purpose |
|---|---|
| `crates/metadata` | Hasura v2 metadata types + YAML directory loader (`!include`) |
| `crates/catalog` | Postgres introspection (pg_catalog) |
| `crates/schema` | Per-role GraphQL schema generation |
| `crates/ir` | Intermediate representation — the SQL-free boundary |
| `crates/sqlgen` | IR → one Postgres SQL statement |
| `crates/server` | axum HTTP server: `/v1/graphql` (+ws), relay, auth; `migrate`/`validate` subcommands. No runtime admin/`run_sql` API |
| `crates/conformance` | Native conformance harness + Hasura-derived fixtures (Apache 2.0) |
| `tests/hasura` | Legacy pytest harness (optional cross-check; superseded by `crates/conformance`, safe to delete) |

## Quick start

```sh
make build
make test           # unit + snapshot tests (no database needed)
make conformance    # full conformance suite (needs Postgres, see below)
make run            # serves :8080 with the fixture metadata
```

## Deploy model (schema + metadata)

Schema (DDL) and metadata are separate concerns and neither is mutated at
runtime — the serving binary has no `run_sql` / metadata-mutation surface:

```sh
dist-api migrate  --migrations-dir migrations   # versioned SQL (refinery), DDL only
dist-api validate --metadata-dir metadata       # YAML metadata vs the migrated DB
dist-api serve                                  # boots from the migrated DB + YAML metadata
```

DDL lives in `migrations/` (`V{n}__name.sql`). Metadata is desired-state
YAML (Hasura v3 directory: `version.yaml`, `databases/`, `inherited_roles.yaml`,
`query_collections.yaml`, `allow_list.yaml`, `remote_schemas.yaml`,
`actions.yaml`) loaded at boot; `validate` fails the deploy if it is
inconsistent with the schema.

Conformance Postgres: `postgis/postgis:16-3.4` reachable as
`postgresql://postgres:postgres@127.0.0.1:15432/postgres` (override via
`PG_URL`). Each suite spawns its own engine on a fresh database, so runs
are hermetic and parallel.

CI (GitHub Actions) runs both test tiers on every push/PR and uploads
release binaries for linux-x86_64 and macos-aarch64 as build artifacts.

## Roadmap

Status of the Hasura v2 surface. Every "done" item is backed by a passing
module in the native conformance harness (`crates/conformance/tests/`, run
with `make conformance`); [tests/hasura/COVERAGE.md](tests/hasura/COVERAGE.md)
has the per-suite detail.

### Done

- **Queries** — per-role row filters (session vars, legacy `$op` spellings,
  implicit `_eq`, array literals), column masks, permission limits,
  relationships (FK + manual, object/array), aggregates (incl.
  order-by-relationship-aggregate), computed fields (scalar + table-valued,
  with args, in filters), `_exists`, column-to-column comparisons, jsonb and
  PostGIS operators, `_by_pk`.
- **Mutations** — insert (with `on_conflict`/upsert, column presets, check
  expressions with exact error shapes), update (`_set`/`_inc`), delete,
  `_by_pk`/`_one` variants, `returning`, `backend_only` permissions, in one
  transaction.
- **Relay** — connections, `node(id)`, global ids, cursor pagination
  (`first/after`, `last/before`).
- **Subscriptions** — live queries (1s polling with change detection),
  protocol error frames, JWT token-expiry close.
- **Auth** — JWT (complete, incl. JWK fetch with cache-control refresh),
  webhook auth hook (GET/POST, 401 → unauthorized-role fallback),
  `HASURA_GRAPHQL_UNAUTHORIZED_ROLE`, admin-secret as API-level auth only,
  trusted-header semantics.
- **Inherited roles** — cell-level NULLing, guarded aggregates/computed
  fields, cycle detection with exact path.
- **Allowlist / query collections** — `__typename`-insensitive matching.
- **Introspection** — real per-role `__schema`/`__type`.
- **Actions (synchronous)** — webhook handlers, custom type system
  (input/output objects, scalars, enums), full output shaping + Hasura's
  response-validation error messages, handler-error surfacing, handlers that
  call back into the engine, and output-object → tracked-table relationships
  (resolved under the calling role's permissions).
- **Deploy** — `migrate` (refinery DDL), `validate` (metadata vs DB),
  boot-from-YAML; multi-source metadata; per-source pools/catalogs.

### Partial

- **Remote schemas** — implemented in the engine (role-scoped SDL
  permissions, request validation with exact errors, `{{ENV}}` url templates
  + header forwarding, schema customization, `@preset` args, per-row remote
  relationships) and exercised by the legacy pytest cross-check (16/23), but
  **not yet ported to the native conformance harness**, so it is not counted
  among the "Done" items above. Mixed introspection+remote root queries
  (split & merge) and customized-schema multi-step flows remain incomplete.
- **Introspection completeness** — aggregate detail types, computed-field
  args, function roots, some mutation by_pk/one roots in `__schema`.
- **Actions** — done as above; *remaining:* output → remote-schema joins,
  request/response (Kriti) transforms, asynchronous actions, action
  introspection, response-header forwarding.

### Not planned (by design)

- **No admin role / no runtime admin API** — no `run_sql`, no metadata
  mutation over HTTP. Configuration is deploy-time only (`migrate` + YAML).
  This is a deliberate security posture, not a gap.
- **Event triggers / scheduled triggers** — no role-scoped surface.
- **Non-Postgres backends** (MSSQL, etc.).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
Conformance fixtures are derived from Hasura's tests-py suite (Apache 2.0,
see `crates/conformance/fixtures/LICENSE.hasura`).
