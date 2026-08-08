---
name: donat-rules
description: Use when writing a guard a donat command or process will assert, normalising a provider value into a typed state, or replacing branching logic with a decision table.
---

# Rules, types and decision tables

`metadata/rules.yaml` holds three things: the **types** the whole application
speaks in, the **rules** that decide something, and the **decision tables** that
route. Commands, processes and validators all compile against them, so a guard
written once is the same guard everywhere it is asserted.

## Types

Declare enums and object shapes before anything references them.

```yaml
types:
  - name: PaymentOutcome
    enum: [authorized, declined, failed, challenged, captured, voided, refunded, chargeback]
  - name: PaymentState
    enum: [pending, authorized, captured, voided, refunded, failed, chargeback]
  - name: BookingOutcome
    enum: [confirmed, cancelled, no_show, expired]

  - name: ReturnApprovalLine
    object:
      return_item_id: uuid!
      approved_quantity: int!
```

Scalar spellings are `int!`, `bigint!`, `bool!`, `string!`, `uuid!`,
`timestamptz!`; a trailing `!` means non-null and a list is `"[T!]!"`.

Distinguish the **outcome** a provider reports from the **state** you keep.
`PaymentOutcome` is what came back over HTTP; `PaymentState` is what the row
says. A rule converts one into the other, and that conversion is the only place
the mapping lives.

## Rules

A rule is a name, typed parameters, a result type and one expression.

```yaml
rules:
  - name: can_reserve_stock
    parameters: { on_hand: int!, reserved: int!, requested: int! }
    result: bool!
    expression: "requested > 0 && reserved + requested <= on_hand"

  - name: basis_points_amount
    parameters: { amount_minor: bigint!, basis_points: int! }
    result: bigint!
    expression: "(amount_minor * basis_points) / 10000"

  - name: payment_outcome_is
    parameters: { actual: PaymentOutcome!, expected: PaymentOutcome! }
    result: bool!
    expression: "actual == expected"

  - name: normalize_payment_outcome_state
    parameters: { outcome: PaymentOutcome! }
    result: PaymentState!
    expression: >-
      outcome == PaymentOutcome::authorized ? PaymentState::authorized
      : outcome == PaymentOutcome::challenged ? PaymentState::pending
      : PaymentState::failed
```

Enum members are written `TypeName::member`. Comparison is `==`, boolean
operators are `&&` `||` `!`, and the conditional is `cond ? a : b`. Strings are
double-quoted (escape them inside a YAML double-quoted scalar, or use a block
scalar as above).

Rules are small on purpose. `add_int` and `subtract_minor` exist in the petshop
because a command step may only *assert a rule* or *compute one*, never inline
arithmetic — which is what keeps arithmetic reviewable and testable.

Name a rule after what it decides, not how: `can_capture_payment_amount`,
`return_approved_quantity_is_bounded`, `approval_was_rejected`.

## Nullability

The rule profile **refuses to read a nullable value**, and there is no
flow-sensitive refinement — guarding with `is_null(x) || x > 3` does not help,
because the second arm still reads a nullable value. A rule parameter is
therefore almost always non-null (`int!`), and the caller is responsible for
having proved presence. In a validator that proof is a `not_null` or
`when_present` entry; see `donat-validators`.

## Decision tables

When the logic is "these inputs route to that outcome", a table beats a chain
of conditionals: the rows are reviewable side by side and each has a stable id
that shows up in the result.

```yaml
decision_tables:
  - name: fraud_route
    inputs: { payment_outcome: PaymentOutcome!, score: int! }
    output: { hold: bool!, route: string! }
    hit_policy: first
    rows:
      - id: high_score_review
        when:
          payment_outcome: "payment_outcome == PaymentOutcome::authorized"
          score: "score >= 80"
        output: { hold: true, route: manual_review }
      - id: default_clear
        when: { payment_outcome: "true", score: "true" }
        output: { hold: false, route: clear }
    test_cases:
      - name: high score authorization requires review
        input: { payment_outcome: authorized, score: 90 }
        expect:
          output: { hold: true, route: manual_review }
          matched_row_id: high_score_review
```

- `when` carries **one expression per input column**, and a row matches when
  all of them are true. `"true"` is the idiomatic "don't care".
- `hit_policy: first` takes the first matching row in document order. Always
  give such a table a final all-`"true"` row — a table with no match is an
  error, and "no route" is rarely what the domain means.
- `hit_policy: unique` requires **exactly one** row to match. Use it when
  overlapping rows would be a modelling bug you want reported, not resolved by
  ordering.

## Test cases run at deploy time

`test_cases` is not documentation. Each case asserts both the `output` and the
`matched_row_id`, and a case that does not hold is a **deploy failure** —
`donat validate` reports it and the engine refuses to serve.

Assert the row id, not just the output. Two rows that produce the same output
for different reasons are exactly the pair that a refactor silently swaps, and
the id is what catches it.

Write a case per row, including the default row. A routing table without a case
for its fallback is the one whose fallback stops being reachable.

## Where rules are used

- **Command steps** — `assert: { rule: <name>, with: {...} }` refuses the
  transaction when the rule is false. See `donat-commands`.
- **Process branching** — `when: { cases: [{ rule: ..., next: ... }], default: ... }`.
  See `donat-processes`.
- **Normalisation** — a provider's string becomes a typed state through a rule,
  so no other layer has to know the provider's vocabulary.

## Files to read

- [`examples/petshop/metadata/rules.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/rules.yaml) — 41 types, 60 rules, 10 decision
  tables. The lifecycle gates (`can_transition_*`) are the pattern to copy for
  a state machine's legality rules.
- [`crates/conformance/fixtures/rules/`](https://github.com/donatlabs/donat/tree/main/crates/conformance/fixtures/rules) — the exact contract, including what a
  failing test case and an unmatched `unique` table report.
