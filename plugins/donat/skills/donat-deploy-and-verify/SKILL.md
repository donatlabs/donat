---
name: donat-deploy-and-verify
description: Use when setting up a local donat stand, wiring CI, deploying, writing tests for a donat app, or diagnosing an empty result, a rejected request or a parked process.
---

# Running and verifying an application

## The order, and why it is the order

```sh
donat migrate  --migrations-dir migrations    # DDL, then Process revisions
donat validate --metadata-dir metadata        # metadata against the real schema
donat serve                                   # reads both, runs neither
```

`validate` checks metadata against the schema **as it actually is**, so running
it before `migrate` passes for the wrong reason. Both belong in CI and in the
deploy pipeline; a metadata error must be a deploy failure, never a request
failure.

What `validate` catches: a permission naming a column that does not exist, a
validator whose expression cannot type-check against the declared nullability,
a command step writing an untracked table, a decision-table test case that does
not hold, an MCP tool published for a role the table does not grant. It is the
closest thing this architecture has to a compiler — treat a red `validate` the
way you would treat a compile error.

## Local stand

```sh
docker build -t ghcr.io/donatlabs/donat:latest .
cd examples/petshop
docker compose up          # engine, Postgres, MinIO, mock providers
docker compose down -v     # reset, dropping the seeded volume
```

Copy `examples/petshop-rest/docker-compose.yml` for a new application — it is
the smaller of the two and has no provider stubs to strip out.

## Environment

The ones that shape behaviour rather than merely point at things:

| Variable | Meaning |
|---|---|
| `DONAT_GRAPHQL_DATABASE_URL` | the Postgres source |
| `DONAT_METADATA_DIR` | the metadata directory to boot from |
| `DONAT_PORT` | listen port |
| `DONAT_GRAPHQL_ADMIN_SECRET` | marks a request **trusted to assert a role** — API auth, never a permission |
| `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` | the role an unauthenticated request runs as; unset means such a request is rejected |
| `DONAT_GRAPHQL_JWT_SECRET` | JWT verification config — the production way to establish role and session variables |
| `DONAT_GRAPHQL_ENABLED_APIS` | restrict the mounted surfaces, e.g. `graphql` |
| `DONAT_GRAPHQL_ENABLE_ALLOWLIST` | serve only saved operations |
| `DONAT_REQUEST_TIMEOUT_SECONDS`, `DONAT_PG_STATEMENT_TIMEOUT_SECONDS` | request and statement bounds |
| `DONAT_UPSTREAM_MAX_BODY_BYTES` | ceiling on upstream response bodies |
| `DONAT_PG_SSL_ROOT_CERT` | TLS to Postgres |
| `DONAT_SHUTDOWN_GRACE_SECONDS`, `DONAT_SHUTDOWN_READINESS_DELAY_SECONDS` | the two-phase drain |
| `DONAT_PROCESS_WORKERS_DISABLED`, `DONAT_PROCESS_POLL_MILLISECONDS`, `DONAT_PROCESS_TRANSITION_CONCURRENCY` | process worker runtime |
| `DONAT_GRAPHQL_MAX_ACTIVE_SUBSCRIPTIONS`, `DONAT_GRAPHQL_MAX_CONCURRENT_SUBSCRIPTION_POLLS` | live-query bounds |
| `DONAT_LOG_FORMAT` | structured logging |

Secrets referenced from metadata (`value_from_env:`) are read from the same
environment. Never put a credential in a metadata file.

## Authentication and roles

In production, issue a JWT carrying the role and the session variables. The
role selects the permission set; the session variables (`x-donat-user-id` and
friends) are what row filters compare against, and a client cannot influence
them.

For a local stand or a test, `X-Donat-Admin-Secret` marks the request as
trusted so it may assert `X-Donat-Role` and session headers by hand:

```bash
curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H 'x-donat-admin-secret: petshop-secret' \
  -H 'x-donat-role: customer' \
  -H 'x-donat-user-id: customer-1' \
  -d '{"query":"{ orders { id order_status } }"}'
```

A trusted request with **no** role is not privileged: it falls back to
`DONAT_GRAPHQL_UNAUTHORIZED_ROLE`, or is rejected with
`x-donat-role header is required`. There is no admin role, so "run it as
nobody to see everything" is not a thing that exists.

## Health and drain

- `/healthz` — liveness. Is the process alive?
- `/readyz` — readiness. Should the load balancer send traffic?

They are separate on purpose. On `SIGTERM` the engine fails readiness first,
waits `DONAT_SHUTDOWN_READINESS_DELAY_SECONDS` for the load balancer to notice,
then drains in-flight work within `DONAT_SHUTDOWN_GRACE_SECONDS`. Point the
rolling deployment's readiness probe at `/readyz` and its liveness probe at
`/healthz`; wiring both to the same endpoint defeats the drain.

`/v1/version` reports what is running.

## Testing an application

**Test the refusals, not just the successes.** A permission is only proven by
the request it turns away. For every role and table, assert at least:

1. the role sees its own rows,
2. a *different* session's rows are absent — an empty list, not an error,
3. a role with no permission gets the access-denied contract,
4. a validator's message comes back verbatim, with code `validation-failed`.

For commands and processes, assert:

5. a replayed idempotency key returns the original result and writes nothing new,
6. a guard that should fail rolls back **every** step, not just its own,
7. a process reaches its terminal state, and its failure branches are reachable —
   route a provider stub to an error and assert the `fail` code.

The petshop's black-box system tests are the model: they drive the real binary
over HTTP with scripted provider stubs, and assert the journal in
`donat.process_*` rather than trusting a response body.

## Inspecting a running process

```sh
donat process inspect        --source default --instance <uuid>
donat process verify-history --source default --instance <uuid>
```

Both are read-only; `verify-history` exits non-zero on an inconsistency. There
is deliberately no admin HTTP surface for the journal. If an operator needs to
*list* instances, the supported route is to declare the `donat.process_*`
tables as ordinary tables with an ordinary per-role `select_permission` — a
permission, not a bypass.

## Common failures

| Symptom | Cause |
|---|---|
| Empty result where rows exist | the row filter, usually a session variable that is not what you think — echo it back in a query first |
| `x-donat-role header is required` | trusted request with no role, and no unauthorized-role configured |
| `validate` fails on an expression | a nullable column read without `not_null:` / `when_present:` — see `donat-validators` |
| Upsert rejected | `on_conflict.constraint` does not name a real unique constraint |
| Duplicate provider effect | a mutating connector operation with no `provider_idempotent` evidence, or a retry window longer than the provider's key retention |
| Process parked forever | a `wait` with no `deadline`/`on_timeout`, or a `request` error class with no route |

## Files to read

- [`examples/petshop-rest/docker-compose.yml`](https://github.com/donatlabs/donat/blob/main/examples/petshop-rest/docker-compose.yml)
  — the smaller stand; copy this shape for a new application
- [`examples/petshop/docker-compose.yml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/docker-compose.yml)
  — the full one: engine, Postgres, MinIO, mock providers, and every env var in
  context
- [`migrations/README.md`](https://github.com/donatlabs/donat/blob/main/migrations/README.md)
  — the deploy order, and why it is that order
- [`crates/conformance`](https://github.com/donatlabs/donat/tree/main/crates/conformance)
  — how the project itself drives the real binary over HTTP; the model for an
  application's own black-box tests
