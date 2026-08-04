---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Durable waits linearize through one version-qualified timer event

## Context

A Process wait must survive binary restarts without an in-memory timer, reject
signals committed before the wait became receptive, and choose exactly one
winner when a signal and timeout become due together. A separate mutable wait
row would add another state machine whose version and closure would have to
commit with the Process event journal.

Command signals are durable outbox rows. Worker polling time is not their
business arrival time: a delayed worker must not let a later timeout overtake a
signal that was committed while the wait was already receptive. Closed wait
correlations must remain queryable so a late signal is auditable as
`unexpected_state` rather than becoming an apparent new match.

## Decision

Entering either wait variant consumes its `start` or `continue` token,
increments the instance version without changing state, and inserts one
`timer` event in the same source-local transaction. The event payload pins the
wait state and new version. A signal wait additionally stores its validated
correlation and signal name; a decision-table timer stores the immutable
decision output used both for its delay and eventual state output. Its
`available_at` uses the owning Postgres clock or the declared absolute
deadline. A timezone-free `timestamp` deadline is interpreted explicitly as
UTC; it never uses the server or database session timezone.

A command-signal consumer validates the request against its pinned revision,
admits retained revisions only when their signal-contract fingerprint is
identical, and accepts only a marker that existed no later than the outbox
row's `created_at` and whose deadline is not earlier than that time. A
non-receptive instance never contributes ambiguity when exactly one other
instance was receptive at arrival time. The resulting signal event uses that
outbox time as `available_at`, not worker wake-up time.

Before a due timeout advances, its locked transaction checks for a committed
pending request with the same correlation, a compatible signal-contract
fingerprint, and an outbox time inside the marker/deadline interval. If one
exists, the timeout yields so the signal consumer can materialize the earlier
event. Signal events sort before timers at an equal `available_at`. Signal and
timer transitions lock the same instance version. The winner consumes its
event, advances once, and marks every other signal/timer event for that wait
version failed in the same transaction.

The closed timer marker remains durable history. Late-signal classification
uses a partial `jsonb_path_ops` index over signal-bearing timer payloads; it
does not scan or infer from redacted provider text.

## Alternatives

| Option | Why Not |
| --- | --- |
| Tokio sleep or an in-memory timer wheel | Restart would lose business state and multiple binaries would race independently. |
| A separate `process_waits` table | It duplicates the event/version state machine and creates another atomic closure protocol. |
| Order signals by worker polling time | Backlog or deployment delay could let a later timeout overtake an already committed signal. |
| Leave the losing event pending | It creates permanent due work and allows a stale event to target a later state. |
| Match only the active revision | Rolling deployment would strand live instances whose retained signal ABI is unchanged. |

## Consequences

Wait entry, timeout, and signal delivery reuse the existing Process journal and
one optimistic instance version. Timers are restart-safe, early signals are
never buffered, a delayed worker does not extend the business deadline,
polling latency and multiple binaries do not reorder committed business
events, and a race has one durable winner.

Timer payload shape and the wait-history index are now part of the runtime ABI.
Closed competitors use event status `failed` to mean permanently ineligible,
while the winning transition log records how many competing events it closed.
