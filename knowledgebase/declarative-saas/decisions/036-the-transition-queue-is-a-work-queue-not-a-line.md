---
type: decision
status: accepted
date: 2026-08-02
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# The transition queue is a work queue, not a line

## Context

One consumer per source applied one transition at a time, always the oldest due
event. Two consequences followed from that single sentence.

A transition that could not be applied stayed the oldest. The consumer logged
the failure, slept, and claimed the same event again — four times a second,
indefinitely — while every other instance in the deployment waited behind it.
Three Petshop scenarios reached that state during development, each from a
different cause, and each required a row to be removed by hand. Nothing on any
API surface reported it: every request still answered `200`.

A transition that was merely *slow* did the same thing without failing at all.
An instance waiting on a lock held the consumer for as long as the wait, and
that wait was every other instance's latency.

The deterministic half was fixed first: a failure that will refuse again ends
its own instance rather than being retried (ADR 034). That removed the failures
we can name. It did nothing for a failure that is genuinely transient — a
deadlock, a serialization failure, a starved pool — which is *supposed* to be
retried, and which held the queue while it was.

## Decision

Transitions are applied by several workers per source, and a failing one steps
aside instead of holding its place.

**Workers.** A supervised, fixed number of long-lived workers per source
(`DONAT_PROCESS_TRANSITION_CONCURRENCY`, default 8) each claim and apply one
transition at a time. There is no dispatcher and no queue in the engine: the
`process_events` table is the hand-off, and `FOR UPDATE … SKIP LOCKED` is what
makes two workers pick two instances rather than race for one. Within a
deployment an in-memory registry keeps a second worker from preparing an
instance the first is already inside — the lock would refuse it anyway; this
just stops the work being done twice. One instance's transitions stay
serialized because the worker holding it holds its row.

**Backoff.** A transition that fails for a transient reason increments the
event's `attempts` and moves its `available_at` into the future: exponential
from 50ms, capped at 30s, with full jitter derived from the event id and the
attempt. The increment is relative and the new count comes back from the row,
so two deployments failing the same event cannot lose one between them, and the
give-up point is decided on what is durable rather than on what was read. The
write goes on the connection the transition already holds — a starved pool is
one of the conditions being deferred, and asking the pool for another
connection to step aside with would be asking for the cause.

The event stops being due, the worker goes back for other instances, and the
retry is spaced by the schedule rather than by the poll interval. The jitter is
derived rather than sampled so a fleet spreads out and the same failure
reproduces the same schedule.

**Supervision.** A spawned task that panics is simply gone, and several of
them lose themselves one at a time — a deployment that gets slower and
eventually stops, which is the failure this queue exists to prevent arriving by
another route. A worker that dies is started again after a short delay, and the
log names it.

**A ceiling.** After twelve transient failures the instance is failed with
`transition_retry_exhausted` and one log line naming the cause. A durable retry
that never gives up is indistinguishable from a deployment that has stopped.

**Evidence.** `attempts` and `available_at` on `process_events` are the durable
answer to "is this deployment stuck, and on what". Before this, that question
had no answer short of reading the log.

The database error now travels with the failure rather than being flattened
into a message. Without it the classifier could not tell a lock timeout from a
constraint violation, and a condition that clears on its own ended the instance.

## Alternatives

| Option | Why Not |
|--------|---------|
| One worker, but skip the failing event | Skipping without recording an attempt is a busy loop over the same failure, and leaves no evidence that anything is wrong. |
| A dispatcher task feeding workers over a channel | A second queue in front of the durable one, with its own loss and restart semantics. The table already orders and hands off work. |
| A round of N workers per poll tick, awaited together | The round lasts as long as its slowest worker, so a three-second lock wait stops the other workers from picking up anything new. Long-lived workers have no such barrier. |
| Per-instance workers | Unbounded: a deployment with ten thousand instances would open ten thousand connections. |
| Declare the retry policy in metadata | Nothing in a flow describes how long a deadlock should keep it out of the queue. It is a property of the engine and its database, not of the business. |

## Consequences

A slow or failing instance costs its own latency and nobody else's, which is
what the queue was always supposed to give. The cost is bounded concurrency
against the source — eight in-flight transitions per source by default, tunable
— and a retry schedule that is the engine's rather than the operator's.

The attempt ceiling makes give-up explicit: an instance that cannot make a
transition twelve times over roughly a minute of backoff ends, and says why.
