# Pethub

**Pethub is the platform that hosts Petshop stores.**

Petshop is one store. Pethub is what runs thousands of them: a merchant gets a
store, their people get roles inside it, and the whole Petshop commerce domain
runs inside their own isolated tenant.

This example exists to prove one claim:

> Multitenancy is an engine capability, not a domain concern. Nothing that
> makes Petshop multitenant lives in Petshop. Everything is in `metadata/` and
> `migrations/` here.

Read that precisely: it is *tenancy* that required no change to the store, not
that the store is frozen. Petshop is edited when Petshop has a bug — and it had
one, fixed alongside this: within a single store, sellers were not isolated
from each other. That is a domain rule, it belongs in the store's own YAML, and
`crates/conformance/tests/petshop.rs` now holds it there.

`git diff ../petshop` is the proof, and
`crates/conformance/tests/pethub.rs` is the executable form of it: it asserts
that the store's metadata still loads on its own, still knows nothing about
tenants, and that composition adds exactly Pethub's four platform tables and
takes nothing away.

## What is metadata and what is DDL

The split is the point.

**Metadata is the whole tenancy surface** — four files, none of which name a
Petshop table except to exempt one:

| File | What it decides |
|---|---|
| `extends.yaml` | that Petshop is composed in rather than copied |
| `tenancy.yaml` | which source is tenanted, which claim carries the tenant, which column holds it, and which registry decides who is served |
| `iam.yaml` | what a merchant's own roles may do inside the store the predicate already put them in |
| `quotas.yaml` | what a plan caps, and where the counter that enforces it lives |

**DDL is what a platform owes a domain it does not own**: the column. Under
`binding: row_key` that is one `donat migrate` and every store has it — which
is also why onboarding a merchant runs no DDL at all. A schema-per-tenant or
database-per-tenant binding would turn the same guarantee into a fan-out over N
targets with a partial-failure story, and would move provisioning into a
control plane.

The migration does three things, and the second and third are the ones people
forget:

1. **The column**, on all sixty tables, plus an index and the same column
   threaded through all fourteen views. A view is a table to the engine — it is
   tracked, so it is scoped, so it has to carry the tenant.
2. **Natural keys become per-tenant.** `UNIQUE (slug)` on `product` is a
   collision between stores the moment there are two, and the refusal tells the
   second merchant that somebody else took the name. Surrogate keys are left
   alone: a uuid does not repeat across stores, so scoping one buys nothing.
3. **References to a customer become composite.** The same person shops at two
   stores, so `customer_id` cannot be globally unique — and once it is
   `(tenant_id, customer_id)`, a cart in one store *cannot name a customer in
   another* in the database, underneath the predicate rather than beside it.

## What the engine does with it

Nothing in `../petshop/metadata` changes, including the permissions that read
`filter: {}` — which is the ordinary shape and exactly the one a hand-rolled
tenancy gets wrong. The compiler ANDs the tenant predicate in after the role's
own filters, so `filter: {}` means *every row of my store*.

Forgetting a table is a boot failure. Every tracked relation either carries
`tenant_id` or says why it does not: `store` is keyed by its own identifier
(under `keys:`), and `plan` is platform reference data (under `exempt:`). A
sixty-first table with neither fails `donat validate`, naming itself.

## Running it

```sh
donat migrate --migrations-dir ../petshop/migrations   # the store's DDL
donat migrate --migrations-dir migrations              # the platform's
donat validate --metadata-dir metadata
```

`validate` is the gate. It introspects the database and proves that the column
each table is *assumed* to carry is really there and really the same type as
the identifier the registry hands out — two columns of different types never
compare equal, and that failure is silent rather than loud.

## What this example does not do

- **An anonymous storefront needs a guest token.** With `trust: jwt_claim`
  there is no tenant without a claim, so Petshop's `anonymous` role cannot
  browse a store without one. Resolving the tenant from the request's host
  instead is a separate decision.
- **Onboarding is seeded, not commanded.** A `register_merchant` command would
  declare `tenant: { establishes: … }`; here the two stores are rows the
  conformance suite inserts, because the subject of this example is the
  isolation rather than the sign-up.
- **One payment account, shared.** Per-tenant connector credentials are out of
  scope, which is also why the provider-supplied unique constraints are left
  global — see the note in the migration.
