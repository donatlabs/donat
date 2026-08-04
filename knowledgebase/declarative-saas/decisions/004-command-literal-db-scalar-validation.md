---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
  - "[[003-declarative-domain-commands]]"
---

# Command metadata literals use catalog-derived database scalar descriptors

## Context

The initial static command-catalog slice validates a command value through its
`StaticType`. That is appropriate for command arguments, `insert_many` items,
prior-step outputs, and Rules bindings, but it deliberately normalizes several
distinct PostgreSQL types into the same GraphQL-facing scalar. For example,
`int2`, `int4`, and `int8` all become an integer-shaped static type, while
`varchar(n)`, `bpchar(n)`, `numeric(p,s)`, and timestamp precision are not
represented at all.

That normalization cannot safely validate a metadata literal that is written to
a concrete PostgreSQL column. It can accept an out-of-range `int8` string or
silently treat an unsupported PostgreSQL type as a string. The command catalog
must reject such metadata before a candidate engine is published. It must not
solve the problem by widening the GraphQL schema, the Rules type system, or the
command `StaticType` model.

## Decision

`ColumnInfo` retains both the PostgreSQL `pg_type` name and the raw
`pg_attribute.atttypmod` value returned by PostgreSQL introspection. PostgreSQL
columns always receive the raw integer; non-PostgreSQL catalog constructors use
the no-modifier sentinel because commands already reject non-PostgreSQL
sources. The catalog remains a catalog snapshot, not a new public scalar API.

The schema command compiler derives a private `CommandScalarDescriptor` only
when a `literal` value is bound to a concrete catalog column in a command
object or complete primary-key predicate. The descriptor has the column's
PostgreSQL type, raw modifier, and nullability. It is an implementation detail
of deploy-time command validation: it is not added to `StaticType`, GraphQL,
Rules, command arguments, item values, prior-step values, or rule values.
Those existing paths keep their established typing and assignment rules.

The descriptor accepts only the following PostgreSQL scalar families. Every
other `pg_type`, and every modifier that is not valid for the listed family,
rejects the metadata literal deterministically during deployment. There is no
fallback to `String`, `Float`, or another static scalar.

| PostgreSQL family | Accepted literal and modifier rule |
| --- | --- |
| `bool` | A JSON boolean only; modifier must be `-1`. |
| `int2`, `int4`, `int8` | An integral JSON number or an integral string with optional leading minus and no whitespace, plus sign, decimal point, or exponent. Parse exactly and require the signed range of the concrete width: `-32768..32767`, `-2147483648..2147483647`, or `-9223372036854775808..9223372036854775807`. Modifier must be `-1`. |
| `float4`, `float8` | A JSON number or numeric string that parses into the concrete IEEE width and is finite after that concrete-width conversion. `NaN`, positive infinity, negative infinity, and finite `f64` inputs that overflow `float4` are rejected. Modifier must be `-1`. |
| `numeric`, `decimal` | A JSON number or string matching `-?(0|[1-9][0-9]*)(\\.[0-9]+)?`. Whitespace, plus signs, exponent notation, and non-finite spellings are rejected. `atttypmod = -1` is unconstrained. Otherwise decode PostgreSQL's numeric modifier after removing `VARHDRSZ` (`4`): precision is the high 16 bits and scale is the signed low 11 bits. Apply PostgreSQL numeric rounding for the declared scale, then require that the rounded value fits the declared precision and scale, including negative scales. A malformed precision/scale pair is rejected. |
| `uuid` | A canonical UUID string accepted by the existing UUID parser; modifier must be `-1`. |
| `date` | A valid `YYYY-MM-DD` calendar date string; modifier must be `-1`. |
| `timestamp`, `timestamp without time zone` | A valid local timestamp string in the command's existing timestamp grammar. Modifier `-1` permits up to six fractional-second digits; a modifier from `0` through `6` permits no more than that many digits. Other modifiers are rejected. |
| `timestamptz`, `timestamp with time zone` | A valid RFC 3339 timestamp string with an offset. The same `-1` or `0..=6` fractional-second rule applies. |
| `text`, `varchar`, `bpchar`, `name`, `citext` | A string only. `text` and `citext` require modifier `-1` and are unbounded. `varchar` and `bpchar` accept `-1` or a modifier of at least `4`; the latter permits at most `atttypmod - 4` Unicode characters. `name` requires modifier `-1` and permits at most PostgreSQL's `NAMEDATALEN - 1` bytes (63 bytes for the supported PostgreSQL 16 build). |

Object and list literals remain forbidden. A `literal: null` is valid only
when the descriptor was derived from a nullable concrete column. The compiled
command preserves the concrete SQL type needed to render that null as a typed
SQL null; a non-nullable column is a deploy-time validation error.

Pure value contexts such as `fixed_rows` and command results have no
destination column from which to derive a type. Non-null scalar literals
retain inference, while an otherwise uninferable nullable literal uses the
closed form
`{ literal: null, as: uuid }`. The annotation must resolve through the same
command and Rules type catalog to exactly one nullable scalar; lists, objects,
unknown names, and non-null annotations reject deployment. Request planning
retains the resolved PostgreSQL representation. Result literals obtain that
cast from the compiled result contract rather than reparsing the annotation.

Validation stays deploy-time only. `donat validate`, candidate-engine
construction, and `migrate --metadata-dir` report the error before serving;
the serving path does not introspect, issue DDL, mutate metadata, or loosen a
role check. Commands continue to run through explicit classic roles, and
successful command execution still compiles to one PostgreSQL statement with
no Rust row-by-row response assembly.

## Alternatives

| Option | Why not |
| --- | --- |
| Expand `StaticType` to carry PostgreSQL widths and modifiers | Makes command-only database facts part of argument, item, result, and Rules typing, creating an unnecessary second broad type model. |
| Reuse GraphQL scalar names as the literal validator | GraphQL `Int`, `Float`, and `String` cannot express concrete integer widths, numeric typmods, string limits, or timestamp precision. |
| Let PostgreSQL discover invalid literals at command execution | Turns deploy-time metadata errors into request-time failures and can leave error behavior dependent on a particular runtime path. |
| Accept unknown types as strings or generic numerics | Reintroduces the information loss that caused the rejected Task 2 range and can silently change stored data. |
| Treat an untyped result `null` as JSONB | Makes a field's GraphQL and SQL types depend on an implementation fallback instead of metadata. |
| Add runtime DDL or an administrative bypass to inspect types on demand | Violates the deploy-time configuration model and the explicit-role/no-admin boundary. |

## Consequences

The catalog gets one additional raw PostgreSQL field and every `ColumnInfo`
literal must be updated. The command compiler gains a narrow private validator
with tests at exact numeric, string, time, nullability, and unsupported-type
boundaries. This is deliberately more work than checking the existing static
scalar, but it makes command metadata deterministic and preserves the existing
public GraphQL and Rules contracts.

The current `feat(commands): validate static command catalog` commit remains
unaccepted. Its former review passes do not cover this decision. Recovery work
must amend or fix that command-validation range, add the catalog typmod slice,
and pass fresh review of the complete corrected range.
