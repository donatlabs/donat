---
type: decision
status: accepted
date: 2026-07-23
features:
  - "[[api-surfaces]]"
---

# Defer MCP publication of actions with output relationships

## Context

MCP action tools automatically generate a GraphQL selection from the action's
custom output type. That selection can safely include scalar fields and nested
custom output objects, but an output-object relationship points to a tracked
table and needs a role-scoped target selection. Publishing the relationship
name alone is invalid GraphQL, while inventing a target-column selection could
leak fields or conflict with the action relationship resolver's permission
rules.

## Decision

Reject an MCP action tool when its output type, including nested custom output
objects, declares a relationship. The metadata loader fails before boot with
an explicit unsupported-output-relationship error.

MCP action publication continues to support scalar output, custom output
objects, lists, and recursive custom types. A future relationship-capable
version must define a role-aware target selection strategy, generate the full
selection, and add conformance coverage before lifting this validation.

## Alternatives

| Option | Why Not |
|---|---|
| Omit relationships from the generated selection | Silently loses declared action output data. |
| Select a guessed target field such as `id` | The target may not expose that field to the role and a guess changes the action contract. |
| Accept a free-form selection in MCP metadata | It adds an unvalidated GraphQL surface and duplicates schema/permission concerns. |

## Consequences

Configured action tools either return their supported declared output or fail
metadata validation; they no longer present a partial result as complete.
Action-output relationships remain available through GraphQL until MCP gains a
complete role-scoped selection contract.
