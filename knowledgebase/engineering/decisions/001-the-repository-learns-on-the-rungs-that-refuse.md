---
type: decision
status: accepted
date: 2026-08-23
features:
  - "[[engineering]]"
---

# The repository learns on the rungs that refuse

## Context

The question that started this was whether to adopt "loop engineering" — the
discipline, coined in June 2026, of designing systems that prompt coding
agents on a schedule instead of prompting them by hand — and its July
successor "graph engineering". The reference material was read at the source:
the `cobusgreyling/loop-engineering` repository (docs, patterns, `gate.yaml`,
the `tools/` monorepo), the essays by Osmani and Greyling, Orosz's survey of
practitioners, the `codejunkie99/graph-engineering` skill, and the one
controlled study in the field (Google DeepMind × MIT, "Towards a Science of
Scaling Agent Systems", 180 configurations).

The goal behind the question was narrower than the question: **agents should
write better code in this repository over time.** Neither discipline is about
that. Loop engineering is about *when* an agent runs (schedule, durable state,
a maker/checker split); graph engineering is about *what depends on what*
(fan-out, a verifier in a separate context, one owner of the merge, a human
gate on the irreversible edge). Both are worth knowing, and both are silent on
improvement, because the thing they orchestrate does not improve: the model is
the same every morning. What can get better is what the repository tells the
agent before it acts and what the repository refuses to accept afterwards.

Those two surfaces were audited. The first is large — `CLAUDE.md`, 25 plugin
skills, 126 ADRs. The second is strong where it is mechanical (the conformance
fixtures as ground truth, `clippy -D warnings`, `cargo fmt --check`, insta,
`cargo audit --deny warnings`) and absent where it matters most:

- The BLOCKING RULES — no admin role, fixtures are ground truth, snapshots are
  reviewed and never blind-accepted — existed only as prose. An agent
  optimising for a green build has five short cuts that prose does not stop:
  `cargo insta accept`, editing the fixture, raising a `sleep`, `#[ignore]`,
  deleting the test.
- `cargo audit` ran only on push, so an advisory published on a Tuesday made
  main red until somebody pushed on Friday. `.cargo/audit.toml` excuses nine
  advisories on the written condition that a removable excuse is removed, and
  nothing checked the condition.
- The conformance harness waits on the clock in three suites (`sleep` of
  50–1200 ms in `event_triggers`, `file_attachments`, `petshop_process`),
  which is the usual source of a flake — and the usual target of an agent
  "fixing" one.
- Plugin skills were edited without measurement. An instrument for exactly this
  question — DonatBench's paired `compare` — is proposed separately (PR #33);
  the skills had been touched in two commits of `main`'s history with nothing
  measuring whether the edits helped.

The loop-engineering toolkit was also tried rather than only read:
`loop init --with-foundry` scaffolded seven root files, three generic skills,
a verifier agent, and a nested git worktree for a mock run; `loop audit` then
scored the result **100/100, "L3 — good candidate for unattended execution"**.
The score counted the presence of the toolkit's own files. It gave nothing for
the fixtures, the ADRs or the gates, credited a constraints file naming paths
this repository does not have (`auth/`, `payments/`) and omitting the ones it
does (`fixtures/`, `snapshots/`), and counted a mock session of thirty tokens
and no tool calls as "real usage signals". It measures conformance to the
method, not the property we care about. Its own output said as much: *"RUN a
loop and commit the updated STATE.md. This creates the loopActivity evidence
that pushes you toward real L2/L3 scores."*

## Decision

**A lesson lands on the lowest rung that can hold it, and the pull request
says which.** The rungs, from weakest to strongest: a sentence in chat; a
memory or an ADR; a skill or `CLAUDE.md`; a reviewer's checklist; a test,
fixture, lint or CI gate. Each material review finding moves one rung down
from where it currently lives. This is now part of the feature-completion
review in `CLAUDE.md`, and it is the only mechanism by which "agents write
better code" means anything here.

**The rules that can be read from a diff are read from the diff.**
`scripts/check_change_gate.py` (`make gate`, CI job `change-gate`) has two
kinds of finding. *Hard* findings fail and cannot be excused: a committed
`.snap.new`, or a retired name — `ADMIN_ROLE`, `DONAT_GRAPHQL_ADMIN_SECRET`,
`X-Donat-Admin-Secret` — reappearing in engine sources, or `run_sql` in
`crates/server/src`. *Excusable* findings are changes that are sometimes right
and always worth a sentence — an existing fixture or snapshot rewritten, the
toolchain bumped, an advisory excused, a `sleep` added to a conformance test,
a test ignored, a net loss of `#[test]`s, a plugin skill edited — and each kind
is excused by one line in the pull request description, `gate:<kind>
<reason>`. A *new* fixture or snapshot is free, because that is how the TDD
loop starts; rewriting an existing one is what needs a reason. The gate checks
that the change was named; the reviewer reads the reason. It is gameable by
design, and that is the point: a short cut taken in the open is a review
comment, a short cut taken silently is a regression.

**What changes on a clock is checked on a clock.** `.github/workflows/advisories.yml`
runs the same `cargo audit --deny warnings` nightly, plus
`scripts/check_audit_excuses.py`, which runs the audit without the config and
reports every excused advisory the unconfigured run did not raise. It opens
one issue and fixes nothing; a bump or an edit to the excuse list goes through
a pull request, where the gate asks for `gate:audit-ignore`.

**A red build is classified before it is touched** — flake, regression or
infrastructure — with three attempts on one failure and then a written stop.
This is the one part of the loop-engineering failure catalogue taken over
nearly verbatim, because it describes what an agent does to a timing-dependent
suite, and ours has three.

**A skill edit names a measurement that it helps or says why it needs none.**
The gate's `skills` kind makes an unmeasured skill edit say so in the one place
it is reviewed. A corpus to run the measurement against — DonatBench, proposed
separately — is not a dependency of this decision: until one is on `main`, the
honest answer is "no corpus yet", and the gate accepts it.

**Unattended work is a worktree and a pull request, nothing more.** A nightly
loop on the maintainer's own machine (`make setup-loop-infrastructure`;
`scripts/loop.sh` runs a job named by a skill under `.claude/skills/<job>/`)
cuts a fresh worktree from `origin/main`, runs one job headless under the
maintainer's own subscription, and opens a pull request — with a hard time and
turn limit, one job at a time behind a lock, the worktree removed whatever
happens, and one line per run in a journal outside the repository. A systemd
*user* timer rather than cron, because `Persistent=true` runs a night the
machine was off the next time it is on, which a desktop needs. The first job,
`fix-advisories`, resolves what `cargo audit` reports by upgrading a dependency
and never by excusing it — an excuse is a human's reachability argument, and
`make gate` refuses a pull request that adds one unasked. The loop's only way
to touch `main` is the same pull request a person's is: read, then merged, by a
person. This is the repository's one L2 surface, and it is L2 precisely because
the gate and the human review stand between the loop and `main`.

**Nothing from the toolkit is adopted.** No `STATE.md`, `LOOP.md`,
`loop-constraints.md`, budget or run log — `PLAN.md`, `specs/` and this
knowledge base already hold the project's state, and a second source of truth
is the toolkit's own failure mode number two. No readiness score. No generic
triage, verifier or constraints skills — a verifier that does not know the
engine has to be rebuilt before the conformance suite runs is weaker than
`make conformance`, and an agent told to trust it would be worse off.

## Alternatives

| Option | Why Not |
|--------|---------|
| Adopt the loop-engineering toolkit (`loop init`, `gate.yaml` + `loop-gate`, `STATE.md`) | `loop-gate` takes a list of paths and a denylist; it does not read the diff, cannot tell a new fixture from a rewritten one (so it would block the first step of the TDD loop), cannot grep for a retired name, and has no way to carry a reason. The state files duplicate `PLAN.md` and `specs/`. The score measures the toolkit's presence. |
| GitHub branch protection / `CODEOWNERS` on the guarded paths | Protects by path, not by status or content; the same "new is free, rewritten needs a reason" distinction is impossible, and an owner approval carries no written reason. |
| A scheduled "CI sweeper" agent (L2) | There is no inbound queue to sweep — 42 pull requests and 4 issues in the repository's history — and the failures worth sweeping are the timing flakes, which are exactly what an unattended agent fixes wrongly. A scheduled triage was tried as a cloud routine; it is report-only and harmless, and is expected to report nothing. |
| Dependabot | Would reopen the question of who reviews a lockfile bump every morning; the nightly audit reports the advisories that matter and leaves the bump to a reviewed pull request. May be revisited. |
| Run DonatBench's `compare` in CI on skill changes | Needs the Claude Code CLI, an API key and Docker stands on a runner, and costs money per pull request — a decision to make when `evals/` is on `main`. Until then the gate makes an unmeasured skill edit say so. |
| Leave the rules as prose | Prose is read by whoever reads it. Both blocking rules were already violated in spirit by the toolkit scaffold within an hour of it being installed, by an agent that had `CLAUDE.md` in context. |

## Consequences

What we get: the five short cuts to a green build now leave a named trace in
every pull request; the retired admin-role names cannot come back quietly; a
new advisory is known the morning after it is published rather than at the
next push; a stale excuse is reported instead of accumulating; the
feature-completion review has a rule for where a finding goes, not only that
it is addressed; and the measurement that exists for skills is asked for by
name.

What we pay: a pull request that rewrites a fixture or a snapshot owes a line
of explanation, and an agent that cannot write one has to stop and ask — that
is the intended cost. The gate's patterns are a list, and a list is maintained:
a new retired name, a new guarded path, goes into `check_change_gate.py` with a
case in its self-test. The marker is a name, not a proof; the reviewer still
reads. The nightly run costs a runner-minute and at most one open issue.

What was measured: on this branch's own diff against `main` (667 files) the
gate found two rewritten fixtures (the refusal moving from "missing role
header" to "unauthenticated request", [[api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]]),
ten edited skills and nine excused advisories, and nothing false, in 0.6 s.
`check_audit_excuses.py` against today's advisory database: nine excused, nine
raised without the config, no stale entry.

## Sources

- cobusgreyling/loop-engineering — <https://github.com/cobusgreyling/loop-engineering>
  (`docs/safety.md`, `docs/failure-modes.md`, `docs/anti-patterns.md`,
  `tools/loop-gate`, `tools/loop-audit`)
- Addy Osmani, "Loop Engineering" — <https://addyosmani.com/blog/loop-engineering/>
- Gergely Orosz, "What is loop engineering?" — <https://newsletter.pragmaticengineer.com/p/what-is-loop-engineering>
- Cobus Greyling, "Graph Engineering is an old idea reborn" — <https://cobusgreyling.substack.com/p/graph-engineering-is-an-old-idea>
- codejunkie99/graph-engineering, `references/task-graphs.md` — <https://github.com/codejunkie99/graph-engineering>
- Google DeepMind × MIT, "Towards a Science of Scaling Agent Systems" — <https://research.google/blog/towards-a-science-of-scaling-agent-systems-when-and-why-agent-systems-work/>
- Eigent, "Graph Engineering for AI Agents" (loops as nodes, anchors, counter-metrics) — <https://www.eigent.ai/blog/graph-engineering-ai-agents>
