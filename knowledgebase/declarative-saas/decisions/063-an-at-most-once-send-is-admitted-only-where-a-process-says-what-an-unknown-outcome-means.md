---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# An at-most-once send is admitted only where a Process says what an unknown outcome means

## Context

[[042-the-effect-gate-admits-evidence-not-methods]] admits two executable
mutating classes, both of them *provider* guarantees: `ExplicitKey`, where the
provider deduplicates on a key it documents and retains, and `NaturalMethod`,
where the provider documents a `PUT` or `DELETE` against a fixed resource
identity as repeat-safe. Everything else is `InventoryOnly` — declared, typed,
tested, and unreachable from a Process.

That left the programme with 75 declared mutating operations across 28
connectors that no Process could reach, and
`crates/connectors/src/providers/INVENTORY.md` is the record of why: for most of
them, the provider publishes a complete request contract with no idempotency key
in it, and the connector cannot invent one. A Slack message, a SendGrid mail
send, a Twilio SMS, a Jira issue, a Teams post, an SES email — every write the
product actually asks for is in that list. Spec 010 §7 named the way out and
declined to take it: "a batch that wants such a write executable must first land
the ADR that introduces a third class with explicit at-most-once opt-in at the
Process level. That ADR is out of scope here and is not assumed."

The reason it could not be assumed is that the class is genuinely weaker than
the two already admitted. A durable activity may be retried, or taken over after
a worker loses its lease, and neither retry nor takeover is safe for a write the
provider will happily perform twice. The only thing Donat can do for such an
operation is decide **not** to make the second send — which means that when a
worker is lost, the Process is left with a send whose outcome nobody knows.

## Decision

**`EffectClass::AtMostOnce` is admitted on evidence of an absence, and a Process
reaches it only through a per-activity opt-in.**

`NoIdempotencyEvidence::searched` takes the two things `INVENTORY.md` already
recorded for every one of these operations: which documentation was searched —
`AbsenceSearch::PublishedContract` for an endpoint reference that enumerates its
whole request contract, `MachineReadableDescription` for an OpenAPI, discovery
document, or GraphQL schema in which the term does not occur — and **what a
second send would produce**. The consequence is required because it is the thing
an operator accepts: "a second delivered email", "a second charge", "a second
workflow run".

The opt-in is `at_most_once: true` on `ProcessRequestActivity` and
`ProcessRequestState`, together with a mandatory `on_ambiguous` destination.
Compilation refuses four ways, each with the metadata path of the declaration
that is wrong: an at-most-once operation referenced without the opt-in; the
opt-in on an operation whose provider absorbs a duplicate; the opt-in without
the route; and the route without the opt-in. The last two are
[[034-a-declaration-the-runtime-ignores-is-a-defect]] in both directions.

**The opt-in is per activity, not per connector, because the trade is a property
of the flow rather than of the operation.** The same `mail.send` is acceptable
in a Process that reconciles afterwards — a later state that asks the provider
what happened, or a human queue — and unacceptable in one that will silently
drop a customer's only notification. A connector-level switch would let one
Process's judgement admit an operation for every other Process in the
deployment, which is exactly the "promotion by proximity" `INVENTORY.md` said
the class must not have.

**`on_ambiguous` is not an `on_error` route, because an ambiguous send is not a
failure.** `on_error` routes *failures*: the engine knows what happened and
which of the eight closed classes it was, and a route or the fallback claims it.
An ambiguous send is the absence of that knowledge — the authorization was
claimed, the worker that owned it was lost, and no outcome was recorded. No
class in the closed set means "unknown", and overloading `permanent` or
`timeout` would tell the Process something the engine does not know. So the
outcome carries the Donat-owned code `provider_send_ambiguous`, the transition
consumer routes it **by code** to `on_ambiguous` before any `on_error` route or
fallback is consulted, and the journal records it as
`activity_ambiguous_routed` rather than `activity_error_routed`.

**The runtime enforcement is one row, claimed once.**
`process_activity_provider_steps` already existed to record
`first_provider_attempt_at` before a provider-idempotent send; an at-most-once
activity inserts into it with `ON CONFLICT DO NOTHING` and reads the affected
count. One row inserted is authorization to send. Zero rows means the
authorization was already claimed — by an earlier attempt or by a worker that
has since died — and the send is refused as ambiguous. The row commits before
any byte leaves, so the guarantee is exactly *at most once*: it is never
*exactly once*, and the ambiguous route fires whether the request was sent, was
never sent, or was sent and answered into a void.

**`retry_on` must be empty and `max_attempts` must be exactly one.** This is the
sharpest part of the change, and the reasoning is worth writing down. A
retryable failure class on an at-most-once activity means a second provider
attempt. The four classes the runtime actually retries — `transport`, `timeout`,
`http_429`, `http_5xx` — are precisely the ones where a request may already have
reached the provider: a timeout is the ambiguous case *by definition*, a
transport error cannot distinguish a connection that never opened from a
response that never came back, and a `5xx` on a non-idempotent write may sit on
top of a completed one. There is no subset of them that is safe here. Worse,
declaring one would be inert: the send authorization is already claimed, so the
second attempt would be refused and land on the ambiguous route regardless,
after burning the retry interval. A declaration the runtime cannot honour is a
defect, so compilation refuses it rather than ignoring it. `max_attempts` above
one is refused for the same reason. A lease *takeover* is not an attempt and is
not affected: it still happens, and it is the path that produces the ambiguous
outcome.

**The class is visible to the catalog, because the runtime acts on it.**
`OperationEffect` gains a third variant, `AtMostOnce`, which
[[049-a-connector-publishes-the-declaration-it-was-admitted-on]] explicitly
declined to add for `NaturalMethod` on the grounds that it would buy nothing at
runtime. Here it buys everything: the process compiler cannot enforce the opt-in
without seeing the class, and the activity worker cannot claim the one send
authorization without it. It is a format change to `OperationEffectMaterialV1`
and to the owner manifest in
[[012-canonical-catalog-projections-and-persisted-header-capabilities]], and it
is additive — every existing operation's canonical bytes and hashes are
unchanged, which the fixed public pipeline hashes in
`crates/connector-catalog/tests/canonical_hashes.rs` prove.

**43 of the 75 inventory-only operations were reclassified**, plus two `aws_ses`
sends and the standard-queue `aws_sqs.message.send`
([[046-an-effect-class-can-depend-on-deploy-time-configuration]]), for 46 in
total. The bar is both halves of the evidence: a recorded search that found no
mechanism, **and** a recorded consequence that is not the same outcome as the
first send. Twenty-nine operations deliberately stay `InventoryOnly`, in four
groups that `INVENTORY.md` now names: writes a provider documents as repeat-safe
(they want a class that *keeps* the retry, not one that trades it away); partial
updates for which no consequence is recorded at all; two operations where the
provider publishes a client-supplied deduplicating identifier this connector has
not bound (`microsoft_outlook.event.create`, `google_calendar.event.insert`);
and OpenAI's two, on this repository's own recorded judgement that a duplicate
generative call is a charge nobody can look up and an answer nobody can
reproduce.

## What this supersedes

* **ADR 042's closing paragraph** claimed a third class would be "one variant in
  `EffectKind`, one arm in `EffectClass::is_executable`, and its own evidence
  type", with no caller changing. That is true of the SDK and **false of the
  engine**, and the difference is the whole of this ADR. Executability in the
  SDK is a necessary condition, not the gate: the class had to become visible in
  a second enum (`OperationEffect`) and its versioned canonical material, the
  process compiler had to learn four refusals and a new pair of metadata fields,
  the state graph had to learn a new transition target, the activity worker had
  to learn to claim one send authorization, and the transition consumer had to
  learn a destination that is not an error route. `Effect::class` is indeed the
  only question the SDK's callers ask, and that part of the claim held.
* **Spec 010 §7's table and its closing sentence**: "A batch that wants such a
  write executable must first land the ADR that introduces a third class with
  explicit at-most-once opt-in at the Process level. That ADR is out of scope
  here and is not assumed." This is that ADR. The table now has a fourth row,
  and `InventoryOnly` remains a real class for everything that fails the
  evidence bar. Spec 010 §15's first open decision is closed; the second — a
  `ProviderIdempotent` class whose evidence is a documented repeat-safe write on
  a method HTTP does not define repeat-safety for — is explicitly *not* closed,
  and is what `intercom.company.create_or_update`, `aws_sqs.message.delete`, and
  the three Google operations that are idempotent in effect are still waiting
  for.
* **ADR 049's mapping table** gains its third answer. The catalog effect no
  longer answers only "does this need a key?"; it also answers "may this be sent
  more than once at all?", and `AtMostOnce` is the one class that says no.

## Alternatives

| Option | Why Not |
|--------|---------|
| Make the class a connector-level property, so an operation is at-most-once everywhere | The trade is "never twice, sometimes never", and whether that is acceptable depends on what the Process does next. One deployment's judgement would silently admit the operation for every other Process in the source — the promotion-by-proximity failure `INVENTORY.md` named. |
| Route the ambiguous outcome through `on_error` with a new `ProcessErrorKind::Ambiguous` | It would be catchable by an `on_error` fallback, which is how every *failure* is caught, and a fallback written for failures would then silently absorb an unknown. It would also put a non-failure into a closed class set whose eight values are a conformance contract. |
| Let the ambiguous outcome reuse `timeout` | It tells the Process something the engine does not know: that nothing reached the provider. That is the single most expensive wrong answer this class can give. |
| Permit `retry_on: [http_429]`, on the argument that a rate-limited request was rejected | Not published as a contract by any provider in this population, and a `429` from an edge that had already forwarded the request is indistinguishable from one that had not. Admitting one class would also make the rule a judgement per provider rather than a property of the class. |
| Let `max_attempts > 1` stand, since the runtime refuses the second send anyway | The declaration would be inert: it would burn a retry interval and land on the ambiguous route regardless. ADR 034 is exactly about declarations that parse, deploy, and do nothing. |
| Keep `OperationEffect` at two variants and add a separate `at_most_once: bool` to `OperationSpec` | It splits one question — what is this operation's effect class — across two fields, and it is a *larger* canonical change: every operation's material would gain a key, where a new tagged variant leaves every existing operation's bytes untouched. |
| Reclassify every inventory-only mutation, since the opt-in is what gates them | Two of them publish a deduplicating identifier this connector could bind, and six are documented as repeat-safe. Giving a repeat-safe write a class that forbids the retry is a worse contract than leaving it unreachable, and stepping past a real provider mechanism with an opt-in is the failure ADR 042 exists to prevent. |
| Derive the opt-in from the presence of `on_ambiguous` alone | The opt-in would be implicit, and the one thing this class must not be is implicit. Two fields make the refusal readable in both directions. |

## Consequences

46 provider writes across 22 connectors are reachable from a Process for the
first time, and every one of them arrives with a named consequence attached to
its declaration. An operator writing `at_most_once: true` is accepting, in
writing, that the send may never happen; the ADR, the module, and the projection
all say what "may never happen" costs for that specific operation.

The failure mode this introduces is a Process instance sitting in a state the
deployment chose for "sent, outcome unknown". That is the point — it is visible,
it is named by the deployment, and it is reachable by an ordinary transition —
but it is a state every deployment using this class now has to design. A
deployment that routes `on_ambiguous` straight to a `fail` state has chosen to
give up on reconciliation, and that choice is at least legible in the metadata.

An at-most-once activity has no retries at all, which means an ordinary
`connection refused` before the send ends the activity rather than trying again
a second later. That cost is real and it is deliberate: the engine cannot tell a
connection that never opened from one whose response was lost, and this class
exists precisely for the operations where guessing wrong is expensive.

Three costs are worth naming. The send authorization row is claimed even when
the request is never made, so a worker that dies between the commit and the
first byte produces an ambiguous outcome for a send that certainly did not
happen — conservative, and not tightenable without a second commit point the
provider cannot participate in. `OperationEffectMaterialV1` grew a variant,
which is a canonical format change, and the owner manifest in ADR 012 was
edited to match. And the class rests on a *reviewer* reading the evidence
constructor's two strings, exactly as ADR 042's `ReadOnlyAssertion` does: a
consequence sentence that is wrong is not machine-detectable, and the type only
guarantees that somebody had to write one.
