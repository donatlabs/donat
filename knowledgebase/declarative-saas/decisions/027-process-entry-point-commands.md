---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# A Process is reachable only through one guarded entry-point Command

## Context

A Process can only be created by a Command's committed `start_process` outbox
(see [[021-pinned-source-local-process-start-consumption]]). Petshop declared
eleven Processes but only three Commands with that effect, so eight modules —
including checkout, fulfilment, returns, subscriptions, and payouts — were
metadata nobody could execute. Their compiled contracts, states, and connector
pins were all verified, yet no caller could reach them.

The obvious shape, giving each Process a Command that also performs its first
domain write, does not work: every Petshop Process already opens with a
Command state that does exactly that write under its own compiled role. A
starter that repeated it would either duplicate the row or race its own
Process.

## Decision

Each Process has exactly one entry-point Command whose whole job is to admit
the request. It writes no domain row: it proves the caller may start this
Process, publishes the typed input, and declares the `start_process` effect.
The Process's own first Command state remains the sole writer of the first
domain row.

Admission is a guard, not a formality. Where the request names an existing
row, the entry point reads it with `select_one ... require_found: true` and,
for customer-facing modules, filters it by the caller's session variable, so a
wrong or foreign identifier is rejected at the API boundary instead of becoming
an audited Process failure. Where a Process input describes recorded state
rather than caller intent — the amount, currency, status, and provider
reference a reconciliation compares against — the entry point reads those
values from the source instead of accepting them, and asserts any value it must
still take from the caller against the recorded one with a Rule.

`process_key` stays optional. A Process whose identity is the request itself
uses only its idempotency key; one anchored to a domain row keeps naming that
row.

Reachability is a compiled contract, not a convention: the Petshop candidate
test fails if any compiled Process has no Command that starts it.

## Alternatives

| Option | Why Not |
| --- | --- |
| Let the entry point perform the first domain write | Duplicates the write the Process's first Command state already owns, and races it. |
| Add a per-Process request table to anchor `process_key` | Adds a table per module whose only content is "someone asked", when the idempotency key already carries that identity. |
| Publish a generic `start_<process>` GraphQL field | Reintroduces a process-management API, which Spec 005 excludes; the domain Command is the public surface. |
| Accept every Process input from the caller | Lets an operator start a reconciliation against fabricated expectations, or a customer against another customer's order. |
| Leave the eight modules unreachable | Their behavior can never be exercised, so their contracts are unverifiable claims. |

## Consequences

Every Petshop module is now executable end to end from the public API, and the
count of entry points equals the count of Processes by construction.

An entry-point Command commits a transaction that writes no business row. That
is deliberate: it still writes the durable outbox and the command journal, so
replay and idempotency behave exactly as they do for any other Command.

A caller can still start a Process that later fails on a state the entry point
did not check — a cart that empties between admission and quoting, for example.
The guard narrows the common wrong-identifier case; it does not turn the entry
point into a second copy of the Process's own preconditions.
