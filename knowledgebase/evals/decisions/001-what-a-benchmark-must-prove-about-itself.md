---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[evals]]"
---

# What a benchmark must prove about itself before its numbers are read

## Context

The evals corpus scores applications an agent builds. Its output is a number,
and a number is believed in a way a paragraph is not — so the ways this corpus
can produce a *confident wrong* number matter more than the ways it can produce
no number at all.

Eight of them have already happened here, none hypothetical:

1. The first mutant sweep printed `caught 189/189 (100.0%)`. 106 of those kills
   were one test that fails on the pristine store, because this stack does not
   run the object store it needs. A red baseline is indistinguishable from
   detection.
2. Six mutants died to a full disk while their metadata was being copied, and
   were filed as "the engine refused them" — a harness failure wearing a
   verdict's clothes.
3. An aborted sweep left engines holding the worker ports. The next control run
   drove one of them, found its database dropped, and reported 143 red tests —
   which would have condemned the entire black-box suite.
4. Task 001 declared one killing scenario per anti-oracle. Splitting one of them
   revealed it was compound: the half about the warehouse never discriminated at
   all, and the task had been resting on a single assertion while appearing to
   rest on two.
5. The function that turns a run into a score read a field shape that does not
   exist, and classified every attempt — including a clean pass — as "never
   built". It would have reported a flat zero for any model.
6. An attempt read 10 scenarios of 10 red with all four gates green. The same
   answer, on a fresh stand, read 8 of 10 — the two real defects it actually
   had. A stand that deploys and then does not serve is shaped exactly like a
   hopeless answer.
7. An independent review of the finished harness found four more, none of which
   had fired yet: a junit report left on disk from the previous candidate and
   read as this one's when pytest produced none; a stale mock provider on a
   port nobody guarded, which a worker would drive as if it owned it; two
   `PETSHOP_FAST_*` variables inherited from the developer's shell, which would
   have run part of every mutant's suite against the *pristine* store and
   published the result as `survived`; and a `commands/booking/` directory
   missing from the coverage map, which filed five real defects as
   "nothing tests this" while a module that tests it sat in the suite.
8. Task 001 was checking two things it never asked for: the amount recorded
   against a payment, and that every authorization attempt carry one shared
   idempotency key — the latter on the *happy path*, where a store that charged
   exactly once without a key had broken no stated rule and failed anyway.

Each was found by looking, not by the harness objecting. That is the problem
this decision addresses. The seventh entry is the sharpest version of it: those
four were found by a reviewer reading the code, and every one of them produces
a number that looks exactly like a real result.

## Decision

A task or sweep must prove eight things about itself, and the harness enforces
each one rather than documenting it.

**A control, before any verdict.** `sweep.py --control` runs the unmodified
store through the identical path, and a sweep refuses to start unless a green
control is on file. A test that fails on a correct store kills every mutant it
touches.

**A harness failure is never a verdict.** Only `validate` and `deploy` refusals
count as the engine refusing a defect; every other internal failure is `error`,
excluded from the denominator and reported separately. On its first outing this
caught seven ENOSPC failures that the earlier classifier would have counted as
static detection.

The rule needed enforcing twice, which is the point of writing it down. An
independent review found that an engine which *failed to boot* — a lost port, an
OOM, a slow box — still arrived here as a `deploy` failure and was published as
"the invariant is enforced statically". Booting now has a phase of its own
(`serve`) that falls to `error`. The published 44% happened to be unaffected —
all 49 static kills in that run carry `refused_at: validate`, checkable in
`results.jsonl` — but on a loaded machine the same run would have reported a
higher number for a worse reason. A guard is only as good as the paths that
cannot reach around it.

**A stand is yours only if you started it.** Both runners refuse a port that
already answers.

**Two independent killers per anti-oracle.** `caught_by` is a list, minimum two,
enforced by `verify-oracles`. One assertion is a single point of failure for a
whole task: the day it drifts, the task stops discriminating and nothing goes
red to say so. "Independent" means a different surface — the shopper's orders,
support's books, the provider's call log — not the same reading rephrased.

**Total failure is read twice.** An attempt where *every* scenario fails on
green gates gets a second stand, and the second reading is the verdict. Total
failure is rare enough to pay for, and the two readings settle it: agreement
means the answer really is that bad, disagreement means the first reading was
the harness talking. The attempt that prompted this was wrong either way, so
the score was unharmed — but the same fault lands on a correct answer just as
easily, and nothing in the record would have shown it.

**Nothing addresses a stand it did not start, and nothing reads a file it did
not write.** Both ports are refused if something already answers, not just the
engine's. Every child process gets an environment with `PETSHOP_FAST_*`,
`DONAT_DATABASE_URL` and `DONAT_METADATA_DIR` stripped, because each of them
silently redirects part of a run to a different store. The junit report is
deleted before pytest runs, so a run that produces none fails loudly instead of
inheriting the last candidate's verdicts. These are one rule wearing three
hats: **a reading is only evidence if it came from the thing under test.**

**A task may not check what it never asked.** Every state literal a scenario
demands — `{"cancelled"}`, `{"authorized"}` — is looked for in the brief, and
`verify-oracles` warns when one is missing. It reads words rather than meaning,
so it warns instead of failing, and that was enough: task 001 was asserting a
recorded amount the prompt never mentioned, and demanding a shared idempotency
key on the *happy path*, where a store that charged exactly once without one
broke no stated rule and failed anyway. Underspecification is the most common
defect in benchmark tasks — 38.3% of SWE-bench samples were flagged for it — and
it is invisible from inside, because the author knows what they meant.

**An oracle must agree with itself.** `verify-oracles --stability K` runs the
reference solution K times and fails on any scenario whose verdict varies.
Petshop already contains load-sensitive tests; a flaky scenario inside a task
marks correct answers wrong at random, and looks exactly like a model failing.

Scoring keeps the same discipline. Four outcomes — `voided`, `unbuilt`,
`wrong`, `pass` — and `voided` (an answer that edited files the task closed)
leaves the denominator entirely. A benchmark that counts containment failures
as model failures rewards a leaky harness. It is reported beside the score,
never inside it, because a rising void rate invalidates the score above it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Trust the suite; investigate only when a number looks wrong | The failures above all looked *right*: 100% detection, a clean compiler column, a red suite. Suspicion triggered by implausibility catches only implausible lies. |
| Exclude known-bad tests by name | Requires knowing which are bad. The control discovers them; a hand-maintained exclusion list rots into a way to hide regressions. |
| One killer per anti-oracle, reviewed carefully | Review is what produced the compound assertion in the first place. The rule costs one extra scenario and does not depend on anyone staying alert. |
| Count `voided` as a failure | Makes the score go up when containment gets worse — precisely backwards. |
| Partial credit per scenario as the headline | Rewards satisfying one branch and abandoning the rest. Kept as diagnostics, never as the number. |

## Consequences

Verification costs more: a full `verify-oracles` for one task is a null
candidate, an oracle, and every anti-oracle, each a full stand; stability
multiplies the oracle by K. Targeted flags (`--anti`, `-k`, `--stability`)
exist so the authoring loop does not pay the whole bill on every iteration.

Tasks are more work to write — two independent observations per defect means
finding two surfaces where the same wrongness shows, which is genuinely harder
than writing one assertion twice.

In exchange, every number the corpus prints has a stated reason to be believed,
and the failure modes above are now caught by the harness rather than by
someone happening to look. The rule that generalises: **a benchmark's first
subject is itself**, and the evidence it produces about itself is a
precondition for reading anything else it produces.
