---
type: decision
status: accepted
date: 2026-08-10
features:
  - "[[evals]]"
---

# Tuning is a paired question, and the corpus must answer it separately

## Context

The corpus was built to answer "can an agent build applications on donat". The
day-to-day question turned out to be a different one: **"did that edit to a
skill help?"** This repository ships a plugin of twenty skills, and those are
the artifact being iterated on.

The two questions want opposite things from a benchmark.

An absolute score has to be stable, held out, and large enough to carry a
believable interval. Ours cannot be any of those yet: two tasks, both derived
from Petshop — one cluster, not two samples — and at k=3 the interval on 2 of 3
is `[0.21, 0.94]`. Reporting a change from 0.667 to 1.0 on that evidence is
reporting noise.

A tuning signal wants the opposite: sensitivity, cheapness, and enough detail to
say *what to write next*. It does not need to know how good the agent is in
absolute terms — it needs to know whether this version beats the last one.

Three facts forced the design.

1. **The first measurement was a bare-model baseline and nobody noticed.** The
   six attempts ran in workspaces under `~/.cache`, outside the repository, so
   the plugin was never loaded. The number everyone read as "donat's score" is
   the score *without* the skills. That is a useful baseline and an accidental
   one, and an accident that changes the meaning of a number is a design defect.
2. **Task-level verdicts cannot see a skill edit.** At k=3 the rate moves in
   thirds. An edit that takes an attempt from six of ten scenarios to nine of
   ten shows up as nothing at all.
3. **Tuning against the tasks you report from destroys the report.** With two
   tasks, both about ambiguous provider answers, the skills can be overfitted to
   them in a couple of iterations.

## Decision

**Skills are a variable of the run, recorded by content hash.** `--skills`
installs a skill set into the workspace; the result carries its hash, file count
and name. A run that inherits whatever the developer has installed is neither
reproducible nor comparable to the run before it. The hash is of the content,
not a version string, because a version people forget to bump is exactly how two
different skill sets come to share a name. No skills is a real arm — the
baseline — not an absence.

**Comparisons are paired by (task, attempt, scenario).** Task difficulty and
scenario difficulty are common to both arms and cancel; what remains is the arm.
This is why a corpus of two tasks can still answer a tuning question when it
cannot answer an absolute one. The statistic is the share of *discordant*
scenario verdicts that went the new way, with an exact Clopper-Pearson interval
— the counts are single digits, and a normal approximation on eleven
observations is a decoration rather than an interval.

The honest limit is stated in the tool: the attempts are not the same work done
twice. Each is a fresh session with its own randomness, so this is a comparison
of arms on matched work, not a repeated measurement.

**The scenario rate is the tuning metric; `pass@1` stays the headline.** Three
attempts carry three task verdicts and thirty scenario verdicts — ten times the
resolution. It is reported beside `pass@1` and never instead of it: an answer
that satisfies most branches and abandons the rest is not most of a working
store, and a corpus that rewards partial credit will get partial applications.

**A task's usefulness for tuning is its baseline reading, and it must be
measured.** Task 002 scores 3 of 3 bare; no edit can show a gain on it. Tasks
are calibrated by running the bare arm first, and one that saturates is kept as
a regression guard rather than counted as tuning capacity.

**The corpus is split before tuning starts, not after.** Tasks declare
`split: dev|holdout`. `agent.py` refuses a holdout task without an explicit
flag. Dev is where skills are tuned and where paired numbers come from; holdout
is where the absolute number comes from, read rarely and never during an
iteration.

**The output is a list, not only a number.** `compare` prints which scenarios
the new arm broke and which fail in *both* arms. The second list is the answer
to "what goes in the skill next", and no score can produce it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Absolute scores only, more attempts | To get ±0.1 around 0.7 needs ~80–100 independent attempts: 15–20 tasks × k=5. That is two weeks of task authoring, and it answers the wrong question during a tuning loop anyway. |
| Reuse the existing bare-model attempts as the baseline arm | They predate the reread and the regression check. Mixing harness versions inside a paired comparison is the subtle invalidity this whole corpus exists to avoid; the arm is cheap to re-run. |
| Compare on task-level pass only | Invisible to the changes being made. Three bits per arm cannot resolve a skill edit. |
| Score partial credit as the headline | Rewards satisfying one branch and abandoning the rest. Kept as a diagnostic. |
| One shared task pool, tune and report from it | Goodhart. The number becomes a report on how well the skills were fitted to two tasks. |

## Consequences

A tuning iteration costs two arms rather than one — on task 001 at k=3 that is
six attempts, roughly `$14` and two hours, most of it the regression suite. That
is affordable per skill edit and not per commit, which matches how it should be
used.

The holdout split is currently **empty**. Until a task is authored there, this
corpus can answer "better than before" and cannot answer "how good" — and the
harness now says so rather than letting a dev-split number stand in.

Paired reading also makes one known flaw harmless: the oracle is readable on the
same filesystem, so absolute scores rest on the agent not having looked. In a
paired comparison the leak is present in both arms and cancels. It still has to
be closed before any absolute number leaves the team; it no longer blocks the
tuning loop.
