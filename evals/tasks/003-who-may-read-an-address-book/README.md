# 003 — who may read an address book

## Why this task exists

Two reasons, and the first is measured.

The mutant sweep put three `open-row-filter` defects into
`public_customer_address.yaml` — the select, update and delete filters removed
in turn, so one shopper reads or edits another's addresses — and Petshop's own
suite noticed none of them. Across the corpus that operator survived 14 times
in 22. It is the largest unmeasured class in the store, and the only one whose
failure is a data-isolation breach rather than a business-logic mistake: a
shopper's home address, readable by strangers, through the ordinary API.

The second reason is the shape of the corpus. Tasks 001 and 002 are both about
ambiguous answers from a payment provider — one cluster measured twice. This
task has no process, no connector and no ambiguity in it at all. It is
declaration only, and it is the first task that varies in *kind*.

## Held out

`split: holdout`. This is the only place an absolute number can come from: dev
tasks are what skills and prompts are tuned against, so a score read from them
is a score tuned on. `agent.py` refuses this task without `--holdout`, and the
refusal explains itself. Nothing in a tuning iteration may read it.

## The example map

| Rule | Example | Expected |
|---|---|---|
| A shopper keeps a book | adds, corrects, removes an address | all three work on their own rows |
| Only the owner's rows | two shoppers, one list | no row belonging to the other |
| Only the owner's rows | the stranger's row, asked for by id | not found |
| The owner is not chosen by the caller | insert claiming another customer's id | refused, or filed under the caller |
| Reads and writes are one boundary | rename a stranger's row | unchanged |
| Reads and writes are one boundary | delete a stranger's row | still there |
| Support looks | any shopper's address | visible |
| A visitor | anything | nothing |

Two ambiguities were resolved into the prompt while writing this:

1. What should happen when a shopper *claims* another customer's id on insert —
   refusal and silent reassignment are both defensible, so the prompt names the
   one thing that must not happen instead of demanding a mechanism.
2. Whether support may also write. The prompt points at what the store already
   does for comparable tables rather than inventing a rule, because a task
   should not require a candidate to guess a policy nobody stated.

## The anti-oracles

Both deploy and pass `donat validate`, and both leave everything a single
shopper can observe exactly right.

| Anti-oracle | The defect | Dies on |
|---|---|---|
| `every-shopper-sees-every-address` | the read filter is opened | the owner's list, and a lookup by id |
| `writes-reach-any-row` | the update and delete guards are opened | a rename, and a delete |

One candidate anti-oracle was **rejected**, and it is worth remembering why.
Taking the owner from the request — dropping the `set` that forces
`customer_id` to the caller — looked like the obvious second defect. It fails
*every* scenario including the happy path, because the column is not nullable
and an ordinary insert stops working. An anti-oracle that breaks the store is
not a plausible-but-wrong application; it measures nothing that the gates do
not already catch.

A second attempt on the write side also had to be corrected: opening only the
update `filter` left the store still refusing the edit, because the update
`check` names the owner column and so doubles as a row guard. Both had to be
opened for the defect to be real. That is a detail about donat worth having in
writing, and it came out of the task refusing to discriminate.

## What the baseline showed

The fixture — the store with this table not exposed at all — passes **239** of
Petshop's own black-box tests, and the oracle passes the same 239. The store's
suite has nothing to say about the address book either way, which is exactly
why three seeded defects survived in it, and it means every one of those 239 is
guarded for this task: `pass_to_pass` is 239 and the task's own subject is
empty.

That split is computed from the fixture and the oracle test by test, the way
SWE-bench computes FAIL_TO_PASS, which this task is the first to make possible
— unlike 001 and 002, removing this piece leaves a store that still deploys.
