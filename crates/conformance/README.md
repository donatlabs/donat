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

### tandt ClickHouse contract

The tandt sidecar pins the twelve production GraphQL operations from
`tandt-backend` revision `c780834e50f53e5b4e94f1f33e88748a443f98ec`.
Its manifest maps each operation to the upstream source path and a SHA-256 of
the executable query fixture. It provisions isolated `analytics` and `logs`
databases in real ClickHouse, starts the real Donat binary with a production-
shaped Hasura `configuration.template`, checks exact ordered response bytes,
and includes a mixed Postgres/ClickHouse request.

```sh
docker compose -f docker-compose.conformance.yml up -d --wait
cargo build -p donat-server --bin donat
DONAT_EXTERNAL_DB_TESTS=1 \
PG_URL=postgresql://postgres:postgres@127.0.0.1:15432/postgres \
CLICKHOUSE_URL=http://donat:donat@127.0.0.1:18123 \
  cargo test -p donat-conformance --test tandt_clickhouse_contract -- \
  --test-threads=1 --nocapture
```

`AnalyticsDashboardStats` is intentionally pinned unchanged as a negative
scalar contract. Its inline Date and DateTime strings are data, never SQL.
`AnalyticsDashboardStatsSafe` is a separate Donat-side deployment companion
that uses `$document_since: date!` and `$code_events_since: timestamp!`; tandt
must adopt that request before the dashboard can execute successfully. The
separate `ClickHouseComplexValues` case owns JSON, Map, Tuple, and Array round
trips and does not change `logs_application_logs.context`, which remains the
pinned String field.

The pinned `AnalyticsAggregationOperations` document has no `$offset`
variable or `offset:` argument. Its fixture therefore verifies the first
descending page exactly; testing a second page would require a separate tandt
query change and must not be smuggled into the pinned document.

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
