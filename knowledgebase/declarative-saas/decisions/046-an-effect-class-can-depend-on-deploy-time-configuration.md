---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[declarative-saas]]"
---

# An effect class can depend on deploy-time configuration

## Context

Spec 017 (Batch F, AWS) is the first batch where one connector operation has two
different, both documented, repeat semantics depending on how the deployment
configured its target.

`aws_sqs.message.send` is the clear case. On a FIFO queue Amazon documents the
binding, the behaviour, and the retention — "If a message with a particular
`MessageDeduplicationId` is sent successfully, any messages sent with the same
`MessageDeduplicationId` are accepted successfully but aren't delivered during
the 5-minute deduplication interval" — which is exactly the evidence
`ProviderIdempotent::ExplicitKey` is admitted on (ADR 042). On a standard queue
Amazon documents the opposite: "Standard queues ensure at-least-once message
delivery, but due to the highly distributed architecture, more than one copy of
a message might be delivered", and `MessageDeduplicationId` "applies only to
FIFO (first-in-first-out) queues". One operation id, one request shape, two
contracts.

`aws_s3.object.delete` is the same shape with a different provider. On an
unversioned bucket "Amazon S3 will permanently delete the object", so a repeated
`DELETE` of one key leaves the same one absent object. On a versioning-enabled
bucket a keyless delete "creates a delete marker over the current version of the
object", so a second identical send leaves a *second* delete marker — one more
resource than the first send left.

Before this batch the effect class was fixed where the operation was declared,
which is before any deployment exists. Nothing in the SDK could say "this class
holds for this deployment's target and not for that one".

## Decision

The class stays a property of the compiled `Operation`, and a connector module
compiles its operations **per instance** from validated deploy-time
configuration. `aws_sqs` builds `message.send` with the documented FIFO evidence
on a FIFO queue and with `inventory_only` — carrying Amazon's at-least-once
sentence as its recorded reason — on a standard queue. The queue type is
configuration, it is validated at startup against the provider's own test for it
("To determine whether a queue is FIFO, you can check whether `QueueName` ends
with the `.fifo` suffix"), and a deployment that disagrees with its own queue
name is refused before a listener opens.

The static `Connector` declaration keeps the class the operation *can* reach, so
the registry still sees one honest description of the operation, and the
instance's own `admit_operation` is the gate a deployment meets. Nothing about
`EffectClass`, `Effect`, or `ExplicitKeyEvidence` changed: the evidence
constructor still refuses an incomplete set, and a margin that is not strictly
smaller than the documented retention still does not build.

The retention brought a second consequence with it. A provider's deduplication
window is finite — Amazon states that "If a message is sent successfully but the
acknowledgement is lost and the message is resent with the same
`MessageDeduplicationId` after the deduplication interval, Amazon SQS can't
detect duplicate messages" — so a durable activity that keeps retrying past the
window stops being idempotent while still holding an idempotency key. The SQS
module therefore carries a **send horizon**: a deployment's retry window must fit
inside the documented interval less the clock safety margin, checked at startup.
Equality is admitted; one millisecond more is refused.

## Alternatives

| Option | Why Not |
|--------|---------|
| Declare `message.send` `ExplicitKey` once and let a standard-queue deployment enable it | It would publish an idempotency key the provider does not read. The class would mean "someone typed a field name", which is the failure ADR 042 exists to prevent |
| Declare it `InventoryOnly` once, so no AWS send is ever executable | Throws away the batch's whole point: a FIFO send is the first non-Stripe operation that reaches an executable mutating class on documented evidence |
| Split it into `message.send_fifo` and `message.send_standard` | Two operation ids for one provider action, and a deployment could enable the wrong one against the right queue. The queue type is not something an operation id can check |
| Keep one class and refuse the whole standard-queue connector at startup | Standard queues are legitimately readable; refusing `message.receive` and `queue.attributes` because `message.send` is not repeat-safe punishes a deployment for an operation it never enabled |
| Trust the deployment's declared queue type without checking the name | The provider publishes a check for exactly this, and a mislabelled queue would silently turn an at-least-once send into an "executable" one |

## Consequences

An effect class now answers the question "for this deployment", and the answer is
computed once, at startup, from configuration a request cannot reach. A reviewer
reading a connector module sees both branches and the provider sentence behind
each, rather than one class and a footnote.

The cost is that a module's operations are built twice: once as the static
declaration and once per instance. The two builds share one function, so they
cannot drift in request shape, but a module that forgot to thread its
configuration through would render the declaration's placeholder and fail loudly
rather than quietly sending to the wrong target — which is why the placeholder is
an input slot no caller can fill rather than a plausible default.

This does not introduce a third executable class. An operation whose provider
documents repeat-safety that the SDK's two mutating classes cannot express —
`aws_sqs.message.delete`, which Amazon documents as safe to repeat but which the
AWS JSON protocol expresses as a `POST` — stays inventory-only with that fact
recorded on it. It is evidence for the at-most-once opt-in ADR, not a reason to
widen the gate here.
