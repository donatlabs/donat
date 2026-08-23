---
type: decision
status: proposed
date: 2026-08-23
features:
  - "[[declarative-saas]]"
  - "[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]]"
---

# A tenant leaves the way it arrived

## Context

[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] names
tenant offboarding among the things deferred "so they are not mistaken for
oversights". It is the deferral that stops a deployment being run rather than
merely being incomplete: a platform that cannot delete a customer cannot
operate under GDPR, and one that cannot hand a customer their data cannot
answer a portability request either.

Onboarding needs no DDL — `binding: row_key` was chosen so a store is rows.
Offboarding is the same claim read backwards, and it has gone untested. Nobody
has asked this engine to remove a tenant, and the machinery that would do it —
every tracked table, the tenant key on each, the order their references impose
— is exactly what
[[../../../specs/010-tenancy-migration-generator|the migration generator]]
already derives.

## Decision

Two commands, one derivation:

```
donat tenancy export --tenant <id> --out <dir>
donat tenancy erase  --tenant <id> --confirm <id>
```

**Export first, always.** They are the same walk of the same tables in the same
order; one writes JSON and one issues `DELETE`. Building erase without export
would answer the regulator's second question and not the first, and a
deployment that can only delete will keep customers it meant to remove.

**The order is derived, not written.** Deleting a tenant means deleting from
every table the key reaches, children before parents, and the catalogue holds
the references that say which is which. A hand-maintained list is the same
mistake this branch has already made twice — the sixtieth table, and the index
keyed by a column that had been rescoped.

**A serving tenant is refused.** Erase requires the registry to have stopped
serving it first, so removal is two deliberate acts with a gap between them,
and the gap is where somebody notices. `--confirm <id>` must repeat the tenant
id: a flag that is merely present is a flag that gets pasted.

**Shared tables are refused rather than filtered.** A table declared
`exempt: shared` holds nobody's rows in particular, so a tenant-scoped delete
over it is meaningless; meeting one is a declaration error and stops the run
naming it. `scope_via` tables are reached through the relationship that scopes
them, because that is what the declaration says they are.

**It removes what the tenant produced, not only what it wrote.** Files under
the tenant's object prefix, its process instances and their journals, its
command idempotency rows, its quota counters and its grants. Each of those
carries the tenant today because
[[097-a-tenant-is-a-compiler-layer-not-a-filter-somebody-remembered]] put it
there; erasure is the first thing that reads them back for that purpose.

**Retention is not decided here.** A deployment obliged to keep invoices for
seven years exports before it erases and keeps the export. Teaching the tool
which tables survive would put a legal judgement in a config file and make the
tool the place people argue about it. What the tool guarantees is that it
names every table it will touch before touching one.

## Alternatives

| Option | Why not |
|--------|---------|
| An HTTP endpoint | A management API, which this engine does not have. Deleting a customer over the same socket that serves them is the shape [[../api-surfaces/decisions/013-a-role-is-established-by-a-verified-token-or-a-hook-and-by-nothing-else]] removed. |
| `ON DELETE CASCADE` from the registry row | Silent, unordered, and invisible in review — and it would delete through references the declaration deliberately made composite. Erasure should be a statement you can read. |
| Anonymise in place instead of deleting | A different obligation with a different answer per column, and the one thing a generic tool cannot decide. Export plus delete is the honest primitive. |
| Wait for `binding: schema` and drop a schema | The binding that exists is `row_key`, and a deployment cannot wait for a different one to answer a subject access request. |
| Leave it to the operator's SQL | That SQL is the derivation this decision is about. Written by hand it is wrong on the sixtieth table. |

## Consequences

A deployment can answer both questions a data protection authority asks, and
the answer is a command with output a person can read rather than a migration
somebody wrote under time pressure.

What it does not give: erasure from backups, which is a retention policy rather
than an engine feature, and erasure from an upstream a connector sent data to.
Both are named in the command's own output, because a tool that stays silent
about them lets an operator believe it did more than it did.
