---
type: decision
status: accepted
date: 2026-08-02
features:
  - "[[declarative-saas]]"
  - "[[004-commands]]"
---

# An idempotency scope may read a lookup, never a write

## Context

A command narrows its idempotency key with a scope, and the metadata format
lets a scope value name a step result — `scope: [{ step: reconciliation,
column: id }]` — so that one resolution key closes one case rather than one
case per key holder. The schema compiler accepted such a scope and checked
that the step existed and was a single scalar row.

The renderer, however, emitted the invocation gate before any step CTE. A scope
reading `_cmd_step_0` therefore compiled to SQL that referenced a relation the
statement had not defined yet, and Postgres refused the whole command with
`42P01 relation "_cmd_step_0" does not exist`, surfaced to callers as
`data-exception`. Petshop's `resolve_payment_reconciliation` was the only
command in the store with that shape, so the entire "a person closes a
reconciliation the store could not settle" branch was dead — the declaration
was accepted at deploy time and could never execute.

## Decision

The lookup a scope reads runs before the claim is elected. SQLgen hoists the
step prefix up to and including the last step the scope names, emits it gated
on the guards alone, and skips re-emitting it in the step loop — every gate
that prefix declares still applies in declaration order, only the relation
moves. A step may reference only steps declared before it, so hoisting the
prefix carries every value that prefix needs without a dependency walk.

This is sound only while that prefix writes nothing: a claim that had to write
before electing itself would write again on every replay, which is the opposite
of what idempotency means. The schema compiler now rejects a scope whose prefix
contains an insert, update, delete or allocation, naming the writing step. What
remains legal is exactly what is safe — a scope is computed from lookups, and a
hoisted lookup running once more on a replay is not something a caller can
observe.

## Alternatives

| Option | Why Not |
|--------|---------|
| Reject every step scope at deploy time | The format declares the feature and the compiler already typed it; removing it would take away the only way to scope a key by the row being acted on, and would break metadata that loads today. |
| Emit the claim after all steps | The claim is what decides whether the steps run at all; electing it afterwards would run the writes before knowing they were a replay. |
| Walk the value graph and hoist only the steps the scope truly needs | Same result as the prefix in every case a write-free prefix allows, at the cost of a transitive reference walker over every step and value variant. |

## Consequences

A command may key its idempotency on the row it is about, which is what
resolution, reconciliation and "close this case" commands need. The hoisted
lookup runs on replays too — one extra read, no observable effect. Metadata
that scopes behind a write is refused at deploy time with the offending step
named, rather than compiling into a statement Postgres refuses at runtime.
