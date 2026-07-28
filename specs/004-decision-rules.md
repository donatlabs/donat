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

The supported CEL profile contains literals; typed object and list access;
equality, comparison, boolean operators, arithmetic, ternary expressions, and
the size, is_null, startsWith, and endsWith standard functions. It follows CEL's
null-aware behavior only where the result can be compiled without semantic
drift; unsupported null-sensitive constructs are validation errors.
`decimal` is a Donat profile extension mapped to Postgres `numeric`; it is not
silently converted through CEL `double`.

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

The implementation stores the original expression, a profile_version, a
canonical typed AST, and a source hash. It does not store CEL protobufs or use
an upstream generated evaluator. The same AST is the only input to the
Postgres lowerer and the closed-context process evaluator.

| Type | Accepted literal and operations | Notes |
| --- | --- | --- |
| bool | true, false, !, &&, ||, ternary condition | a guard passes only when its result is true |
| string | quoted UTF-8, ==, !=, startsWith, endsWith, size | no regex, locale, or implicit numeric conversion |
| int | base-10 integer, arithmetic and comparison with int | overflow is a validation or runtime rejection, never wraparound |
| decimal | base-10 decimal, arithmetic and comparison with decimal | lowered to Postgres numeric; never converted through double |
| uuid, date, timestamp | typed context values, == and != only | literals require an explicit typed cast form |
| enum | declared enum symbol, == and != | no conversion to string |
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
digest of the typed input. It does not expose a decision-table HTTP, GraphQL,
REST, or MCP endpoint. A command or process may consume a typed output field
only in a later explicit action; it never creates implicit control flow.

## 9. Rejection and observability contract

A command assert whose rule evaluates false uses the Spec 003
validation-failed GraphQL envelope and rolls back the statement. A process
guard whose rule evaluates false records a transition-log outcome guard_false
with the rule name and redacted bindings, consumes the triggering event, and
does not create a connector job or state transition. This is an expected
business outcome, not a worker crash or a retry.

Rules have no direct GraphQL, REST, or MCP endpoint. Their source, canonical
AST hash, and referenced definition version are included in metadata snapshot
diagnostics and process transition logs. Secrets are never valid rule bindings.

## 10. Additional validation and TDD matrix

| Test ID | Layer | Required assertion |
| --- | --- | --- |
| rule_rejects_implicit_cast | metadata unit | string plus int and decimal plus int fail validation |
| rule_nullable_comparison_is_explicit | metadata unit | nullable ordering comparison fails; is_null succeeds |
| rule_limits_source_and_depth | parser unit | 4 KiB and 64-level boundaries are deterministic |
| rule_binding_is_complete | command/process planner test | missing, extra, and incompatible bindings identify parameter name |
| rule_sql_rust_differential | rules crate property test | same typed input gives the same value in both evaluators |
| decision_requires_default_row | metadata unit | table without all-true terminal row is rejected |
| decision_first_row_wins | rules unit | overlapping rows select declaration-order result |
| decision_unique_requires_exactly_one_row | rules unit | zero and multiple matching rows reject with row ids |
| decision_test_case_is_checked | validate integration | expected output and matched row id mismatch prevents deploy |
| decision_trace_is_redacted | process integration | trace exposes row/outcomes but no raw secret binding |
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
