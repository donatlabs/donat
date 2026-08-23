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
| `DONAT_OIDC` | the engine's own browser login: `/auth/login`, `/auth/callback`, `/auth/logout` — one JSON object |
| `DONAT_OIDC_*` | the same fields, one variable each: `..._PUBLIC_URL`, `..._LOGIN_API`, `..._TOKEN_ENDPOINT`, `..._CLIENT_ID`, `..._CLIENT_SECRET`, `..._SCOPES`, `..._COOKIE_SECURE`, `..._ADMIN_KEY`, `..._ADMIN_ROLE` |
| `DONAT_UI_DIR` | a directory of built platform-UI files the engine serves itself; unset or empty serves none |
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

Prefer the flat `DONAT_OIDC_*` variables to the JSON object. Two of them are
not fields but the facts the rest follow from — `..._PUBLIC_URL` is the address
a **browser** uses, and this engine's sign-in screen and callback are at known
paths on it; `..._LOGIN_API` is the address the **engine** reaches the provider
on. Confusing those two is the mistake that produces a login refusing
everything without saying why, and naming each once is what stops it. A secret
also gets to be its own variable rather than a substring inside a JSON string,
which is what lets it come from a file or a secret mount.

Setting the same field both ways is refused at boot, by name. `token_endpoint`
stays explicit whichever form you use: it differs per provider, and a default
would be a guess about somebody else's software.

`DONAT_GRAPHQL_JWT_SECRET` stays one object. Its `claims_map` says where in
*that* provider's token the roles are, and no default can supply it — a wrong
guess does not fail loudly, it hands somebody a session with no role.

`DONAT_UI_DIR` is why a stand can be one container. The engine serves those
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

An application's tests are declarations too. A `*_test.yaml` sits beside the
metadata file it exercises — `public_orders.yaml` → `public_orders_test.yaml`,
`flows/checkout-payment.yaml` → `flows/checkout-payment_test.yaml` — the way Go
keeps `_test.go` beside the source. The loader never boots a test file as
metadata. `donat.test.yaml` at the application root says what the engine needs:

```yaml
metadata: metadata
migrations: migrations
engine_env:
  PETSHOP_PAYMENT_BASE_URL: ${providers}      # the runner's provider stub
  PETSHOP_PAYMENT_API_TOKEN: test-token
```

```sh
donat test --app-dir . --database-url postgresql://postgres:postgres@127.0.0.1:5432/postgres
donat test --filter checkout          # cases whose <file>::<name> contains it
```

Every test case gets a fresh database, both sets of migrations, the Process
revisions, and the real binary serving the metadata; a role comes from the
runner's authentication hook, which turns `X-Donat-Role` / `X-Donat-User-Id`
into a session. Steps, in the order they run:

```yaml
vars: { customer: customer-1 }                           # constants, as ${customer}
tests:
  - name: a shopper checks out and the process authorizes the order
    steps:
      - providers: !include ../testdata/providers.yaml   # path → 200 body; a list is a queue of {status, body}
      - sql: insert into cart (customer_id) values ('customer-1') returning id
        capture: { cart_id: id }                         # ${cart_id} in every later step
      - as: { role: customer, user: customer-1 }
      - graphql: 'mutation { start_checkout(cart_id: ${cart_id}, request_id: "…") { cart_id } }'
        expect: { data: { start_checkout: { cart_id: "${cart_id}" } } }   # subset; matchers below
      - await: { terminal: checkout_payment, expect: { payment_status: authorized } }
      - await: { receptive: return_refund, state: await_support_decision }  # before sending a signal
      - await: { row: grooming_booking, capture: { booking_id: id } }
      - sql: select status from payment where order_id = '${order_id}'
        expect: [{ status: authorized }]                 # as many rows as listed
      - sql: insert into cart_line (cart_id, variant_id, quantity) values (1, 1, -1)
        error: check_violation
      - calls: { path: /v1/payment-authorizations, count: 1, body: { currency: USD } }
      - hold: /v1/payment-authorizations                 # act while the engine is mid-call …
      - await: { held: /v1/payment-authorizations }
      - release: /v1/payment-authorizations              # … then let it finish
      - url: /v1/graphql                                 # the conformance fixture shape, compared exactly
        headers: { X-Donat-Role: anonymous }
        query: { query: "{ customer { id } }" }
        response: { errors: [ … ] }
      - for:                                             # a table: the same steps, one row per example
          - { role: customer, field: update_orders }
          - { role: staff, field: update_refund }
        do:
          - as: { role: "${item.role}" }
          - graphql: "mutation { ${item.field}(where: {}, _set: {}) { affected_rows } }"
            expect: { errors: [{ message: "field '${item.field}' not found in type: 'mutation_root'" }] }
```

Matchers in an `expect`: `@any`, `@present`, `@uuid`, `@number`, `@string`,
`@bool`, `@gt N` / `@gte N` / `@lt N` / `@lte N`, `@regex R`, `@len N`.

That `for` is the whole of what the format takes from a programming language.
There is no `if`, no expression, no nested loop, no loop over a computed
value — and there will not be. A test that seems to need one is a check that
belongs elsewhere: a decision table's `test_cases`, a validator, a CHECK
constraint, or, rarely, a Rust test in `crates/server/tests` that says why it
could not be data.

`await` polls the journal, never the clock; a failure prints the durable
process state and names the engine log. A whole-string `${name}` keeps the
captured value's type, so a captured amount reaches a provider body as a
number.

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
   route a provider stub to an error (`providers` with a `[{status: 503}]`
   queue) and assert the `fail` code.

The petshop's tests under `examples/petshop/metadata/**/*_test.yaml` are the
model; `scripts/check_app_tests.py` refuses a table that grants a role
something without a test beside it.

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
