# Spec 004 — Declarative decision rules

Status: proposed. Rules provide branch conditions and business decisions used by
commands and processes without embedding arbitrary application code.

## 1. Goal and non-goals

The feature supplies two declarative forms:

- a typed boolean or value expression for guards, derived fields, and routing;
- a named decision table for auditable policy such as pricing bands, approval
  thresholds, and retry routing.

It adopts a deliberately small, type-checked CEL profile rather than a
general-purpose scripting runtime. Expressions are deterministic, bounded,
side-effect free, and cannot read the network, clock, filesystem, environment,
or database beyond values explicitly placed in their context.

Phase 1 excludes user-defined functions, loops, recursion, regular
expressions, dynamic property names, and mutation from expressions. A rule
cannot change permissions or choose an arbitrary SQL identifier.

## 2. Metadata surface

rules.yaml is a top-level metadata section containing rules and decision_tables.

```yaml
types:
  - name: OrderStatus
    enum: [draft, submitted, approved]
  - name: CreateOrderLine
    object:
      sku: string!
      status: OrderStatus!

rules:
  - name: order_request_is_well_formed
    parameters:
      lines: "[CreateOrderLine!]!"
    result: bool!
    expression: size(lines) > 0

decision_tables:
  - name: invoice_approval
    inputs:
      amount: decimal!
      country: string!
    output:
      route: string!
      approval_queue: string
    hit_policy: first
    rows:
      - id: high_value
        when: { amount: "amount > 10000", country: "true" }
        output: { route: manual_approval, approval_queue: finance }
      - id: low_value_us
        when: { amount: "true", country: "country == 'US'" }
        output: { route: automatic, approval_queue: null }
      - id: default
        when: { amount: "true", country: "true" }
        output: { route: manual_approval, approval_queue: finance }
    test_cases:
      - name: US invoice below threshold
        input: { amount: 100, country: US }
        expect:
          output: { route: automatic, approval_queue: null }
          matched_row_id: low_value_us
```

The supported CEL profile contains literals; typed object and list access;
equality, comparison, boolean operators, arithmetic, ternary expressions, and
the size, is_null, startsWith, and endsWith standard functions. It follows CEL's
null-aware behavior only where the result can be compiled without semantic
drift; unsupported null-sensitive constructs are validation errors.
`decimal` is a Donat profile extension mapped to Postgres `numeric`; it is not
silently converted through CEL `double`.

`types` is an optional, ordered declaration list inside the same `rules.yaml`
wrapper. A declaration has exactly one of `enum` (a non-empty list of unique
symbols) or `object` (a non-empty map of static field names to rule type
references). A reference may use a declared name, a scalar profile type, list
brackets, and `!` for non-nullability. Declarations are resolved before rules
and decision tables; duplicate names, primitive-name collisions, unknown
references, and cycles are deploy-time validation errors. Enum symbols use
`TypeName::symbol` in source expressions, so they cannot be mistaken for an
ambient binding. A resolved enum is nominal: its internal form is
`RuleType::Enum { name: String, symbols: Vec<String> }`, and an enum-literal
AST node retains both `enum_name` and `symbol`. Consequently
`OrderStatus::draft == OtherStatus::draft` is rejected even if the symbol text
is shared; an enum value is never silently reduced to a string or to a bare
symbol. A resolved object similarly retains
`RuleType::Object { name: String, fields: BTreeMap<String, RuleType> }`; the
name and lexically framed fields are part of its canonical type identity.

The metadata representation remains exactly one wrapper:
`RulesMetadata { types, rules, decision_tables }`. It is empty only when all
three collections are empty. The metadata writer emits `rules.yaml` whenever
any collection is non-empty, including a types-only wrapper, and it must retain
`types` while omitting empty `rules` and `decision_tables` keys. The loader and
serde round trip accept a types-only `rules.yaml`; introducing a separate
`decision_tables.yaml` or type-only metadata section is prohibited.

Phase 1 supports `first` and `unique` hit policies. Every row has a stable
`id`; its `when` field is a map keyed by every declared input, so YAML key
order is never business semantics. `first` evaluates rows in declaration order
and may overlap; it requires a final all-`true` default row. `unique` evaluates
every row and requires exactly one match at runtime: zero or multiple matches
are a typed decision rejection, not an arbitrary first-row fallback. The
validator type-checks every condition, output value, and test case against the
declared shapes.

`test_cases` are declarative validation fixtures, executed by `donat validate`.
Each asserts both the complete output and `matched_row_id`. A decision output
is business data only: it cannot select `run_as_role`, grant a permission, or
choose an arbitrary connector. Names such as `approval_queue` may label a
declared branch; the branch still names its fixed command and explicit role.

## 3. Context, compilation, and safety

A command or process explicitly binds rule parameters from command arguments,
prior command steps, or process state. A rule never gains access to an ambient
request or an unlisted session variable. Session values must be exposed under a
named typed session parameter to be usable.

For command guards and command result expressions, the rule compiler lowers
the supported profile into a parenthesized, type-cast SQL expression inside the
same generated statement. It uses only validated column identifiers and query
parameters. It never injects CEL source directly into SQL.

For a process transition, evaluation receives the persisted JSON state,
recorded connector result, and declared signal payload. The implementation
uses the same typed AST; it may evaluate the closed data context in Rust, but
must produce the same result as the SQL lowering for the shared Phase-1
profile. Differential tests enforce that requirement.

Rules are compiled and validated when a metadata snapshot is published.
Invalid CEL, unknown names, invalid type conversions, unexhausted decision
tables, and unknown output fields fail donat validate and prevent the new
snapshot from serving.

## 4. Test-first acceptance contract

| Behavior | First failing test | Regression proof |
| --- | --- | --- |
| Boolean expression accepts a permitted order | command conformance fixture | command CTE includes the guard and returns result |
| Type mismatch is rejected at deploy time | metadata unit fixture | donat validate reports rule name and expression location |
| Decision row precedence is stable | metadata unit test | first matching row wins and reports its stable row id |
| Unique-table ambiguity is rejected | rules unit test | zero or multiple matching rows returns a typed decision rejection |
| Default decision row is used | metadata unit test | first-policy final all-true row is selected; no implicit null is produced |
| Decision test case is executable | validate integration | expected output and matched row id are checked before serving |
| SQL and Rust evaluators agree | rule crate property/differential test | generated typed inputs produce identical values |
| Unsafe syntax is rejected | parser unit test | no generated SQL contains CEL source text |

CEL conformance cases are ported one behavior at a time into Donat-owned Rust
tests. The port records the original test identifier and expected result; it
does not import upstream generated code.

## 5. Reference porting plan

| Upstream | Immutable revision | Files/behavior used | License and treatment |
| --- | --- | --- | --- |
| [google/cel-spec](https://github.com/google/cel-spec/tree/cb51b4176013ad19bd00df94be273c322916a620) | cb51b4176013ad19bd00df94be273c322916a620 (v0.25.2) | language semantics and selected conformance/ cases | Apache-2.0; semantics and individual expected-value cases are ported into Donat tests; no generated Go/protobuf code is imported |
| Donat crates/sqlgen and crates/metadata | current Donat revision | type validation, SQL quoting, snapshot practice | native implementation reference |

If a future Rust CEL parser/evaluator is ported or vendored, the change must
add the exact upstream revision, copied file list, notice location, and tests
that prove Donat's restricted profile. The initial implementation is an
independent Rust parser, AST, and lowerer with only the table above as its
reference.


## 6. Normative Donat CEL profile

The implementation stores the original expression, profile version `1`, a
canonical typed AST, a SHA-256 source hash, and a SHA-256 canonical-AST hash.
The same AST is the only input to the Postgres lowerer and the closed-context
process evaluator. A decision-table revision is the SHA-256 hash of its
profile version, resolved type declarations, hit policy, ordered rows,
conditions, and output schema; a same-name changed definition therefore has a
different revision.

### Canonical byte encoding v1

Every canonical artifact is encoded before hashing. This is a protocol, not a
property of a Rust `serde` implementation. `MAGIC` is exactly the 22 bytes
`44 4f 4e 41 54 2d 52 55 4c 45 53 2d 43 41 4e 4f 4e 49 43 41 4c 00`, rendered
as `b"DONAT-RULES-CANONICAL\0"`. The root byte stream is exactly
`MAGIC || U16BE(1) || RECORD`, where `U16BE(1)` is `00 01` and `RECORD` begins
with one root tag: `0x01` typed rule, `0x02` decision definition, or `0x03`
decoded typed input.

#### Common framing

| Form | Exact bytes |
| --- | --- |
| `U32(n)` | unsigned 32-bit big-endian integer `n` |
| `S(s)` | `U32(utf8_bytes(s).len()) || utf8_bytes(s)` |
| `L<X>(items)` | `U32(items.len()) || X(item_0) || ... || X(item_n)` in supplied order |
| `M<X>(entries)` | `U32(entries.len()) || S(key_0) || X(value_0) || ...`, with unique keys sorted by unsigned lexicographic order of their UTF-8 byte sequences |
| `B(v)` | one byte: `00` for false and `01` for true |

Every count and string length is `U32`; no platform-width integer, YAML map
order, or serializer output is permitted. `L` preserves declaration or input
order. `M` is the only collection that sorts, and it always sorts textual keys
by raw UTF-8 bytes rather than locale or Unicode collation.

#### Resolved type grammar

`T` is exactly one tag and payload from this table. A named object is retained
as a nominal declaration name plus its lexically framed fields; nested nullable
wrappers are normalized before encoding.

| `T` tag | Resolved RuleType | Payload after tag |
| --- | --- | --- |
| `10` | bool | none |
| `11` | string | none |
| `12` | int | none |
| `13` | decimal | none |
| `14` | uuid | none |
| `15` | date | none |
| `16` | timestamp | none |
| `17` | enum | `S(enum_name) || L<S>(symbols)` in enum declaration order |
| `18` | list | `T(item_type)` |
| `19` | object | `S(object_declaration_name) || M<T>(field_name -> resolved field type)` |
| `1a` | nullable | `T(inner_type)` |

Thus type identity, each list/nullable wrapper, object declaration name and
fields, and enum name and symbols all contribute bytes.

#### Typed AST grammar

Every `E` is `tag || payload || T(resolved_result_type)`, recursively at every
node. Source spans, descriptions, YAML formatting, and source-map insertion
order are excluded. The node payloads are exact:

| `E` tag | Node | Payload after tag, before trailing result `T` |
| --- | --- | --- |
| `20` | literal | literal sub-tag and literal payload from the next table |
| `21` | binding name | `S(name)` |
| `22` | enum symbol | `S(enum_name) || S(symbol)` |
| `23` | list literal | `L<E>(items)` |
| `24` | member | `E(receiver) || S(field)` |
| `25` | literal index | `E(receiver) || U32(index)` |
| `26` | function call | function tag then `L<E>(arguments)` in call order |
| `27` | unary | unary-operator tag then `E(operand)` |
| `28` | binary | binary-operator tag then `E(left) || E(right)` |
| `29` | conditional | `E(condition) || E(when_true) || E(when_false)` |

| Literal sub-tag | Literal | Payload |
| --- | --- | --- |
| `00` | null | none |
| `01` | bool | `B(value)` |
| `02` | int | `S(minimal_signed_base10)` |
| `03` | decimal | `D(decimal)` defined below |
| `04` | string | `S(value)` |

| Function tag | Function | Unary tag | Operator |
| --- | --- | --- | --- |
| `00` | size | `00` | not |
| `01` | is_null | `01` | negate |
| `02` | startsWith |  |  |
| `03` | endsWith |  |  |

The binary-operator byte is `00` or, `01` and, `02` equal, `03` not_equal,
`04` less_than, `05` less_than_or_equal, `06` greater_than,
`07` greater_than_or_equal, `08` add, `09` subtract, `0a` multiply, or `0b`
divide. Function argument counts are validated by the profile, but their
canonical payload is always the ordered `L<E>` form above.

#### Typed value grammar

`Q` is `T(resolved_type) || V(value)`. `V` is exactly one value tag and payload
below; a `30` null is valid only when its preceding `T` is nullable.

| `V` tag | Value | Payload |
| --- | --- | --- |
| `30` | null | none |
| `31` | bool | `B(value)` |
| `32` | string | `S(value)` |
| `33` | int | `S(minimal_signed_base10)` |
| `34` | decimal | `D(decimal)` |
| `35` | uuid | `S(lowercase_hyphenated_uuid)` |
| `36` | date | `S(ISO-8601_calendar_date)` |
| `37` | timestamp | `S(UTC_RFC3339_timestamp)` |
| `38` | enum | `S(enum_name) || S(symbol)` |
| `39` | list | `L<Q>(items)` in input order |
| `3a` | object | `M<Q>(field_name -> field value)` |

`minimal_signed_base10` has no plus sign or leading zeroes except `0`.
`D(decimal)` is one sign byte (`00` non-negative, `01` negative), then
`S(significant_digits) || U32(scale)` after stripping fractional trailing
zeroes; zero is sign `00`, digits `"0"`, scale `0`. This gives decimals one
canonical mathematical form without converting through floating point.

#### Canonical compiled records

Resolved declarations use `40 || S(enum_name) || L<S>(symbols)` for an enum
and `41 || S(object_name) || M<T>(field_name -> field type)` for an object.
The three root record payloads, immediately after `MAGIC || U16BE(1) || root
tag`, are:

| Root tag | Record payload in exact order |
| --- | --- |
| `01` typed rule | `S(rule_name) || M<T>(binding_name -> binding_type) || T(declared_result) || E(expression)` |
| `02` decision | `S(table_name) || L<declaration>(resolved type declarations) || M<T>(input schema) || M<T>(output schema) || hit_policy` (`00` first, `01` unique) `|| L<row>(rows) || L<case>(test cases)` |
| `03` decoded input | `M<Q>(binding_name -> decoded typed value)` |

A row is `S(row_id) || M<E>(input_name -> checked condition) || M<Q>(output_name
-> typed output)`. A test case is `S(case_name) || M<Q>(input_name -> typed
input) || M<Q>(output_name -> typed expected output) || S(matched_row_id)`.
The declaration, row, and test-case lists retain source declaration order; all
schemas, fields, bindings, conditions, and outputs use `M` and therefore sort.
Descriptions never participate. `source_sha256` is SHA-256 of raw UTF-8 source
only; canonical-AST, decision-revision, and decoded-input digests are SHA-256
of the complete framed records above.

Golden byte and hash vectors are part of the compatibility contract. Tests
must prove map-order invariance and that changes to a type declaration, row
order, condition, output, or decoded input produce a changed affected hash.

| Type | Accepted literal and operations | Notes |
| --- | --- | --- |
| bool | true, false, !, &&, ||, ternary condition | a guard passes only when its result is true |
| string | quoted UTF-8, ==, !=, startsWith, endsWith, size | no regex, locale, or implicit numeric conversion |
| int | base-10 integer, arithmetic and comparison with int | overflow is a validation or runtime rejection, never wraparound |
| decimal | base-10 decimal, arithmetic and comparison with decimal | lowered to Postgres numeric; never converted through double |
| uuid, date, timestamp | typed context values, == and != only | literals require an explicit typed cast form |
| enum | declared `TypeName::symbol`, == and != only within the same named enum | nominal identity includes declared enum name; no conversion to string |
| list<T> | literal list, index by literal integer, size | no comprehensions, map, filter, fold, or dynamic index |
| object | declared field access by static name | no map, reflection, or dynamic property access |

There are no implicit conversions between string, numeric, UUID, temporal, or
enum types. A nullable value may be compared only to null with == or !=, or
passed to is_null. Other operations on nullable operands are metadata
validation errors. This intentionally avoids SQL three-valued logic drift:
is_null(x) compiles to x IS NULL; x == null compiles to x IS NULL; and x !=
null compiles to x IS NOT NULL.

The grammar permits only names, static field selectors, literals, parentheses,
the operators listed above, and the named functions size, is_null, startsWith,
and endsWith. Expression source is limited to 4 KiB, nesting to 64 AST levels,
and list literals to 256 items. These limits are validated at metadata load,
not discovered under production request load.

Static object and literal-list access is total: a missing object key or an
out-of-range list index evaluates to `null` in both the Rust evaluator and
Postgres lowerer. The result of either access is therefore nullable, and may
only be used with `is_null`, `== null`, or `!= null`. The profile has no
flow-sensitive exception for a preceding conditional: an implementation must
not claim that a predicate proves a later access non-null. This avoids a Rust
error versus SQL NULL semantic split while preserving the profile's explicit
null discipline.

`&&` and `||` must have Rust short-circuit semantics in SQL. They lower only
to parenthesized `CASE` expressions (`CASE WHEN left THEN right ELSE FALSE END`
and `CASE WHEN left THEN TRUE ELSE right END`); they do not lower to raw SQL
`AND` or `OR`. This protects a row-derived right operand such as
`false && (numerator / denominator > 0)` when `denominator` is a typed bound
column containing zero. A syntactic literal zero divisor, including the
right-hand `1 / 0` form, is rejected at deploy-time before lowering; `CASE` is
not claimed to protect PostgreSQL planner-time evaluation of a constant
division by zero.

## 7. Explicit bindings and execution contexts

A rule declaration has no ambient variables. Every use supplies every parameter
by name. For example, a command step can read a customer with select_one and
then assert a rule over it:

~~~yaml
- name: customer
  select_one:
    table: public.customers
    by_pk: { id: { arg: customer_id } }
    returning: [id, status, credit_limit]
- name: credit_check
  assert:
    rule: customer_can_order
    with:
      customer: { step: customer }
      requested_total: { arg: requested_total }
    message: customer is not allowed to order
~~~

The consumer validates that bindings are complete, non-duplicate, and
assignable to the declared parameter type. A command rule context may contain
only arguments, prior step values, and explicitly declared session variables.
A process rule context may contain only immutable instance input, current state,
the claimed event payload, and the completed activity result. Neither context
performs a hidden SELECT or observes the current clock.

For a command, a boolean rule is lowered to SQL inside the command statement.
The lowerer accepts only a fully type-checked AST and renders parentheses around
every operator; source text never reaches SQL rendering. For a process, the
same AST evaluates in Rust against JSON decoded into its declared types. A
differential property test generates valid values for every common type and
requires equal SQL and Rust outcomes, including null behavior and decimal
scale.

## 8. Decision-table contract

A decision table is ordered metadata, not a Postgres relation, GraphQL object,
or executable workflow. A row matches when every named `when` expression is
boolean true. Its immutable `id` is included in validation diagnostics and
process audit records; optional descriptions are documentation only.

For `first`, rows may overlap and the first full match supplies the complete
typed output object. A final row is mandatory and must contain literal `true`
for every declared input. For `unique`, rows are evaluated as a set and exactly
one must match; a zero-match or multi-match result is a typed decision
rejection. A table with an unknown/missing condition key, no rows, a malformed
first default, an invalid unique result, or output missing a required field
fails validation or evaluation as stated above.

The engine records a redacted decision trace containing definition revision,
table name, matched row id or rejection class, per-condition booleans, and a
SHA-256 digest of canonical decoded typed input. It does not expose a
decision-table HTTP, GraphQL, REST, or MCP endpoint. A command or process may
consume a typed output field only in a later explicit action; it never creates
implicit control flow. A data field is assignable only to an exactly typed
declared destination. A future route field may map a finite declared enum to
fixed actions at deploy time; no decision value can directly select a role,
permission, command identifier, connector, connector operation, or arbitrary
SQL identifier.

## 9. Rejection and observability contract

A command assert whose rule evaluates false uses the Spec 003
validation-failed GraphQL envelope and rolls back the statement. A process
guard whose rule evaluates false records a transition-log outcome guard_false
with the rule name and redacted bindings, consumes the triggering event, and
does not create a connector job or state transition. This is an expected
business outcome, not a worker crash or a retry.

Rules have no direct GraphQL, REST, or MCP endpoint. Their source, profile
version, canonical AST hash, and referenced definition version are included in
metadata snapshot diagnostics and process transition logs. Every semantic
expression diagnostic carries a stable `metadata_path`, expression owner/name,
and half-open UTF-8 byte span without including raw source or bindings. A rule
error uses a path such as `rules.yaml.rules[0].expression`; a decision-condition
error uses `rules.yaml.decision_tables[0].rows[1].when.amount` and identifies
the table, row id, and input name as its owner. Secrets are never valid rule
bindings.

## 10. Additional validation and TDD matrix

| Test ID | Layer | Required assertion |
| --- | --- | --- |
| rule_rejects_implicit_cast | metadata unit | string plus int and decimal plus int fail validation |
| rule_nullable_comparison_is_explicit | metadata unit | nullable ordering comparison fails; is_null succeeds |
| rule_nullable_wrapper_is_normalized | rules unit | `nullable(nullable(T))` equals `nullable(T)` and nullable fields/items do not add wrappers |
| rule_access_has_no_flow_refinement | rules unit | access remains null-only even below a preceding `is_null` conditional |
| rule_limits_source_and_depth | parser unit | 4 KiB and 64-level boundaries are deterministic |
| rule_semantic_diagnostic_has_metadata_path | validate integration | rule and decision-condition failures report owner, exact metadata path, and byte span |
| rule_binding_is_complete | command/process planner test | missing, extra, and incompatible bindings identify parameter name |
| rule_sql_rust_differential | rules crate property test | same typed input gives the same value in both evaluators |
| rule_sql_short_circuits_row_divisor | live Postgres differential | typed bound denominator column equal to zero is not evaluated for false-and/true-or |
| rule_rejects_literal_zero_divisor | rules unit | a syntactic `1 / 0` expression fails deploy-time validation rather than relying on CASE |
| decision_requires_default_row | metadata unit | table without all-true terminal row is rejected |
| decision_first_row_wins | rules unit | overlapping rows select declaration-order result |
| decision_unique_requires_exactly_one_row | rules unit | zero and multiple matching rows reject with row ids |
| decision_test_case_is_checked | validate integration | expected output and matched row id mismatch prevents deploy |
| decision_trace_is_redacted | process integration | trace exposes row/outcomes but no raw secret binding |
| rule_canonical_encoding_is_stable | rules canonical unit | golden bytes/hashes, lexical map invariance, and semantic-change hash cases are exact |
| process_guard_false_is_terminal_for_event | process integration | event is logged once, no retry and no connector activity |
| rule_sql_is_not_source_text | sqlgen insta | rendered SQL contains validated operators and parameters only |

Every CEL-derived test copied into Donat carries a comment with the upstream
test identifier, commit, and license. The test is rewritten as Rust data and
assertions; upstream generated protobuf or Go source is not imported.

## 11. Reference extraction ledger

| Reference | Immutable files or directory | Allowed use | Donat destination |
| --- | --- | --- | --- |
| google/cel-spec at cb51b4176013ad19bd00df94be273c322916a620 | doc/, conformance/proto2, conformance/proto3, conformance/test | Apache-2.0 semantics and individual expected-value cases | crates/rules tests with upstream identifier comments |
| Donat crates/metadata | current metadata types and loader | directory-format validation and error location style | metadata rule types and loader tests |
| Donat crates/sqlgen | current BoolExp/mutation quoting practice | safe SQL construction and snapshot review | rules SQL lowerer tests |
| Donat crates/server/src/gql.rs | current structured database-error translation | reuse the established GraphQL envelope | command guard integration test |

The google/cel-spec repository announces a future move; this spec pins the
commit above rather than a branch name. A change that imports a parser,
evaluator, test fixture, or generated artifact must first add its exact paths
and notice to the central reference porting register and to this table.


## 12. Component ownership boundaries

| Area | Required ownership | Prohibited shortcut |
| --- | --- | --- |
| Language core | new crates/rules workspace crate | embedding a JavaScript engine or evaluating source text in server handlers |
| Metadata | crates/metadata rule and decision-table declarations | storing untyped JSON expressions until request execution |
| Static checking | crates/rules type checker called from metadata validation | discovering unknown names or casts during a command |
| SQL lowering | crates/rules typed AST to a SQL expression used by crates/sqlgen | interpolating expression source into SQL |
| Process evaluation | crates/rules closed-context evaluator called by process worker | a second ad hoc JSON expression implementation |
| Coverage | crates/rules unit/property tests plus command/process conformance | claiming CEL compatibility from parser tests alone |

The rules crate exports a profile-versioned AST and stable diagnostic location
type. It must not depend on axum, tokio-postgres, or the server crate; this
keeps metadata validation and differential testing independent of HTTP runtime.
