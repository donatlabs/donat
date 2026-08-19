---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
---

# Petshop is a modular single-tenant pressure suite

## Context

A single checkout and payment happy path does not exercise the range of
declarative behavior needed by substantial SaaS products. Extending that one
flow with every pricing, inventory, payment, fulfilment, B2B, marketplace,
subscription, booking, and approval concern would make the example
unreadable. Treating marketplace vendors as platform tenants would also mix a
store-domain concern with engine-wide data isolation.

## Decision

Petshop becomes one single-tenant reference application composed of independent
YAML modules over a shared commerce core. The modules cover retail pricing,
multi-location inventory, payments, partial fulfilment, returns,
subscriptions, B2B approval, marketplace payouts, booking, prescription
review, and operations.

The desired active YAML is authored and reviewed before missing runtime
behavior or test coverage is implemented. Petshop may remain intentionally red
during that contract-first interval. Multitenancy is excluded from the
Petshop domain model and will be designed as an engine-wide capability that
composes with every data and execution surface.

## Alternatives

| Option | Why Not |
| --- | --- |
| One comprehensive checkout flow | Couples unrelated lifecycles and becomes difficult to understand, validate, and recover. |
| One repository example per scenario | Duplicates the commerce core and does not prove that runtime capabilities compose. |
| Model vendors and B2B organizations as tenants | Confuses domain ownership with platform isolation and leaves non-store SaaS examples without a common tenancy model. |
| Implement runtime primitives before writing product YAML | Makes the DSL follow implementation convenience rather than representative user needs. |

## Consequences

The example becomes larger, but every module has a clear product purpose and
can justify generic runtime behavior independently. A bounded fan-out
primitive may be introduced because both shipment allocation and vendor
payouts require it; no general loop or arbitrary workflow language follows
from that need.

The active example can temporarily fail to compile while its YAML is ahead of
runtime support. That state must be explicit and short-lived. Multitenancy
requires its own design, ADR, conformance boundary, and implementation plan.

*Delivered 2026-08-18:
[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] and
[[098-a-compiled-role-is-the-shape-and-a-grant-is-the-scope]], with the
`tenancy` and `pethub` conformance suites as the boundary. `examples/pethub`
composes this example's metadata unchanged and adds only a platform layer —
which is what "an engine-wide capability that composes with every data and
execution surface" was supposed to mean, now asserted rather than intended.*
