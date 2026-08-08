---
name: donat-commands
description: Use when a donat domain operation needs more than one write, a guard at write time, or safety under retry — anything a plain CRUD mutation cannot express.
---

# Commands

A command is one **synchronous database transaction** exposed as an ordinary
GraphQL mutation. Its steps run in order; any guard that fails rolls the whole
thing back. It never calls a provider directly — that is a connector, reached
from a process.

Reach for a command when a plain CRUD mutation is not enough: several writes
that must commit together, a guard that must hold at write time, or an
operation that must be safe to retry.

## The shape

```yaml
name: reserve_grooming_slot
source: default
permissions:
  - role: customer
arguments:
  - name: service_resource_id
    type: uuid!
  - name: slot_key
    type: string!
  - name: request_id
    type: uuid!
steps:
  - name: hold_deadline_is_valid
    assert:
      rule: booking_hold_deadline_is_valid
      with:
        database_time:   { database_time: now }
        hold_expires_at: { arg: hold_expires_at }
        starts_at:       { arg: starts_at }
  - name: booking
    insert:
      table: public.grooming_booking
      object:
        customer_id: { session_variable: x-donat-user-id }
        slot_key:    { arg: slot_key }
        status:      { literal: held }
      returning: [id, customer_id, slot_key, status]
result:
  booking_id: { step: booking, column: id }
  status:     { step: booking, column: status }
idempotency:
  key: { argument: request_id }
  scope:
    - { session_variable: x-donat-user-id }
  retention: 30d
```

`permissions` lists the roles that may call it. There is no admin role to fall
back on — a command with no permission entry is callable by nobody. The steps'
writes still go through the tables' own per-role permissions.

## Value references

Every value in a step is one of these. They are the whole vocabulary; there is
no inline expression.

| Reference | Reads |
|---|---|
| `{ arg: name }` | a command argument |
| `{ literal: value }` | a constant; `{ literal: null, as: string }` for a typed null |
| `{ session_variable: x-donat-user-id }` | the caller's session |
| `{ step: s, column: c }` | one column of an earlier step |
| `{ step: s }` | the whole projection of an earlier step (for `result` or `for_each`) |
| `{ item: c }` | inside a batch step, a column of the current item |
| `{ current_column: c }` | inside an update, the column's value **before** the write |
| `{ rule: r, with: {...} }` | a computed rule result; nests |
| `{ database_time: now }` | the transaction's clock — never a client's |
| `{ activity_key: k, as: uuid }` | a process activity's stable key |

`{ database_time: now }` matters: a deadline compared against a client-supplied
clock is not a deadline.

## Step kinds

**Read**

- `select_one` — `table`, `by` (equality predicate), `returning`,
  `require_found: true`. Use `by` to re-assert ownership and state, not just
  identity: `{ id: {arg: ...}, customer_id: {session_variable: ...}, status: {literal: awaiting_tax} }`
  is a lock on the state machine, not a lookup.
- `select_many` — plus `order_by`, `require_non_empty`, `maximum_rows`. Always
  bound it; a batch with no ceiling is an unbounded statement.

**Guard**

- `assert` — `rule` + `with`. False rolls back the transaction.
- `assert_when` — the same, gated by
  `when: { argument_equals: { argument: outcome, value: label_created } }`.
  Use it where an outcome branch has its own legality rule.

**Compute**

- `project` — `values:` of named results, each a value reference or a nested
  `rule`. This is where arithmetic goes; steps do not inline expressions.
- `fixed_rows` — a small literal table (`maximum_rows`) to feed a batch write,
  when the rows come from the command rather than from a query.

**Write**

- `insert` — `table`, `object`, `returning`.
- `insert_many` — plus `for_each: { step: s }` and `maximum_items`.
- `update` — `table`, `where`, `set`, `returning`, `require_affected: true`.
- `update_many` — plus `for_each`, `by` per item, `require_each: true`, and an
  optional per-row `check: { rule: ..., with: ... }` that can read
  `current_column`.
- `update_when` — an update gated by a `when:` condition; pair it with
  `require_affected: false` when not matching is a legitimate outcome.

`returning` is not optional decoration: it is how a later step, the `result`,
and the row permission's `check` see what was written.

## Guards belong on the step, not before it

```yaml
- name: reserve_stock
  update_many:
    table: public.inventory_stock
    for_each: { step: quoted_lines }
    by: { variant_id: { item: variant_id } }
    set:
      reserved:
        rule: add_int
        with:
          left:  { current_column: reserved }
          right: { item: quantity }
    check:
      rule: can_reserve_stock
      with:
        on_hand:   { current_column: on_hand }
        reserved:  { current_column: reserved }
        requested: { item: quantity }
    require_each: true
```

The `check` is evaluated against the row being written, inside the same
statement. A `select` that reads stock followed by an `assert` would be a
check-then-act race; this is not.

Where two callers can both pass a guard legitimately, the database has to be
the arbiter — put a unique constraint in the migration. `reserve_grooming_slot`
asserts the deadline and relies on `UNIQUE (slot_key)` for the race. See
`donat-schema-and-migrations`.

## Idempotency

```yaml
idempotency:
  key: { argument: request_id }
  scope:
    - { session_variable: x-donat-user-id }
  retention: 30d
```

The key is a client-supplied argument; the scope is what it is unique *within*.
Scope by the caller for a caller-initiated command, or by the domain key
(`slot_key`) where two callers must not both succeed. A replay returns the
original result rather than writing twice.

Give every state-changing command an idempotency key. A mobile client on a
flaky network retries, and "the second charge" is the classic outcome of
leaving this out.

## Effects: handing off to a durable process

A command may commit domain writes **and** the intent to start or signal a
process in the same transaction, through a transactional outbox. It does not
call the process inline.

```yaml
# producer: start intent
effects:
  - start_process:
      process: b2b_order_approval
      process_key: { step: approval, column: id }
      input:
        quote_id:     { step: quote, column: id }
        approval_id:  { step: approval, column: id }
        total_minor:  { step: quote, column: total_minor }
      idempotency_key: { argument: request_id }

# producer: signal intent, from a different command
effects:
  - signal_process:
      process: b2b_order_approval
      signal: approver_decision
      correlate:
        approval_id: { step: approve, column: id }
      payload:
        decision: { literal: approved }
      idempotency_key: { argument: request_id }
```

Exactly one command declares the `start_process` effect for a given process —
that command is the module's public entry point. Everything else signals.

## Conventions worth keeping

- **One command, one domain decision.** `record_payment_outcome` records; it
  does not also ship. Composition is the process's job.
- **Name it as an action in the imperative**: `approve_return`,
  `allocate_inventory`, `release_expired_checkout`.
- **Group files by domain** — `commands/checkout/`, `commands/payments/` — and
  list each one in `commands.yaml`.
- **Bound every batch.** `maximum_rows` and `maximum_items` are not tuning
  knobs; they are what stops one request becoming an unbounded statement.
- **Terminal outcomes get their own command.** `finalize_declined_checkout`,
  `finalize_return_rejection` — a process routing to a named terminal command
  is far easier to audit than a flag on a shared one.

## Files to read

- [`examples/petshop/metadata/commands/booking/reserve-grooming-slot.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/commands/booking/reserve-grooming-slot.yaml) — the
  smallest complete command
- [`examples/petshop/metadata/commands/checkout/begin-checkout.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/commands/checkout/begin-checkout.yaml) — every
  step kind in one file: `select_one`, `select_many`, `assert`, `project`,
  `update_many` with a `check`, `insert`, `insert_many`, `fixed_rows`, `update`
- [`examples/petshop/metadata/commands/b2b/submit-quote.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/commands/b2b/submit-quote.yaml) — `start_process`
- [`examples/petshop/metadata/commands/fulfilment/record-shipment-result.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/commands/fulfilment/record-shipment-result.yaml) —
  `assert_when`
- [`crates/conformance/tests/commands.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/commands.rs) — the behaviour CI asserts
