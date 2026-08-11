---
type: findings
date: 2026-08-10
features:
  - "[[evals]]"
---

# What the compiler keeps saying, and what the skills did not

The first use of the corpus as a tuning instrument rather than a scoreboard.
Across every agent attempt on record, `validate` refused a candidate exactly
twice, and both refusals were the same two rules — neither of which the plugin's
twenty skills mentioned at all.

That is the whole method here: the gate failures are the engine telling you, in
its own words, which of its rules a reader of the skills does not learn. Not a
guess about what to document, and not a rule invented because it sounded
important.

## The two

**A state's output is readable only where it always happened.**

```
processes[2].states[13].command.arguments.payload:
  state output `reconcile_void.normalized_payload`
  is not available on every transition path
```

`{ state: s, field: f }` is legal only if *every* path reaching the current
state went through `s`. This is the rule branching flows hit, and donat is for
branching flows — a state reached both directly and through a reconciliation
detour has nothing from the detour on the direct path. The skills documented the
reference syntax in a table and said nothing about the graph condition on it.

**Nullability travels with the value.**

```
processes[0].states[3].request.input.request_id:
  nullable String is not assignable to String
```

Nullable sources are ordinary — an input declared without `!`, a nullable column
behind a command result, a response field a provider does not always send — and
none of the skills said what happens when one meets a required field, or that
`require_non_null` is the narrowing, or that it is an assertion re-checked at run
time rather than a coercion. Written the wrong way it moves a build failure into
production.

Both are now in `donat-processes`, stated as engine rules with the compiler's
own message quoted, and with the wrong ways out named: routing a path away from
a merge point to satisfy the reachability check, and sprinkling
`require_non_null` to silence the type check. A rule without its failure mode is
half a rule.

## Why these and not others

The temptation with a corpus of two tasks is to write documentation for the two
tasks. The guard used here was to take only what the *compiler* complained
about, and to state it at the level the compiler works at — the process value
graph — with no mention of payments, checkout, or anything else from the fixture
that produced the evidence.

The business failures were left alone deliberately. The bare model failed the
same two scenarios twice — proven absence, and investigating an ambiguous charge
— and the plugin arm failed neither. That gap is already covered; writing more
about it would be tuning against the one task that exposed it.

## The other direction: what the task was checking silently

The same method run backwards. Instead of asking what the engine refused, ask
what every scenario *asserts* and whether the prompt ever said it. Anything a
task punishes without stating is underspecification, and underspecification is
the single most common way benchmarks go wrong — 38.3% of SWE-bench samples were
flagged for it.

Task 001 had two. Task 002 had none.

**The amount.** Two scenarios assert the recorded payment amount equals the
order total, and in the reconciled case that it is the amount the *provider*
reports rather than the store's own arithmetic. The prompt listed the payment
statuses support can read and never mentioned an amount at all.

**The idempotency key.** `charged_once` asserts every authorize call carries a
non-null key and that all of them carry the same one — and it runs on the
**happy path**, where there is a single successful call. A store that authorizes
once, successfully, with no key has charged exactly once and broken no stated
rule, and it fails. That is the SWE-bench pathology exactly: a test that rejects
a functionally correct answer over a mechanism nobody asked for.

The fix in both cases was the prompt, not the test. The requirements are real —
the key is what makes at-most-once effects work at all — they were simply never
said out loud. Weakening the assertions would have thrown away a property worth
having; stating them removes a trap without touching the part of the task that
is actually hard.

Which raises its own hazard: **editing a prompt ends a paired comparison**, and
the labels go on looking comparable. Attempts now record a fingerprint of the
whole task directory, and `compare` warns when two arms answered different
questions. The task is a variable of the run exactly as the skill set is.

## What it cost, and what remains unproven

Two arms of three attempts, roughly `$14` and two hours, to find two rules. The
third arm then ran the same task against the edited skills:

| arm | pass@1 | pass^k | scenarios | outcomes |
|---|---|---|---|---|
| bare | 0.333 `[0.06, 0.79]` | 0 | 26/30 | 1 pass, 2 wrong |
| plugin | 0.667 `[0.21, 0.94]` | 0 | 20/20 | 2 pass, 1 unbuilt |
| plugin, edited | 1.0 `[0.44, 1.0]` | 1 | 30/30 | 3 pass |

`pass^k = 1` for the first time — every attempt passed, which is the reliability
reading rather than the average one.

The paired detail is more informative than the table. Against the unedited
plugin: one `unbuilt -> pass`, and **zero** scenario verdicts moved. Where both
arms built, both were already perfect; the entire difference is the gate. The
edit did what it was written for and nothing else, which is the outcome to
prefer over a broad unexplained improvement.

And the weakness, stated plainly: the nullability rule was written by reading
one failing attempt, and then confirmed by re-running that same task. The
failure occurred once in three; its absence in three more is weak evidence, not
proof. `[0.398, 1.0]` on four discordant scenarios says the same. The real test
is the other task, which the edits have never seen.

## The generalisation test, and why it could not answer

The edits were found on task 001, so the test that matters is task 002, which
they have never seen. Both arms ran it, k=3:

| arm | pass@1 | pass^k | scenarios |
|---|---|---|---|
| bare | 1.0 | 1 | 18/18 |
| plugin, edited | 1.0 | 1 | 18/18 |

**Identical, because task 002 is at the ceiling for the bare model.** Three of
three without any skills at all. A task the baseline already solves every time
has no room to show an improvement; it can only detect a regression, and it
detected none.

That is a finding about the corpus rather than about the skills. A tuning
instrument needs tasks where the baseline sits in the *middle* — 001, at one of
three bare, is such a task and it is currently the only one. Task difficulty has
to be calibrated against the baseline deliberately, the way Terminal-Bench
curated 229 submissions down to 89. Two tasks, one of them saturated, is one
usable tuning task.

Across both tasks paired, six matched attempts: two `wrong -> pass`, four
scenario verdicts moved, all of them to the edited arm, nothing lost anywhere —
no scenario regressed, no store regression among the guarded tests, nothing
unbuilt. The interval on four discordant verdicts is `[0.398, 1.0]`, which is
not separation.

## What it cost, and what remains unproven

The honest limit: the gains are all on the one task the edits were derived from,
and the one task that could have contradicted them had no room to. "These edits
help" remains a direction supported by every observation and proven by none.

Two rules is also a small return for four hours and roughly `$28`, and the
return will keep shrinking: the compiler only has so many things to say, and
what remains after it runs out are the failures no gate can catch.
