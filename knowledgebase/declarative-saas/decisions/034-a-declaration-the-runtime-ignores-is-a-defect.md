---
type: decision
status: accepted
date: 2026-08-02
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# What a Process declares, the runtime obeys — and one instance's refusal is its own

## Context

Black-box tests of the Petshop deployment found three failures that no unit or
conformance test could see, because each needed a whole store running.

Two were declarations the runtime never read. A connector operation states the
header its provider deduplicates on (`effect.provider_idempotent`'s fixed
binding) and which statuses are retryable (`error_map`); the transport bound
the header only from the legacy `http.idempotency` field and classified
statuses only from the legacy `error_classification` list. Petshop declares
neither legacy field, so every payment authorization went out with no
idempotency key at all, and a 500 the operation mapped to `http_5xx` was
classified `Permanent` — one HTTP call where the flow declared three attempts.
A Process wait likewise declares `persist_before_match`; nothing read it, so a
Command signal committed before its wait became receptive was recorded
`unmatched` and dropped. A warehouse that scanned a receipt the moment approval
landed left the shopper's refund stranded forever, with every Command
answered `200` and the return reading `inspected`.

The third was worse. A Command that violated a unique index inside a Process
transition returned an error the consumer could only retry. It retried about
four times a second, forever, and because the transition consumer is shared,
**every other Process in the deployment stopped advancing** — checkouts sat in
`prepare_quote` until somebody removed the conflicting row by hand. ADR 022
already said a constraint failure is not recoverable; the runtime treated it as
if it were transient.

## Decision

A declaration the runtime ignores is a defect, not a documentation gap. The
transport binds its idempotency header from the effect when the legacy field is
absent, and classifies a non-success status by the operation's own `error_map`
before falling back to the built-in handling. `persist_before_match` means what
it says: entering the wait returns the correlated `unmatched` signals to the
queue, and such a wait may take a signal that predates it — but only as a
fallback, never as a competitor to a wait that was already receptive when the
signal was committed.

A refusal that will refuse again fails its own instance and nothing else.
SQLSTATE class 23 (integrity constraint) and class 42 (syntax and access rule)
end the instance with a safe code; everything else — serialization failures,
deadlocks, a dropped connection — stays retryable, which is what a durable
runtime is for. The relation and constraint that refused go to the log, per
ADR 031; the journal keeps only the code, because a constraint name is an
operator concern and not a Process fact.

## Alternatives

| Option | Why Not |
| --- | --- |
| Require deployments to repeat themselves in the legacy fields | Two ways to say one thing, with the newer one silently inert. Metadata that validates and deploys must also take effect. |
| Retry a constraint violation with a backoff instead of failing | It cannot succeed: the same write refuses the same row. A slower loop starves the queue more politely. |
| Give the poisoned transition its own consumer, or skip past it | Treats the symptom. The instance is finished either way, and skipping leaves a `running` instance nobody will ever resolve. |
| Buffer every early signal, not just declared ones | Changes correlation semantics for waits that did not ask for it, and ADR 023 chose those semantics deliberately. The flag exists precisely to opt in. |
| Let a persisted wait compete with a receptive one | Two waits would claim one signal and the delivery would be ambiguous, which the receptive wait already resolved correctly. |

## Consequences

Petshop's black-box suite goes from four documented defects to none, and runs
in a third of the time — the flows no longer wait out stalled Processes. A
domain conflict now ends one instance and leaves the store trading.

The new failure mode is visible rather than silent: an instance that hits a
constraint is `failed` with `command_constraint_violation`, one transition-log
entry, and a log line naming the relation. Operators who relied on the old
behaviour to "fix the data and let it retry" lose that: the instance is
finished and a new one has to be started. That is the honest reading of the
state — the old loop was not waiting for a fix, it was refusing to admit one
was needed.

`persist_before_match` costs one indexed `UPDATE` per wait entry that declares
it, and only for waits that declare it.

Three defects of the same shape in one deployment is the finding behind this
decision: a field that parses, validates, deploys and does nothing is
indistinguishable from a working one until something in production quietly
fails. The compiled catalog carries these fields; the tests that would have
caught them are black-box, because each one only misbehaves with the whole
store running (see `tests-system/`).
