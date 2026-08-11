---
type: research
status: draft
date: 2026-08-09
---

# The mutant sweep: what would we notice?

Two hundred copies of the checked-in Petshop, each with exactly one business
defect seeded into its metadata, each deployed and put in front of the store's
own black-box suite. No agent, no prompts, no money.

This is the behavioural twin of the validator mutation suite proposed in the
`donat validate` RFC, and it exists for the reason
[[research-how-to-build-an-evals-framework]] gives: a benchmark's number is
only worth reading if the tests behind it can tell a correct application from a
plausible wrong one. Task 001 answered that question for one task with three
hand-written anti-oracles. This answers it for the whole store.

Artefacts: `evals/mutants.py` (operators and corpus), `evals/sweep.py`
(parallel stands and classification), `evals/mutants/` (the corpus itself,
one reviewable patch per mutant).

## The three-way verdict, and why the first one is a win

```
compiler   validate or the Process deploy refused it
tests      it deployed, and the black-box suite noticed
survived   it deployed, the tests that own its behaviour passed, and the
           store is wrong
uncovered  nothing on this stand owns its behaviour
```

`compiler` is not a failed experiment. It is the thesis of this repository
measured directly: every invariant that moves out of prose and into the closed
grammar or the compiler is a defect nobody can ship. A high compiler column is
the engine doing its job, and the mutants it eats are the ones an eval task
must **not** be built from — they measure the gate, not the suite.

`survived` is the output that matters. Each survivor is simultaneously a hole
in the behavioural suite and a ready-made anti-oracle: a store that deploys,
passes every check, and is wrong.

## The operators

Twelve, chosen because each one is a defect a competent person could plausibly
write, and each produces a patch small enough to read:

| operator | the defect |
|---|---|
| `ambiguity-to-failure` | every error route gives up instead of reconciling |
| `default-to-first-case` | an unrecognised outcome takes the first case's branch |
| `swap-case-targets` | two decision branches are wired to each other's outcome |
| `single-attempt` | a transient provider failure is no longer retried |
| `no-retry-on-timeout` | a timed-out call is treated as final |
| `early-signal-dropped` | `persist_before_match` off: an early signal is lost (ADR 034) |
| `drop-assert` | a business rule the command asserts is no longer checked |
| `require-found-false` | a missing row no longer refuses the command |
| `require-affected-false` | a write that changed nothing is accepted |
| `drop-state-guard` | the state a command requires drops out of its lookup |
| `open-row-filter` | a role's row filter is removed: it sees every customer |
| `flip-comparison` | a rule boundary moves by one |

358 sites exist across the store; the corpus takes 200, round-robin over
operators so that any prefix — a sample, or a sweep that ran out of night — is
a spread across every kind of defect rather than two hundred of the first one.
Three per operator per file, because ten identical-looking defects in one flow
measure the same hole ten times.

## Three ways the first runs lied, and the guards that came out of them

The first full sweep printed `caught 189/189 (100.0%)`. It was wrong, and the
three reasons are the durable output of this work — each is now a precondition
the harness enforces rather than a thing to remember.

**1. A red baseline reads exactly like detection.** 106 of the 137 "the tests
caught it" verdicts came from one test — an avatar upload that needs an object
store this stack deliberately does not run. It failed on every stand,
regardless of the mutant. *Guard:* `sweep.py --control` runs the pristine store
through the identical path, and a sweep refuses to start unless a control is on
file and green. A benchmark without a control measures its own plumbing.

**2. A harness failure is not a verdict.** Six mutants died to a full disk
while copying their metadata, and the classifier filed them under `compiler` —
"the engine refused it" — because any internal failure took that branch.
*Guard:* only `validate` and `deploy` refusals count as `compiler`; everything
else is `error`, the compose step retries once, and the sweep refuses to start
without disk headroom.

**3. A stand that answers is only yours if you started it.** An aborted sweep
left four engines holding the worker ports for hours. The next control run
drove one of them, found its database dropped, and reported 143 red tests —
which would have condemned the whole suite as broken. *Guard:*
`Stand.refuse_if_taken`, the rule `tests-system/stack.sh` has always had and
the sweep did not.

None of these were subtle failures of judgement; all three were the difference
between a number and a measurement, and only the first was predicted.

## Results

Two hundred mutants, two hundred verdicts. No mutant is excluded: the seven
that died to a full disk and the five that had no test module mapped were
re-judged after both faults were fixed.

```
survived   115        compiler    50
tests       35
```

**85 of 200 caught — 42%.** Static refusal accounts for 25 points of that
and the black-box suite for 17. The rate held between 42% and 48% across four
runs of different lengths, so it is the store's real detection power and not an
artifact of which mutants ran first.

| operator | n | validate | tests | survived | caught |
|---|---:|---:|---:|---:|---:|
| `drop-assert` | 23 | 0 | 1 | 22 | 4% |
| `drop-state-guard` | 22 | 0 | 3 | 19 | 13% |
| `single-attempt` | 20 | 0 | 3 | 17 | 15% |
| `no-retry-on-timeout` | 20 | 0 | 5 | 15 | 25% |
| `open-row-filter` | 22 | 0 | 8 | 14 | 36% |
| `default-to-first-case` | 18 | 6 | 4 | 8 | 55% |
| `swap-case-targets` | 10 | 0 | 3 | 7 | 30% |
| `early-signal-dropped` | 8 | 0 | 3 | 5 | 37% |
| `require-affected-false` | 22 | 15 | 3 | 4 | 81% |
| `flip-comparison` | 3 | 0 | 1 | 2 | 33% |
| `ambiguity-to-failure` | 9 | 7 | 1 | 1 | 88% |
| `require-found-false` | 23 | 22 | 0 | 1 | 95% |

Every one of the 50 static kills came from `validate`; the Process deploy
refused nothing that `validate` had let through. The three operators at the top
of that column — a command that no longer requires the row it edits, a write
that accepts having changed nothing, an error route that gives up instead of
reconciling — are the declared preconditions donat checks by construction.
For the other operators, static analysis contributes **zero**: that is the exact
boundary of what the engine promises to catch, measured rather than assumed.

Where the survivors live:

| file | survivors |
|---|---:|
| `flows/subscription-renewal.yaml` | 12 |
| `flows/return-refund.yaml` | 9 |
| `flows/checkout-cancellation.yaml` | 7 |
| `flows/partial-fulfilment.yaml` | 6 |
| `flows/b2b-order-approval.yaml` | 5 |
| `commands/checkout/finalize-declined-checkout.yaml` | 5 |
| `commands/checkout/finalize-pending-order-cancellation.yaml` | 4 |
| `flows/authorized-order-cancellation.yaml` | 4 |

The suite's strength is concentrated where it was written by hand: eleven of the
35 test-kills are in `checkout-payment` alone. `partial-fulfilment` took six
defects and the suite noticed none of them.

Two operators deserve naming. `drop-assert` — a business rule the command
asserts is simply removed — is the worst result in the corpus: 23 mutants, one
caught, 22 survivors, and **nothing** from `validate`. An assertion that
stopped happening is invisible to the compiler, because the metadata is still
valid, and invisible to the suite, because the rule it enforced was never the
thing a scenario reached for. This is the single largest hole the sweep found. `open-row-filter`, which hands one role
another customer's rows, survived 14 times out of 22.

## How it is made affordable

A stand per mutant is the cost, and three things bring it down:

1. **One template database.** DDL and opening stock are identical for every
   mutant, so they are migrated once and the database is cloned per run. Only
   the Process revisions differ, and those deploy in seconds.
2. **Coverage subsets.** Each mutated file names the black-box modules that own
   its behaviour, so a payment defect runs the payment suites rather than all
   205 tests.
3. **Parallel stands.** Each worker owns a database, an engine port and its own
   mock-provider process — separate, because a scripted answer claimed by
   another worker's durable work is indistinguishable from a store
   misbehaving.

The subset is also the one thing that could invent a survivor that is not real,
so **every survivor is re-run against the whole suite before it is believed**.

## What this is not

It does not measure an agent. A mutant is not a task: there is no prompt, no
oracle to write and nothing for anybody to build. What it produces is the
evidence that the *judging half* of the corpus works — and, in the survivors, a
worklist of tasks worth authoring, each already carrying the anti-oracle that
proves it measures something.

## What the sweep was for: task 002

The first use of a survivor as task material, and the point of the exercise.
`flows/authorized-order-cancellation.yaml` took fourteen defects; Petshop's own
suite noticed one. Every survivor there was the same shape — a payment provider
that stumbles or goes quiet while the store is trying to release a hold — so
that is what `tasks/002-cancelling-an-authorized-order` asks a candidate to get
right, and its two anti-oracles are two of those survivors, unmodified.

That is the loop closing: the sweep says where the store cannot tell right from
wrong, and each of those places becomes a task where a candidate has to be right
without a test to lean on. A benchmark assembled only from behaviour the suite
already checks would measure the suite, not the candidate.
