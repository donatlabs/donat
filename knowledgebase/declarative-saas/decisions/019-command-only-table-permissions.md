---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Commands use a separate least-privilege table permission plane

## Context

Real domain operations need internal lifecycle columns and append-only audit
tables that must not become generic GraphQL, REST, or MCP CRUD roots. Reusing
ordinary table permissions forced a choice between rejecting a valid Command
and exposing raw writes that bypass the Command's guards, idempotency, and
future durable effects.

The engine still has no admin role or permission bypass. A Command is callable
only by one of its explicit classic roles, and every database operation must
remain constrained by a deploy-time permission for that same role.

## Decision

A tracked table may declare `command_select_permissions`,
`command_insert_permissions`, `command_update_permissions`, and
`command_delete_permissions`. They have the same closed column, filter, check,
and preset shapes as their ordinary counterparts, but only Command compilation
and request planning can resolve them. Schema generation and generic
GraphQL/REST/MCP planning ignore them completely.

For a Command table operation, an explicit or inherited command permission is
preferred. Ordinary permissions are a compatibility fallback only when no
reachable command permission exists for that role and operation. A reachable
but malformed, conflicting, or insufficient command permission fails closed;
the planner must not silently fall back to a broader ordinary permission.

Command invocation permission remains an additional gate rather than a table
permission substitute. Source, role, column, filter, check, and preset
validation all remain mandatory, and the resolved permission predicates stay
inside the Command's single PostgreSQL statement.

## Alternatives

| Option | Why Not |
| --- | --- |
| Widen ordinary CRUD permissions for worker roles | Exposes lifecycle tables and direct mutations outside the Command contract. |
| Let Commands bypass table permissions | Reintroduces the forbidden admin-style permission bypass. |
| Put hidden tables in a separate service or database | Adds deployment topology without improving the declarative authorization model. |
| Fall back after an invalid command permission | Turns a restrictive configuration mistake into broader access. |

## Consequences

Petshop can expose narrow customer catalog reads while authorizing checkout,
payment, fulfilment, return, and audit Commands against internal columns and
tables. Introspection tests must prove that command roots remain visible while
the corresponding generic CRUD roots remain absent. Negative tests must cover
wrong roles, insufficient columns, filters, checks, inherited roles, and the
no-fallback-on-conflict rule.
