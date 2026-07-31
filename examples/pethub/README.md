# Pethub example

**Pethub is the platform that hosts Petshop stores.**

Petshop is one store. Pethub is the SaaS that runs thousands of them: a
merchant signs up, gets a store, invites staff, defines their own roles, and
runs the entire Petshop commerce domain inside their own isolated tenant.

This example exists to prove one claim:

> Multitenancy is an engine capability, not a domain concern. The Petshop
> business YAML is included here **byte-for-byte unchanged**. Everything that
> makes it multitenant lives in `metadata/tenancy.yaml`, `metadata/iam.yaml`,
> and the platform tables and commands below.

If a future change requires editing a file under `../petshop/metadata` to make
tenancy work, the tenancy design has failed and must be revised instead. That
is the acceptance criterion for this example, and it is what
[ADR-015](../../knowledgebase/declarative-saas/decisions/015-petshop-modular-pressure-suite.md)
means by "an engine-wide capability that composes with every data and
execution surface".

## Status: contract-first, intentionally red

This is active metadata, not a sketch — and the runtime does not implement
`tenancy:` or `iam:` yet. The YAML is written and reviewed as a user-facing
contract first; the implementation follows from failing conformance cases.
This mirrors how the Petshop pressure suite was built (ADR-013, ADR-015).

## The two layers

| | Compiled roles | Tenant IAM |
|---|---|---|
| Examples | `customer`, `staff`, `fulfilment`, `support`, `merchant_owner` | `inventory_clerk`, `weekend_support`, whatever the merchant invents |
| Defines | the **shape**: which tables, columns and operations exist at all | the **scope**: which rows and which actions |
| Lives in | metadata, git, deploy-time | rows in the tenant's own tables, runtime |
| Changed by | the platform, by deploying | the merchant, through commands |
| How many | a handful | unbounded, different per tenant |

A merchant cannot invent a compiled role — that would mean compiling a new
GraphQL schema per tenant. A merchant *can* invent an IAM role, point it at an
existing compiled role, and grant it a narrower set of actions. This is the
same split Hasura users build by hand with `user_project_roles` tables and
`_exists` predicates; here the engine derives the predicates instead.

## What is in wave 1

Merchant signup and store creation, staff invitations, per-tenant IAM roles
and grants, ownership transfer, and plan entitlements (a plan caps products,
members and locations).

## What is deliberately deferred to wave 2

Each of these needs its own decision before it gets YAML, and folding them in
now would make this example unreviewable:

- **Platform billing** — the only legitimate cross-tenant read. Needs an
  explicit, audited cross-tenant role, not a hole in a filter.
- **Support impersonation** — a platform operator acting inside a tenant.
  Needs a time-bounded, audited contract.
- **Per-tenant connector credentials** — every store has its own payment
  account. [ADR-010](../../knowledgebase/declarative-saas/decisions/010-static-community-connector-factory-and-runtime-boundaries.md)
  explicitly left this open.
- **Tenant offboarding** — export and erasure.
- **Per-tenant cron fan-out** — `donat.cron_events` is unique on
  `(trigger_name, scheduled_time)`, so subscription renewal for one tenant
  would consume the occurrence for all of them.

## Layout

```text
examples/pethub/
├── metadata/
│   ├── version.yaml
│   ├── tenancy.yaml          # NEW engine object: how a tenant is identified and enforced
│   ├── iam.yaml              # NEW engine object: how in-tenant grants become predicates
│   ├── rules.yaml            # platform rules and enums
│   ├── commands.yaml         # platform commands + every Petshop command, unchanged
│   └── databases/
│       └── default/tables/   # platform tables + every Petshop table, unchanged
└── migrations/               # platform DDL only; the store DDL comes from Petshop
```
