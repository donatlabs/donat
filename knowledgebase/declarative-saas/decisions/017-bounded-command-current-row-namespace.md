---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Bounded command transforms use a distinct current-row namespace

## Context

`project_many` and `decision_many` transform one bounded source row at a time.
Petshop metadata needs direct and nested Rule access to that row, while
`update_many` already uses `current_column` for the matched database target.
Reusing `item` would conflate bounded source rows with insert/update input
items and make alias selection ambiguous in SQL generation.

## Decision

`current_column` resolves against the bounded source row in `project_many` and
`decision_many`, using the source field's exact type and nullability and the
fixed SQL alias `_cmd_input`. Nested Rule bindings use the same scope.
`update_many` retains its existing `_cmd_target` current-row meaning.

`item` remains the insert/update-many input-item namespace. Scalar projects,
ordinary selects and inserts, results, and effects do not gain a current-row
scope. Unknown fields are rejected against the active bounded-row or
update-target namespace during deployment compilation.

## Alternatives

| Option | Why Not |
|--------|---------|
| Reuse `item` for bounded transforms | Conflates source-row and mutation-item semantics and weakens diagnostics. |
| Lower bounded `current_column` to `item` in IR | Erases the namespace before SQL rendering and risks selecting the wrong alias. |
| Add arbitrary row aliases to metadata | Creates an unnecessary identifier surface instead of a closed contextual binding. |

## Consequences

The compiler, planner, Rule lowering, and SQL renderer preserve one explicit
namespace from metadata through the one-statement command. Decision passthrough
fields and declared ordering remain unchanged.
