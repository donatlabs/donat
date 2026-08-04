---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Verified inbound delivery correlates through the durable wait marker

## Context

Petshop checkout, refund, and payout Processes must continue when a provider
confirms work that the engine cannot observe by polling. The provider retries
aggressively, replays the same event, and signs raw bytes, so a delivery path
that acknowledges before commit loses events and one that re-parses a
re-serialized body cannot verify a signature at all.

A provider event is not a domain Command signal. `wait.signal` is written by an
explicit role-qualified Command outbox, while a webhook arrives unauthenticated
at an HTTP route and is authenticated only by the connector module. Reusing one
variant would let provider text manufacture a Command signal.

Correlation must survive the transaction boundary. The receiving instance is
identified by declared correlation fields, not by a URL path, and at large
store volume a delivery cannot lock every waiting instance to find its target.

## Decision

`wait.webhook` is a separate closed state variant naming `connector`,
`trigger`, correlation fields, an optional guard, and a deadline. The connector
verifies raw bytes, then hands the runtime only a bounded
`VerifiedInboundEvent`: provider event ID, event type, contract-validated
output, payload digest, and redacted metadata. Raw bodies, signature material,
and credentials are structurally absent from that type, and its `Debug` prints
the digest as `<sha256>`.

Persistence splits by purpose. `process_inbound_events` is the dedupe ledger,
unique on `(source_name, connector_instance, provider_event_id)`;
`process_inbound_deliveries` is append-only audit for every attempt. One
source-local transaction writes the delivery row, inserts or observes the
dedupe row, and — only for `accepted` — appends the Process event and links
both `instance_id` and `process_event_id` before commit. Every other committed
outcome leaves both links null, so instance inspection uses an indexed
relational predicate and never joins on redacted payloads or provider text.

Correlation is resolved from the durable wait marker rather than recomputed
from mutable state. Entering the wait already persisted the timer event that
carries `connector_instance`, `trigger`, `route`, and the evaluated
`correlation`; delivery locks only markers whose correlation matches, plus the
short window of instances that have not yet entered their wait. Migration V10
indexes both paths: a partial index on
`(source_name, instance_id, status, created_at, id)` for the exact receptive
marker and a `jsonb_path_ops` GIN index for late-delivery marker history. No
index covers a raw provider payload, because none is retained.

Delivery is fail-closed. A marker whose pinned trigger or correlation does not
match its compiled wait aborts the whole transaction, so no acknowledgeable
audit row is produced. Outcomes are closed: `accepted`, `duplicate`,
`unmatched`, `ambiguous`, `guard_false`, `unexpected_state`. A signal that
arrives while the instance is not receptive is never buffered; it records
`unexpected_state` when the correlation target is known and `unmatched` when it
is not. An event committed at or before the deadline wins against a
concurrently firing timeout, so the result does not depend on polling order.

Invalid signatures write exactly one redacted delivery row with
`invalid_signature`, require no trusted provider ID, and touch neither the
dedupe ledger nor Process state. The route returns empty `204` only after the
complete transaction commits, empty `503` for any post-verification failure,
and leaves the connector-owned `404`/`413`/`400` matrix unchanged.

## Alternatives

| Option | Why Not |
| --- | --- |
| Reuse `wait.signal` for provider events | Lets unauthenticated provider text manufacture a role-qualified Command signal. |
| One ingress table for dedupe and audit | Cannot record repeated attempts while keeping one provider identity, or keep an invalid signature auditable without a provider ID. |
| Acknowledge on successful verification | Returns 2xx before durable acceptance, so a crash silently drops a provider event the sender will not resend. |
| Recompute correlation from current instance state | Reads state that may have advanced since the wait was entered, and forces a lock over every waiting instance. |
| Buffer early or non-receptive signals | Reintroduces an implicit queue and lets a stale event advance a later, unrelated wait. |
| Persist raw bodies for replay | Retains provider secrets and PII the Phase-1 schema deliberately excludes; the digest already proves identity. |

## Consequences

Inbound delivery adds no worker, queue, or webhook microservice: an HTTP
request commits inside the same source-local journal the pollers use. Provider
retries stay cheap because a duplicate short-circuits after the dedupe lookup
and creates no event.

The correlation-narrowed lock means a delivery whose correlation matches many
live instances still serializes against all of them; the bounded `ambiguous`
outcome makes that visible rather than picking a winner. Because correlation
lives in the marker, changing a compiled wait's `correlate` list does not
retarget instances already waiting under the previous revision — they keep the
correlation pinned at entry, which is the same revision-pinning rule the rest
of the Process runtime follows.
