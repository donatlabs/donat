# Evals — measuring whether an agent can build a donat application

> The fourth verification layer, next to skills (generation), `donat validate`
> (static correctness) and `crates/conformance` (protocol conformance). This
> domain holds the research and decisions behind DonatBench: how a task is
> specified, what may be scored, and what may only be gated.

**Status: research, August 2026.** The harness is implemented: `evals/run.py` (one candidate, one task), `evals/agent.py` (k attempts, conduct and cost), `evals/sweep.py` (the mutant corpus at volume), and two tasks under `evals/tasks/`. The one hard
dependency is machine-readable `donat validate` output, without which a run
cannot classify why an application failed.

**Nothing in this domain runs in CI.** Evals are driven from the Makefile, the
way the `perf` targets are: a pipeline never calls a model, so it can never be
slow, costly or nondeterministic because of an agent. The AI-free
`evals-verify-oracles` target may be promoted to CI once the corpus settles.

## Research

- [[research-how-to-build-an-evals-framework]] — what the published agentic
  benchmarks (Terminal-Bench, METR Task Standard, Inspect AI, SWE-bench
  Verified, τ-bench) agree on, how benchmarks fail in practice, and the
  resulting shape for DonatBench: four gates and one behavioural score, a task
  as a versioned directory with an oracle, and honest statistics for a corpus
  of tens rather than thousands of tasks.

- [[research-how-other-systems-verify-business-behaviour]] — how Camunda,
  Temporal, Step Functions, Jepsen/FoundationDB, BDD practice, mutation
  testing, Terraform/OPA and Stripe prove that a declared application behaves
  correctly, and the seven deltas that research puts into our plan (history
  verification per instance, graph coverage as a report, mocks keyed by
  connector operation, seeded replayable worlds, a crash-restart world, example
  maps against ambiguity, negative tasks).

- [[findings-first-task]] — what building task 001 by hand actually taught, run
  by run: the anti-oracles all died where they should, the suite was stronger
  than predicted, and the validator's index-based diagnostic paths showed up as
  a real obstacle rather than a predicted one.

- [[findings-mutant-sweep]] — two hundred seeded business defects run against
  the store's own suite: 44% caught, the exact boundary of what `validate`
  refuses versus what only behaviour can catch, and the three ways the first
  runs produced a confident wrong number before the guards existed.

- [[findings-the-first-holdout]] — the corpus's first held-out task and first
  absolute number: 3 of 3 bare on a permissions task, which alongside the other
  two says declarative permissions are easy for the model and ambiguity is not.
  Also the task-validity defect that cost it a point, and the calibration
  problem it confirms — two of three tasks sit at the ceiling.

- [[findings-what-the-compiler-keeps-saying]] — the corpus used as a tuning
  instrument for the first time: across every attempt on record `validate`
  refused exactly two things, both were rules the plugin's twenty skills never
  mentioned, and both are now written down at the level the compiler works at.

- [[research-how-agentic-benchmarks-are-graded]] — how the field grades agents
  now, and DonatBench audited line by line against the Agentic Benchmark
  Checklist: where we are stronger than asked (mutation testing, an enforced
  trivial-agent gate) and the four items we fail.

- [[findings-first-measurement]] — the corpus pointed at a model for the first
  time: 2 of 3 on both tasks, nothing unbuilt and nothing voided, both misses on
  the ambiguous branch, and a passing answer whose reconciliation half never
  ran.

## Decisions

- [[decisions/002-tuning-is-a-paired-question]] — the corpus has to answer
  "did that skill edit help?" as well as "how good is the agent", and the two
  want opposite things: skills recorded by content hash, comparisons paired by
  (task, attempt, scenario), the scenario rate as the sensitive reading, and a
  dev/holdout split declared before any tuning starts.

- [[decisions/001-what-a-benchmark-must-prove-about-itself]] — the five things
  a task or sweep must demonstrate about itself before its numbers mean
  anything, each enforced by the harness rather than remembered: a green
  control, harness failures kept out of the verdicts, no borrowed stands, two
  independent killers per anti-oracle, and an oracle that agrees with itself.
