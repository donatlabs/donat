---
name: donat-deploy-and-verify
description: Use when setting up a local donat stand, wiring CI, deploying, writing tests for a donat app, or diagnosing an empty result, a rejected request or a parked process.
---

# Running and verifying an application

## The order, and why it is the order

```sh
donat migrate  --migrations-dir /usr/share/donat/migrations   # the engine's own donat.* schema
donat migrate  --migrations-dir migrations                     # this application's schema
donat migrate  --migrations-dir /usr/share/donat/migrations \
               --metadata-dir metadata --source default        # Process revisions, if any
donat validate --metadata-dir metadata              # metadata against the real schema
donat serve                                         # reads both, runs neither
```

The first line is the engine's own schema — cron state, command claims, Process
journals — which ships **inside the image** at `/usr/share/donat/migrations`, beside
the binary that applies it. Nothing to vendor and nothing to mount. It is not
optional for any deployment: the serving engine checks its helpers at boot
(`donat.check_violation` among them) and refuses to start without them, however
little of the format the metadata uses. Both sets share one history table,
which is why every migration is timestamped rather than numbered.

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
| `DONAT_GRAPHQL_UNAUTHORIZED_ROLE` | the role an unauthenticated request runs as; unset means such a request is rejected |
| `DONAT_GRAPHQL_JWT_SECRET` | JWT verification config — how role and session variables are established |
| `DONAT_GRAPHQL_AUTH_HOOK` | a service that resolves the session instead, given the request's headers |
| `DONAT_OIDC` | the engine's own browser login: `/auth/login`, `/auth/callback`, `/auth/logout` |
| `DONAT_ADMIN_DIR` | a directory of built platform-UI files the engine serves itself; unset or empty serves none |
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

`DONAT_ADMIN_DIR` is why a stand can be one container. The engine serves those
files as a router fallback — after every one of its own paths, never in front
of one — so the platform UI, the API and the identity provider proxy are one
origin without a reverse proxy in front of anything. That matters for signing
in rather than for tidiness: the provider's session cookie is `__Host-`-prefixed
and it compares `Origin` against its own public URL, so a login page served
from a second origin is refused. Serving the UI elsewhere is still supported —
leave the variable unset and put a proxy in front — but then the origin is
yours to keep consistent.

## Authentication and roles

In production, issue a JWT carrying the role and the session variables. The
role selects the permission set; the session variables (`x-donat-user-id` and
friends) are what row filters compare against, and a client cannot influence
them.

A boot with none of `DONAT_GRAPHQL_JWT_SECRET`, `DONAT_GRAPHQL_AUTH_HOOK` or
`DONAT_GRAPHQL_UNAUTHORIZED_ROLE` **refuses to start**: it could resolve a
session for nobody. For a local stand, the cheapest of the three is a token you
sign yourself (`examples/mint-token.sh`), sent like any other:

```bash
TOKEN=$(examples/mint-token.sh customer customer-1)

curl -s localhost:8080/v1/graphql \
  -H 'content-type: application/json' \
  -H "authorization: Bearer $TOKEN" \
  -d '{"query":"{ orders { id order_status } }"}'
```

A request that names **no** role is not privileged: it falls back to
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
| `no authentication was supplied and this deployment sets no unauthorized role` | nothing verified the request, and no unauthorized-role configured |
| `Authentication hook unauthorized this request` | the hook refused it, and no unauthorized-role configured |
| `validate` fails on an expression | a nullable column read without `not_null:` / `when_present:` — see `donat-validators` |
| Upsert rejected | `on_conflict.constraint` does not name a real unique constraint |
| `revision ... is not deployed as active`, retrying forever | the Process deploy step was skipped — the `migrate` that also reads `--metadata-dir` |
| An error naming a `donat.*` table nobody recognises | the engine's own migrations were not applied — the `migrate` run against `/usr/share/donat/migrations` is missing |
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
