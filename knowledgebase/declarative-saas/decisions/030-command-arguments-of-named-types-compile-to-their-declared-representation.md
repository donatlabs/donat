---
type: decision
status: accepted
date: 2026-07-31
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Command arguments of named types compile to their declared representation

## Context

Compiling a Command argument to SQL needs its Postgres type: the argument is
rendered as a literal cast, and conditions such as `argument_equals` compare
it against a declared value in the same type. The mapping covered the built-in
spellings — `bigint`, `uuid`, `timestamptz` — and fell back to `jsonb` for
anything else.

Everything else includes every named metadata type: the enums, objects and
lists declared in `rules.yaml`. For an object or a list `jsonb` is right. For
an enum it is wrong: an enum value is a string in every other representation
the runtime uses — the rule engine types it as text, the signal payload carries
it as a JSON string, the GraphQL request writes it unquoted. Rendering it as
JSON produced `'accepted'::jsonb`, a value no argument of that type can take,
and the statement failed with SQLSTATE 22P02. The Petshop
`record_return_inspection` command could not be called at all, and the response
body — deliberately opaque for command database errors — said only "command
database error".

## Decision

A named type resolves through the compiled rule catalog, which already holds
the declared shape of every metadata type, and the argument compiles to the
representation that shape implies: an enum is text, an object or list is jsonb,
a declared scalar alias is its scalar. Only a type the catalog does not know
falls back to jsonb.

This uses the same mapping the rule compiler uses for rule bindings, so an
argument and a rule parameter of the same declared type agree on their SQL
type by construction rather than by two parallel tables staying in step.

## Alternatives

| Option | Why Not |
|--------|---------|
| Special-case enum names in the argument mapping | Duplicates the catalog's knowledge in a second place that can drift from it. |
| Render every named type as text | Objects and lists are JSON; text would break every list argument. |
| Reject named types as command arguments | Enums are the natural type for a decision a caller supplies. |

## Consequences

Enum arguments work in conditions, idempotency scopes and every other place an
argument's SQL type is needed. The compiler now depends on the rule catalog to
type an argument, which is already an input to command compilation. A metadata
type declared after the commands that use it still resolves, because the
catalog is compiled before commands are planned.
