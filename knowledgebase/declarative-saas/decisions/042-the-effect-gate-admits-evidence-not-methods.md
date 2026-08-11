---
type: decision
status: accepted
date: 2026-08-09
features:
  - "[[declarative-saas]]"
---

# The effect gate admits evidence, and a connector is one declaration

## Context

Spec 010 §7 makes effect classification the hard gate on connector work: a
durable activity may be retried or taken over after an ambiguous worker loss,
so an operation that cannot survive being sent twice is not executable. Two
executable mutating classes are admitted — `ProviderIdempotent::ExplicitKey`
and `ProviderIdempotent::NaturalMethod` — and everything else is
`InventoryOnly`: declared, typed, tested, and unreachable from a Process.

Before this slice the SDK had no notion of an effect at all. The gate existed
in one place only, `crates/server/src/connectors/catalog.rs`, as the rule that
an operation without a complete `effect`/`bounds`/`error_map`/`capacity`
contract is not published to process compilation. That rule is correct and
stays, but it is expressed in catalog terms and says nothing to a hand-written
connector module, which is where every batch's operations will be written.

The obvious shape for the SDK gate — "a mutating method may not be ReadOnly" —
turned out to be wrong for the product. The Petshop metadata declares seven
`POST` operations as `read_only`: quotes, lookups, reconciliations, searches
whose selector does not fit a query string. Providers publish reads as `POST`
routinely. A gate keyed on the method would have made every one of them
inventory-only, which buys no safety and would have broken working
deployments.

## Decision

The gate admits *evidence*, not methods. `Effect` is an opaque type over a
private enum with four constructors, and each one takes the evidence its class
requires:

- `read_only()` — a `GET`, where the method itself is the evidence. Declaring
  it on a mutating method does not build.
- `read_only_documented(statement)` — a mutation-shaped read, where the
  provider's own contract states the call creates and changes nothing. The
  statement is required precisely because the method no longer proves it, and
  it is the thing a reviewer checks. Declaring it on a `GET` does not build
  either: a `GET` needs no assertion, so making one is a defect rather than
  caution.
- `provider_idempotent_explicit_key(evidence)` — the binding, the uniqueness
  scope, the documented minimum retention, and a clock safety margin *strictly*
  smaller than that retention. `ExplicitKeyEvidence::documented` is the only
  constructor and refuses an incomplete or out-of-order set, so an operation
  cannot reach this class with a header name and nothing else.
- `provider_idempotent_natural_method(statement)` — `PUT` or `DELETE` only,
  with the provider statement; any other method does not build.
- `inventory_only(reason)` — everything else, and the reason is required.

An operation that declares no class at all is not executable, and
`Connector::build` refuses to publish one, so "every operation carries a class"
holds for everything a deployment can reach while an SDK fixture can still
render a request without inventing a classification.

The declarative `http` connector compiles the same classes out of deployment
metadata, and the mapping is the catalog's own rule said in the SDK's words: a
`GET` is read-only; a `read_only` declaration on a mutating method is read-only
on the deployment's assertion, because the deployment authored the operation; a
complete `provider_idempotent` contract is `ExplicitKey`; and everything else —
including the legacy bare `idempotency: { header }`, which names a binding but
publishes no retention to keep a margin under — is inventory-only. Where the
catalog publishes an operation the SDK classifies inventory-only, registry
construction now fails rather than letting two descriptions of one operation
disagree.

A connector is one `Connector` declaration: name, contract version, origin,
credential specification, operations, triggers. The compiled module table is
keyed on `&'static Connector`, and each entry carries the module's own
deploy-time metadata rules, which used to be a `match` on the module name
inside `state.rs`. Adding a connector is a module file and a table line;
forgetting to widen a second file is no longer possible.

Three notes on the declaration types. `Origin` stays the *resolved*
scheme/host/port a request renders against, and spec 010 §4's
`Origin::TemplatedHost` is `OriginSpec::TemplatedHost`: keeping the template
and the resolved origin apart makes resolution a deploy-time step with a type
of its own instead of a check every render repeats. `OriginSpec` has a third
variant, `DeploymentOrigin`, because the declarative connector's origin is
genuinely a configuration key — the alternative was to describe that connector
dishonestly. And a webhook trigger's verification is a closed set of schemes
over exact raw bytes, with the Stripe verifier re-expressed on it so the
contract has a real consumer rather than a speculative one.

## Alternatives

| Option | Why Not |
|--------|---------|
| Gate on the method: no mutating method may be `ReadOnly` | Refuses the seven `POST` reads the Petshop metadata already declares and every provider that publishes a search as a `POST`; buys no safety, because a create would simply be declared inventory-only either way |
| Let `ExplicitKey` accept a binding alone, without retention or scope | That is the legacy `idempotency: { header }` form, and a key a provider has already forgotten is not an idempotency key. Admitting it would make the class mean "someone typed a header name" |
| Reject inventory-only operations at metadata validation for every connector | The declarative connector's inventory-only operations are deployable today and run through the legacy transport path; the gate that matters is that they are never *published* to process compilation. Rejecting them at startup would break working deployments to enforce a rule they already obey |
| Require the effect on `OperationBuilder::build` | Every SDK fixture that renders a request would have to invent a classification, and several would have to invent idempotency evidence to stay buildable. The gate belongs where a connector is assembled |
| Keep the per-module `match` in `state.rs` | Adding a connector then means editing a file that has nothing to do with connectors, and forgetting to is silent |

## Consequences

An operation that mutates and cannot show admitted evidence has no spelling
that makes it executable, and the failure arrives at build time with the
missing piece named. The classes are visible to the registry, so a deployment
enabling an unknown or inventory-only operation is refused before a listener
opens, with the metadata path in the message.

The gate is not absolute, and it should not be read as one. Two classes rest on
an assertion rather than a proof: a provider statement for a mutation-shaped
read, and a deployment's own declaration for the connector whose operations a
deployment authors. Both are recorded in the type — `ReadOnlyAssertion` names
which one applies — so a review can see exactly what is standing behind an
executable class, but neither is machine-checkable and neither pretends to be.

The third executable class the product decision contemplates — a mutating class
with an explicit at-most-once opt-in at the Process level — is one variant in
`EffectKind`, one arm in `EffectClass::is_executable`, and its own evidence
type. Every caller asks `Effect::class`, so no caller matches on a class name
of its own and none of them changes when it lands.
