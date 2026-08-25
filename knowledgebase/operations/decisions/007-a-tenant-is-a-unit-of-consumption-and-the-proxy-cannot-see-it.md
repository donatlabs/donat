---
type: decision
status: proposed
date: 2026-08-23
features:
  - "[[operations]]"
  - "[[declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]"
---

# A tenant is a unit of consumption, and the proxy cannot see it

## Context

[[001-bounded-and-drainable-by-default]] bounds a request: a statement timeout
under a request deadline, an upstream response read against a ceiling, a
drainable loop. Every one of those bounds is per request. None of them is per
*caller*, so a thousand cheap requests from one tenant cost exactly what a
thousand from a thousand tenants cost, and the eleventh store on a deployment
finds out about the tenth by getting slower.

[[declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]
made the tenant a compiler layer, so isolation of *data* is finished.
`quotas.yaml` ([[declarative-saas/decisions/098-a-compiled-role-is-the-shape-and-a-grant-is-the-scope]])
bounds how many rows a tenant *holds*. Neither says anything about how much of
the engine a tenant *uses*, and that is the half a platform is judged on once
the leaks are gone.

Two platforms name it independently
([[platform/research-multitenancy-elsewhere]]): Hasura Cloud keys a rate limit
on a session variable — `unique_params: ["x-hasura-team-id"]` — beside depth,
node, time and batch limits; Nile lists "performance isolation between tenants"
as one of the reasons it rebuilt Postgres.

## The rule this has to fit

Spec 008 already states the engine's position, in a comment on the one limit it
does enforce:

> The engine does no network-level rate limiting anywhere — that belongs to the
> reverse proxy — but these two are counted against rows it owns, which a proxy
> cannot see.

That is not an obstacle to this decision; it is its spine. **The engine bounds
what a proxy cannot see, and nothing else.** The question for each limit below
is not "is this useful" but "can a proxy already do it".

## Decision

`limits.yaml`, per role, keyed by tenant where `tenancy.yaml` exists:

```yaml
source: default

# Fields in one operation, counted over the parsed document — the measure
# Hasura calls a node limit. Not rows: what this bounds is how much a caller
# may ask for in one go, before any of it is planned.
nodes:
  global: 10000
  per_role:
    customer: 2000

# Operations per minute, counted per tenant. A proxy can rate-limit by address
# and by header; it cannot rate-limit by tenant, because the tenant is a claim
# inside a token the proxy does not verify. That is the same argument spec 008
# makes for rows, and it is the only reason this belongs here at all.
requests:
  per_role:
    customer: { per_minute: 600 }
```

**A node ceiling is the engine's because the engine already parsed the
document.** This is weaker than the claim made for the tenant below, and saying
so is the point: a proxy *could* count fields, by reimplementing a GraphQL
parser and keeping it in step with the schema this engine generates. It does
not, and asking every deployment to build one is not an answer. The count is
taken from the document the engine parses anyway, before any statement is
built, so a refusal costs one traversal.

Depth is the wrong measure and is already bounded (`MAX_QUERY_DEPTH`): a
hundred roots with a hundred fields each is ten thousand nodes at depth two.
Nodes describe what was asked for; depth describes only how far down it
reaches.

**A request ceiling is the engine's only because of the claim.** Keyed on
anything a proxy can read — an address, a header — it belongs to the proxy and
this declaration must not accept it. Keyed on the tenant it cannot, because
establishing the tenant means verifying the token, which is this engine's job
and not the proxy's.

**It is per replica, and says so.** A deployment behind three replicas gets
roughly three times the declared rate. Making it exact means shared state —
Redis, or a table written on every request — and a request-path dependency on
either is a worse failure than an approximate ceiling. The declaration is a
guard against one tenant taking everything, not a billing meter, and the
documentation says that in those words rather than leaving an operator to
discover it.

**No time limit here.** A per-role statement timeout would be a second answer
to a question `pool_settings.statement_timeout` and `DONAT_REQUEST_TIMEOUT_SECONDS`
already answer, and configuration naming a fact twice is what
[[006-configuration-names-a-fact-once-and-refuses-to-choose]] refuses.

## Alternatives

| Option | Why not |
|--------|---------|
| Leave all of it to the reverse proxy | It cannot see nodes, and it cannot see the tenant. Those two are exactly the residue. |
| Rate-limit on the address or a header | A proxy does that better and already does. Accepting it here would be a second place to configure one thing. |
| Exact counting through Redis or a table | Puts a network dependency, or a write, on every request. A ceiling that is approximate and always up beats one that is exact and can fail closed. |
| A depth limit per role | Depth is already bounded and is the wrong measure; nodes is the one that describes the cost. |
| Bill for it instead of bounding it | A meter and a ceiling are different features. This one keeps a deployment answering; metering is a platform concern with its own storage. |

## Consequences

A tenant that asks for too much is refused with an ordinary donat error naming
the ceiling, rather than making the other tenants slower. What it does not give
is fairness under a burst inside the window, or any guarantee across replicas —
both are named above rather than implied.

Where no `limits.yaml` exists nothing changes, which is what lets this land
without touching a deployment that has not asked for it.
