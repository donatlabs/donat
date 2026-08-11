---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A deduplication that lapses when the incident is resolved is not a retention, and a rejection is not an absorption

## Context

[[067-a-retention-with-an-escape-clause-is-not-a-minimum-retention]] closed with
three recorded shapes of idempotency near-miss, and an invitation: "A reviewer
meeting a fourth provider now has three precedents to sort it against rather than
one rule to reinterpret." Spec 027 named PagerDuty as the batch's likely
`ExplicitKey` candidate and asked for exactly that sorting.

PagerDuty publishes two deduplication mechanisms, and they are not the same
thing.

**The Events API v2's `dedup_key`** is the famous one, and it is the plain "wrong
endpoint" shape. PagerDuty's own published schema for it declares the server
`https://events.pagerduty.com/v2` — a different origin from the
`https://api.pagerduty.com` this connector compiles — and describes the field as
"The key used to correlate triggers, acknowledges, and resolves for the same
alert", which is an *alert lifecycle* correlator rather than a statement about
repeating a request. It is Linear's `OAuthApplicationCreateInput.idempotencyKey`
and Salesforce's UI API key one more time.

**`incident.incident_key` is on the endpoint this connector declares**, which is
what makes it interesting. `POST /incidents` publishes it in the request body:

> A string which identifies the incident. Sending subsequent requests
> referencing the same service and with the same `incident_key` will result in
> those requests being rejected if an open incident matches that `incident_key`.

That sentence carries two of the three things
[[042-the-effect-gate-admits-evidence-not-methods]] asks for. There is a binding
(a body property this connector could write) and there is a uniqueness scope
(the same service and the same key). It is also the first mechanism in this
programme to be published on the exact create a connector declares and still not
reach the class.

## Decision

**`pagerduty.incident.create` is `AtMostOnce`, and PagerDuty's own sentence is
quoted inside the evidence that refused it.**

Three things are wrong with it as an `ExplicitKey`, and only the second is a
shape ADR 067 already had a name for.

**A rejection is not an absorption.** This is the new one, and it is the
sharpest. `ExplicitKey` tells the activity worker one thing: *send again, the
provider will absorb it, and the second response describes the first send's
outcome*. PagerDuty publishes the opposite. A duplicate that arrives while the
incident is open is **rejected** — the Process gets a failure for a send that
succeeded, which is a third outcome, not the first one repeated. AWS SQS FIFO
returns the original message id and Zendesk replays the original response; both
of those are absorptions. A refusal is a different contract wearing the same
word.

**There is no window.** PagerDuty publishes no retention of any kind for
`incident_key`: not a duration, not a count, not a response header. Its lifetime
is a *resource lifecycle* — the incident's — and an incident is resolved by a
human clicking a button, by an automation, or by the service's own
`auto_resolve_timeout`, at a moment nothing in this repository can observe.
`ExplicitKeyEvidence::documented` takes a `Duration` and demands a clock safety
margin strictly under it, and there is no number here to cite. This is Microsoft's
`transactionId` and Mercado Pago's `X-Idempotency-Key` in a new disguise: not "a
window nobody wrote down" but "a window that is not measured in time at all".

**The escape clause is the resolution.** The moment the incident closes, the same
request with the same key opens a **second** incident, and pages whoever is on
call for it. That is monday.com's per-user budget with a lifecycle in place of a
memory limit: a condition the provider states, that the connector cannot observe,
under which the mechanism silently stops working.

So the near-miss register now has a fourth shape, and it is a *composite*: the
mechanism is on the right endpoint, and it fails on the window, on the escape
clause, and on what it does instead of absorbing. The three earlier shapes each
failed on one thing; sorting PagerDuty against them required reading what the
class *promises the runtime* rather than counting how many of ADR 042's three
boxes were ticked.

**`incident_key` stays a declared input.** It is a field of the incident
PagerDuty publishes, and a deployment that wants PagerDuty's own behaviour — one
open incident per key per service — may send one. What it does not do is reach
the effect gate: nothing in the runtime writes it, nothing reads it back, and no
class rests on it. That is deliberate, and it is the opposite of ADR 067's
"bind the header anyway" alternative, which was refused because a *binding* is
what `ExplicitKey` is admitted on. An ordinary input is not a binding: it is a
value a Process chose, indistinguishable to the runtime from a title.

## Alternatives

| Option | Why Not |
|--------|---------|
| Classify it `ExplicitKey` on the binding and the scope, and record the missing window as a caveat | The class's whole content is "a repeat is absorbed". PagerDuty publishes "a repeat is rejected, unless the incident closed, in which case a repeat is a second incident". Recording that beside the class would be a note nothing enforces — ADR 067's first refusal, word for word |
| Set a small declared retention, on the argument that a Process resolves incidents slowly | The margin would be a guess about an *event*, not a duration: an incident can be resolved one second after it opens. A guess that is usually right is the worst kind of idempotency guarantee |
| Treat the documented rejection as the deduplication, and map it to success | The engine would be inventing a success out of a failure it cannot distinguish from a validation error. PagerDuty publishes no `code` for the duplicate case, so there is nothing to key that on even if it were sound |
| Use the Events API v2 instead, where `dedup_key` is first-class | A different origin is a different connector ([[074-a-second-origin-is-a-second-connector-and-a-download-is-composed-under-its-bound]]), and the Events API is an ingestion surface with a routing key per service integration rather than an account credential — a different credential, a different scope, and a different set of operations. It is a connector somebody may write; it is not this one |
| Leave the create `InventoryOnly` rather than `AtMostOnce`, since a mechanism exists | ADR 063 is admitted on a recorded search and a recorded consequence, and both exist here. Refusing the class because the provider published something that does not work would leave the most ordinary monitoring write in the workspace unreachable for a reason that helps nobody |
| Bind `incident_key` from the activity's stable key without claiming the class | A declaration the gate ignores is [[034-a-declaration-the-runtime-ignores-is-a-defect]] pointed the other way, and the runtime would be writing a value whose only documented effect is to make the retry *fail* |

## Consequences

The near-miss register has a fourth shape and it is the one a reviewer is most
likely to get wrong, because it looks the most like a hit: the mechanism is
published, it is on the right endpoint, it names its scope, and it is still not
an idempotency key. The evidence string carries PagerDuty's sentence and the
words "a rejection rather than an absorption, with no published retention, which
lapses the moment the incident is resolved", so the disqualification travels with
the operation rather than living only here.

`incident.create` and `incident_note.create` are reachable from a Process only
through an activity that declares `at_most_once` and a route for an outcome
nobody can know. For paging, that is the right trade: a duplicate page is loud
and expensive, and an ambiguous send is visible and routed.

The cost is the same one ADR 067 named. PagerDuty's mechanism is genuinely useful
to a *caller who retries by hand*, and this connector buys nothing from it. The
input is still there for a deployment that wants it, which is the most this
repository can honestly offer.
