# Spec 009 — Declarative inbound webhooks

Status: proposed. This specification generalises inbound webhook delivery from
a property of the Stripe module into a capability any connector module can
carry, declared in metadata the way outbound operations already are. It adds no
runtime plugin loading and no new bypass.

It supersedes the Stripe-shaped version of the same request: that proposal is
correct about the two walls and about the fix for the second one, and it
generalises in the wrong place.

## 1. What is true today, verified

- **Only Stripe can receive.** `RegistryInstance::Http` has no `webhook` field
  at all (`crates/server/src/connectors/mod.rs:168`). Inbound is not a
  connector capability; it is a Stripe feature. The whole path lives in two
  files — `stripe.rs` (12 references) and `mod.rs` (8).
- **One trigger per instance.** `RegistryInstance::Stripe.webhook` is a single
  `CompiledWebhookTrigger` (`mod.rs:175`), not a set.
- **A process cannot wait twice.** `crates/processes/src/lib.rs:1441` —
  "process transitions must form an acyclic graph".
- **An unmatched event is audited and dropped.** `InboundOutcome::Unmatched`
  (`crates/server/src/processes/inbound.rs:66`) writes a delivery row and
  nothing else. That is the seam the fix uses.
- **The dedupe ledger exists and is unique per provider event.**
  `donat.process_inbound_events`
  (`migrations/V20260731002214__donat_processes.sql:386`).
- **`process_start_requests` assumes a command produced it.**
  `command_invocation_id uuid not null` (same file, line 157).

All six hold as of `6bad681`.

## 2. The correction

The original section A moves `webhook` from one value to a map **inside the
Stripe variant**. That fixes recurring billing and leaves inbound webhooks a
thing only Stripe can do — so the second provider that needs one is another
hand-written module, and the generic HTTP connector, which is what most
integrations actually use, still cannot be called back at all.

The seam is one level down. Note what already exists on the **outbound** side:

```yaml
response:
  provider_event_id: { json_pointer: /provider_event_id, type: string!, max_bytes: 256 }
  status:            { json_pointer: /status, type: PaymentOutcome! }
```

A typed, bounded, declarative contract mapping a provider's JSON onto engine
types. Inbound has no equivalent, which is *why* Stripe's normalizers are
hand-written Rust — not because inbound is inherently special.

**So: give inbound the same declarative treatment, and Stripe's built-in
triggers become defaults rather than the mechanism.**

## 3. A — inbound as a connector capability

### Metadata

An instance declares how its callbacks are authenticated and which events it
subscribes to. Undeclared means unreachable, exactly as an undeclared operation
is.

```yaml
name: github
module: http
config:
  base_url: { value_from_env: GITHUB_API }
inbound:
  # How to prove the bytes came from the provider. Verification happens over
  # the exact raw body, before any JSON is parsed.
  signature:
    header: X-Hub-Signature-256
    scheme: hmac_sha256
    encoding: hex
    prefix: "sha256="
    signed_payload: body              # or "{timestamp}.{body}"
    timestamp_header: X-Hub-Timestamp # only with the templated form
    tolerance_seconds: 300
    secret: { value_from_env: GITHUB_WEBHOOK_SECRET }
  triggers:
    - trigger: issue_opened
      # Which verified event this is. Evaluated after verification, never before.
      select:
        - { json_pointer: /action, equals: opened }
        - { json_pointer: /issue/state, equals: open }
      output:
        provider_event_id: { json_pointer: /id, type: string!, max_bytes: 256 }
        issue_number:      { json_pointer: /issue/number, type: int! }
        title:             { json_pointer: /issue/title, type: string!, max_bytes: 512 }
```

`output` is the same vocabulary as an operation's `response`, including
`max_bytes`, typed enums and the epoch-to-`timestamptz` conversion. Nothing new
to learn and nothing new to review.

### Registry

- `CompiledWebhookTrigger` moves out of the Stripe variant into a shared
  `CompiledInboundTriggers` carried by every `RegistryInstance`.
- Each trigger keeps its **own** configuration fingerprint, so a revision
  pinned against one is unaffected when another is added. This is the property
  that makes adding a trigger a non-event for running instances, and it must
  survive the move.
- `raw_body_max_bytes` is the maximum across the set.

### Ingress

The order in `receive` inverts, and the ordering is the security property:

```rust
verify(headers, raw_body) -> Result<(TriggerId, VerifiedInboundEvent), WebhookRejection>
```

Verification first, over the exact raw bytes. Only then is the event's own
content allowed to select a trigger, and only then is a spec resolved. The
existing 404-before-body-read behaviour for an undeclared instance is
deliberate and survives unchanged.

### Compatibility

Stripe keeps its built-in signature scheme and its built-in
`checkout.session.completed` trigger. An instance with no `inbound:` block
behaves exactly as today. Existing deployments are untouched.

## 4. B — Stripe's recurring-billing events

These become the first *consumers* of section 3 rather than its implementation.
`invoice.paid`, `invoice.payment_failed` and `customer.subscription.deleted`
ship as built-in trigger definitions in the Stripe module, expressible in the
same declarative form a user would write for any other provider.

The two judgement calls from the original stand, and both are general rather
than Stripe-specific:

- **A nullable provider field is rejected, not emitted as nullable.** The rule
  profile refuses to read a nullable value, so a nullable output pushes the
  problem into every expression downstream. An invoice with no subscription is
  `UnsupportedEvent`.
- **Provider representations are converted at the boundary.** Unix epochs
  become `timestamptz` in the mapping, for the same reason `payment_status` is
  normalised rather than passed through.

One field is added to the existing trigger: `subscription_id: string!` on
`checkout.session.completed`, rejected when the session is in subscription mode
and the field is absent. Every later event correlates on exactly that.

`billing_reason` stays in the output contract rather than being filtered inside
the normalizer, so the application decides which events record money. Stripe's
first invoice fires alongside the checkout event; an application that records
from both records the same money twice under two provider ids. That is the
application's decision to get right, and the engine should not make it
invisible.

## 5. C — a verified event may begin work

Today a verified event can only advance an instance already parked on a
matching `wait.webhook`. A renewal does not fit that shape, and every
workaround is closed: cyclic transitions are refused at compile time, unrolled
waits bound a subscription's life to however many were written, a cron trigger
delivers to a URL and therefore needs a receiver, and there is no
read-through-to-a-provider primitive.

**A trigger may name a command.** The engine invokes it inside the same
source-local transaction that writes the delivery audit and the dedupe row.

```yaml
    - trigger: invoice.paid
      on_event:
        command: record_renewal_payment
        run_as: billing
        arguments:
          provider_event_id: { event: provider_event_id }
          amount_minor:      { event: amount_paid_minor }
          period_end:        { event: period_end }
        idempotency_key: { event: provider_event_id }
```

- Reuses `donat.command_invocations`, so replay protection is the mechanism
  commands already have rather than a second one.
- No schema change.
- `run_as` is an ordinary declared role, resolving through its own table
  permissions. A command the role may not call is refused, not escalated.

Starting a *process* from an event is deliberately out of scope. It is more
general and only earns its keep when the work itself waits — a dunning schedule
with deadlines. It also needs a migration, because `process_start_requests`
requires `command_invocation_id`, so it would have to become nullable with an
`inbound_delivery_id` beside it, a per-origin unique constraint and a check
that exactly one origin is set. Worth doing when a case demands it; not now.

## 6. D — invariants that must not move

1. **Signature before JSON, always.** A malformed or hostile unverified payload
   never becomes anything the rest of the system can see. Section 3 reorders
   what happens *after* verification and must not touch what happens before it.
2. **The dedupe ledger stays the single once-only guarantee.** A command
   invoked from an event commits in the same transaction as its audit row, so
   "audited" and "invoked" cannot come apart.
3. **A verified but unsubscribed event is acknowledged, not refused.** Write a
   delivery row, return 204. A 400 makes the provider retry and eventually
   disable the endpoint, taking the events we do want down with it.
4. **An undeclared instance stays indistinguishable from an absent route.**
   404 before the body is read.
5. **No new bypass.** Everything an event causes resolves through a declared
   role's ordinary permissions. There is no admin role, and an inbound event
   must not become one.
6. **Failure is fail-closed.** Anything after successful verification that
   cannot commit returns 503 with nothing written, so the provider retries into
   the dedupe ledger rather than the event being lost.

## 7. E — tests

**Unit, per signature scheme rather than per provider**: a correctly signed
body maps to its declared output including conversions; a body failing a
`select` guard is `UnsupportedEvent`; a tampered body fails verification; a
timestamp outside the tolerance fails.

**Conformance, extending `crates/conformance/tests/process_inbound.rs`:**

- A signed event invokes the declared command exactly once, as the declared
  role.
- The same event again: one delivery row added, no second invocation.
- A verified event of an unsubscribed type returns 204 and invokes nothing.
- A forged signature writes one redacted delivery row, touches neither the
  ledger nor any state, and never needs a trusted provider id.
- A command the declared role may not call is refused, and the refusal is
  audited rather than escalated.
- Two triggers on one instance stay independent: adding the second does not
  disturb a revision pinned against the first.
- **A `module: http` instance receives a webhook end to end** — the case that
  proves this is no longer a Stripe feature.

## 8. Sequence and size

| Step | Needs | Schema | Rough size |
|---|---|---|---|
| A — inbound as a connector capability, declarative | — | none | ~2 days |
| B — Stripe's three events as built-in triggers | A | none | ~1 day |
| C — a trigger may invoke a command | A | none | ~2 days |
| Tests across all three | A B C | none | ~1.5 days |

Roughly a day and a half more than the Stripe-shaped version, and it is the
difference between one provider and every provider. The judgement is all in C;
A and B are mechanical once the declarative form is settled.

Sizes are read off the code, not measured by having written it.

## 9. Deliberately out of scope

Proration and mid-period plan changes. Refunds and chargebacks. A dunning
schedule with its own deadlines — that is where starting a process from an
event earns its keep, and it is a separate conversation. Tax and invoicing
documents. Creating Checkout Sessions from a process, which stays unreachable
and stays unnecessary: payment links carry the checkout id as
`client_reference_id`, which is what correlation runs on.

Form-encoded request bodies remain the one thing the generic HTTP connector
cannot express on the outbound side. Cancelling a subscription does not need
them — `POST /v1/subscriptions/{id}?cancel_at_period_end=true` passes its
parameters in the query string — but it is worth recording as the next gap.
