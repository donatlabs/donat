---
type: findings
date: 2026-08-11
features:
  - "[[evals]]"
---

# The first held-out task, and the first absolute number

Task 003 asks a candidate to expose a table nobody can reach: the shopper's
delivery address book, present in the database and absent from the API. Declare
who may read it, who may change it, and whose rows are whose.

It is the corpus's first `split: holdout` task — the only place an absolute
score can come from, because dev tasks are what skills and prompts are tuned
against and a number read from those is a number tuned on.

## Why an address book

The mutant sweep put three `open-row-filter` defects into this one file — the
select, update and delete guards removed in turn — and Petshop's own suite
noticed **none** of them. Across the corpus that operator survived 14 times in
22. It is the largest unmeasured class in the store and the only one whose
failure is a data-isolation breach rather than a business-logic mistake.

It is also the first task that differs in *kind*. Tasks 001 and 002 are both
ambiguous-payment-provider tasks — one cluster, measured twice. This one has no
process, no connector and no ambiguity: it is declaration only.

## The number

Bare model, no skills, k=3:

```
pass@1     1.0  ci95 [0.438, 1.0]     pass^k 1
scenarios  27/27 = 1.0  ci95 [0.875, 1.0]
regression 0 of 239 guarded tests broken
```

Three of three, every scenario, on a task the model had never seen and whose
oracle it could not have read for wording it did not have. Read alongside the
other two, that is the useful shape: **declarative permissions are easy for the
model and durable-process reasoning under ambiguity is not.** The only task in
the corpus where the bare model reliably fails is the one about what to do when
a payment provider will not say what happened.

## What it cost the task to be fair

The first reading was 2 of 3, and the missing attempt failed `validate` on an
invented column name — `address_line_1` where the store has `line1`. The
migrations are not in the workspace and no other metadata names those columns,
so the task was, in part, a guess about a naming convention. Two attempts
guessed right and one did not.

That is a task-validity defect of exactly the kind ABC names: solvable if and
only if the agent has the target capability, and guessing a column name is not
the capability being measured. The prompt now lists the columns, says why, and
says that guessing is not what is being asked. With that fixed the reading is
3 of 3 — the whole of the difference was the guess.

Worth stating plainly: the number moved from 0.667 to 1.0 because the *task*
got fairer, not because anything got better. A corpus that does not record
which of the two happened will eventually report one as the other.

## And the calibration problem it confirms

Three tasks now, and the bare model scores 3 of 3 on two of them. A task at the
ceiling can detect a regression and can contribute to an absolute score; it
cannot show that an edit to a skill helped, because there is no room above it.

| task | split | bare pass@1 | use |
|---|---|---|---|
| 001 checkout-ambiguous-payment | dev | 0.333 | tuning |
| 002 cancelling-an-authorized-order | dev | 1.0 | regression guard |
| 003 who-may-read-an-address-book | holdout | 1.0 | absolute reading |

One usable tuning task. New tasks should be run bare *before* they are trusted
as tuning capacity, and kept when they land in the middle — the discipline
Terminal-Bench applied when it curated 229 submissions down to 89.

## Two things the task taught about donat

Neither was known before the task refused to discriminate.

**An update `check` that names the owner column is also a row guard.** The
first version of the `writes-reach-any-row` anti-oracle opened only the update
`filter`, and the store went on refusing the edit: the check applies to the
resulting row, which still belongs to the victim. Both have to be opened for
the defect to be real — which is what a careless author does anyway.

**Forcing the owner with `set` is load-bearing in a way a `check` is not.** An
anti-oracle that dropped it failed *every* scenario including the happy path,
because the column is not nullable and an ordinary insert stops working. It was
rejected: an anti-oracle that breaks the store measures nothing the gates do
not already catch.
