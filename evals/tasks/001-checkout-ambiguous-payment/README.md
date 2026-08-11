# 001 — checkout with an ambiguous payment

## Why this task exists

It is the failure the engine was built around. A side-effecting provider call
can time out *after* the card was charged, and no amount of retrying tells the
store which happened. Everything else in the task — retries, idempotency, a
declined card releasing stock — is ordinary work that a competent answer gets
right on the way past. Rule 3 in the prompt is the one that separates a design
that understood the problem from one that shipped a state machine.

The reference solution is the flow the checked-in Petshop actually ships, so
this task's ground truth is a store that has been exercised by the black-box
suite for months, not something written for a benchmark.

## The example map

Written before the prompt was finalised. Each rule became at least one
concrete example; the examples that had no obvious answer were the ambiguities,
and each one was resolved in the prompt rather than left for the candidate to
guess.

| Rule | Example | Expected |
|---|---|---|
| Never take money twice | shopper clicks pay twice with the same request id | one order, one authorization |
| Never take money twice | the store retries a call the provider already saw | provider sees a replay, not a second charge |
| Retry what is worth retrying | provider answers 500, then 200 | order authorized, sale not lost |
| Retry what is worth retrying | provider declines the card | order cancelled, no money held |
| Never claim what you cannot prove | call times out, provider *did* charge | order authorized, amount matches the provider's |
| Never claim what you cannot prove | call times out, provider proves it never charged | no money held |
| When the provider cannot say | call times out, provider cannot prove either way | checkout stops for a person; stock stays held |
| A declined card releases the stock | provider declines | the reserved units are back on the shelf |

Three ambiguities surfaced while writing this map, all now answered in the
prompt:

1. *"Retry transient failures"* did not say what happens **after** the last
   attempt. Rules 3 and 4 now do.
2. *"Never double charge"* is about the provider, but a reader could take it as
   "never call twice". Rule 1 says charges, and the retry rule says calls.
3. Nothing said what a stalled checkout should look like from outside. Rule 4
   says a human picks it up with the goods still held, which is what makes the
   difference observable at all.

## The anti-oracles

Each is the reference solution with one thing changed — see the diff, it is
small on purpose. All three deploy and pass `donat validate --strict`; that is
the point, since anything the gate catches proves nothing about the scenarios.

| Anti-oracle | The defect | Dies in |
|---|---|---|
| `ambiguity-to-failure` | the ambiguous route gives up instead of reconciling | provider charges, then goes silent |
| `success-default` | an outcome the store has no case for is recorded as authorized | provider challenges the card |
| `unproven-absence-treated-as-decline` | "not found" is treated as "did not happen", ignoring whether the provider could prove it | provider cannot prove either way |

Two candidate anti-oracles were **rejected** while writing these, and both are
worth remembering:

- *unstable idempotency key* — the compiler already refuses it
  (`crates/processes/src/lib.rs:2110`). It never reaches the scenarios, so it
  measures the gate, not the suite.
- *reconcile by calling `authorize` again* — with the same stable key the
  provider deduplicates, so this is a legitimate alternative design, not a
  defect. An anti-oracle that punishes correct work is worse than no
  anti-oracle at all.
