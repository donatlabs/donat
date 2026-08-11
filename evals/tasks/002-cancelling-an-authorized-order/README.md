# 002 — cancelling an order the store has already charged for

## Why this task exists

Task 001 is about a call whose outcome the store cannot learn. This one is
about a call the store must keep making until it lands. The money is already
with the provider, so every failure mode costs the shopper directly: give up
after one 5xx and the hold outlives the order it was taken for.

It exists as a second task rather than a variation because the failure it
measures is the one the mutant sweep says the black-box suite is worst at
seeing. Across 200 seeded defects, the two retry operators produced 40 mutants
and the suite caught 8 of them — `single-attempt` survived 17 of 20,
`no-retry-on-timeout` 15 of 20. Both anti-oracles here are those operators
applied to this flow, which is the intended pipeline: a survivor is a hole in
the suite and a ready-made anti-oracle for a task
([[findings-mutant-sweep]]).

## The example map

| Rule | Example | Expected |
|---|---|---|
| A stumble is not an answer | provider answers 500, then releases the hold | order `cancelled`, payment `voided` |
| A stumble is not an answer | the same, read from the books | no payment still holding funds |
| Silence is not an answer | the first void call never returns | the store keeps going until it knows |
| Silence is not an answer | the same, read from the books | the hold does not survive the silence |
| Never twice | the shopper clicks cancel twice | one void at the provider |
| "No" is an answer | provider refuses the void | the order does **not** read `cancelled` |
| A case, not a mess | nothing can be proven either way | a person picks it up, money untouched |

Two ambiguities were resolved into the prompt while writing this:

1. A refused void leaves the payment in whatever state the *mutation* already
   wrote, not in `authorized` — the claim happens before the process starts. An
   early draft of the prompt said `authorized`, which is a state the store
   cannot be in on that path. See below.
2. "Never twice" spans two mechanisms — the mutation's claim stops a second
   process, the idempotency key stops a second provider call — and the prompt
   now says which one it means where.

## The anti-oracles

Both deploy and pass `donat validate`, which is what makes them worth running:
anything the gate catches measures the gate.

| Anti-oracle | The defect | Dies in |
|---|---|---|
| `single-attempt` | `max_attempts: 1`, retry classes untouched | provider stumbles once, then voids |
| `no-retry-on-timeout` | `timeout` removed from `retry_on`, everything else retried | the first void call never returns |

Each is caught by two independent scenarios — the shopper's own orders and
support's books — so neither rests on a single assertion.

## What the first measurement found in this task

Worth keeping, because it is the most useful thing that happened to task 002.
The first model given the brief reported two errors in it before solving it:

- the prompt listed the wrong inputs for the process it asks the candidate to
  build (`order_id, payment_id, request_id`, where the command actually passes
  five and keys idempotency on `request_id`);
- it described support reading `authorized` on the refusal path, which the
  claim written by the mutation makes impossible.

Both were real. The second also reached into a scenario, which computed
`money_still_held` and then did not assert on it — a dead value that read like
a claim. The task had already passed `verify-oracles`, a stability run and
human review. The prose is what the candidate is given, and nothing in the
harness reads prose.
