---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
---

# Derive the declarative runtime from the Petshop reference store

## Context

The proposed durable-process specification contains a broad workflow grammar,
but the existing Petshop example is only a CRUD demonstration. Designing the
runtime before a representative SaaS application made it difficult to
distinguish required primitives from speculative framework features.

Petshop also has concrete integrity gaps: client-provided line prices,
non-atomic order creation, a non-quantitative availability flag, unrestricted
manual status changes, and no payment, fulfilment, cancellation, or refund
lifecycle.

## Decision

Replace Petshop with a conventional pet-supplies reference store and use its
black-box business scenarios as the executable requirements for declarative
logic.

Implement the synchronous store core first with existing CRUD, Commands, and
Rules plus only the bounded set-based command additions required by atomic
multi-line checkout. Then implement a provider-neutral mock-payment flow using
real HTTP requests, a deterministic local recorder/callback in CI, and an
optional RequestBin-like endpoint for manual inspection.

Do not implement the complete proposed process grammar ahead of the failing
product cases. Each new runtime primitive must be justified by at least one
Petshop acceptance case while remaining provider-neutral and reusable.

## Alternatives

| Option | Why Not |
| --- | --- |
| Implement the full generic process proposal first | Speculative states and journals may not match the minimum real store needs. |
| Hard-code Petshop services in Rust or PostgreSQL functions | Creates application-specific code and hides which declarative capability is missing. |
| Keep Petshop as CRUD and test the runtime with synthetic fixtures only | Does not prove that the combined data, command, rule, connector, and authorization surfaces build a real SaaS product. |
| Integrate live Stripe immediately | Adds provider and credential complexity before the provider-neutral request, retry, callback, and recovery contract is proven. |

## Consequences

The first delivery takes longer than a synthetic workflow demo because it
includes a credible commerce domain and concurrency tests. In return, every
runtime feature has a product justification, the example becomes useful as a
reference architecture, and future Stripe or community connectors can reuse a
stable payment-flow boundary instead of defining business semantics.
