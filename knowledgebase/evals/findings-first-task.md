---
type: research
status: draft
date: 2026-08-09
---

# What the first task taught

Step 1 of the build order in [[research-how-to-build-an-evals-framework]]: one
task, an oracle, three anti-oracles, no agent. The question it exists to answer
is not "how do agents do" but "**would we be able to tell**". Everything below
is from runs, not from design.

The artefacts are `evals/` in this repository:
`evals/tasks/001-checkout-ambiguous-payment`, `evals/run.py`, `evals/agent.py`.

## The result

```
oracle                                    7/7 scenarios, 7 instances, history ok
anti/ambiguity-to-failure                 FAIL test_a_charge_the_store_never_learned_about
anti/success-default                      FAIL test_an_authorized_order_has_money_behind_it
anti/unproven-absence-treated-as-decline  FAIL test_an_unproven_absence_is_not_a_decline

business-case detection power 3/3
```

Every anti-oracle passed `migrate`, `validate --strict` and `deploy`, so each
one reached the scoring phase and the verdict is about the scenario suite
rather than about the gate. Each died on exactly the assertion it was written
to trip, and on no other — the discrimination is specific, not a blanket
failure.

## What was predicted wrong

The step-1 prediction was that *some anti-oracles would survive*, and that
discovering a weak scenario suite would be the main value of the exercise. That
did not happen: the suite killed all three on the first attempt. The prediction
was wrong for a reason worth keeping: the scenarios were assembled from the
Petshop black-box suite's existing idioms, which have had months of contact
with a real store. A suite written from scratch for a new domain should not be
expected to behave this well, and the anti-oracle gate is exactly what will
say so.

## What the runs showed that the design did not

**A null candidate is a useful third data point.** An empty answer — the
fixture with nothing added — was run deliberately. It fails at `validate`,
which proves the fixture really is missing something and the task is not
accidentally satisfiable by doing nothing. This is cheap and belongs in
`verify-oracles`.

**The validator's diagnostic is usable but its path is not.** The null
candidate produced:

```
inconsistency: commands[0].effects[0].start_process.process:
  process 'default.checkout_payment' does not exist
```

The message is exactly right; `commands[0].effects[0]` is not. An agent has to
count array entries in a file it did not write to find out which command is
meant. This is the index-versus-name point from the validate-v2 review, met in
practice rather than in review, and it is now the strongest argument for name-
based semantic paths in phase 0.

**Both output streams matter.** `donat validate` lists the inconsistencies on
stdout and reports only the *count* on stderr. A harness that captures the
conventional stream — stderr on failure — records "1 inconsistency(ies)" and
throws away the only sentence that says what to fix. Worth remembering when
phase 0 designs `--format json`.

**Process history verification is free and worth having.** `donat process
verify-history` over every instance a run produced adds seconds and needs no
knowledge of the candidate's design. Seven instances per run, all coherent,
across four different stores. It has caught nothing yet, which is the correct
result for a passing store; it costs nothing to keep.

**The compiler forces one anti-oracle to be honest.** Routing past the
reconciliation states without removing them is a compile error — a state
unreachable from `start_at` is rejected. So `ambiguity-to-failure` had to
delete them, which is a fair description of what that design actually is. A
closed grammar makes "plausible but wrong" harder to write, which is good for
the product and slightly more work for the corpus.

**Two anti-oracles were rejected during authoring**, and the reasons generalise:

- *an unstable idempotency key* — already a compile error, so it measures the
  gate, not the suite;
- *reconciling by re-issuing `authorize`* — with the same stable key the
  provider deduplicates, so this is a legitimate alternative design. This is
  the equivalent-mutant hazard from the mutation-testing literature, and it
  arrived on the first task rather than the tenth.

## What this cost, and what it did not

No new framework was needed to answer the question. `evals/run.py` is a few
hundred lines that compose metadata, raise a stand and read a JUnit file; the
stand, the steerable providers, the role tokens and the polling helpers were
already in `tests-system`. The only change to existing code was moving the
Petshop fixtures from `tests-system/conftest.py` into
`petshop_qa/fixtures.py` so two suites can load them.

That is the build order working as intended: the framework is what is left over
after the task is done by hand, not what has to exist before it.

## Step 2: the first agent attempt

Claude Code carrying the `plugins/donat` skills (cache 0.3.0, verified
byte-identical to the working tree), one attempt, a workspace outside the
repository. It solved the task:

```
attempt 1  306s, exit 0, 2 files written
[compose:pass  migrate:pass  validate:pass  deploy:pass]  7/7 scenarios, history ok
```

Three things came out of that single run, and two of them matter more than the
score.

**The scenarios accepted a design that is not the oracle's.** The answer is 327
lines against the oracle's 236, with nineteen states against fifteen and
different names throughout (`open_order` where the oracle says `checkout`,
`route_authorization_lookup` where it says `route_reconciled_authorization`).
It also handles one branch *differently on purpose*: where the oracle sends a
**proven** absence to manual reconciliation, the agent cancels the checkout and
releases the stock, which is a defensible reading of the same brief. Both pass,
because the assertion for that world says "no money held and not authorized"
rather than naming a state. This is the design-independence requirement met in
practice, and it is the single most reassuring result so far — a suite written
against the oracle's shape would have failed a correct answer.

**The workspace had to be moved out of the repository first.** The oracle for
this task *is* the flow the checked-in Petshop ships, so an agent working
anywhere under the repo can read the answer. Caught while wiring the adapter,
not by a run. Any task derived from `examples/` has this property, and it is a
harness rule rather than a per-task note: the workspace holds the fixture and
the brief, and nothing else.

**The task does not discriminate at the top of the range.** One task solved
first try tells us nothing about how good an agent is, only that this task is
not a barrier for this scaffold. That is a legitimate result for the corpus —
every benchmark needs a floor — but it means task 002 must be harder, and it
means the interesting comparison now is against the *bare* scaffold, which
isolates what the skills contribute.

The agent could not run `donat validate` (no engine, no database in the
workspace) and got it right by reading anyway. That is worth re-testing once
there is a task it fails: the value of phase-0 diagnostics is in the repair
loop, and this run never needed one.

## Still open after step 1

- **The crash-restart world** (delta 5 of
  [[research-how-other-systems-verify-business-behaviour]]) is not implemented.
  Nothing in this repository kills the engine mid-Process, and durable
  execution is the central claim.
- **Graph coverage** (delta 2) is not recorded. The candidate's compiled graph
  is available; the run record does not yet say which of its transitions the
  scenarios reached.
- **One task is not a measurement.** The next thing that changes anything is a
  second and third task in a different class, because that is what turns the
  invariant list into a library and shows whether the format survives contact
  with a task that is not about payments — and because task 001 turned out to
  be a floor rather than a discriminator.
- **The skills' contribution is unmeasured.** The same task run against Claude
  Code *without* the plugin is the cheapest next data point: one number that
  says whether `plugins/donat` is doing work.

## Task 002, and the prompt defect only a good answer could find

Three independent attempts at `002-cancelling-an-authorized-order`, the task
built from mutants that survived Petshop's own suite. All three deployed and
validated; two passed every scenario; one failed exactly the pair that kills
the `no-retry-on-timeout` anti-oracle. pass@1 0.667 at $2.23 an attempt.

The failing answer is the interesting one, because it was not careless. It
re-issued the void only on a *proven* absence:

```yaml
- matches: { found: false, terminal_absence_proven: true }
  next: retry_void
default: void_outcome_unknown      # left for a human
```

That is the correct rule for **taking** money — it is what task 001 exists to
teach, and it is what the store's own checkout flow does. Applied to **giving
money back** it strands the shopper, because the asymmetry runs the other way:
a duplicate authorization charges someone twice, a duplicate void costs nobody
anything. Asking the provider again is not deciding on its behalf.

The prompt did not say so. Rule 2 said silence proves nothing and deciding
either way is inventing an answer, and the answer followed that to the letter.
By the rule this corpus is held to — a prompt is fixed only when an answer
that is *right about the business* fails — this qualified: nothing false was
recorded, no money was lost, a case was left for support. The prompt now states
the asymmetry outright.

Worth keeping as the general shape: the scenarios were right, the anti-oracles
were right, and the defect was in the one artefact nothing else checks — the
English. A task's prose is the part of it with no oracle, and the only thing
that audits it is a competent answer failing for a defensible reason.

## Task 001 under k=3, and what the second killer bought

Three attempts, all four gates passed by all three, nothing voided, pass@1
0.667 — the same shape as task 002 and, at this sample size, the same number
for the same reason: one attempt in three gets the ambiguity wrong.

The failing answer never called the provider's lookup at all. It decided the
fate of an in-doubt charge from what it already had, which is precisely the
defect the `ambiguity-to-failure` anti-oracle encodes — an independent attempt
reproducing, unprompted, the mistake the corpus was built to catch. The
assertion that caught it, `test_an_ambiguous_charge_is_investigated_not_written_off`,
is one of the second killers added the same day to satisfy the two-per-anti
rule; the older assertion caught it too, but the task no longer depends on
either one alone.

One methodological caveat, recorded so the numbers are not over-read: the
scenarios are `serial` and share one store, so a single wedged Process instance
makes later scenarios fail for reasons of their own. Ten red lines are not ten
independent findings. `pass@1` is unaffected — it is all-or-nothing per attempt
— but the per-scenario column in a failing run is a symptom list, not evidence.
