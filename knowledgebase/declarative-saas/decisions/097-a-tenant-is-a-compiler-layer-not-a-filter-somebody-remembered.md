---
type: decision
status: accepted
date: 2026-08-18
tags: [tenancy, permissions, authentication, deploy-gate]
features:
  - "[[declarative-saas]]"
---

# A tenant is a compiler layer, not a filter somebody remembered

## Context

The engine had no tenant. Every deployment that needed one invented it, and the
invention was always the same shape: a `tenant_id` column, and a
`{tenant_id: {_eq: X-Donat-Tenant-Id}}` repeated in the `filter` and the `check`
of every permission of every table.

[[platform/research-what-a-platform-needs]] §2 named this and named the failure
mode — "every deployment invents it, and the inventions differ in ways that only
show up as a leak" — and asked for an ADR.
[[015-petshop-modular-pressure-suite]] had already excluded tenancy from the
domain example on the grounds that it "will be designed as an engine-wide
capability that composes with every data and execution surface".

Repetition is the whole problem. A hundred and fifty-eight tables need a
hundred and fifty-eight copies of one predicate, in four places each, and the
hundred and fifty-ninth table is added by someone who does not know that. There
is no way to review the absence of a filter: a permission with `filter: {}` is
indistinguishable from a permission whose author decided the table was public.

## Decision

**Tenancy is declared once, in `tenancy.yaml`, and applied by the compiler.**
A deployment names the source, the session variable, the tenant key column, and
the registry table. Nothing else in the metadata mentions a tenant. The
Petshop business YAML that Pethub composes is included unchanged, which is the
acceptance criterion the design set for itself.

Four properties carry the guarantee.

**Forgetting a table is a boot failure.** Every tracked table in the tenanted
source either carries the tenant key, or says why it does not — under `keys:`
because its key has another name, or under `exempt:` because the rows genuinely
belong to no single tenant. A table that says neither stops the deployment,
naming itself, and the refusal names the three correct answers because the two
wrong reflexes — untrack it, exempt it — are the ones that leak. That the
column really exists is proved separately, against an introspected catalog,
because a table nobody mentioned carries the default key *by rule* rather than
by declaration. The type is proved too: a `text` tenant key against a `uuid`
registry never compares equal, and that failure is invisible — it serves an
empty page rather than an error.

**The predicate goes in at one place for reads.** `Planner::permission_predicate`
is the choke point every read passes through — roots, nested relationships,
aggregates, Relay, subscriptions, REST, MCP, and the `_exists` inside a user
filter. It is ANDed *after* the OR of the role's own filters, not inside it,
and that ordering is the decision: `combined_filter` answers "no restriction"
as soon as any of the role's filters is `{}`, and an unrestricted filter on a
tenanted table is the ordinary case rather than the exception. Applying the
tenant afterwards is what makes `filter: {}` mean "every row of my tenant".

**Writes have five choke points, and all five call one helper.** Insert, the
`DO UPDATE` branch of an upsert, update, delete, and the command plane each
assemble their own predicate; `write_permission_filter` is what they share. On
top of it the tenant column is injected as an ordinary permission preset, which
means it overrides the caller's value: an insert naming another tenant's id
does not fail, it lands in the caller's own tenant, exactly as a preset on
`user_id` behaves today. The command plane is included deliberately — it is a
separate, narrower set of permissions ([[019-command-only-table-permissions]])
that never falls back to the ordinary ones, so it would not have inherited the
predicate.

**The tenant is a claim, and never a header.** It reaches the engine the way a
role does — from a verified token or an authentication hook — and from nothing
else. Unlike `X-Donat-Role`, which selects among roles a token already granted,
there is nothing for a tenant header to select among: the claim is the single
value. A request that carries none is refused rather than answered with an
empty page, because an empty page is what a misconfigured token looks like and
the difference matters at three in the morning. This follows
[[api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]];
the earlier draft of this design predated the removal of the admin secret and
allowed a trusted request to supply the header, which is no longer a thing that
exists.

Two escapes are declared rather than discovered. A `scope_via` table — a row
belonging to several tenants — is scoped by a correlated traversal of a named
relationship, and may hold no ordinary write permission at all, because there
is no single value a write could be bounded by. And a `cross_tenant_reads`
entry replaces the tenant bound with a subject bound for one table and one
role: without it a person who has just signed in belongs to some set of tenants
and is in none of them, so a store switcher cannot be built. It substitutes,
never relaxes — the row is still bounded, the bound is still the engine's, and
the same role may not write that table.

The binding is `row_key`: one database, a tenant column on every table. It is
the only variant the enum has. Naming `schema` and `source` without
implementing them would be a declaration the runtime ignores
([[034-a-declaration-the-runtime-ignores-is-a-defect]]); the field exists so
that adding one later stays a single declaration change rather than a rewrite
of the data model, the permissions, and the commands.

**Two blocks the first draft declared are not here.** A `journals:` block
naming what the engine does to its own catalogs, and a `role_resolution:` block
proving the caller holds the compiled role they named in this tenant. Both were
written, both were validated, and neither was read by anything that serves a
request — which is the defect
[[034-a-declaration-the-runtime-ignores-is-a-defect]] is about, and it is worse
in an isolation rule than anywhere else because it reads as a guarantee. What
the journals block described happens unconditionally and is described below
instead; what `role_resolution` described is what
[[098-a-compiled-role-is-the-shape-and-a-grant-is-the-scope]] does, through a
relation that is actually consulted.

**The journals needed saying separately.** The command replay journal is keyed
`(command_identity, scope_hash, key)` and `command_identity` is source +
command + role only ([[008-source-and-role-qualified-command-identity]]). The
tenant appears nowhere in it, so two tenants that pick the same idempotency key
— a request id, an order number, anything a client generates — would each be
served the other's recorded result. Nothing about that is visible from the data
plane: both callers get a well-formed answer for a command they did run. The
tenant therefore joins every idempotency scope in a tenanted source. A durable
process needed the same treatment for a different reason: it outlives the
request that started it, so the tenant joins every role's compiled caller
contract and is persisted with the instance, and a fixed-role state lifts
exactly that one variable back out — not the whole caller session, because a
fixed role is meant to carry no caller identity.

Cron needed nothing. A cron trigger in this engine calls a webhook with a
static payload and touches no table, so there is no occurrence for one tenant
to consume from another. A per-tenant cron would change that, and it is out of
scope.

**What the DDL owes, and what it is easy to miss.** The column is the obvious
half. `examples/pethub` makes the other two visible: a `UNIQUE` over a *natural*
key — a slug, an SKU, an email — is a collision between tenants the moment
there are two of them, and the refusal discloses that somebody else took the
name, so those become `(tenant_id, …)`. Surrogate keys are deliberately left
alone, because a uuid does not repeat across tenants. And a reference to a
row a person identifies — a customer id — becomes composite, which buys more
than the constraint it replaces: a child row then *cannot* name a parent in
another tenant in the database, underneath the predicate rather than beside it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Leave it to each deployment (status quo) | The predicate is repeated per table per operation, absence is unreviewable, and the failure is a leak rather than an error. This is what the research note called the failure mode. |
| Postgres row-level security | Moves the rule into DDL, where `donat validate` cannot read it and the metadata no longer describes what is served. It also needs an identity on the connection; identity reaches Postgres here as escaped literals in generated SQL, never as a GUC, and adding a `SET LOCAL` per request would break the pooled-session bounds [[operations/decisions/001-bounded-and-drainable-by-default]] depends on. |
| A per-tenant compiled schema | `role_schemas` is keyed by role and rendered once at boot; keying it by tenant multiplies the render cost and memory by tenant count, for tenants that share one shape. |
| Schema- or database-per-tenant first | Onboarding then needs DDL, and the serving engine runs none. It becomes a control plane, a fan-out over N migration targets, and a partial-failure story — before anything has proved the predicate itself. |
| `_exists` for a `scope_via` table | `_exists` switches the predicate context to the remote table entirely, so it cannot say "related to *this* row". |
| A `X-Donat-Tenant-Id` header for service-to-service callers | A header that names a tenant is a header that grants access. Such callers mint a token, which is what every other caller does. |
| Apply the tenant inside `combined_filter` | It returns "no restriction" as soon as one of the role's filters is `{}` — the ordinary case — so the tenant would be dropped exactly where it is most needed. |

## Consequences

A deployment declares one file and every surface is scoped: GraphQL, Relay,
subscriptions, REST, MCP, commands, and durable processes, because they all
reach the database through the planner. Adding a table scopes it. Adding a
table *wrongly* stops the deploy.

What it costs:

- **An anonymous surface needs a token.** With `trust: jwt_claim` there is no
  tenant without a claim, so a storefront that serves logged-out shoppers needs
  a guest token from the identity provider. Resolving the tenant from the
  request's host would remove that, and is a separate decision.
- **Every escape is a line in one file.** `exempt:`, `cross_tenant_reads:` and
  `unscoped_steps: audited` are deliberately awkward and deliberately
  countable. They should stay countable on one hand.
- **A tenanted source is Postgres-only** until the conformance backend matrix
  covers another one ([[multi-backend/decisions/006-mandatory-conformance-backend-matrix]]).
- **A produced file needs its tenant named in the activity.** `local.document`,
  `local.code` and `local.image` write through an artifact store built once per
  source, which carries no session — so the tenant travels with the activity
  instead, under `claim_tenant`, the way `claim_session_key` already does. It
  is read by the registry before the input reaches the capability and never
  passed to one: which store a file belongs to is not a capability's decision,
  and a capability that could name one could name another tenant's. A tenanted
  deployment that omits it stores the file outside its tenant's prefix, so the
  declaration has to be written the way any other identity binding is.
- **A declared cross-tenant read is not gated by the registry.** It cannot be:
  the caller of one has no tenant to look the registry up by — that is the
  state it exists for. So a suspended store still appears in the list of
  stores somebody belongs to, which is also how they would learn it is
  suspended.
- **A suspended tenant goes quiet on reads and is refused on every write.**
  The registry's `serving:` gate rides in the check of an insert, an update and
  a delete alike, so all three say so rather than reporting that they changed
  nothing. A read has no check to carry it and answers empty. "Refused before
  planning" would need a round trip before the statement, which this engine
  does not take.
- **One tenanted source per deployment.** A mutation may already target only
  one datasource, so a second tenanted source would need a story for a write
  that spans both before it could mean anything.

Deferred, and named here so they are not mistaken for oversights: platform
billing (the only legitimate cross-tenant read, which needs an audited role
rather than a hole in a filter), support impersonation, per-tenant connector
credentials ([[010-static-community-connector-factory-and-runtime-boundaries]]
left this open), tenant offboarding, and per-tenant cron fan-out.
