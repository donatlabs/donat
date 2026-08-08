---
name: donat-processes
description: Use when a donat operation spans more than one transaction, waits on a human, timer or provider, must compensate a partial failure, or must survive an engine restart.
---

# Durable processes

A process is a state machine whose journal — state, signal inbox, activity
attempts, timers — lives in the same Postgres database as your rows. A state
transition and the rows it wrote commit together or not at all. It is not a
background job with a status column.

Only a definition declaring `kind: process` is durable.

## When a process, and when a command

| Need | Use |
|---|---|
| Several writes that commit together, now | a **command** |
| Wait for a person, a timer or a provider | a **process** |
| Call an external system | a process state; the connector is never called from a command |
| Compensate a partial failure | a **process** |

A command is synchronous and returns. A process outlives the request.

## The shape

```yaml
name: grooming_booking
kind: process
version: 1
source: default
permissions:
  - role: customer
    owner_session_variable: x-donat-user-id
owner:
  type: string!
  capture: { session_variable: x-donat-user-id }
input:
  - { name: slot_key, type: string! }
  - { name: hold_expires_at, type: timestamptz! }
  - { name: request_id, type: uuid! }
output:
  - { name: booking_id, type: uuid! }
  - { name: status, type: BookingState! }
idempotency:
  key: { input: request_id }
  scope:
    - { input: slot_key }
signals:
  - name: booking_transition_recorded
    correlation:
      booking_id: uuid!
    payload:
      outcome: BookingOutcome!
start_at: reserve_slot
states: [...]
```

`owner.capture` freezes the caller's identity into the instance at start. Every
later state that runs `run_as: caller` runs as that captured session — which is
how a process makes permission-checked reads on the caller's behalf without
anything resembling a bypass.

`version: 1` plus pinned revisions means a running instance keeps the
definition it started under. Deploying a new version does not rewrite history
mid-flight.

## Entry point

Exactly one command declares the `start_process` effect for a process, and that
command is the module's public entry point. Clients call the command; nothing
calls the process directly.

```graphql
mutation { start_checkout(cart_id: 1, request_id: "…") { cart_id owner_user_id } }
```

Everything after that is signals.

## State kinds

### `command` — a transactional step

```yaml
- id: reserve_slot
  command:
    name: reserve_grooming_slot
    run_as: caller               # or a declared worker role, e.g. booking_worker
    arguments:
      slot_key: { input: slot_key }
      request_id: { input: request_id }
    next: await_customer_confirmation
```

`run_as: caller` uses the captured owner session. A named role is an ordinary
role with its own explicit table permissions — `booking_worker`,
`payment_worker`, `fulfilment`. None is privileged.

### `request` — a connector call

```yaml
- id: request_tax_quote
  request:
    connector: mock_tax
    operation: quote_order
    input:
      checkout_quote_id: { state: prepare_quote, field: checkout_quote_id }
    timeout:
      schedule_to_start: 5s
      start_to_close: 3s
    retry:
      retry_on: [transport, timeout, http_429, http_5xx]
      max_attempts: 3
      initial_interval: 100ms
      max_interval: 1s
      jitter: deterministic_full
    next: checkout
    on_error:
      routes:
        - { kinds: [authentication, validation], next: tax_quote_failed }
        - { kinds: [permanent, invariant, timeout, retry_exhausted], next: tax_quote_failed }
      fallback: { next: tax_quote_failed }
```

`on_error` is not optional in practice: an error class with no route is a
process that stops where nobody is looking. Route every class, and give the
`fallback` somewhere real to go.

### `wait` — park on a signal or a timer

```yaml
- id: await_customer_confirmation
  wait:
    signal: booking_transition_recorded
    role: customer
    verification: required
    persist_before_match: true
    correlate:
      booking_id: { state: reserve_slot, field: booking_id }
    deadline: { state: reserve_slot, field: hold_expires_at }
    next: route_hold_transition
    on_timeout: expire_hold
```

- `correlate` binds the signal to *this* instance by a domain key.
- `persist_before_match: true` means a signal that arrives before the wait
  became receptive is recorded and matched, not lost. Turn it on wherever the
  signalling command can commit first — which is most places.
- `deadline` takes a duration (`2d`) or a value from an earlier state, and
  `on_timeout` names the state that handles it. A wait without a timeout route
  can park forever.
- `role` and `verification: required` mean the signal is authenticated; a
  signal is not a webhook you trust.

### `when` — branch on rules

```yaml
- id: route_hold_transition
  when:
    cases:
      - rule: booking_outcome_is
        with:
          actual:   { state: await_customer_confirmation, field: outcome }
          expected: { literal: confirmed }
        next: confirmed
      - rule: booking_outcome_is
        with: { actual: {...}, expected: { literal: cancelled } }
        next: cancelled
    default: unexpected_hold_transition
```

Give `default` a `fail` state rather than a happy path. An outcome nobody
anticipated should stop loudly.

### `for_each` — bounded fan-out

```yaml
- id: request_payouts
  for_each:
    input: { state: create_payout_candidates, field: payouts }
    item_key: vendor_id
    max_items: 64
    max_concurrency: 8
    completion: collect
    request:
      connector: mock_payout
      operation: create_payout
      input:
        payout_id: { item: id }
    next: reconcile_payout_requests
```

Each item gets its own journal entry, so a partial failure is inspectable per
item. `completion: collect` yields `successful_items` and `failed_items`, which
the next state routes on — `preserve_input: true` carries the original item
through for a lookup pass. `max_items` and `max_concurrency` are required
discipline, not tuning.

### `output` and `fail` — terminals

```yaml
- id: confirmed
  output:
    values:
      booking_id: { state: reserve_slot, field: booking_id }
      status: { literal: confirmed }

- id: unexpected_hold_transition
  fail:
    code: unexpected_booking_hold_transition
    message: The grooming booking hold received an invalid transition
```

Every branch ends in one or the other. A `fail` code is a contract — name it
after what was violated, and keep it stable.

## Value references inside states

| Reference | Reads |
|---|---|
| `{ input: name }` | the process input |
| `{ state: s, field: f }` | a field of an earlier state's result |
| `{ item: f }` | inside `for_each`, the current item |
| `{ literal: v }` | a constant |
| `{ activity_key: k, as: uuid }` | a stable per-activity key |
| `{ session_variable: … }` | the captured owner session |

`activity_key` is the mechanism behind at-most-once effects: the key is stable
across retries and worker takeover, so a provider that honours an idempotency
header sees one operation no matter how many attempts happen.

## Ambiguity is a branch, not an error

A timeout does not mean "it did not happen". When a mutation may have taken
effect, do not guess — declare the read-only lookup that decides and route on
its answer:

```
request_authorization → (timeout) → lookup_authorization → mutation_found → …
                                                        → terminal_absence → …
                                                        → unproven → fail
```

Money or inventory stays claimed while the lookup cannot prove the terminal
effect or its absence, and an unproven outcome becomes a bounded manual
reconciliation rather than a silent write-off. This is the single most valuable
pattern in the petshop; copy it for every provider mutation that can be
ambiguous.

## Operating one

```sh
donat process inspect        --source default --instance <uuid>   # read-only
donat process verify-history --source default --instance <uuid>   # non-zero on inconsistency
```

The journal tables live in the `donat` schema. There is no admin HTTP surface
for them by design.

## Files to read

- [`examples/petshop/metadata/flows/grooming-booking.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/flows/grooming-booking.yaml) — the smallest
  complete process: command, wait with deadline, `when`, three terminals
- [`examples/petshop/metadata/flows/checkout-payment.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/flows/checkout-payment.yaml) — connector requests,
  retry and error routing, the ambiguity lookup
- [`examples/petshop/metadata/flows/vendor-payout.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/flows/vendor-payout.yaml) — bounded fan-out with
  `collect` and a reconciliation pass
- [`crates/conformance/tests/petshop_process.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/petshop_process.rs) — the flows driven end to end
