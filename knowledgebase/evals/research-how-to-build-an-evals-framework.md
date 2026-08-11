---
type: research
status: draft
date: 2026-08-09
---

# How to build an evals framework, and what that means for DonatBench

Research note written before any DonatBench code exists, to answer one
question: *what does a benchmark for "an agent builds a working donat
application" have to look like so that its number is worth reading a year from
now?*

It is deliberately grounded twice — in the published practice of the people who
build agentic benchmarks, and in the harnesses this repository already has.
Most of what a benchmark needs, donat already owns; what it does not own is the
part that decides whether the benchmark survives contact with a model that
optimises against it.

## 1. What we already have

An eval framework is a runner, an environment, a task corpus and a verifier.
Three of the four exist here under other names.

| Asset | What it already is |
|---|---|
| `crates/conformance` | A runner: spawns the engine, one database per suite (`conf_<name>`), parallel-safe, YAML fixtures, exact response comparison. The *shape* of a task runner. |
| `tests-system/stack.sh` + `docker-compose.yml` | A reproducible environment: Postgres, five mock providers, engine built from the working tree, a second "fast" stand whose periods run in seconds. |
| `examples/petshop/mock-providers` | Steerable external world — a control plane (`PETSHOP_PROVIDERS_CONTROL=1`) that scripts declines, 5xx and slow answers, and records what was actually sent. |
| `tests-system/tests/*.py` | 28 black-box behavioural suites over HTTP, per-role JWTs, no database peeking. The *shape* of a scenario verifier. |
| `examples/petshop` | A large, real, checked-in correct application. The positive corpus. |
| `donat validate` | A static verifier — today `Vec<String>` and fail-fast, see the validate-v2 RFC. |
| `plugins/donat/skills/*` | The scaffold under test: what the agent is given when it builds an app. |

The gap is not infrastructure. It is *task specification, verifier discipline
and measurement*.

## 2. The anatomy everyone converged on

Independently, the credible harnesses describe a task the same way.
Terminal-Bench states four components per task: a natural-language
instruction, a containerised environment, a programmatic verification suite,
and **an oracle solution demonstrating a valid approach**. The METR Task
Standard formalises the same thing as a `TaskFamily` class with
`get_instructions`, `install`, `start`, `score`, plus a resource manifest and a
declared `standard_version`; it deliberately says nothing about the agent, so
that runtimes can be swapped under a fixed task. Inspect AI splits the runner
along a third axis — `dataset` (samples), `solver` (how an answer is produced,
up to a full tool-using agent), `scorer` (how it is judged) — with sandboxing
and structured transcript logs as first-class concerns.

Three invariants fall out, and they are the ones to copy:

1. **The task declares its own environment and its own score.** A task is a
   directory, not a row in a spreadsheet, and it is versioned.
2. **The agent is not part of the task.** Swapping Claude Code for Codex, or
   changing the skills in `plugins/donat`, must not require touching a task.
   This is also the only way the bench can measure the *skills* rather than the
   model.
3. **Every task ships an oracle.** A reference solution that the harness runs
   in CI is the cheapest known defence against broken tasks — and broken tasks
   are the dominant failure mode (§3).

## 3. How benchmarks fail

This is the literature worth internalising, because each failure has a
mechanical countermeasure.

**Task quality — the biggest one.** When OpenAI had SWE-bench human-annotated,
38.3% of samples were flagged for underspecified problem statements and 61.1%
for unit tests that could mark a valid solution wrong. Later work on SWE-bench
Pro found 27–34% of tasks broken by pipeline analysis and human review, and an
audit of frequently-failed problems found the majority had test cases that
reject functionally correct submissions. A benchmark's headline number is
mostly a measurement of its own task hygiene.

*Countermeasure:* oracle-solution CI on every task; a second author reviews
every prompt for ambiguity; scenario tests must assert *outcomes*, not one
particular design.

**Verifier exploitability.** Rule-based verifiers have low recall and brittle
specifications; model-based verifiers are substantially more exploitable,
because the policy learns to produce output that satisfies the acceptance
logic rather than the task. Composite, layered, deterministic rewards with
explicit anti-hacking criteria are the current recommendation.

*Countermeasure for donat, specifically:* an empty metadata directory passes
`donat validate`. Static validity must therefore be a **gate**, never a score.
The Text-to-Terraform benchmarks do exactly this — security metrics are
computed only over generations that pass `terraform validate`, and failures are
recorded separately rather than scored.

**Grading by shape instead of behaviour.** The text-to-SQL field spent years
learning that exact-match under- and over-counts semantically correct programs,
and converged on execution accuracy. For a declarative format like donat
metadata this is sharper still: there are many correct ways to declare the same
store. Never diff YAML against the oracle.

**Noise.** `pass@1` on a small task set is not a measurement. τ-bench
introduced `pass^k` — all k attempts succeed — precisely because a 90%-per-try
agent is only 57% reliable at k=8, and reported GPT-4o dropping from ~50% to
<25% at `pass^8` on retail. Anthropic's *Adding Error Bars to Evals*
recommends CLT-based standard errors on the mean, **clustered** standard errors
when questions share a source (they can be >3× naive ones), paired-difference
analysis when comparing two systems, several answers per question, and power
analysis to size the set — its rule of thumb is ~1000 questions for good
power on small effects.

*Consequence for us:* DonatBench will have tens of tasks, not a thousand. That
is fine, but it means the honest output is a small set of confidence intervals
and paired comparisons — never a leaderboard decimal. Tasks derived from the
same application (several Petshop-shaped tasks) are a cluster and must be
treated as one.

**Saturation and contamination.** OpenAI has since retired SWE-bench Verified
from its own reporting, citing design and contamination problems. Any corpus
built from a public repository — and `examples/petshop` is public — is training
data the moment it is pushed.

*Countermeasure:* a public split for development plus a private held-out split
whose tasks are never committed to this repo; per-task canary strings; task
versioning with no silent in-place fixes.

## 4. What DonatBench should therefore be

### 4.1 The scoring contract: four gates, one behavioural score

The validate-v2 RFC already proposes `{structural, semantic, architecture,
runtime}` reporting. Keep it, but make the semantics explicit:

```
structural  → gate  (parses, closed grammar)
semantic    → gate  (references, types, permissions resolve)
architecture→ gate  (donat validate --strict clean)
runtime     → SCORE (scenario tests against a deployed stand)
```

Gates are recorded for error *classification* — they tell you where an agent
failed, which is the diagnostic value the RFC wants — but they contribute no
reward. Only behaviour scores. This is the whole anti-reward-hacking design:
the degenerate metadata that maximises every static gate scores zero on the
one axis that counts, and there is no LLM judge anywhere in the loop to
negotiate with.

A fifth, negative axis is worth having from day one: **conduct checks**. Did
the run modify the scenario tests, the oracle, the harness, or the migrations
it was told not to touch? Did it reach the network? Any hit voids the task.
Verified by diffing the working tree and by the sandbox, not by asking.

### 4.2 A task is a directory

Modelled on the METR/Terminal-Bench shape and on what `stack.sh` already does:

```
evals/tasks/<NNN>-<slug>/
  task.yaml           # id, version, prompt, tags, difficulty, budget,
                      # canary, fixture, writable paths, gates, scenarios
  seed/               # starting repo state handed to the agent (may be empty)
    migrations/       # DDL the task pre-supplies, or nothing if DDL is the task
    metadata/
  oracle/             # reference metadata (+ migrations) that must score 1.0
  anti/<defect>/      # plausible-but-wrong solutions that must fail (§4.7)
  README.md           # authoring notes, why this task exists, known ambiguities
```

`oracle/` lives outside the agent's writable tree; the harness mounts it only
when it verifies the task itself. Tasks are flat and numbered. They are **not**
foldered by engine feature (`processes/`, `connectors/`, …): a payment task
exercises processes, connectors and architecture at once, so any such tree
forces arbitrary placement, and — worse — it quietly pushes authors to write
prompts *about a feature* rather than about a business goal, which measures
transcription of the documentation. The cut by area belongs in the report, off
`tags:`.

### 4.3 Task format, first draft

```yaml
id: checkout-ambiguous-payment-001
version: 1
tags: [processes, connectors, ambiguity, payments]
difficulty: hard
canary: "DONATBENCH-CANARY-…"

fixture: petshop-base@3
writable: [metadata/, migrations/]
budget: { wall_clock: 20m, tokens: 400k }

prompt: |
  Add checkout payment authorization.
  Provider may time out after charging the card.
  Retry transient failures.
  Never double charge.

gates:
  migrate: pass
  validate: pass          # --strict, JSON; diagnostics recorded verbatim
  deploy: pass

scenarios:
  - world: provider_returns_200
    act: checkout
    assert: [order_authorized, charged_once, money_conserved]
  - world: provider_returns_500_then_200
    act: checkout
    assert: [order_authorized, charged_once, money_conserved]
  - world: provider_times_out_without_charge
    act: checkout
    assert: [instance_settles, not_charged, inventory_released]
  - world: provider_times_out_after_charge
    act: checkout
    assert: [instance_settles, charged_once, money_conserved,
             inventory_not_released_while_unproven]

oracle: oracle/
```

Two rules make this format work, and both are the difference between a corpus
that ages well and one that rots.

**No per-task structural assertions.** The tempting field is a list of named
invariants over the produced metadata — "the provider mutation carries an
idempotency key", "retry is bounded", "the timeout route does not go straight
to failure". Every one of those is misplaced. The first two cannot fail: a
`ProviderIdempotent` operation without a stable key and a `retry` with
`max_attempts: 0` are already compile errors, and `retry` itself is a required
field (`crates/processes/src/lib.rs:2100-2125`, `:2267`;
`crates/metadata/src/types.rs:1372`). The last two are the validator's
`PROC006`/`PROC007`, which — once implemented — apply to every task at the
gate rather than to one task by hand. Writing them per task produces a second,
untested implementation of the validator that drifts from the first, and it
freezes one particular correct design, which is precisely the failure that made
61.1% of SWE-bench samples unfair.

> Statically checkable ⇒ it belongs in `donat validate`, as a gate for every
> task. An eval asserts only what is visible in the behaviour of a deployed
> application.

**Scenarios name a world, an action and reusable outcome invariants.** A
scenario list of provider behaviours (`provider_times_out_after_charge`) names
the stimulus but not the claim. The claim belongs in a shared, closed library —
`money_conserved`, `charged_once`, `no_stranded_instance`,
`inventory_released` — checked over the public HTTP surface and the mock
providers' evidence log, never over the database or over state names the agent
chose. Those invariants are reused across tasks, are cheap to review once, and
survive any correct design. `examples/petshop/mock-providers` and
`provider-evidence/` already provide both halves.

The fourth scenario is the whole point of this task: the provider charged and
then went silent. It cannot be passed by guessing the shape of a solution.

### 4.4 The runner, and why it stays out of CI

One crate, the same way `crates/conformance` is one crate — but a **binary**,
not a test target. A run is k attempts × N tasks, each one an agent working for
minutes with network access and a paid API key; `cargo test` is the wrong entry
point for that, and `make test` must never reach it.

Driven from the Makefile only, following the precedent of the `perf` targets,
which are already documented as local-only and never apply pass/fail
thresholds:

```make
evals-run:            # local only: runs agents, costs money, needs API keys
evals-verify-oracles: # local only for now: no agent, no network, no keys
evals-report:         # confidence intervals, paired diffs, per-gate breakdown
```

**Nothing here runs in CI.** That is a deliberate decision, not an oversight:
CI stays free of anything that calls a model, so a pipeline can never be
flaky, costly or nondeterministic because of an agent. `evals-verify-oracles`
is the one target with no AI in it at all — it deploys each task's reference
solution and runs its scenarios — so it is the only candidate to be promoted
to CI later, once the task corpus stops moving. Until then it is a Makefile
target a human runs after touching the corpus.

Phases per attempt, all of them already implemented somewhere in this repo:

1. **provision** — fresh Postgres database, fresh mock-provider instance,
   seeded task state (`stack.sh` pattern, one DB per attempt as conformance
   already does per suite).
2. **agent** — run the adapter in the sandbox with the task's writable paths.
   The adapter is a thin process contract (`prompt in, working tree out`) so
   Claude Code, Codex and a plain API loop are interchangeable.
3. **gate** — `donat migrate`, then `donat validate --strict --format json`.
   Record diagnostics verbatim: this is also the corpus that tells us which
   validator diagnostics agents actually hit.
4. **deploy** — start the engine on the produced metadata; failure here is a
   distinct outcome from a validation failure and must be recorded as such.
5. **score** — run `tests/` over HTTP against the stand, as `tests-system`
   does, with per-role JWTs and no database access.
6. **record** — one JSONL line per attempt with every phase outcome, token and
   wall-clock cost, and the transcript path.

Cost and latency are first-class recorded metrics, not footnotes. An agent that
scores 0.8 for ten times the tokens is a different product decision.

### 4.5 Task authoring rules

- Every prompt states the business goal and the acceptance criteria in the
  domain's language, never in donat's. If a prompt names `for_each` or
  `persist_before_match`, it is testing transcription, not construction.
- Every task must be solvable by the oracle *and* fail for at least one
  plausible wrong design — otherwise it measures nothing.
- Ambiguity is declared in `README.md` and resolved in `prompt.md`. The
  SWE-bench annotation result says this is where a third of the corpus rots.
- Difficulty is a declared tag, and the corpus deliberately spans it. All-hard
  saturates at zero and measures nothing either.
- Scenario tests assert observable outcomes through the public surface: an
  order reaches `fulfilled`, a declined payment releases inventory, a duplicate
  request does not double-charge. They must not assert state names, table
  layouts, or command spellings that a different correct design would not use.

### 4.6 The task classes worth having first

Ordered by how much signal per authoring hour they give, and drawn from the
failure modes the engine itself was built around:

1. **Ambiguous external effect** — a provider that times out after possibly
   charging. Tests assert no double charge and no premature inventory release.
   This is the validate-v2 `PROC007` rule with a behavioural ground truth.
2. **Durable wait correlation** — a signal that can arrive before the wait is
   receptive (ADR 025/034). Tests assert the refund is never stranded.
3. **Bounded fan-out** — a batch that must not become unbounded work.
4. **Permission surface** — a role matrix where the wrong declaration leaks
   another customer's rows; `tests-system/tests/test_attacks.py` is the
   template, and it is also the class where the *no admin role* rule bites.
5. **Idempotent retry under provider retention** — the horizon check the
   compiler already does statically now has a runtime witness.
6. **Schema + metadata together** — the task supplies no migrations, so DDL and
   declaration must agree.

Each is a small family (§2, METR "task family") — same environment, several
prompts — which is also why clustered standard errors matter.

### 4.7 Measuring the business case, not the design

The hardest question in this whole design is not "did it deploy" but "does it
do what the business asked". Four things make that measurable without
prescribing a solution.

**The task pins the observable contract, never the implementation.** A test
cannot address an application whose surface it does not know, so the brief must
state what a client calls and what it can observe: the operation a storefront
invokes, the field it polls, and the closed set of values that field may take.
Everything behind it — states, commands, tables, error routing, which work is a
Process at all — stays free. This is the line to hold: pin more and the task
degenerates into transcription; pin less and no design-independent assertion
can be written. Real briefs come with a client contract, so this is also the
honest shape.

**Business rules are traced to assertions.** Each rule in the prompt ("never
double charge", "a cancelled order keeps no money", "staff may not read another
customer's address") maps to at least one scenario assertion, and the report is
per rule. "0.6" says nothing; "happy path and retry pass, refund window and
role isolation fail" is a finding. A rule with no assertion is an unmeasured
rule and the authoring review must reject it.

**Business correctness for donat lives in the adverse worlds.** The happy path
is table stakes; what the engine exists for is what happens when the provider
times out after charging, when a webhook is delivered twice, when two shoppers
take the last unit, when a signal arrives before its wait is receptive, when a
deadline expires, and when the engine is killed mid-Process and restarted. A
task that only scripts success measures nothing that donat is for. The existing
suites are the template — `test_edges_and_races.py`, `test_provider_failure_
branches.py`, `test_time_based_branches.py`, and the seconds-scale stand for
deadlines. Crash-and-restart is the one dimension `tests-system` does not
exercise today, and durable Processes are exactly the claim that needs it.

**The invariant library is the reusable half.**
`tests-system/tests/test_store_integrity.py` is already the prototype: "no
shelf is oversold", "no order is both paid for and given back", "a cancelled
order never keeps the money", "an authorized order has money behind it". These
are business facts checked over the public surface, they never mention a state
name, and they hold for every correct design. A task selects from the library
rather than authoring assertions, and every invariant is re-checked after every
world, not only after the world it was written for.

#### Anti-oracles: measuring whether the task measures anything

An oracle proves a task is solvable. It does not prove the task can tell a good
application from a plausible bad one — and that is the property that decides
whether the benchmark's business coverage is real. So each task ships
counter-examples next to its reference solution:

```
oracle/                 # must score 1.0
anti/timeout-to-failure/        # routes the ambiguous timeout straight to failed
anti/no-reconciliation/         # retries, never reconciles the provider
anti/early-inventory-release/   # releases the hold before the outcome is proven
anti/leaky-role/                # staff can read another customer's order
```

Every anti-oracle is a *plausible* application — it deploys, it passes
`validate`, it satisfies the happy path — carrying exactly one business defect.
The harness asserts that each one fails, and fails on the *named* assertion it
was built to trip. A task whose anti-oracles pass is a task that measures
nothing, and it is caught by `make evals-verify-oracles` before any agent is
ever run, at zero model cost.

This yields the only honest coverage metric available here:

```
business-case detection power = anti-oracles caught / anti-oracles authored
```

It is the behavioural twin of the validator's mutation score, and it has the
same caveat: an anti-oracle that fails to deploy or fails `validate` proves
nothing — the gate already caught it, and the scenario suite was never
exercised. Only anti-oracles that reach the scoring phase count. Writing them
is also the cheapest way to discover that a business rule in the prompt has no
assertion behind it.

Every element of this section has an ancestor in an existing system —
Camunda's process-path coverage, Temporal's replay tests, Step Functions'
mocked worlds, Jepsen's fault injection, mutation testing, Terraform's
`expect_failures`, Stripe's idempotency semantics. See
[[research-how-other-systems-verify-business-behaviour]], whose §9 lists seven
deltas this note did not have.

## 5. Relationship to the three existing layers

The RFC's closing diagram is right, and the bench slots in as a fourth
consumer rather than a replacement:

```
skills      → generation          (plugins/donat)
validate    → static correctness  (deterministic verifier, gate)
conformance → protocol conformance (donat v2 contract, unchanged)
donatbench  → construction ability (does an agent build a working app)
```

Two boundaries must be kept sharp:

- **Conformance is not a bench.** Its fixtures are ground truth for the Donat
  v2 surface. Nothing in DonatBench may edit them, and DonatBench failures are
  never evidence about conformance.
- **The validator's mutation suite is not the bench.** Mutation score measures
  the *validator*; DonatBench measures the *agent*. They share a corpus and
  nothing else. (And, as noted in the RFC review, a mutation corpus is only
  meaningful if the mutants still compile today.)

## 6. Dependencies and sequencing

DonatBench cannot be built well before validate-v2's phase 0, because phase 3
of the runner needs machine-readable diagnostics to classify a failure at all —
a `Vec<String>` gives `architecture: 0` with no reason. That is the one hard
ordering constraint. Everything else can be built in parallel:

1. validate-v2 phase 0 (structured diagnostics, `--format json`, stable codes).
2. Task schema + runner skeleton + `make evals-verify-oracles`, with **two**
   tasks.
3. Agent adapter contract; first real measurement of the current skills.
4. Corpus growth to ~30 tasks across the six classes, public split.
5. Private held-out split, reported separately.

Two tasks with an oracle gate is a better artefact than thirty without one.

## 7. Open questions

- **Where do tasks live?** In-repo is contamination; a sibling private repo
  splits CI. Probably: public split here, private split elsewhere, one schema.
- **Is the DDL in or out of scope for the agent?** Both are legitimate tasks,
  but they measure different things and should be separate tags.
- **How is a partially correct app scored?** Per-test fractional credit is more
  informative but easier to game than all-or-nothing per task. Recommendation:
  record both, headline the strict one, and never mix them in one number.
- **How many attempts?** `pass^k` needs k ≥ 5 to say anything; at tens of tasks
  and agent-scale cost this is the dominant budget line.
- **Do we measure the skills or the model?** Both, but only if the skill bundle
  is pinned by version in the run record.

## Sources

- [Terminal-Bench 2.0 — task components and curation](https://snorkel.ai/blog/terminal-bench-2-0-raising-the-bar-for-ai-agent-evaluation/)
- [METR Task Standard](https://github.com/METR/task-standard)
- [Inspect AI — datasets, solvers, scorers, sandboxing](https://inspect.aisi.org.uk/)
- [Introducing SWE-bench Verified (OpenAI)](https://openai.com/index/introducing-swe-bench-verified/)
- [Why we no longer evaluate SWE-bench Verified (OpenAI)](https://openai.com/index/why-we-no-longer-evaluate-swe-bench-verified/)
- [Are "Solved Issues" in SWE-bench Really Solved Correctly?](https://software-lab.org/publications/icse2026_SWE-bench-correctness.pdf)
- [UTBoost: Rigorous Evaluation of Coding Agents on SWE-Bench](https://arxiv.org/pdf/2506.09289)
- [τ-bench: Tool-Agent-User Interaction (pass^k)](https://arxiv.org/abs/2406.12045)
- [Adding Error Bars to Evals (Miller, Anthropic)](https://arxiv.org/abs/2411.00640)
- [Reward Hacking in RLVR — survey of verifier failure modes](https://www.emergentmind.com/topics/reward-hacking-in-rlvr)
- [Reward Hacking Mitigation using Verifiable Composite Rewards](https://arxiv.org/pdf/2509.15557)
- [Security-First Evaluation of Text-to-Terraform](https://arxiv.org/html/2608.02672)
- [Jackal: execution-based Text-to-JQL benchmark](https://arxiv.org/pdf/2509.23579)
