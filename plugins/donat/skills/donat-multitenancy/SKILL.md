---
name: donat-multitenancy
description: Use when an application serves more than one customer organisation - declaring tenancy.yaml, per-tenant isolation, in-tenant grants (iam.yaml), plan ceilings (quotas.yaml), or composing a platform layer over a domain with extends.
---

# Multitenancy

A tenant is not a column you remember to compare in every permission of every
table. It is one declaration, and the compiler does the rest.

That distinction is the whole skill. A hand-rolled tenant repeats the same
predicate in the `filter` and the `check` of every permission of every table,
and the sixtieth table is the one somebody forgets. Here, forgetting stops the
deployment instead.

## The declaration

`metadata/tenancy.yaml`:

```yaml
source: default
binding: row_key            # one database, a tenant column on every table
variable: X-Donat-Tenant-Id
trust: jwt_claim            # a verified token, and nothing else
key: tenant_id

registry:
  table: { schema: public, name: store }
  key: id
  status: { column: status, serving: [active] }

# A table whose tenant key has another name.
keys:
  - { table: { schema: public, name: store }, key: id }

# A table that genuinely belongs to no single tenant.
exempt:
  - table: { schema: public, name: plan }
    shared: read_only
```

Once that exists, a permission written `filter: {}` means **every row of my
tenant**. Nested selections, Relay, subscriptions, REST, MCP, commands and
durable processes inherit it, because the predicate is added at the one point
every table resolution passes through.

## Two properties, and both are deploy-time

**Forgetting a table stops the deployment.** Every tracked table in the
tenanted source either carries the key, or names itself under `keys:` because
its key is spelled differently, or under `exempt:` because it belongs to
nobody. A table that says none of the three fails `donat validate` and refuses
to boot, naming itself.

**The tenant is a claim, never a header.** It arrives the way a role does, from
a verified token or an authentication hook. `X-Donat-Role` selects among roles
a token already granted; there is nothing for a tenant header to select among,
so no header names one. A request that cannot say which tenant it is in is
refused, not answered with an empty page. See `donat-authentication` for
putting the claim in the token (`DONAT_OIDC_TENANT_CLAIM`).

## Nesting: every level is scoped on its own

A nested selection resolves the *remote* table's permissions, so a three-level
query gets three independent predicates:

```sql
FROM "public"."product"  WHERE "tenant_id" = '…'   -- level 1
  FROM "public"."review"   WHERE … AND "tenant_id" = '…'   -- level 2
    FROM "public"."comment"  WHERE … AND "tenant_id" = '…' -- level 3
```

Depth inherits nothing. That is the point: a child row whose foreign key
points into another tenant — through a bug, a migration or a race — is turned
away by its own key rather than admitted by its parent's. Make the database say
the same thing from the other side by giving cross-tenant references composite
foreign keys, `(tenant_id, id)`.

## `exempt` is a decision, not an escape

| Spelling | Means |
|---|---|
| `shared: read_only` | Platform reference data. A write permission on it for a tenant-facing role is refused at load. |
| `scope_via: <relationship>` | Reached through a relationship that is itself scoped, rather than by a key of its own. |

Putting the registry table under `exempt:` instead of `keys:` would publish
every tenant to every tenant. It is keyed by its own identifier because its
rows each belong to exactly one tenant — the one they are.

## Commands and processes

A command declares where its tenant comes from:

| Spelling | Use |
|---|---|
| `tenant: none` | the default |
| `tenant: establishes` | registration: the command creates the tenant it then writes into |
| `tenant: { from: <step> }` | the tenant is read from a row an earlier step resolved |

A read before the establishing step resolves normally; a **write** before it is
refused, because that row would belong to nobody. A step may be
`tenant: unscoped` only where `unscoped_steps: audited` is declared, and
`donat validate` lists every such use.

The tenant joins every command idempotency scope and rides with a durable
process instance, so two tenants that pick the same idempotency key do not
replay each other's results, and a restarted instance keeps its tenant.

## In-tenant authorization: `iam.yaml`

Tenancy answers *which store*. It does not answer *what this person may do
inside it*, and two people holding the same compiled role often differ.

**A compiled role is the shape** — which tables and operations exist at all. It
lives in metadata, in git, and changes by deploying. **A grant is the scope** —
rows the tenant writes for its own people, so access changes without a deploy.

```yaml
grants:
  table: { schema: public, name: iam_grant }
  subject: { column: user_id, variable: X-Donat-User-Id }
  tenant:  { column: tenant_id }
  action:  { column: action }
governed_roles: [staff]
wildcards: ["{resource}:*", "*:{verb}", "*:*"]
actions:
  default: { select: "{table}:read", insert: "{table}:create" }
```

Wildcards are expanded at deploy into equality lists an index answers; nothing a
merchant wrote is ever executed as a pattern. `reserved_actions` are barred by
the database on the table the tenant writes, not by whichever command happens
to write the row — so a role able to grant actions still cannot grant itself
platform ones.

Workers hold no grants, and shoppers are not staff: putting `customer` or
`anonymous` under `governed_roles` denies every request they make.

## Plan ceilings: `quotas.yaml`

A ceiling is a layer over the domain's own insert permission, never an edit to
it. The counter moves **inside** the statement that performs the write — a
`COUNT(*)` taken beforehand is the version everybody writes and the one that
does not hold, because under READ COMMITTED every concurrent writer reads the
same pre-lock count and every one of them passes.

`donat validate` refuses a plan ceiling no entitlement consumes, and refuses a
command that writes a counted table without going through the entitlement.

## Composing a platform over a domain: `extends.yaml`

```yaml
extends:
  - path: ../../petshop/metadata
```

The domain is composed in, not copied, so there is no second copy to drift and
`git diff` against the base is the proof it was not edited. Merging **refuses
to override**: a collision is an error, never a silent replacement, because an
overlay that could quietly replace a base permission would make every audit of
the base meaningless.

The one exception is the permission-bounds policy, where the stricter of the
two answers wins — a base that requires unbounded permissions to declare
themselves must not be loosened by composing on top of it.

## Isolation is not ownership

The commonest mistake, and one this repository made in its own example.

Tenancy separates customers of the platform. It says nothing about who owns a
row *inside* one tenant. In a marketplace, one store contains many sellers, and
scoping only by tenant gives every seller every other seller's orders — the
predicate is satisfied and the rows are still not theirs.

Ownership cannot be compiled the way a tenant is: the path to an owner differs
per table, and whether a table has one at all differs per role, because the
catalogue belongs to every shopper and to no seller. So it stays in the
permission's own `filter`, and what makes it reviewable is
`unbounded_permissions: declared` — see `donat-tables-and-permissions`.

## Checklist

1. `tenancy.yaml` written; `donat validate` green — it names any table that
   carries no key and claims no exemption.
2. The tenant claim reaches the token, and a request without one is refused.
3. Cross-tenant foreign keys made composite in a migration.
4. Natural keys scoped per tenant — a slug unique in a store, not globally.
5. Views redefined to carry the driving table's tenant key; a view is a table
   to the engine, therefore tracked, therefore scoped.
6. Two tenants, the same query, disjoint rows — and a write naming the other
   tenant's id lands in the caller's own.
7. A suspended tenant is refused, and the refusal is a different shape from an
   empty page.

## Files to read

- [`examples/pethub/metadata/tenancy.yaml`](https://github.com/donatlabs/donat/blob/main/examples/pethub/metadata/tenancy.yaml) — the whole declaration, commented
- [`examples/pethub/metadata/iam.yaml`](https://github.com/donatlabs/donat/blob/main/examples/pethub/metadata/iam.yaml) and [`quotas.yaml`](https://github.com/donatlabs/donat/blob/main/examples/pethub/metadata/quotas.yaml) — grants and ceilings
- [`examples/pethub/migrations/V20260819000002__pethub_tenant_column.sql`](https://github.com/donatlabs/donat/blob/main/examples/pethub/migrations/V20260819000002__pethub_tenant_column.sql) — what it costs in the database: the column and index on every table, views redefined, natural keys scoped, references made composite
- [`crates/conformance/tests/pethub.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/pethub.rs) — two tenants, one query, disjoint rows
- ADRs: [097 tenancy](https://github.com/donatlabs/donat/blob/main/knowledgebase/declarative-saas/decisions/097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered.md), [098 grants and quotas](https://github.com/donatlabs/donat/blob/main/knowledgebase/declarative-saas/decisions/098-a-compiled-role-is-the-shape-and-a-grant-is-the-scope.md)
