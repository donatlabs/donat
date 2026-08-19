---
type: decision
status: accepted
date: 2026-08-19
tags: [tenancy, iam, permissions, quotas]
features:
  - "[[declarative-saas]]"
---

# A compiled role is the shape, a grant is the scope

## Context

[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] decides
which rows exist for a caller. It leaves open which of the operations on those
rows that caller may perform — and for a platform, that question has an answer
the platform cannot write down, because the answer differs per tenant and
changes at runtime.

A tenant wants roles of its own: an inventory clerk, a weekend supporter,
whatever the business invented last month. It cannot have *compiled* roles,
because a compiled role decides which tables and columns exist at all, and one
schema per tenant is a schema render per tenant.

The state of the art is to build this by hand: a grants table, and an
`_exists` against it repeated in the `filter` and the `check` of every
permission of every table. It works, and it stops scaling at around a hundred
and sixty tables — not because the SQL is slow but because the predicate is
written as many times as there are permissions, and a missing one is invisible.

## Decision

**Two layers, and keeping them apart is the design.**

| | Compiled role | Grant |
|---|---|---|
| Decides | the **shape** — which tables, columns and operations exist | the **scope** — which actions the caller holds |
| Lives in | metadata, git, deploy-time | rows in the tenant's own tables |
| Changed by | the platform, by deploying | the tenant, at runtime |
| How many | a handful | unbounded, different per tenant |

`iam.yaml` names the flattened grant relation — one row per (tenant, subject,
action), with any role hierarchy expanded by the database so the predicate
never walks one — and which compiled roles are served through it. A storefront
shopper is deliberately not among them: it is tenant-scoped and holds no
grants, and forcing it through the relation would deny every request it makes.
The list is explicit rather than inferred, because "this role is not governed"
is a decision and an inferred one is a decision nobody made.

A table operation names the action it needs by template — `{table}:read` by
default, so a table is governed the moment it is tracked — with overrides that
group several tables under one business resource, because an order and its
lines are one thing a merchant grants access to rather than two.

**Wildcards are expanded, not matched.** `product:*` becomes a member of the
short list of action strings the predicate compares for equality, which an
index answers. Nothing a tenant wrote is ever executed as a pattern.

**Where the gate lands differs by operation, and the difference is written
down rather than discovered:**

| Operation | Where | A caller without the action sees |
|---|---|---|
| select | the row predicate | no rows |
| insert | the check | a refusal |
| update | the check | a refusal |
| delete | the check | a refusal |
| command | a statement-level gate that raises | a refusal naming the command |

Every write says no; only a read goes quiet, and that is not a compromise — it
is what a role that cannot see a table already gets.

Getting there took one change to the IR. A delete had no `check`, because the
Donat permission format has none for it: there is no row afterwards to check.
But a *gate* is not a check on the row — it is row-independent, and evaluating
it over the rows a delete removed is exactly right. So `DeleteMutation` grew
one, carrying only what the engine adds. The alternative was to gate deletes in
the predicate, and "your delete matched no rows" is not an answer to "may I
delete this".

The same distinction settled where every other gate goes. A bound belongs in
the predicate — it decides which rows are yours. A gate belongs in the check —
it decides whether you may act at all. Put a gate in an update's predicate and
a suspended tenant is told it changed nothing, which reads as "there was
nothing to do"; put it in the check and it is told the store is suspended.

**A command is gated once, as a whole.** `cancel_order` requires
`order:cancel`, which is deliberately not `orders:update`: a role may be
allowed to read and edit orders and still not to cancel one. Gating each step
instead would report a command that ran and changed nothing. Inside a command
the *table* actions are not applied, because the command plane is a separate,
narrower set of permissions ([[019-command-only-table-permissions]]) and
requiring both would mean two grants for one operation, one of which nobody
asked for.

**The sharpest edge is IAM administering itself.** A role able to grant actions
can grant itself anything. `reserved_actions` bars the actions that belong to
the platform, and it is enforced as an ordinary check on the table the tenant
writes — named by `grants.written_via` — rather than as a rule the command that
writes it is trusted to remember. Declaring reservations without naming that
table is refused at load: a reservation with nowhere to be enforced is a
promise with no gate behind it.

**Quotas are the same move again.** A platform must cap what a tenant holds and
may not edit the domain's insert permission to do it, so `quotas.yaml` ANDs a
ceiling in exactly as tenancy ANDs a predicate. The counter moves *inside* the
statement that performs the write: `UPDATE usage SET n = n + (SELECT count(*)
FROM <the write>)`, then a check against the plan's maximum. Counting first and
writing second is the version everybody writes and the one that does not hold —
under READ COMMITTED a statement's snapshot is fixed before it executes, so
concurrent writers all read the same pre-lock count and all pass. Locking the
usage row makes the second writer wait, re-read, and be refused. A ceiling that
no entitlement consumes is refused at load, because a limit with no writer to
gate is fiction and fiction in a limits table is how a plan quietly stops
meaning anything.

## Alternatives

| Option | Why Not |
|--------|---------|
| Let each deployment hand-roll `_exists` grants | The predicate is written once per permission and a missing one is invisible. This is the thing the layer exists to remove. |
| A compiled role per tenant role | A compiled role is a rendered schema. Unbounded tenant-invented roles would mean unbounded schema renders. |
| Grants inlined into the token | Faster still, and revocation then lags until the token expires. A tenant removing somebody's access expects it to take effect. |
| `LIKE` matching on the action column | Turns a tenant-written string into an executed pattern and defeats the index. Expanding wildcards into an equality list does neither. |
| Gate a command by filtering each step | Reports a command that ran and changed nothing, which is the wrong answer to "may I". |
| Enforce reserved actions in the command that writes grants | Correct only for as long as that is the only writer. The database is the only place that is true of. |
| `COUNT(*)` before the write for quotas | Every concurrent writer reads the same pre-lock snapshot and passes. This is the race the design exists to close. |

## Consequences

A tenant invents roles and grants without a deploy, and the engine derives the
predicate rather than a person writing one per table. Adding a table makes it
governed. Adding an action to a plan is a row.

What it costs:

- **A governed role needs a subject.** The grant relation is keyed by (tenant,
  subject, action), so a request with no `x-donat-user-id` is refused for a
  governed role — the same shape as a request with no tenant.
- **The grant relation is read on every request.** It is one uncorrelated
  `EXISTS` over an indexed table, which Postgres evaluates once per statement.
  Resolving it as a single request CTE instead is an optimisation this leaves
  open, not a semantic difference.
- **A counted table is not a command's to write.** The counter moves inside
  the statement an *ordinary* insert or delete performs; a command's steps
  carry none. A row written through a command would therefore cross the
  ceiling without moving it, so a command write permission on a counted table
  is refused at load. Counting a command's writes is a feature this does not
  have, and a limit that holds for some write paths is worse than one that
  holds for none — a tenant that notices simply picks the other path.
- **A quota costs a row lock per write.** Two writes into the same tenant's
  quota serialise on its usage row. That is the price of the ceiling holding,
  and it is per tenant rather than global.
