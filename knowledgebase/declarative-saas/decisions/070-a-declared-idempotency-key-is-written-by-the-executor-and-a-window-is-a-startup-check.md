---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A declared idempotency key is written by the executor, and its window is a startup check

## Context

Spec 026 (Batch J, payments and billing) is the first batch whose *ordinary*
connectors reach `ProviderIdempotent::ExplicitKey`. Before it, exactly two
operations in the programme carried a key, and both wrote it themselves:
`stripe.checkout.create_session` is a hand-written processor module that inserts
`Idempotency-Key` into a `HeaderMap` of its own, and `aws_sqs.message.send`
binds `MessageDeduplicationId` inside the body its own `plan` assembles.

Every other connector is *declaration-driven*: `DeclaredProvider::plan` renders
the operation, the executor applies the credential plan, and the whole request
is the declaration's. That path took `idempotency_key: &str` from the durable
activity and named it `_idempotency_key` — it discarded it. The SDK is stricter
in the other direction than that suggests: `OperationBuilder::build` refuses a
declaration that names the header its own class binds ("an operation must not
declare the header its idempotency binding owns"), so a module could not have
written the key itself either. A declared connector whose class was
`ExplicitKey` would therefore have sent every request **with no key at all**,
been published to process compilation as idempotent, and been retried by a
durable activity on that promise.

Nothing had that shape until this batch, and then three providers did at once:
Xero (`Idempotency-Key`, six minutes, per app), PayPal (`PayPal-Request-Id`, six
hours), and Chargebee (`chargebee-idempotency-key`, thirty minutes). Xero landed
here; the other two are recorded in `providers/INVENTORY.md` with the rest of
their evidence.

The second half of the problem is the window. `aws_sqs` already established that
a documented retention is finite and that "a durable activity that keeps
retrying past it stops being deduplicated while still holding an idempotency
key" ([[046-an-effect-class-can-depend-on-deploy-time-configuration]]), and it
solved that with a **send horizon** checked at startup. Xero's window is six
minutes — twenty-five times smaller than AWS's five — and Xero publishes exactly
what happens past it: "Repeating the same key after expiry won't produce this
error and will instead be processed as a new key, this should be avoided". For
`payment.create` that second processing is a second payment.

## Decision

**The key is written where the class is declared: in the SDK, by the executor.**
`Operation::apply_idempotency_key(&mut RequestPlan, &str)` writes the durable
activity's stable key into the binding the operation's class was admitted on. It
is a **no-op** for every class that binds none, so it can be called
unconditionally; `DeclaredProvider::plan` and the Xero runtime both call it after
rendering and before the credential is applied. A key that is not a valid header
value — one carrying `\r\n`, or one past the SDK's header ceiling — is an
`invariant` failure rather than an escaped or truncated header.

A `BodyPointer` binding is **refused** by this method rather than silently
skipped. A body binding is filled where the body is assembled (`aws_sqs`), and a
caller that reached this method for one is describing an operation it cannot
render; answering `Ok(())` there would be the exact defect this ADR closes, one
layer down.

**The send horizon is a per-deployment setting checked at startup, not at send
time.** `xero.send_horizon_ms` defaults to the documented retention less the
clock safety margin and may be narrowed but never widened: equality is admitted
and one millisecond more is refused, with the metadata path in the message. The
margin is Donat policy rather than provider evidence, and the effect gate
already refuses a margin that is not strictly smaller than the retention, so the
number the class publishes and the number the deployment is held to cannot drift
apart — a unit test asserts `retention - margin == SEND_HORIZON` on the compiled
operation itself.

**The refusal is a conformance case, because it is the one an operator meets.**
`xero_startup.yaml` declares a horizon one millisecond past the window and the
engine refuses to serve, naming `config.settings.send_horizon_ms`. That is the
same shape as an unreachable operation being refused by its metadata path, and
it is deliberately the case a payments deployment gets wrong first.

## What this supersedes

Nothing is superseded. This is the missing half of
[[049-a-connector-publishes-the-declaration-it-was-admitted-on]]: that ADR made
the catalog publish an operation's binding, scope, retention, and margin so a
Process could see them, and this one makes the executor *spend* the binding it
publishes. [[034-a-declaration-the-runtime-ignores-is-a-defect]] is the rule
being applied, on the one class where ignoring the declaration means sending a
payment twice.

## Alternatives

| Option | Why Not |
|--------|---------|
| Let each module write its own key, as `stripe` and `aws_sqs` do | The SDK refuses a declaration that names the header its class binds, so a declaration-driven module *cannot*. Widening that refusal to let modules write it would put the one header that makes a payment repeat-safe back into per-module code, where forgetting it is silent |
| Make `apply_idempotency_key` an error for a class with no binding, so callers must branch | Every caller would then carry the same `if effect_class == ExplicitKey` test, which is a second copy of the gate `Effect::class` already answers. A no-op keeps one question in one place |
| Bind the key inside `plan_request`, so rendering always includes it | `plan_request` has no activity key to bind — it is the pure declaration render, used by every test and by the pagination walk. Threading a durable identity through it would make an SDK primitive depend on the durable runtime |
| Silently skip a `BodyPointer` binding instead of refusing | It would send a body-keyed operation with no deduplication id and no failure to show for it, which is the defect this ADR exists to close |
| Check the send horizon when the activity sends, rather than at startup | The check would fire per attempt, on a deployment already in production, at the moment the key stops working. A window is a property of the deployment's retry policy, which is known before a listener opens |
| Take the provider's *longest* published retention where two exist (PayPal: 6 hours in the API reference, 45 days in the general guide) | The class promises that a resend inside the horizon is deduplicated. The shortest published number is the only one both statements support, and PayPal's own instruction is "for information about how long the server stores the ID, see the reference for your API" |

## Consequences

A declaration-driven connector can now reach `ProviderIdempotent::ExplicitKey`
and be sent correctly, which is what makes three of this batch's providers worth
writing at all. Xero's ten mutating operations — including `payment.create`, the
operation that moves money — are executable from a Process with no opt-in and no
carve-out, because the provider really does deduplicate them.

The costs are worth naming. The engine now writes a header no declaration
mentions, which is invisible in the module and visible only in the class and in
this ADR; the tests answer that by asserting, per operation, that the rendered
request carries no key and the applied one does. And a deployment inherits a
send horizon it did not choose — six minutes less a minute, for Xero — which is
short enough that a long activity retry policy will meet it. That is the honest
price of a six-minute window, and meeting it at startup is better than meeting it
as a duplicated payment.
