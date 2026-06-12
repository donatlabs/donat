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
| `crates/server` | axum HTTP server: `/v1/graphql`, `/v1/query`, websockets, auth |
| `crates/conformance` | Native conformance harness + Hasura-derived fixtures (Apache 2.0) |
| `tests/hasura` | Legacy pytest harness (optional cross-check; superseded by `crates/conformance`, safe to delete) |

## Quick start

```sh
make build
make test           # unit + snapshot tests (no database needed)
make conformance    # full conformance suite (needs Postgres, see below)
make run            # serves :8080 with the fixture metadata
```

Conformance Postgres: `postgis/postgis:16-3.4` reachable as
`postgresql://postgres:postgres@127.0.0.1:15432/postgres` (override via
`PG_URL`). Each suite spawns its own engine on a fresh database, so runs
are hermetic and parallel.

CI (GitHub Actions) runs both test tiers on every push/PR and uploads
release binaries for linux-x86_64 and macos-aarch64 as build artifacts.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
Conformance fixtures are derived from Hasura's tests-py suite (Apache 2.0,
see `crates/conformance/fixtures/LICENSE.hasura`).
