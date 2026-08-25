---
type: decision
status: accepted
date: 2026-08-20
features:
  - "[[declarative-saas]]"
  - "[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]"
---

# An unbounded permission says so

## Context

`filter: {}` means two different things and looks the same both times. It is
what a catalogue permission is supposed to say — every shopper reads the same
listings — and it is also what a permission looks like when the author forgot
who the rows belong to. A reviewer reading either one has no way to tell which
they are holding, because the metadata records the bound and never records the
decision not to have one.

That is not hypothetical. The Petshop example shipped with `vendor` reading and
updating every other seller's orders under `filter: {}`, and the reason it
survived review is precisely that it read like every other deliberate `{}` in
the file. Sixty-one of the example's permissions do bound their caller; a
hundred and ninety-two do not, and until now nothing distinguished the two
hundred deliberate ones from the handful that were mistakes.

[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] solved the
same problem for the tenant by moving the bound into the compiler, and it could
do that because a tenant is one value, on one column, identical for every table
and every role. Ownership is none of those things: the path to an owner differs
per table — `vendor_order` reaches it by `vendor_id`, `purchase_approval` one
hop further through its quote — and whether a table has an owner at all differs
per role, because a marketplace's catalogue is meant to be read by every shopper
and the same table read by a seller is not. There is no rule to declare once.

## Decision

The rule stays in the permission's own `filter`, where it can vary. What becomes
uniform is the *guarantee*: a deployment sets `unbounded_permissions: declared`
in `permissions.yaml`, and every permission that admits rows it does not bound
to the caller must name a reason — `catalogue`, `operator`, `worker` or
`command`. Nothing is injected and nothing is inferred. The author writes it,
`donat validate` checks it, and a reviewer reads it.

"Bounds the caller" is a question about reaching TRUE, not about being
non-empty: `{status: {_eq: 'paid'}}` is a filter under which every caller of the
role still sees the same rows, so it is unbounded and has to say so. The
analysis is the obvious recursive one with one case that earns its keep —
`_or` binds only when *every* arm binds, so `{_or: [{owner: {_eq:
X-Donat-User-Id}}, {}]}` is correctly read as admitting everything, where a
check that grepped for `x-donat-` would have waved it through. `_not` is read as
no bound at all. Every remaining ambiguity resolves towards asking for a
declaration that was not owed rather than accepting one that was.

The tenant variable is deliberately not a caller bound. A permission scoped only
by tenant admits every row of that tenant — every seller's order in one
marketplace — which is exactly the case this exists to make visible, and
counting it as a bound would hide the bug that prompted the decision.

Two rules, both deploy-time, both refusing the load rather than the request:

*An unbounded permission with no reason stops the deployment*, naming the table,
the plane and the role — but only where the deployment asked for the check.
Metadata exported from an existing Donat project has never heard of `unbounded:`
and must still load, so the default is `unchecked`.

*A reason on a permission that does bound its caller stops it too*, always and
regardless of the policy. A declaration the runtime ignores is a defect
([[034-a-declaration-the-runtime-ignores-is-a-defect]]), and a stale `unbounded:`
is worse than an absent one: it tells a reviewer that a bound was considered and
declined on a permission that in fact has one.

`command` is checked rather than believed. It claims the row is chosen by a
command step, and it is accepted only where nothing else can arrive — on a
`command_*` plane, which schema generation ignores entirely, or on an ordinary
plane of a table the role has no ordinary `select_permissions` on, because
`Planner::table_ctx` returns `None` there and the role gets no insert, update or
delete root for the table at all. Anywhere else the claim is false and the
deployment is refused. What stays a reviewer's job is the half no engine can
see: that the step's `by` reaches only rows the caller is entitled to. That is
the audited shape `unscoped_steps: audited` already has.

An overlay may not relax the check. Where `extends:` composes two directories
and either declares it, the composition has it — the one place a merge takes the
stricter of two answers rather than refusing the pair, because a base that
requires its unbounded permissions to be declared must not be quietly loosened
by composing something on top of it.

## Alternatives

| Option | Why Not |
|--------|---------|
| Compile the owner bound the way the tenant is compiled | The same table needs opposite answers for different roles — the catalogue is every shopper's and no seller's — so there is no single predicate to inject, and a per-role per-table declaration is the `filter` block already. |
| A second declarative layer, `ownership.yaml` | Re-states the permission block in a poorer language: `filter` already does relationship traversal, `_or` and nesting. Two places that grant access is more magic, not less. |
| Free text instead of an enum | Unvalidatable, and copy-pasted within a week. An enum makes the author pick an answer a reviewer can disagree with. |
| Make the declaration mandatory everywhere | Breaks the promise that v2 metadata loads without conversion. The check is what a deployment opts into; the vocabulary is always available. |
| Warn instead of refusing | A warning in a deploy log is how the seller hole survived in the first place. |
| Exempt the command planes | They are where a person's role most often reads rows the permission does not bound. Exempting them hides the case that most needs reading. |
| Infer the reason from the role name | `fulfilment` and `support` are both desks people log into and roles processes run as. The metadata cannot tell which without being told. |

## Consequences

Petshop declares 192 reasons — 99 `operator`, 77 `worker`, 9 `catalogue`, 7
`command` — and Pethub 8 more that it inherits the requirement for. Writing them
is a one-time pass; keeping them is one line per new permission, refused at
deploy if forgotten. The distribution is itself the point: a hundred and
seventy-six of them are desks and workers, which is a fact about the example
nobody could previously state.

The check reads metadata only, so it costs nothing at request time and cannot
change what a query returns. It is a statement about the deployment, not a
predicate in a statement.

The analysis is deliberately conservative and will sometimes ask for a
declaration on a permission that does bound its caller in a way it cannot see —
a bound reached only through `_not`, for instance. That direction is the safe
one, and the fix is a sentence in the metadata rather than a change to the
engine.

What this does not do is decide whether a declared reason is *correct*.
`unbounded: operator` on a permission that should have been bounded is still
wrong; it is merely wrong in writing, where a reviewer can see it and argue with
it, instead of wrong by omission where nobody could.
