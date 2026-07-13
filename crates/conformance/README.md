# donat-conformance

Native conformance harness: YAML fixtures executed against a spawned
`donat`, with fixture-format parsing, response comparison (selection-set key
ordering) and a legacy-Apollo websocket transport. This crate is the
conformance source of truth.

## Running

```sh
make conformance
# or a single module:
cargo build -p donat-server --bin donat
cargo test -p donat-conformance --test jwt
```

`make conformance` is the complete Postgres reference suite. It requires
Postgres (`postgis/postgis:16-3.4`) at `PG_URL` (default
`postgresql://postgres:postgres@127.0.0.1:15432/postgres`). Every suite gets
its own database (`conf_<suite>`) and engine process (logs:
`target/conformance-logs/<suite>.log`), so suites run in parallel and never
share state. **Rebuild the engine binary after engine changes** — the harness
spawns the existing `target/debug/donat`.

For the full local backend check, including disposable Postgres, MySQL, and
ClickHouse services:

```sh
make db-up
make conformance-matrix
make db-down
```

`conformance-matrix` runs the shared contract on Postgres, SQLite, MySQL, and
ClickHouse, then runs the live MySQL and ClickHouse runtime suites. The
external runtime tests fail when a configured service is unavailable; ordinary
workspace tests make only a quick no-service probe so they remain usable
without Docker.

## Suites

| Module | Ported from (tests-py) | Tests |
|---|---|---|
| `graphql_queries` | TestGraphqlQueryPermissions | 1 (23 fixtures × http+ws) |
| `agg_relay_introspection` | AggPerm ×2, RelayQueriesPermissions, introspection user | 4 |
| `graphql_mutations` | Insert/Update/Delete permission classes | 3 |
| `v1_queries` | V1 Select/Count/Insert/Update permissions | 4 |
| `roles_inheritance` | Inherited roles: queries, nested, mutations, functions | 5 |
| `auth_env` | Unauthorized role, cookie fallback, function perms, allowlist | 5 |
| `jwt` | The full test_jwt.py matrix (438 of 441; 3 ws-expiry live in subscriptions) | 34 |
| `jwk` | test_jwk.py cache-control/expires refresh timing | 6 |
| `subscriptions` | TestSubscriptionBasic + JWT ws-expiry | 4 |
| `remote_schemas` | Remote-schema permissions/presets/relationships (Rust stub upstream replaces the node services) | 4 |

Out of scope by design: admin/no-role tests (no-admin-role rule — the
functionality is being removed), enterprise-only classes. Excluded cases
are commented at their call sites with reasons.

Conventions for porting more suites: [PORTING.md](PORTING.md).
Fixture provenance and local-patch policy: [fixtures/README.md](fixtures/README.md).
