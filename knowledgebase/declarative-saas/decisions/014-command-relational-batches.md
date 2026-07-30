---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Commands use bounded relational batches for atomic checkout work

## Context

The Petshop checkout command needs to read an ordered pricing row set, derive
totals, and reserve every corresponding inventory row atomically. Existing
single-row command steps and `insert_many` cannot express that work without
either application-specific code, an unbounded predicate language, or a
client-side loop. Each alternative would weaken the explicit-role, one
Postgres-statement, and no-N+1 command invariants.

## Decision

Commands gain only three relational batch forms: `select_many`, `aggregate`,
and `update_many`. A read target can be a tracked table or view; an update
target remains an ordinary Postgres table with a non-empty primary key. The
explicit command role needs the underlying select or update permission, as
applicable. There is no free-form predicate, SQL, identifier template,
function call, join declaration, loop, or dynamic relation.

`select_many` has a non-empty equality map and a declared total order.
Duplicate complete order tuples are rejected before later steps consume the
row set. `aggregate` accepts only a prior row set and produces exactly one row
from `sum`, `count`, `min`, `max`, or `count_distinct`; it has no group, filter,
window, or user expression. `update_many` consumes only a prior row set and
matches every target primary-key column from the current item. It rejects
duplicate input primary keys before DML. With `require_each`, it compares the
distinct input-key count, input count, and affected count, so duplicate keys,
missing rows, and non-one-for-one updates cannot silently succeed.

Row sets stay in one source and one command statement; they do not become a
cross-source value or general result transport. Returned lists preserve their
declared total order. The forms are metadata-only in this initial slice. Serde
rejects malformed closed shapes, but catalog-aware validation owns relation
kind, primary-key, permission, aggregate row-set source, forward-reference,
and `current_column`-scope checks. SQL compilation and execution will enforce
the stated semantic and transaction behavior without adding an admin role,
runtime metadata mutation, or free-SQL surface.

## Alternatives

| Option | Why Not |
| --- | --- |
| General SQL or predicate templates in command metadata | Creates a second SQL language and bypasses deploy-time relation, permission, and type validation. |
| Client-side read, aggregate, and per-row update loop | Breaks atomicity and the one-statement/no-N+1 invariant. |
| Permit arbitrary joins, grouping, filters, or loops | Expands a checkout primitive into an unbounded query/workflow language without a product requirement. |
| Treat views as update targets | Weakens predictable relation safety because an updatable view is not the ordinary primary-key table required for bounded update matching. |

## Consequences

Metadata remains straightforward to audit and can represent the Petshop
pricing-to-reservation batch without executable text. The command compiler must
add catalog-aware semantic rejection tests before it exposes the new forms:
aggregate over a scalar step, a forward step reference, and `current_column`
outside `update_many.set` or `update_many.check` are intentionally Task 5
tests, because deserialization cannot determine them. Runtime implementation
must preserve one source, one statement, explicit role checks, and deterministic
ordered results.
