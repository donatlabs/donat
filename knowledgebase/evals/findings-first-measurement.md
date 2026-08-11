---
type: findings
date: 2026-08-10
features:
  - "[[evals]]"
---

# The first measurement: two tasks, three attempts each

The first numbers the corpus has produced against a model rather than against
itself. Claude Opus 5, `--dangerously-skip-permissions`, one prompt, no
scaffolding beyond the task's own `PROMPT.md`.

Superseded in part: a later paired run of the same task, same bare model, read
**1 of 3** where this one read 2 of 3. Nothing changed between them but the
sample. That is the width of a k=3 interval made visible, and it is the reason
[[decisions/002-tuning-is-a-paired-question]] exists — the numbers below are a
shape, and the paired reading is what can actually see a change.

Read with their date on them. These six attempts ran before the review that
followed them, so they predate the all-red reread, the environment sanitising
and one scenario repair in task 001 (its second killer used to depend on the
first one's reading). None of those changes moves a verdict here — the
environment leaks did not fire, the reread was reconstructed by hand, and the
repaired scenario failed under its anti-oracle before and after — but the
harness that produced these numbers is not byte-for-byte the one in the tree.

| task | pass@1 | scored | unbuilt | wrong | voided | $/attempt |
|---|---:|---:|---:|---:|---:|---:|
| 001 checkout-ambiguous-payment | 0.667 | 3 | 0 | 1 | 0 | 2.45 |
| 002 cancelling-an-authorized-order | 0.667 | 3 | 0 | 1 | 0 | 2.23 |

Two of three, on both. With k=3 that is a shape, not a rate — the interval is
wide enough to cover anything from a third to nearly always — and the corpus
says so itself rather than letting the reader assume otherwise.

What the shape does say: **nothing was unbuilt and nothing was voided.** Every
attempt produced metadata that composed, migrated, validated and deployed, and
no attempt reached outside the files the task opened. The failures were failures
of *behaviour*, which is the only kind this corpus is trying to measure.

## What the failures were

Both misses were in the same place: the ambiguous branch. On 001 the losing
attempt failed `test_a_proven_absence_takes_no_money` and
`test_an_ambiguous_charge_is_investigated_not_written_off` — it built the
retry ladder, the idempotency key and the reconciliation lookup correctly, then
routed a *proven terminal absence* into a human queue instead of releasing the
stock. On 002 the losing attempt failed the two timeout scenarios.

That is the interesting result, not the score. Every attempt got the happy path,
the retries and the idempotency right. What separates them is what they do with
an answer the provider did not give — which is exactly the boundary the tasks
were written to sit on.

## The graph tells a second story

Coverage is a report, not a gate, and it earned its place here. A passing 002
attempt reached 12 of 19 states; the unvisited seven are the entire
reconciliation half of its own flow — `reconcile_refused_void`,
`record_silent_void`, `void_outcome_ambiguous`. The answer passes every
scenario and a third of what it built has never run.

Neither the score nor the suite would ever say that. It is the difference
between "the tests pass" and "the thing works", and it is visible only because
the harness reads the engine's own state graph after the run.

## What the run cost

`$14.06` for six attempts, roughly 1.1M tokens each, five minutes of wall clock
per attempt plus about three for the stand. A k=3 reading of one task is about
`$7` and half an hour. That is affordable enough to run per-change and too
expensive to run per-commit, which matches how the corpus is meant to be used.

## What it cost the harness

One attempt read 10 red of 10 with all four gates green. Re-run on a fresh
stand, the same answer read 8 of 10 — the two real defects above. The score was
unharmed, because the attempt was wrong either way, but the finding is that a
stand which deploys and then does not serve is shaped exactly like a hopeless
answer, and nothing in the record distinguished them.

Now something does: an attempt where every scenario fails on green gates is read
a second time, and the second reading is the verdict
([[decisions/001-what-a-benchmark-must-prove-about-itself]]).

## What the model found in the task

Given task 002, the first attempt reported two errors in the brief before
solving it: the prompt listed the wrong inputs for the process it asks for, and
described a payment state the store cannot be in on the refusal path. Both were
real. The second also touched a scenario, which computed `money_still_held` and
then did not assert on it — a dead value that read like a claim and would have
been false had it been one.

Worth recording plainly: the task had been through `verify-oracles`, a stability
run and human review. The first model pointed at it found two things all three
had missed. The reference solution passing is evidence about the solution, not
about the prose — and the prose is what the model is actually given.
