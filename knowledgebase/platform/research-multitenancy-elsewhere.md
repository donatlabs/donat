---
type: research
status: draft
date: 2026-08-23
---

# Who else builds tenancy in, and what they pay for it

Written after [[declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]
shipped, to answer two questions the ADR asserted without checking: is the
"declare it once" model actually unusual, and is there a platform whose answer
is better than ours.

Both answers are useful, and neither is the one the ADR assumed.

## Hasura does not do this at all

Not in v2, and — the part worth checking, because the metadata model was
rewritten — not in DDN either. `ModelPermissions` is declared per model per
role, with no default filter, no permission template, no inheritance of rules,
and no way to require that every model has one. The documented multi-tenant
answer is a `tenant_id` column, an `X-Hasura-Tenant-Id` session variable, and
the same filter repeated everywhere.

Subgraphs in DDN are a module system for *teams*, not tenants. Our `extends:`
is the nearer analogue and is stricter, because it refuses a collision instead
of overriding.

Corroborated from the other side: Prefect, whose multi-tenant SaaS on Hasura is
Hasura's own showcase talk, describes exactly that shape — tenant column, JWT
carrying user/tenant/role, five roles, views for column slices — plus a Python
ORM layer and an Apollo auth service they had to write themselves.

So the premise of ADR-097 holds, and holds more strongly than it claimed: the
repetition is not a thing careless deployments do, it is the documented
practice of the engine we are compatible with.

## Postgres itself does, and it is stronger where it counts

Supabase and PostgREST both bet on **row-level security**: the policy lives on
the table and the database adds the `WHERE`. That is a genuinely better
property than a compiler layer in one respect — a bug in the application cannot
bypass it, and the bound holds for every client, including `psql` and any
second service on the same database. Our layer binds requests that go through
the engine, and nothing else.

What it costs is session state. The policy needs the tenant, which arrives as a
GUC, and under transaction pooling it must be `SET LOCAL`. A plain `SET`
outlives its transaction and leaks onto the next request sharing that backend —
which is a cross-tenant read produced by one missing keyword. Our tenant is a
literal in the statement; there is no session state to leak.

`knowledgebase/research-metadata-architecture.json` already weighed
introspection+RLS against a metadata store and chose the store, because RLS
ties permissions to Postgres semantics rather than a role model. That decision
stands and is about the *permission model*. It does not cover the narrower
idea below.

## Nile builds it into the database

"PostgreSQL re-engineered for multi-tenant apps": tenants virtualized inside
the database, virtual per-tenant databases, per-tenant backups, per-tenant
placement on shared or dedicated compute, and performance isolation between
tenants.

Its feature list is, almost item for item, what
[[declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]
lists as out of scope: offboarding, export, and the `schema`/`source` bindings.
That is not a reason to copy it — they rewrote a storage layer — but it does
say the deferrals are the right shape, and that `binding:` deserves to stay in
the enum as the place those answers will attach.

## Two things worth taking

**Per-tenant resource limits.** Hasura Cloud's `set_api_limits` keys a rate
limit on a session variable — `unique_params: ["x-hasura-team-id"]` — alongside
depth, node, time and batch limits, global with per-role overrides. Nile names
"performance isolation between tenants" independently, as one of the reasons to
rebuild the database.

Two platforms arriving at the same item from different directions is worth more
than either alone. We have nothing here: `quotas.yaml` bounds how many rows a
tenant *holds* and says nothing about how many requests it *makes*. Inbound
rate limiting does not exist at all; what exists bounds outbound connector
calls. A `node_limit` is also absent and is not the same as the depth guard: a
hundred roots each with a hundred children is ten thousand nodes at depth two.

So isolation is finished and fairness is not, and the shape to copy is already
proven.

**RLS as a second fence for the tenant only.** Not as the permission model —
that was decided — but as policies emitted from `tenancy.yaml` that bind
everything which is *not* the engine: a migration, a `psql` session, a second
service on the same database. The engine keeps its compiler layer and needs no
GUC, so the `SET LOCAL` footgun never arrives. This is defence in depth against
the one gap our model structurally has, and the declaration to derive it from
already exists.

## What not to take

Virtualizing the tenant inside the database is a different class of project.
Engine plugins — Hasura DDN's pre-parse and pre-response HTTP hooks — are a
return to every deployment writing its own, which is what
`declaring-not-coding` exists to prevent.

## Sources

- Hasura DDN [Model Permissions](https://hasura.io/docs/3.0/auth/permissions/model-permissions/), [Engine Plugins](https://hasura.io/docs/3.0/plugins/overview/)
- Hasura v2 [API limits](https://hasura.io/docs/2.0/security/api-limits/), [`set_api_limits` metadata](https://hasura.io/docs/2.0/api-reference/metadata-api/api-limits/), [Roles & session variables](https://hasura.io/docs/2.0/auth/authorization/roles-variables/)
- [Prefect on Hasura](https://hasura.io/blog/architecture-authorization-multi-tenant-saas-platform-with-hasura-prefect), [multi-tenant modelling issue #2836](https://github.com/hasura/graphql-engine/issues/2836)
- [Supabase RLS](https://supabase.com/docs/guides/database/postgres/row-level-security), [RLS patterns for multi-tenant SaaS](https://makerkit.dev/blog/tutorials/supabase-rls-best-practices)
- [PgBouncer transaction pooling and `SET LOCAL`](https://multi-tenant-saas.com/tenant-aware-data-routing-query-scoping/connection-pooling-in-multi-tenant-systems/pgbouncer-transaction-pooling-for-multi-tenant-saas/), [Approaches to tenancy in Postgres](https://planetscale.com/blog/approaches-to-tenancy-in-postgres)
- [Nile](https://www.thenile.dev/), [Re-engineering Postgres for Millions of Tenants](https://www.scylladb.com/tech-talk/the-nile-approach-re-engineering-postgres-for-millions-of-tenants/)
