# dist-api — GraphQL engine over Postgres (Hasura v2-compatible)

A Rust monolith exposing the Hasura v2 surface (metadata format, API shape,
behavior) with a v3-style internal architecture: a hard IR boundary between
the GraphQL layer and the SQL layer. Configuration is entirely file-driven:
a v2 metadata directory plus SQL migrations. No console.

## Architecture

```
                    ┌────────────────────────────────────────────────┐
 HTTP (axum)        │  crates/server                                 │
 /v1/graphql        │  routing, session (X-Hasura-Role, session vars)│
 /v1/metadata       └───────────────┬────────────────────────────────┘
 /v2/query (run_sql)                │
                                    ▼
 crates/metadata ──────► crates/schema ◄────── crates/catalog
 (v2 YAML, !include)     (per-role GraphQL       (pg_catalog
                          schema, fail-fast       introspection)
                          validation)
                                    │ parse + validate + permissions
                                    ▼
                            crates/ir  ◄── the boundary: no SQL above,
                                    │       Postgres-only below
                                    ▼
                            crates/sqlgen
                            (IR → ONE SQL statement with json_agg /
                             LEFT JOIN LATERAL)
                                    │
                                    ▼
                            executor (tokio-postgres)
```

Startup: apply migrations → introspect → overlay metadata (error if YAML
references something that doesn't exist) → build per-role schemas → listen.

## Milestones

- **M0 — skeleton** ✅: workspace, v2 metadata types + loader (`!include`),
  axum server, tests-py vendored.
- **M1 — data** ✅: pg_catalog introspection (tables, columns, PK/FK,
  functions), deadpool pool, `run_sql` (text protocol, auto-untrack of
  dropped objects), legacy v1 `insert` op.
- **M2 — schema** ✅ (as a planner): per-role name resolution with v2
  naming incl. custom root fields; runtime metadata state mutated by
  track/untrack/relationship/permission/function/computed-field commands.
  GraphQL introspection (`__schema`) NOT done yet.
- **M3 — reads** ✅: graphql-parser → planner → IR; fragments, aliases,
  variables (+defaults), @include/@skip, __typename.
- **M4 — compilation** ✅: one SQL statement per operation
  (json_build_object/json_agg, correlated subqueries); insta snapshots.
- **M5 — permissions** ✅: row filters (session vars, `$op` legacy
  spellings, `_exists`, column-to-column with root/relationship paths,
  jsonb + PostGIS operators), column masks, permission limits (aggregate
  `nodes` semantics), computed fields in filters.
- **M6 — mutations** ✅: insert/upsert (on_conflict + update-permission
  filter and presets), update (_set/_inc/by_pk), delete, returning, check
  expressions raised in-statement (`dist_api.check_violation`), exact
  Hasura error shapes, backend_only, transactions.
- **M7 — harness** 🔄: run_suite.sh + triage.py; see
  tests/hasura/COVERAGE.md for the live conformance table.

Next: websocket transport, `--hge-bin` harness mode (env-marked classes),
GraphQL introspection, inherited roles, relay, v1 data API reads. Later:
subscriptions, event triggers, actions, remote schemas.

## Decisions and why

- **Full v2 format** — tests-py applies as-is; metadata exported from
  existing Hasura projects loads without conversion.
- **IR as the boundary** — the core is testable without a database; a second
  data backend, if ever needed, implements the IR instead of rewriting the
  engine.
- **One SQL statement per query** — Hasura v2's key performance property:
  no N+1, no in-process result stitching.
- **No runtime console** — files are the source of truth; `/v1/metadata`
  exists only as a protocol for tests-py and tooling.

## Conformance harness no longer uses the runtime admin API (2026-06-13)

The conformance harness (`crates/conformance`) sets up each suite WITHOUT
the engine's runtime admin API (`/v1/query`, `/v2/query`, `/v1/metadata` —
run_sql + metadata mutation), so that API can later be deleted. The engine
still ships it; the harness simply never calls it.

- `Suite::start()` creates the per-suite database and the postgis extension
  directly via the `postgres` crate (no engine).
- Setup fixtures are parsed and APPLIED in-harness: `run_sql`/`insert` run
  over the suite database via `postgres`; metadata ops (track_table,
  permissions, relationships, inherited roles, query collections,
  computed/remote fields, functions + function permissions, remote schemas
  + permissions) accumulate into an in-memory `dist_metadata::Metadata`.
  mssql_* ops are ignored; unknown ops panic.
- The engine starts LAZILY on the first request: the accumulated metadata
  is serialized to a `version: 3` directory (`version.yaml`,
  `databases/databases.yaml`, and `inherited_roles`/`query_collections`/
  `allow_list`/`remote_schemas` when non-empty) and passed via
  `--metadata-dir`. `Running::post` intercepts the admin-API paths and
  applies them in-harness instead of hitting the engine.
- Teardown of metadata is a no-op (per-suite DB + fresh metadata dir), but
  per-method DATA teardown (run_sql/insert resets between mutation cases)
  still runs against the live database.

**Dropped (admin-API-as-test-step; that API is going away):**

- `crates/conformance/tests/v1_queries.rs` (whole module) — it tested the
  legacy `/v1/query` DATA API directly.
- `crates/conformance/tests/remote_schemas.rs` (whole module, + its
  `support/remote_stub.rs`) — it tested add_remote_schema / SDL validation /
  remote-schema management, and remote-schema EXECUTION needs the engine to
  introspect upstreams at boot, which it does NOT do from YAML. Deferred
  until boot-time upstream introspection exists.
- `roles_inheritance.rs`: the cycle-detection step (export_metadata +
  replace_metadata) in `graphql_inherited_roles_schema`; the
  `resolve_inconsistent_permission.yaml` case (get_inconsistent_metadata +
  runtime permission pivot) in `graphql_mutation_roles_inheritance`; the
  `override_inherited_permission.yaml` case (runtime
  create_function_permission pivot for a non-admin inherited role) in
  `custom_function_permissions_inheritance`.
- `auth_env.rs`: `test_update_query` from the allowlist class (it mutated an
  allowlisted collection at request time and asserted the duplicate-query
  error from the mutation API). Allowlist ENFORCEMENT is kept via metadata.
  The bespoke `EnvEngine` spawner was removed; the unauthorized-role/cookie
  and function-permission suites now use the regular lazy `Suite`/`Running`
  (function perms split into three per-method suites for isolation).

## Known issues (from the 2026-06-13 unit-test review; not yet fixed)

- `crates/server/src/remote.rs::resolve_url_template` substitutes only the
  first `{{VAR}}`; a `}}` preceding `{{` can slice with start>end and panic.
- `apply_presets` Boolean coercion is silent (non-"true" -> false), unlike
  the Int coercion-error path.
- claims_map mode reports a non-array `x-hasura-allowed-roles` as
  `jwt-missing-role-claims`, while direct-claims mode reports
  `jwt-invalid-claims` with the Aeson parse message.
- No include-cycle guard in `crates/metadata/src/loader.rs` and the
  conformance fixture loader (self-include recurses to stack overflow).
- `load_metadata_dir` ignores directory-form `inherited_roles` /
  `query_collections` / `allowlist` / `remote_schemas` (only the
  single-document form carries them).
- `parse_array_literal` (session array literals "{a,b}") splits naively on
  commas — breaks for quoted values containing commas.
- sqlgen renders literals inline with quote-escaping; parameterized
  execution remains a planned refactor (see crates/sqlgen/src/lib.rs).

## Admin role (Hasura parity, 2026-06-13) — remaining gaps

The `admin` role was implemented (reversing the earlier no-admin-role rule):
full permission bypass on the data plane, v1 data API, mutations,
introspection (incl. NON_NULL for FK object relationships), and allowlist.
Two admin-adjacent gaps remain, gated with `FIXME(engine-admin)` /
`FIXME(engine-customized-error)` in crates/conformance/tests/remote_schemas.rs:

- **Admin forwarding through a remote schema.** `match_remote_with`
  (crates/server/src/remote.rs) matches a remote schema only via
  `schema.permissions.find(role == session.role)`; admin has no permission
  entry, so admin queries to remote fields fall through to the local
  planner ("field not found"). Faithful fix: for admin, match by the
  upstream schema (already captured in `AppState.remote_upstreams` at
  add_remote_schema) and forward the query verbatim (skip validate_field /
  apply_presets), still applying decustomize for customized schemas.
  Requires threading the upstream schemas into the matcher.
- **Customized-schema validation error names** (pre-existing, not admin):
  validation errors for a customized remote schema report de-customized
  upstream names/paths instead of the customized spelling the client used,
  because validate_field runs over the decustomized document.

## Admin API removed (2026-06-13)

The runtime admin/management API was DELETED from the engine (not
feature-gated): `crates/server/src/ops.rs` and `remote_validate.rs` removed;
the `/v1/query`, `/v2/query`, `/v1/metadata` routes, `query_api`,
`check_admin_secret`, `optional_session`, and `AppState.remote_upstreams`
are gone. The serving binary has no run_sql and no metadata mutation —
verified live (those routes 404, `/v1/graphql` 200). Configuration is
deploy-time only: `migrate` (DDL) + YAML metadata at boot.

Consequence (accepted): the conformance suites that tested the management
API itself were dropped — `tests/v1_queries.rs` (legacy /v1/query data API)
and `tests/remote_schemas.rs` (+ remote_stub; remote-schema execution needs
boot-time upstream introspection, deferred), plus a few management-API test
STEPS in roles_inheritance.rs / auth_env.rs. The harness now sets up each
suite without the admin API: DDL/seed via direct SQL, metadata accumulated
in-harness and loaded via `--metadata-dir` (lazy engine start). 228 tests
green. Remaining: remote-schema support from YAML needs boot-time upstream
introspection (the data-plane forwarding code in remote.rs remains).
