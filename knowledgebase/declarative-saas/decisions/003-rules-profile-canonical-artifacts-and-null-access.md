---
type: decision
status: accepted
date: 2026-07-28
features:
  - "[[declarative-saas]]"
---

# Rules profile canonical artifacts and null-safe access

## Context

The first Rules implementation parsed and type-checked a restricted CEL
profile, but a whole-branch review found gaps between its public behavior and
Spec 004. In particular, metadata could not declare objects or enums; SQL
could diverge from Rust short-circuiting and JSON access; a trace revision was
only a table name; and semantic errors lost the parser's byte locations.

The runtime must remain one Rust binary with deploy-time YAML metadata, no
admin bypass, no runtime metadata mutation, and no ambient rule capabilities.
The one wrapper must also retain a types-only `rules.yaml`; it cannot disappear
merely because it has no rules or decision tables.

## Decision

`rules.yaml` gains finite named object and enum declarations. Rule references
keep their compact GraphQL-like spelling (`Type!`, `[Type!]!`), while enum
symbols use `Type::symbol` in expression source. Declarations must resolve to
an acyclic owned `RuleType` graph before catalog compilation. Enums are
nominal as `RuleType::Enum { name, symbols }`, and an enum symbol AST node
carries both names. Equal text in `OrderStatus::draft` and
`OtherStatus::draft` is therefore not comparable.

`RulesMetadata::is_empty` includes `types`; the writer preserves a types-only
wrapper and the loader accepts its serde round trip. This is still exactly one
optional `rules.yaml`, not a new file or a separately mutable metadata API.

Every compiled rule retains profile version `1`, original source, a canonical
typed AST, and SHA-256 hashes of source and canonical AST. A decision revision
is SHA-256 over the complete canonical compiled definition, not its mutable
display name. Decision traces hash canonical decoded typed input with SHA-256;
there is no trace endpoint and rule bindings cannot contain secrets. A later
process slice may replace the digest with deployment-keyed HMAC when it
persists traces into an operator-visible journal.

### Canonical encoding v1 (normative duplicate)

`MAGIC` is exactly the 22 bytes
`44 4f 4e 41 54 2d 52 55 4c 45 53 2d 43 41 4e 4f 4e 49 43 41 4c 00`, rendered
as `b"DONAT-RULES-CANONICAL\0"`. The root byte stream is exactly
`MAGIC || U16BE(1) || RECORD`, where `U16BE(1)` is `00 01` and `RECORD` begins
with `01` typed rule, `02` decision, or `03` decoded input. `U32` is an
unsigned 32-bit big-endian integer; `S(s)` is `U32(utf8_bytes(s).len()) ||
utf8_bytes(s)`; `L<X>` is `U32(count)` followed by supplied-order `X` items;
and `M<X>` is `U32(count)` followed by `S(key) || X(value)` pairs with unique
keys sorted by unsigned lexicographic UTF-8 byte sequence. `B` is one byte
(`00` false, `01` true). Every count and string length is `U32`; lists retain
declaration/input order and maps are the only sorted collection.

| Tag | Type payload |
| --- | --- |
| `10` bool, `11` string, `12` int, `13` decimal, `14` uuid, `15` date, `16` timestamp | none |
| `17` enum | `S(enum_name) || L<S>(symbols)` in declaration order |
| `18` list | `T(item)` |
| `19` object | `S(object_declaration_name) || M<T>(field_name -> field type)` |
| `1a` nullable | `T(inner)` |

`T` is exactly one row from this table; nested nullable wrappers are normalized
before encoding. Object declaration name/fields and enum name/symbols are
therefore nominal identity, not display metadata.

Every typed expression is `E = tag || payload || T(resolved_result)`. Its tags
and payloads are `20` literal (literal below), `21` name (`S(name)`), `22` enum
symbol (`S(enum_name) || S(symbol)`), `23` list literal (`L<E>`), `24` member
(`E(receiver) || S(field)`), `25` literal index (`E(receiver) || U32(index)`),
`26` call (function tag then ordered `L<E>(arguments)`), `27` unary (operator
tag then `E(operand)`), `28` binary (operator tag then `E(left) || E(right)`),
or `29` conditional (`E(condition) || E(when_true) || E(when_false)`). Literal
sub-tags are `00` null/no payload, `01` bool/`B`, `02` int/`S(minimal_signed_base10)`,
`03` decimal/`D`, and `04` string/`S`. Function tags are `00` size, `01`
is_null, `02` startsWith, `03` endsWith; unary tags are `00` not and `01`
negate; binary tags are `00` or, `01` and, `02` equal, `03` not_equal, `04`
less_than, `05` less_than_or_equal, `06` greater_than, `07`
greater_than_or_equal, `08` add, `09` subtract, `0a` multiply, and `0b` divide.
The trailing result type applies recursively to every AST node. Spans,
descriptions, YAML formatting, and map insertion order are excluded.

`Q = T || V` is a typed decoded value. `V` tags are `30` null/no payload,
`31` bool/`B`, `32` string/`S`, `33` int/`S(minimal_signed_base10)`, `34`
decimal/`D`, `35` uuid/`S(lowercase_hyphenated_uuid)`, `36` date/
`S(ISO-8601_calendar_date)`, `37` timestamp/`S(UTC_RFC3339_timestamp)`, `38`
enum/`S(enum_name) || S(symbol)`, `39` list/ordered `L<Q>`, and `3a`
object/lexical `M<Q>`. `30` is legal only with nullable `T`. Minimal integers
have no plus sign or leading zeroes except `0`; `D` is sign byte (`00`
non-negative, `01` negative), `S(significant_digits)`, and `U32(scale)` after
stripping fractional trailing zeroes, with zero encoded as `00 || S("0") ||
U32(0)`.

Resolved declarations are `40 || S(enum_name) || L<S>(symbols)` or
`41 || S(object_name) || M<T>(fields)`. The payload after
`MAGIC || U16BE(1) || 01` is
`S(rule_name) || M<T>(bindings) || T(result) || E(expression)`. The payload
after `MAGIC || U16BE(1) || 02` is
`S(table_name) || L<declaration>(types) || M<T>(inputs) ||
M<T>(outputs) || hit_policy` (`00` first, `01` unique) `|| L<row>(rows) ||
L<case>(test cases)`, where a row is `S(id) || M<E>(conditions) || M<Q>(output)`
and a case is `S(name) || M<Q>(input) || M<Q>(expected_output) ||
S(matched_row_id)`. The payload after `MAGIC || U16BE(1) || 03` is
`M<Q>(bindings)`. Declarations,
rows, and cases retain declaration order; all schemas and keyed fields use
`M`. `source_sha256` hashes raw UTF-8 source only; other profile hashes hash
their complete framed records.

Static object and list access is total and nullable. A missing key or
out-of-range index becomes null in Rust and Postgres. The type checker marks
access results nullable, so only explicit null operations can consume them;
there is no flow-sensitive conditional refinement. `RuleType::nullable`
normalizes an already nullable inner type rather than creating nested wrappers.
Postgres lowers `&&` and `||` through `CASE` expressions to preserve Rust's
short-circuit behavior without relying on planner evaluation order. Live tests
must use typed row-derived numerator/denominator columns, not a planner-visible
constant `1 / 0`; the latter is a deploy-time literal-zero validation error.

Rules expose typed value evaluation and lowering in addition to boolean guard
wrappers. Decision outputs are typed business data. Capability selection is
enforced by a future command/process consumer's fixed deploy-time mapping, not
by output-field-name substrings.

## Consequences

Metadata validation becomes stricter and reports `metadata_path`, expression
owner/name, and byte spans for semantic errors from both rule expressions and
decision-row conditions. The rules crate adds a small SHA-256 dependency but
remains independent from axum, server runtime, and database clients. Commands
and processes must use the value API only through exactly typed bindings, and
must never let a decision string select a capability dynamically.

## Alternatives

| Option | Why not |
| --- | --- |
| Make missing access a runtime error in both engines | Requires a new SQL helper migration and database-error mapping before command execution exists. |
| Keep raw `AND`/`OR` | PostgreSQL may evaluate an erroring branch that Rust short-circuits. |
| Use table name as revision | A same-name deployment change is not auditable. |
| Use FNV input digests | It is collision-prone and not a stable profile artifact. |
| Ban all decision fields named like roles/connectors | Names are not authority; legitimate business data would be rejected while aliases remain unsafe. |
