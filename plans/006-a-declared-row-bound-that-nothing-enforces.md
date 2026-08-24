# A declared row bound that nothing enforces

**Effort: S–M. Status: TODO — found while building `modules/notifications`.**

## What is wrong

`select_many` and `update_many` accept `maximum_rows` / `maximum_items`. The
metadata type carries them (`crates/metadata/src/types.rs:847-859`), the command
compiler range-checks them (`crates/schema/src/commands.rs:4892`), and then they
are dropped: the IR's `SelectMany` step has no such field, and the only
`maximum_rows` gate sqlgen emits is for `ProjectMany` and `FixedRows`
(`crates/sqlgen/src/lib.rs:1905-1934`). A `select_many` declaring
`maximum_rows: 64` reads every matching row, and `donat validate` says the
metadata is consistent.

This is [[declarative-saas/decisions/034-a-declaration-the-runtime-ignores-is-a-defect]]
exactly: a declaration the runtime ignores. It is worse than an ordinary one
because of what the bound is *for* — "bounded by default" is one of the
invariants in `PLAN.md`, and this is the one place a command is asked to state
its bound and is not held to it.

## Why it bites harder than it looks

A bounded fan-out is the natural consumer of a bounded read, and the two fail
differently. `for_each` declares `max_items` (1..=256), and an input longer than
that does not trim — it fails the whole instance with
`fanout_max_items_exceeded` (`crates/server/src/processes/transition.rs:1934`).

So `select_many` + `for_each` is a trap: the read is unbounded in fact, the
fan-out is fatal on overflow, and there is no way in the grammar to take "the
first N" of anything. A Process built that way works until the day the table
holds one row more than the fan-out's ceiling, and then it fails identically on
every retry, forever, having claimed nothing.

`modules/notifications` was built that way first and had to be rebuilt: its
digest sweep now takes one recipient group per run and enumerates nothing, with
the scheduler paging `notification.pending_digest` through a `select_permission`
that carries a `limit` — a bound that *is* enforced. That is a good shape and
the module keeps it, but it was arrived at by hitting this, not by choosing it.

## What closing it needs

Two independent pieces, and the first is small:

1. **Enforce the bound.** Carry `maximum_rows` into `IrCommandStep::SelectMany`
   and emit the same `command_business_gate_cte` that `ProjectMany` already
   gets. A step that reads more than it declared is then a business rejection
   with the step's name in it, which is what an operator can act on. Same for
   `update_many`'s `maximum_items` where the source is a step rather than an
   argument list.

2. **Decide what a bound means.** A gate makes overflow a *refusal*. For a
   sweep, a refusal is only marginally better than a wedge — the work still
   never drains. The useful primitive is a bounded read that *truncates* under a
   declared order, which the grammar cannot say today: `select_many` has
   `order_by`, so `limit` is well defined, and a `limit: 64` that returns the
   first 64 in that order would make "drain a backlog a page at a time"
   expressible inside a Process for the first time.

Doing (1) without (2) is still worth it — it converts a silent lie into a loud
one — but (2) is what a caller actually wants, and the two should be designed
together so that `maximum_rows` (a ceiling that refuses) and `limit` (a page
that truncates) are visibly different declarations rather than one field whose
meaning changed.

## Blast radius

Any deployment relying on a declared `maximum_rows` today is relying on nothing,
and turning the gate on will start refusing commands that have been quietly
reading more than they said. That is the correct outcome and it is a behaviour
change: it wants a conformance case per step kind, and a note in the release.

## How to reproduce

Declare a `select_many` with `maximum_rows: 1` over a table holding two matching
rows and read the step's result: both rows come back and no gate fires. The
absence is structural rather than conditional — `CommandExecutionStep::SelectMany`
(`crates/ir/src/lib.rs:563-573`) has no `maximum_rows` field for a bound to
travel in, so there is no path by which one could be enforced.

