---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# A retention is read from the reference that owns the operation

## Context

Spec 026 §2 makes an `ExplicitKey` classification need three things quoted from
the provider: the **binding**, the **uniqueness scope**, and the **retention**.
Every connector before PayPal could answer all three from one page, because
every provider before PayPal published one number. Xero: six minutes.
Chargebee: thirty. AWS SQS: five.

PayPal publishes the binding and the scope once, for its whole REST surface, and
the retention **four different ways**:

* Orders v2 — "The server stores keys for 6 hours. The API callers can request
  the times to up to 72 hours by speaking to their Account Manager."
* Billing Subscriptions v1 — "The server stores keys for 72 hours."
* Payments v2, which is where the refund lives — the header is published ("A
  unique ID identifying the request header for idempotency purposes") with **no
  window at all**.
* Invoicing v2 — the header does not occur anywhere in PayPal's own OpenAPI
  description of it.

Beside those, the general *API requests* guide says, in an example that is
explicitly about a refund: "a user calls refund captured payment with the
`PayPal-Request-Id` header… The user can make the call again with the same ID in
the `PayPal-Request-Id` header for up to 45 days."

The handover into this slice recorded two of those — six hours and 45 days —
and the instruction "a connector must bind the shorter". The instinct is right
and the unit is wrong: there are not two retentions for one key, there are four
answers for four APIs, and 45 days is published for the one API whose own
reference declines to give a number.
[[070-a-declared-idempotency-key-is-written-by-the-executor-and-a-window-is-a-startup-check]]
already chose the tie-break in its alternatives table — "The shortest published
number is the only one both statements support, and PayPal's own instruction is
'for information about how long the server stores the ID, see the reference for
your API'" — but did so believing the two numbers described the same operation.
They do not.

## Decision

**A retention is a property of the API that owns the operation, and it is read
from that API's own reference.** `paypal.order.create` and
`paypal.order.capture` carry Orders v2's six hours;
`paypal.subscription.create` carries Billing Subscriptions v1's seventy-two.
Each operation's citation quotes the sentence from its own reference, so a
reviewer comparing the module against a provider page is comparing like with
like — which is what [[049-a-connector-publishes-the-declaration-it-was-admitted-on]]
asks of every published declaration.

**Where the operation's own reference publishes no window, the class is
refused, whatever a guide says elsewhere.** `paypal.refund.create` is
`InventoryOnly`. PayPal publishes its binding, and publishes the replay
behaviour in its own example — "Demonstrates an idempotent refund request where
the same `PayPal-Request-Id` is used, resulting in a `200 OK` response with the
existing refund details" — and publishes no retention in Payments v2. The
45-day sentence is in a page that itself says "See the API reference to verify
the API supports this header", and PayPal's idempotency page says "for
information about how long the server stores the ID, see the reference for your
API". A number the authoritative page declines to give is not a retention, and
spec 026 §2 is explicit that a documented key with no stated retention is a
near-miss rather than a class. Spec 026 §3 makes the same call for this exact
operation from the other direction: a refund whose idempotency is not documented
is not executable, and it is not a write to casually trade away with an
at-most-once opt-in either. The refusal is a conformance case, because it is the
one a payments deployment meets first.

**A deployment-wide send horizon is bounded by the shortest retention the
instance's operations carry.** ADR 070 made the horizon a startup check, and for
Xero one connector meant one window. PayPal's single instance holds a six-hour
operation and a seventy-two-hour one, and a horizon derived from the longer
would be four hundred times past the shorter — a window inside which PayPal is
not deduplicating a repeated order, it is taking a second payment. So
`paypal::SEND_HORIZON` is the minimum of the declared retentions less the clock
safety margin, a unit test recomputes it from the compiled operations rather
than from the constant, and a deployment that configures one millisecond more is
refused at startup by its metadata path.

**A window a provider offers to arrange privately is not evidence.** Orders v2
says callers "can request the times to up to 72 hours by speaking to their
Account Manager". A deployment that made that arrangement may narrow its own
horizon, and may not widen the class: the connector declares what PayPal
publishes as the default, because the conversation is not something this
repository can read.

## Alternatives

| Option | Why Not |
|--------|---------|
| Declare one retention for the whole connector — the shortest — and use it everywhere | It would publish a *false* citation for the subscription create: the class would claim a six-hour window quoting a sentence about Orders v2. Conservative in effect and wrong in evidence, and the evidence is what a reviewer checks |
| Take the general guide's 45 days for the refund | It builds the programme's most expensive class on a number the provider's own reference declines to state, in a page that tells you to go and read that reference. If it is wrong, the failure is a second refund inside a durable retry |
| Make the refund `AtMostOnce` instead | ADR 063 admits that class on evidence of an **absence**, and there is no absence here: PayPal publishes a key for this operation and an example of it working. Reaching for the weaker class to route around a missing number is exactly the promotion-by-proximity that ADR 042 exists to prevent — and spec 026 §3 says an operator should not casually accept "this customer might be refunded twice, or not at all" |
| Take the 72-hour upgrade Orders v2 offers, and document that a deployment must arrange it | A class whose correctness depends on a phone call nobody in this repository can verify. The default is what the provider publishes to everyone |
| Give each operation its own send horizon rather than one per instance | The horizon is a property of the deployment's retry policy, which is per activity and not per API. One number per instance, bounded by the tightest window that instance can reach, is the bound that is always safe — and a per-operation horizon would have to be re-derived by every caller that schedules a retry |
| Skip `invoice.create`'s at-most-once class and leave it inventory-only too | Invoicing v2 is the case ADR 063 was written for: PayPal's own machine-readable description enumerates every parameter of the endpoint and the term does not occur in it. That is a recorded absence, and the consequence — a second draft invoice — is recorded with it |

## Consequences

Three PayPal writes are executable from a Process with a real six- or
seventy-two-hour window behind them, including the order capture, which is the
operation that takes the money. The refund is declared, typed, tested, and
unreachable, and the module says in one paragraph exactly which sentence PayPal
would have to publish to change that — which is the shape `INVENTORY.md` already
uses for a near-miss and the shape that makes a later slice cheap.

The cost is that this connector's evidence is now read per API rather than per
provider, and a reviewer has to check three references instead of one. That is
a property of PayPal rather than of this design: a provider that publishes four
answers is a provider whose connector has to carry them. The
`paypal_idempotency_evidence_is_complete` proof pins each operation to its own
recorded retention, so a later edit that flattened them would fail rather than
quietly widen a window.

The general rule this establishes for every batch after it: when a provider's
guide and its API reference disagree about a window, the reference that owns the
operation wins, and silence there is a refusal rather than a licence to use the
number found elsewhere.
