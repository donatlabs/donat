# evals — can an agent build this application?

The fourth verification layer, next to the skills that generate donat
applications, `donat validate` which says whether one is statically correct,
and `crates/conformance` which says whether the engine honours the Donat v2
contract. This one asks whether an application somebody *built* behaves like
the business asked.

Design notes: `knowledgebase/evals/`.

## Nothing here runs in CI

Every target is driven from the Makefile, the way `benchmarks/perf` is. A
pipeline must never call a model: it would be slow, costly and
nondeterministic for reasons that have nothing to do with the change under
test. `make evals-verify-oracles` is the one target with no agent in it at all
— it is a candidate for CI later, once the corpus stops moving.

## What a task is

```
tasks/001-checkout-ambiguous-payment/
  task.yaml     the whole contract: fixture, prompt, worlds, expectations
  README.md     the example map, and why this task exists
  oracle/       a reference solution that must score 1.0
  anti/<name>/  a plausible-but-wrong solution that must fail, and how
  scenarios/    the outcome assertions, run against whatever was built
```

A candidate — the oracle, an anti-oracle, or an agent's answer — is an
**overlay**: the files it wrote, dropped onto the fixture. Nothing else about
the store changes, so a scenario is comparing solutions to one problem rather
than two different stores.

## Running

```bash
make evals-verify-oracles         # every task: oracle passes, anti-oracles die
make evals-run TASK=001-checkout-ambiguous-payment CANDIDATE=oracle
make evals-run TASK=001-checkout-ambiguous-payment CANDIDATE=anti/success-default
make evals-run TASK=001-checkout-ambiguous-payment CANDIDATE=/path/to/answer
```

## The tasks

| task | what it asks for | why it exists |
|---|---|---|
| `001-checkout-ambiguous-payment` | take the money at checkout, and be right about it when the provider goes quiet | the ambiguity case donat's Process model exists for |
| `002-cancelling-an-authorized-order` | give the money back when a shopper cancels, through a provider that stumbles | the sweep put 14 defects in this flow and Petshop's own suite caught one |

Every anti-oracle must be killed by **at least two independent scenarios**, and
`verify-oracles` fails a task that declares fewer. One assertion is a single
point of failure for a whole task: the day it quietly weakens, the task stops
telling right from wrong and nothing goes red to say so. Splitting task 001's
compound assertions found exactly that — a check that read like two and was one.

### What the conduct record does not cover

The workspace lives outside the repository and every file the agent touches is
recorded, so an answer that edited a closed file is voided. It is **not** a
sandbox. The oracle for each task is a file in this repository, byte-identical
to the Petshop flow it was taken from, and the default command runs with
`--dangerously-skip-permissions` on the same filesystem: one `cat` of
`examples/petshop/metadata/flows/...` yields a perfect answer whose conduct
record is spotless, because the only file it wrote is a declared writable.

No number produced here is leak-proof against a model that goes looking. Until
the agent runs with the repository unmounted, treat these scores as measuring
capability under good faith — which is what they were built for — and not as
evidence against a motivated one.

An agent run also records what it cannot be trusted to report itself: whether
it stayed inside the files the task opened (an answer that got there by editing
something it was told not to is voided, not failed), what it cost in tokens and
dollars, and how much of the Process graph **it** built the scenarios actually
reached — coverage of the candidate's own graph, reported and never scored.

One reading is enough except when it says *everything* failed. A stand that
deploys and then does not serve fails all scenarios at once with green gates,
which is the same shape as a hopeless answer — so an all-red attempt is read a
second time on a fresh stand and the second reading is the verdict. It happened
on the first real measurement: 10 red of 10, then 8 of 10 on the rerun.

## Tuning something against this corpus

The corpus has two jobs and they pull in opposite directions. One is to say how
good an agent is at building donat applications; the other is to tell you
whether the edit you just made to a skill helped. The second is what a working
day looks like, and it needs a different instrument.

**Paired, not absolute.** With two tasks and three attempts, `pass@1` moving
from 0.667 to 1.0 is two thirds of noise — the interval on 2/3 is `[0.21, 0.94]`.
The same evidence read *paired* is a measurement: run both arms on the same
tasks with the same attempt count, pair every (task, attempt, scenario), and
task and scenario difficulty — common to both arms — cancel out. What is left
is the arm.

```
make evals-arm TASK=001-checkout-ambiguous-payment LABEL=bare
# edit plugins/donat/skills/…
make evals-arm TASK=001-checkout-ambiguous-payment LABEL=v3 SKILLS=plugin
make evals-compare BEFORE=bare AFTER=v3
```

**Skills are a variable of the run, not of the machine.** `--skills` installs a
skill set into the workspace and records a hash of its content with the result.
Two runs are comparable only if that hash is the same or deliberately
different; a run that inherits whatever the developer happens to have installed
is neither reproducible nor comparable to the one before it. Omitting `--skills`
is the bare-model baseline, and it is a real arm — the six attempts of the first
measurement were all bare, because the workspace lives outside this repository
and never saw the plugin.

**Watch the scenario rate, not the task rate.** Three attempts carry three
task-level verdicts and thirty scenario verdicts. An edit that takes an attempt
from six of ten scenarios to nine of ten is invisible in the first and obvious
in the second. It is printed beside `pass@1` and it is a diagnostic, never the
headline: an answer that satisfies most branches and abandons the rest is not
most of a working store.

**Split the corpus before you tune it, not after.** Tasks carry
`split: dev|holdout`. Tuning against a task and then reporting a number from it
measures how well you tuned. `agent.py` refuses a holdout task without an
explicit flag, and the refusal says why.

**The task is a variable too.** Attempts record a fingerprint of the whole task
directory beside the skill-set hash. Fixing an underspecified prompt is a change
to the question, and two arms that answered different questions are not a paired
comparison however carefully they were labelled — `compare` says so rather than
letting the labels carry it.

**When a scenario asserts something the prompt never said, fix the prompt.** The
audit is mechanical: take every assertion in the scenarios and look for it in
the brief. Task 001 was checking two things it never asked for — the recorded
amount, and that every authorization attempt carries one shared idempotency key
— and the second ran on the *happy path*, so a store that charged exactly once
without a key failed a scenario it had not violated. Weakening the assertion
would have discarded a property worth having. Underspecification is the most
common way benchmarks go wrong; the remedy is to say the requirement, not to
stop checking it.

**A task at the ceiling cannot tune anything.** Task 002 scores 3 of 3 for the
bare model, so no skill edit can show a gain there — it can only lose. Run the
bare arm on a new task before trusting it as a tuning task and keep the ones
that land in the middle; a saturated task is still worth having as a regression
guard, but it will never move.

**What `compare` prints that a score cannot.** Which scenarios the new arm
broke, and which fail in *both* arms. The second list is the answer to "what do
I write in the skill next".

`evals/run.py` raises a stand of its own — its own database, its own port
(8090), its own mock providers (8097) — so it never disturbs
`tests-system/stack.sh` or the conformance stack. It builds the engine from
this working tree, applies the store's DDL, deploys the Process revisions, runs
the scenarios, verifies the recorded history of every Process instance the run
produced, and tears the stand down. Logs and the composed metadata stay in
`evals/.state/<task>--<candidate>/`.

The scenarios run under `tests-system/.venv` when it exists — same driver, same
dependencies as the black-box suite.

## The mutant sweep

A second thing lives here, and it is the one that runs at volume. `mutants.py`
seeds one business defect at a time into the checked-in Petshop — 200 of them,
each a reviewable one-hunk patch — and `sweep.py` runs each mutated store
against the store's own black-box suite, several stands at a time.

```bash
make evals-control                 # the pristine store: must be green first
make evals-mutants                 # regenerate the corpus (deterministic)
make evals-sweep                   # the whole corpus, 6 workers
make evals-sweep LIMIT=24          # a spread across every operator
make evals-sweep ONLY=drop-assert  # one operator
```

**The control is a precondition, not advice.** A test that is red on the
pristine store kills every mutant it touches and reads exactly like detection —
that is how the first sweep came to print a confident, wrong 100%. `evals-sweep`
refuses to start unless a control is on file and green.

Every mutant lands in one of four places, and the first two are both wins:

| outcome | meaning |
|---|---|
| `compiler` | `validate` or the Process deploy refused it — the invariant is enforced statically |
| `tests` | it deployed and the black-box suite noticed |
| `survived` | it deployed, the tests that own its behaviour ran and passed, and the store is wrong |
| `uncovered` | nothing on this stand owns its behaviour — an unrun test is not a passing test |

A survivor is the output that matters: a hole in the suite, and a ready-made
anti-oracle for an eval task. Survivors are re-run against the **whole** suite
before they are believed, because the per-mutant subset is what makes 200
stands affordable and is also the only thing that could invent a survivor that
is not real.

No agent, no prompts, no money.

## What is scored, and what is only a gate

`migrate`, `validate` and `deploy` are **gates**: they say where a candidate
fell over, and they contribute nothing to the score. An empty metadata
directory passes `donat validate`, so static validity can never be the reward.
Only the scenarios score, and they assert business outcomes over the public
HTTP surface — never state names, table shapes or command spellings, which a
different correct design would not share.
