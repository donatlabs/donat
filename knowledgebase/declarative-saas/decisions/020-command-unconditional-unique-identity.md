---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Single-row Commands accept unconditional unique-key identities

## Context

Provider and idempotency facts are often identified by a natural unique key
rather than the table's surrogate primary key: for example, one payment per
order or one reconciliation per provider event. Requiring a preliminary read
only to recover the primary key adds metadata and another dependency without
making a guarded update or `select_one` more single-row.

Not every PostgreSQL unique index proves the required invariant. Partial,
expression, invalid, or not-yet-ready indexes do not guarantee uniqueness for
all rows addressed by a closed equality predicate.

## Decision

Catalog introspection retains a table's primary key and every valid, ready,
unconditional unique index whose key positions are ordinary table columns.
Expression keys, predicates, and included non-key columns are excluded.

`select_one`, `update`, and `delete` must bind every column of either the
primary key or one retained unique key. Additional equality guards remain
allowed and are validated normally. An empty predicate, an incomplete key,
or a key backed only by a partial/expression index fails deployment.

The catalog fact is backend-specific and does not become a client-controlled
identifier expression. SQL generation still receives only resolved table
columns and typed values, and the operation remains one PostgreSQL statement.

## Alternatives

| Option | Why Not |
| --- | --- |
| Require only primary keys | Forces artificial reads for safe natural identities and makes common provider workflows verbose. |
| Accept any unique index | Partial and expression indexes do not prove that an arbitrary equality map addresses at most one row. |
| Let metadata declare uniqueness | Duplicates database truth and can become unsafe after schema drift. |
| Catch multi-row updates at runtime | Detects an invalid declarative contract after serving and may reach DML before rejection. |

## Consequences

Schema migrations must create the required unconditional unique key before
metadata validation. Live catalog tests cover accepted and excluded index
forms. Command diagnostics list the primary and admissible unique identities
when no complete key is supplied.
