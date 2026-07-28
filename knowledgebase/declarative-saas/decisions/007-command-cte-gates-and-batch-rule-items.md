---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Command writes depend on materialized gates and typed batch item rows

## Context

Command Rules are lowered before SQL generation and may legally bind an
`insert_many` value with `{ item: field }`. The previous planner retained a
typed item field only when it appeared as a direct column assignment. A Rule
binding therefore passed deploy-time validation but its pre-lowered SQL could
refer to an undefined private item alias at execution.

The former renderer also placed command guards and assertions in the final
result expression. PostgreSQL can short-circuit that expression before an
unneeded writable CTE in simple cases, but this is not an explicit data
dependency that proves a rejected guard or assertion precedes every dependent
write and its `BEFORE` trigger.

A historical validator path also allowed a guard binding to reference a
completed command step. Such a value cannot be a precondition: resolving it
would require the very CTE write that a false guard must prevent.

## Decision

The request planner recursively discovers item values used inside a Rule's
closed bindings. It adds their typed representations to the already resolved
`InsertMany.item_fields` IR. SQLgen renders each validated JSON input item as
a typed one-row derived table with the fixed private alias `_cmd_item`. The
opaque Rule artifact keeps its original quoted alias reference; SQLgen neither
substitutes text into it nor parses Rule source, metadata, or a catalog.

SQLgen emits a materialized guard gate before an idempotency claim or domain
step. The claim and every command DML CTE explicitly depend on that gate. Each
`assert` emits its own materialized gate after the steps that its pre-lowered
Rule references; later DML CTEs depend on it. A final gate makes terminal
assertions observable even when no later DML exists. A false Rule calls the
existing `donat.raise_graphql_error` helper before a dependent DML source is
evaluated, preserving the existing structured error envelope and one-statement
transaction contract.

## Alternatives

| Option | Why Not |
| --- | --- |
| Textually replace `_cmd_item` in Rule SQL for every array element | Turns opaque trusted Rule artifacts into a second string-template language and risks unsafe or incomplete substitution. |
| Reparse Rules, metadata, or the catalog in SQLgen | Violates the resolved SQL-free IR boundary from ADR 005. |
| Keep final `CASE` rejection only | Does not make the pre-DML ordering contract explicit or independently testable with trigger-sensitive behavior. |
| Use a Rust loop for every batch item | Breaks the single Postgres statement and no-N+1 command invariants. |

## Consequences

`insert_many` Rule bindings now have one typed SQL source per concrete item and
preserve public input ordering through the existing private ordinal. Commands
gain a few small read-only gate CTEs, but retain one Postgres statement,
explicit-role checks, idempotency semantics, and rollback on rejection. Live
regressions use `BEFORE INSERT` triggers that raise a distinct SQLSTATE, so a
future accidental change that reaches a write before a false guard or assert
cannot be hidden by eventual rollback.

Guard bindings are therefore limited at deployment to arguments, literals, and
nested Rules composed from those sources. A binding that references a named
step is rejected with the guard path and step name; an `assert` is the
ordered construct for a predicate over a previous step.
