# donat

A GraphQL engine over Postgres: a declarative v2 metadata format, a
per-role GraphQL API, and one SQL statement per operation. The conformance
contract is enforced by a native Rust test harness (`crates/conformance`)
executing fixtures against a real Postgres — no admin role by design: all
data access goes through explicit role permissions. Rust workspace; see
[PLAN.md](PLAN.md) for the architecture and milestones.

## Layout

| Path | Purpose |
|---|---|
| `crates/metadata` | Donat v2 metadata types + YAML directory loader (`!include`) |
| `crates/catalog` | Postgres introspection (pg_catalog) |
| `crates/schema` | Per-role GraphQL schema generation |
| `crates/ir` | Intermediate representation — the SQL-free boundary |
| `crates/sqlgen` | IR → one Postgres SQL statement |
| `crates/server` | axum HTTP server: `/v1/graphql` (+ws), relay, auth; `migrate`/`validate` subcommands. No runtime admin/`run_sql` API |
| `crates/conformance` | Native conformance harness + fixtures (Apache 2.0; see `crates/conformance/fixtures/LICENSE.hasura`) |

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
donat migrate  --migrations-dir migrations   # versioned SQL (refinery), DDL only
donat validate --metadata-dir metadata       # YAML metadata vs the migrated DB
donat serve                                  # boots from the migrated DB + YAML metadata
```

DDL lives in `migrations/` (`V{n}__name.sql`). Metadata is desired-state
YAML (Donat v3 directory: `version.yaml`, `databases/`, `inherited_roles.yaml`,
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

Status of the GraphQL surface. Every "done" item is backed by a passing
module in the native conformance harness (`crates/conformance/tests/`, run
with `make conformance`).

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
  `DONAT_GRAPHQL_UNAUTHORIZED_ROLE`, admin-secret as API-level auth only,
  trusted-header semantics.
- **Inherited roles** — cell-level NULLing, guarded aggregates/computed
  fields, cycle detection with exact path.
- **Allowlist / query collections** — `__typename`-insensitive matching.
- **Introspection** — real per-role `__schema`/`__type`.
- **Actions (synchronous)** — webhook handlers, custom type system
  (input/output objects, scalars, enums), full output shaping + Donat's
  response-validation error messages, handler-error surfacing, handlers that
  call back into the engine, and output-object → tracked-table relationships
  (resolved under the calling role's permissions).
- **Deploy** — `migrate` (refinery DDL), `validate` (metadata vs DB),
  boot-from-YAML; multi-source metadata; per-source pools/catalogs.
- **Cron (scheduled) triggers** — recurring webhooks from YAML
  (`cron_triggers`, Donat shape): a `donat` catalog (created by
  `migrate`), a background delivery loop that materializes occurrences and
  delivers with the Donat scheduled-event envelope, `retry_conf`
  (retries/timeout/tolerance), and per-attempt invocation logs. Multi-pod
  safe with no leader election (`ON CONFLICT` materialization + `FOR UPDATE
  SKIP LOCKED` claim → at-least-once; handlers must be idempotent). Native
  coverage in `crates/conformance/tests/cron_triggers.rs`.

### Partial

- **Remote schemas** — role-scoped SDL permissions + execution (request
  validation with exact errors, forwarding to the upstream, unknown-role
  denial), schema customization (namespace/prefix translation, fragments,
  customization-aware validation errors), and `@preset` arguments (static +
  session presets with coercion, preset-hidden args, input-object/variable
  presets) are covered by the native harness
  (`crates/conformance/tests/remote_schemas.rs`, against a native upstream
  stub). Also implemented in the engine but currently only exercised by the
  legacy pytest cross-check: per-row remote relationships and the admin-only
  metadata-validation flows. General mixed introspection+remote root queries
  (split & merge) remain incomplete.
- **Introspection completeness** — aggregate detail types, computed-field
  args, function roots, some mutation by_pk/one roots in `__schema`.
- **Actions** — done as above; *remaining:* output → remote-schema joins,
  request/response (Kriti) transforms, asynchronous actions, action
  introspection, response-header forwarding.
- **Table event triggers** — webhooks on row insert/update/delete from YAML
  (`event_triggers` under a table). In-transaction capture via per-table
  Postgres triggers writing `donat.event_log` (created by `migrate
  --metadata-dir` reconcile; the serving binary never runs DDL), delivered by
  the shared event loop with the Donat event envelope and `retry_conf`.
  Native coverage in `crates/conformance/tests/event_triggers.rs`
  (insert/update/delete payloads, retry→error). *Remaining:* session-variable
  capture, column-filtered payloads, manual/async events, transforms (see
  `specs/002-event-triggers.md`).

### Not planned (by design)

- **No admin role / no runtime admin API** — no `run_sql`, no metadata
  mutation over HTTP. Configuration is deploy-time only (`migrate` + YAML).
  This is a deliberate security posture, not a gap.
- **One-off scheduled events** — Donat creates these via a runtime
  `create_scheduled_event` mutation, which contradicts the no-admin-API
  posture; out of scope unless declared as a deploy-time seed in YAML. (See
  `knowledgebase/embedded-sdk/decisions/006-cron-triggers-yaml-only.md`.)
- **Non-Postgres backends** (MSSQL, etc.).

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
Some conformance fixtures are derived from a third-party Apache-2.0 test
suite; that upstream license and attribution are retained in
`crates/conformance/fixtures/LICENSE.hasura`.
