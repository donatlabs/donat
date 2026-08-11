---
type: research
date: 2026-08-10
features:
  - "[[evals]]"
---

# How the field grades agents, and where DonatBench sits against it

A second research pass, done after the corpus existed and had produced numbers.
[[research-how-to-build-an-evals-framework]] asked what to build; this one asks
whether what we built is graded the way the field has learned to grade, and it
is organised around the one artifact worth adopting wholesale: the **Agentic
Benchmark Checklist (ABC v1)** (Zhu et al., July 2025), which audits benchmarks
on task validity, outcome validity and reporting.

Their headline finding sets the tone. Of ten popular agentic benchmarks
assessed: **seven had outcome-validity flaws, seven had task-validity flaws,
and all ten had reporting limitations.** SWE-bench Verified can overestimate
agent skill by up to 100% through weak tests; τ-bench's criteria can count an
agent doing *nothing* as success. These are the best-known benchmarks in the
field.

## What the field now knows about test-based grading

**Passing the tests is not the same as being right.** METR had four maintainers
from three repositories review 296 AI pull requests that had already passed the
SWE-bench Verified grader. About **half would not be merged**; the grader ran
~24 points above the maintainers' accept rate. Their rejection categories are
worth memorising, because they are a taxonomy of what tests miss: code quality
against repo standards, *breaks other code*, core functionality not actually
solved, and other. METR also names the structural unfairness — agents get no
chance to iterate on reviewer feedback, unlike the humans they are compared to.

**Test suites are themselves defective at measurable rates.** UTBoost's
analysis found defects affecting 24.4% of SWE-bench Verified. The original
SWE-bench needed 93 developers to curate 500 usable tasks out of it. A
benchmark's headline number is substantially a measurement of its own task
hygiene, and this is now quantified rather than suspected.

**Reliability is a separate axis from capability.** τ-bench's `pass^k` — every
one of k attempts succeeds — exists because a 90%-per-attempt agent is 57%
reliable at k=8. For an application generator this is the more relevant number:
a store you cannot rebuild twice is not a store anyone can deploy.

**Flaws can be found by machine.** Recent work scans agent *transcripts* with
automated detectors (Inspect Scout) for four categories: ground-truth access,
tool failures, answer-format ambiguity, and guessability. It confirmed known
contamination and found unreported vulnerabilities in SWE-bench Verified,
CORE-Bench, KernelBench, CVE-Bench and Terminal-Bench 2.0. The authors are
explicit that scanners supplement rather than replace human audit.

**Quality at scale costs review, not cleverness.** Terminal-Bench curated 229
submitted tasks down to **89**, at roughly **three hours of reviewer attention
per task**, with a pipeline of: oracle solution must pass, *dummy agents must
fail*, LLM-backed checks on test coverage and specification clarity,
adversarial exploit agents hunting for cheats, contributor checklists, and a
post-merge audit by a second reviewer.

## The closest neighbour: infrastructure-as-code

Our shape — generate declarative configuration, judge it by executing against
real infrastructure — is IaC evaluation, not code generation, and that
literature is the most directly transferable.

The **verifier-first** design runs three sequential gates (`terraform validate`
→ `terraform plan` → `opa eval`) and classifies failures per stage
(`VALIDATE_FAIL`, `PLAN_FAIL`, `OPA_FAIL`) rather than collapsing them into
pass/fail. Two findings matter for us:

- **Failure promotion.** As one stage is fixed, failures move to the next.
  Qwen 7B's validation failures fell 144 → 43 while its policy failures *grew*.
  An aggregate score hides this entirely; per-gate reporting is what makes an
  intervention legible.
- **The feedback loop is worth 14–17 points.** With verifier-guided retries at
  k=4: Qwen 7B 45.7% → 62.9%, GPT-4o 70.4% → 84.4%. And it converges fast —
  83% of tasks resolve in one or two attempts, 16% never do. Repair fixes
  validation and planning failures; it cannot fix missing knowledge.

The scale of difficulty is also worth noting: GPT-4 scores 86.6% on Python but
**19.36%** on IaC-Eval Terraform. Declarative generation is a much harder task
than the code-generation numbers suggest, which is context for reading our own.

## DonatBench against ABC v1, item by item

Ours is a *state-modification / end-to-end testing* benchmark in ABC's taxonomy.

### Outcome validity

| Item | Us |
|---|---|
| I.d.1 test cases verified for correctness by human | **Weak.** One author, self-reviewed. The first model given task 002 found two factual errors in it. |
| I.d.2 test quality measured objectively | **Stronger than asked.** ABC wants coverage; we run mutation testing (200 seeded defects, 42% caught) and anti-oracles per task. |
| I.f.1 exercises all relevant parts | **Partial.** Graph coverage is reported; a passing 002 answer left 7 of 19 states unvisited. |
| I.f.2 prevents flaky results | **Yes.** `verify-oracles --stability K` fails a task whose verdicts vary. |
| I.g.1 ground truth includes all achievable states | Partial — scenarios assert outcomes, not a state enumeration. |
| I.g.2 **checks relevant *and irrelevant* states** | **Was missing; now the PASS_TO_PASS suite.** This is ABC asking, in its own words, for exactly what SWE-bench's second test set does. |
| I.g.3 ground truth complex enough to prevent trivial modification | Yes — durable processes with provider interaction. |

### Task validity

| Item | Us |
|---|---|
| II.1 tool versions specified | Yes — pinned toolchain, engine built from the tree. |
| II.2/II.3 tools accessible, errors handled | Yes — mock providers per stand; failures land as gates. |
| II.4 residual state cleared between runs | Yes — a fresh database per candidate. |
| II.5 **agent isolated from ground-truth information** | **No. Our worst item.** The oracle is the checked-in Petshop flow on the same filesystem, and the agent runs with `--dangerously-skip-permissions`. |
| II.6 setup does not drift | Yes — local fixture, no live service. |
| II.7 ground truth verified correct | Yes — the oracle is the shipped flow, exercised for months. |
| II.8 each task verified solvable | Yes — the oracle must pass. |
| II.9 oracle solver included | Yes. |
| II.10 no exploitable path to passing | Partial — conduct catches *writing* outside the writable set; reading the oracle is the open hole (II.5). |

### Reporting

| Item | Us |
|---|---|
| III.1/III.2 open source, open harness | Yes. |
| III.3 **anti-contamination, e.g. a private held-out set** | **No.** Canary strings only; `examples/petshop` is public. |
| III.4 plans to refresh tasks | No. |
| III.5/III.6 states what is measured, and the subject | Partial / yes. |
| III.7/III.8 flaw prevention described, unavoidable flaws discussed | Yes — [[decisions/001-what-a-benchmark-must-prove-about-itself]] is literally this document. |
| III.9 **quantitative analysis of unavoidable flaws** | **No.** The leak is discussed and never measured. |
| III.10 statistical significance, confidence intervals | Added — Wilson intervals and `pass^k` beside `pass@1`. |
| III.11 guidance for interpreting flawed results | Partial. |
| III.12 **non-AI baseline, e.g. human experts** | **No.** We cannot say whether 0.667 is good. |
| III.13 trivial-agent result | **Yes, and enforced.** The null candidate is a gate: a task a do-nothing agent passes is rejected by `verify-oracles`. |

## What this changes

Ranked by what it buys, not by effort.

1. **Isolate the workspace (II.5, II.10).** A container with the task workspace
   mounted and nothing else. Until then every number rests on the agent not
   having looked — which is a claim about goodwill, not about the harness.
2. **Measure the leak instead of discussing it (III.9).** We keep the agent's
   transcript. Scanning it for reads of `examples/petshop/metadata` turns an
   unavoidable-flaw paragraph into a number, and it is the cheapest item here.
   This is the transcript-scanner idea applied to our own record.
3. **A second mode, with the tool loop.** The IaC result — +14 to +17 points
   from verifier feedback at k=4 — says the single-shot number and the
   with-feedback number are different measurements, and the second is the one
   that matches how the plugin is actually used. Report both; the delta says
   whether to invest in the format's documentation or in `validate`'s
   diagnostics.
4. **A human baseline on one task (III.12).** One competent engineer, one task,
   timed. Without it "two of three" has no scale.
5. **A private held-out task (III.3).** Authored outside this repository, never
   committed. The moment the corpus is useful, it is training data.
6. **Second-author review of prompts (I.d.1).** Terminal-Bench spends three
   hours of reviewer attention per task and still rejected 61% of submissions.
   We spend one author's attention and had errors found by the first model that
   read the brief.

## Sources

- Zhu et al., *Establishing Best Practices for Building Rigorous Agentic
  Benchmarks* (ABC v1), arXiv:2507.02825
- METR, *Many SWE-bench-Passing PRs Would Not Be Merged into Main* (2026-03-10)
- *UTBoost: Rigorous Evaluation of Coding Agents on SWE-Bench*, arXiv:2506.09289
- OpenAI, *Introducing SWE-bench Verified*
- Yao et al., *τ-bench*, arXiv:2406.12045; `sierra-research/tau2-bench`
- *Automated Transcript Analysis for Detecting Flaws in Agentic Benchmarks*,
  arXiv:2607.27518
- *Terminal-Bench: Benchmarking Agents on Hard, Realistic Tasks in Command Line
  Interfaces*, arXiv:2601.11868
- *Verifier-First Evaluation of Agentic LLMs for Infrastructure-as-Code
  Generation*, arXiv:2607.20478
