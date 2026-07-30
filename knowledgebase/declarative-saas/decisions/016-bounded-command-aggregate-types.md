---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Bounded command aggregates preserve declared integer width

## Context

PostgreSQL widens `sum(int8)` to `numeric` and returns nullable aggregate
types even when a command has already proved that its bounded input is
non-empty and its selected column is non-null. Exposing those implementation
types made valid Petshop Rule and decision bindings fail deploy-time type
checking. Commands also need to aggregate cardinality-preserving bounded
projections without reopening the declarative grammar.

## Decision

The command compiler tracks a `guaranteed_non_empty` fact through
`select_many(require_non_empty)`, non-empty `fixed_rows`, and
cardinality-preserving `project_many` and `decision_many`. It does not infer
the fact through filtering, updates, allocation, or row-set fields.
`aggregate` accepts any prior bounded row-set producer.

`count` and `count_distinct` are always non-null `int8`. `sum`, `min`, and
`max` are non-null only when the producer is guaranteed non-empty and the
input column is non-null. Integer sums preserve the command DSL width:
`int2`/`int4` accumulate to `int8`, while `int8` accumulates in PostgreSQL's
`numeric` implementation and is checked back into `int8` in the same
statement. Numeric and floating-point aggregates retain their declared
families.

## Alternatives

| Option | Why Not |
|--------|---------|
| Publish PostgreSQL's widened `numeric` result for bigint sums | Leaks a backend implementation detail and breaks exact Rule/decision contracts. |
| Coalesce nullable aggregates | Invents a value for empty or all-null inputs and changes SQL aggregate semantics. |
| Fold or range-check aggregate rows in Rust | Breaks one-statement database assembly and transactional overflow behavior. |

## Consequences

Compiler, execution IR, generated GraphQL types, and SQL casts share one
aggregate output contract. Bigint overflow aborts and rolls back the command
statement. Empty and all-null inputs remain nullable unless the compiler has
proved both required non-emptiness and non-null input values.
