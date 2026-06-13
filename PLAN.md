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
