---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Command argument lists become bounded typed row sources

## Context

Payment capture and return commands receive finite lists of typed business
objects. `insert_many` could consume such a list, but `aggregate` and
`update_many` accepted only prior command row-set steps. Making callers first
persist or project the same input would add artificial work, while an
unbounded JSON-to-row conversion would weaken the command resource contract.

## Decision

`aggregate.from` and `update_many.for_each` may bind a non-null argument list
whose non-null items are declared objects. Such a source must declare
`maximum_items` between 1 and 256. Prior step row sets retain their existing
bounds and must not redeclare this argument-only field.

An argument row source may additionally declare `minimum_items` between 1
and its `maximum_items`. Omission means zero. The compiler uses a positive
minimum as the only static non-empty proof for aggregate nullability; it does
not infer non-emptiness from a preceding Rule. Both request planning and the
generated SQL boundary enforce the lower and upper bounds.

The request planner validates and coerces items through the compiled command
argument contract, rejects values above the declared bound, and lowers the
list to an internal typed `ArgumentRows` IR step. SQL generation materializes
that step with one PostgreSQL `jsonb_to_recordset` CTE and ordinality. It does
not emit one statement or one CTE per item. Aggregation and `update_many`
consume the same closed row-set boundary, so duplicate-key and
`require_each` missing-key gates remain unchanged.

## Alternatives

| Option | Why Not |
|--------|---------|
| Require a preceding `fixed_rows` or persistence step | Duplicates request data in metadata and cannot bind a runtime list without widening another form. |
| Expand one Rust CTE or statement per item | Makes generated SQL proportional in structure to request cardinality and invites row-by-row execution. |
| Accept arbitrary JSON arrays | Loses deploy-time field types and defers malformed item discovery to database casts. |
| Infer a non-empty list from a business Rule | Adds flow-sensitive refinement and allows the type contract to drift from runtime Rule order. |
| Add a general loop construct | Expands the closed command grammar into an unbounded workflow language. |

## Consequences

Real request batches can be aggregated or atomically matched to table rows in
one PostgreSQL statement. Metadata gains an explicit upper bound and an
optional positive lower bound on the two argument-consuming forms, and
execution IR gains one internal row-source variant. Nested object fields
remain JSONB columns; scalar fields retain their compiled contract types and
nullability.
