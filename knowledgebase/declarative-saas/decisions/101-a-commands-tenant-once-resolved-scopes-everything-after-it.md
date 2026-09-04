---
type: decision
status: accepted
date: 2026-09-04
tags: [tenancy, commands, permissions]
features:
  - "[[declarative-saas]]"
  - "[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]"
---

# A command's tenant, once resolved, scopes everything after it

## Context

[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] gave a
command two ways to have a tenant that is not the caller's: `tenant:
establishes` for the command that creates one, and `tenant: { from: <step> }`
for the command that reads it off a row an unscoped `select_one` found. Every
write after that step takes its tenant column from the step's CTE as a preset,
and that part worked.

Three other things did not, and all three were refused for the same stated
reason — "the value lives in another CTE, which a row predicate cannot
reference":

- **A scoped read after the step was refused** at request time
  (`plan_mutation.rs`), because the only tenant the read predicate knew how to
  compare against was the session's, and the session's was the wrong one.
- **An update or a delete anywhere in such a command was refused at deploy**
  (`metadata/src/tenancy.rs`), because its row predicate could not be bounded.
- **The registry's serving gate was skipped** on every write whose tenant came
  from a step (`schema/src/tenancy.rs`), so a suspended tenant's invitation
  could still be accepted out of a token that never named the tenant at all.

The consequence was that a `from` command could read exactly one row and then
only insert. Issue #57 made that concrete: a service identity — a Slack
connector holding a `client_credentials` token — resolves its tenant from a
mapping table by workspace id and then needs the ticket, its comments and its
runs. With the refusal in place it can create rows and see nothing.

## Decision

**The stated reason was wrong, and the fix is to say so in the IR.** The step
a command takes its tenant from is single-row by construction — `from` is
admitted only on a `select_one` keyed on a unique constraint, and `establishes`
only on an `insert` that returns the row it wrote — so `(SELECT <column> FROM
<step cte> LIMIT 1)` is a well-defined scalar, and a row predicate can compare
against it exactly as it compares against a literal. It is the same expression
`CommandExecutionValue::StepColumn` already renders for the write preset; it
was never available to a predicate only because `CompareOp` had no arm for it.

So `CompareOp` gains `CompareStepColumn { cte, column }`, and the planner gains
one notion, `TenantRef`, with two arms: the session's tenant, or a step's
column. Every place that builds a tenant predicate — the read bound, the write
bound on an update or a delete, the traversal for a `scope_via` table, and the
registry's serving gate — takes a `TenantRef` and no longer knows or cares
which arm it holds. The command plane passes the step arm from the point the
tenant step has run; everything else passes the session arm, as before.

**What is scoped after the step, and what is refused before it.** From the
tenant step onward, a read of a tenanted table is bounded by the command's
tenant and gated by the registry, a write's check carries the gate, and an
update's or a delete's predicate carries the bound. One qualification, and it
is a fact about Postgres rather than a choice: for `establishes` the registry
row is written by this statement, in a data-modifying CTE the rest of the
statement cannot see, so the gate is not read there — the bound is, and the
row being written *is* the registry row, with whatever status the command
gave it. `from` reads a tenant that already exists and is gated in full. Before the tenant step
there is nothing to bound by, and that is refused at deploy rather than at
request time: a scoped read of a tenanted table, or any write, placed before
the step the tenant comes from stops the deployment naming the step and the
fix. A read of a shared table before the step is fine, as it always was — a
registration still looks up a plan before it inserts the tenant row. The tenant
step itself keeps its `tenant: unscoped` declaration and its audit.

**What does not change.** Admission is exactly what it was: `unscoped_steps:
audited` in `tenancy.yaml`, `unscoped` only on a `select_one` on a unique key,
and the command must declare `tenant: { from: <that step> }`. The escape stays
on the command, countable, and the unguessable lookup key stays the whole
authorization for the one unscoped read.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep the refusal; split the work into a `from` command and a session-scoped second command | The caller has no session tenant — that is what `from` is for. The second command would need `from` again, and "the ticket, its comments, its runs" becomes N commands for one answer. |
| Resolve the tenant in the engine, then plan a session-scoped statement with it | Two statements per operation, against the one-statement invariant, and the tenant would travel through Rust as a value the engine looked up for itself rather than a claim it verified. |
| Let the JWT configuration declare a lookup that turns a claim into a tenant variable | A tenant the engine derives from a request is a tenant header by another name; [[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] refused that shape and the reason still holds. |
| A role-level registry exemption (issue #57's original Feature 2) | Moves the escape from the command, where it is one countable line reviewed with its steps, to the role, where it is a bypass every command of that role inherits. |
| Join the tenant step's CTE into every later step instead of a scalar subquery | Changes the `FROM` shape of every step kind for one column. The scalar subquery is what the write preset already renders, and Postgres evaluates an uncorrelated scalar subquery once per statement. |
| Keep skipping the serving gate for step-sourced commands | The gate exists so a valid token cannot act on a suspended tenant. A command that resolved the tenant itself is the one most obviously able to look the registry up by it. |

## Consequences

A service identity that resolves its tenant in its first step can now read,
update and delete inside that tenant in the same statement, bounded by the
value it resolved rather than by nothing. The suspended-tenant hole is closed:
accepting an invitation into a store the registry stopped serving is refused,
by the same gate that refuses a member of that store.

What it costs:

- **One more arm in `CompareOp`**, rendered by sqlgen as `<column> = (SELECT
  <column> FROM <cte> LIMIT 1)`. It is produced only by the tenancy layer and
  never by a client filter.
- **A tenant step that found nothing scopes to NULL.** With `require_found:
  false` on the `from` step, the subquery yields NULL, every comparison against
  it is false, reads answer empty and writes are refused. That is the right
  answer — nothing was resolved, so nothing is in scope — and `require_found:
  true` is what a `from` step should declare anyway.
- **The order of steps is now a deploy-time fact.** A write or a scoped read
  before the tenant step used to fail at request time, or not at all; it now
  stops the deployment. The metadata validator names the step to move it after.
- **One conformance case flips.** `join_and_peek` asserted that a scoped read
  after the `from` step is refused; it now asserts that the read is bounded by
  the invitation's tenant and sees nothing of the caller's.
