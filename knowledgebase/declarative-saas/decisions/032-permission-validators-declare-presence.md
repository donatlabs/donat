---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[004-decision-rules]]"
---

# Write permissions validate values, and always declare presence

## Context

A write permission could say who may write a row (`filter`, `check`) and which
columns (`columns`, `set`), but not what a value had to be. Anything
cross-field, string-shaped, or arithmetic fell to a database `CHECK`, and a
`CHECK` binds every writer at once: a migration, a command, and a customer are
all held to the shopper's rule. When one fired, the caller received
PostgreSQL's own text — `Uniqueness violation. duplicate key value violates
unique constraint "…"` — under a code that describes a permission, not a value.

The rule profile (ADR-003) already had the language for this, but it refuses
operations on nullable operands and has no flow-sensitive refinement. A
comparison over a nullable column therefore cannot be written at all, and
`is_null(x) || x > 3` does not rescue it: the second arm still reads a nullable
value. Some way to state what a null means was unavoidable.

## Decision

`insert_permissions` and `update_permissions` accept an ordered `validate`
list. Each entry carries its own message and exactly one predicate spelling:

| Spelling | Meaning |
| --- | --- |
| `expression` | rule-profile source, type checked against the table's columns |
| `not_null: <column>` | the column must be present; refines it to non-null for **later** entries |
| `when_present: <column>` | scopes one expression to rows where the column is present, and refines it non-null **inside that entry only** |

Entries are evaluated in document order and the first violated entry is the one
reported, as `validation-failed` with the author's message through the existing
`donat.raise_graphql_error` envelope. The established `permission-error` shape
for `check` is untouched, and a permission failure is reported before any
validator, so a role never receives a message about a value it was not allowed
to submit.

A validator passes only on TRUE. The gate is the one the generic check already
used — `WHERE (expr) IS NOT TRUE` over the CTE of written rows — so an unknown
value is a violation, and the expression reads the row after presets and column
defaults rather than the submitted object.

Compilation happens once, when a source's planner index is built, and never at
request time. Metadata that does not compile refuses publication: the engine
logs the table, role and entry index and does not serve. A key that failed to
compile is retained as its diagnostic rather than dropped, so reaching it
returns a plan error instead of silently skipping a declared check.

Presence is declared, never inferred. The rule profile is not relaxed.

## Alternatives

| Option | Why Not |
| --- | --- |
| Allow nullable operands inside validator expressions, treating null as a violation | The runtime already refuses the row, so this only changes which message is shown — at the cost of a null-discipline exception in the language commands also depend on. |
| Infer the guard from an `is_null` arm | Flow-sensitive refinement is exactly what ADR-003 declined; one context cannot have it without the type checker having it everywhere. |
| Leave value rules to database CHECK constraints | A constraint binds every writer, cannot vary by role, and answers with PostgreSQL text under a permission code. |
| Compile expressions at request time | Puts CEL parsing on the mutation hot path and turns a metadata error into a per-request failure. |
| Reuse the `permission-error` envelope | Conflates "you may not write this row" with "this value is unacceptable", and its message is fixed, so several validators would be indistinguishable. |

## Consequences

Petshop gains per-role value contracts a constraint cannot express: a cart line
capped at 20 units for shoppers only, address lengths that exist for the
carrier label, a grading floor for staff-created variants. A nullable column
is usable in an expression only after its presence is declared, which is a
deploy-time error when forgotten rather than a surprise in production.

Nested object inserts are refused when the target table's role declares
validators: the child rows land in a CTE the planner does not name, so the
lowered expression would target the wrong rows. Failing the plan keeps the
declared check enforced; enforcing it there needs the nested CTE name to become
part of the planner/SQLgen contract, as `INSERT_ROW_ALIAS` already is.

Database constraint violations still surface as PostgreSQL text under
`permission-error`. That shape is a separate decision and is deliberately left
untouched here.
