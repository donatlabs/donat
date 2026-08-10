---
name: donat-automation
description: Use when something must happen on a schedule, when a row changes, or when an outside system calls in - cron triggers, event triggers, verified inbound webhooks and actions, and which of them need no receiver at all.
---

# Scheduling, reacting, and being called

Almost none of this needs a service you write. The reflex — "a trigger fires a
webhook, so somebody must receive it" — is wrong for three of the four
primitives, and knowing which is the difference between a YAML change and a
new system to operate.

| Requirement | Primitive | Receiver? |
|---|---|---|
| "if nobody answers within N, do X" | process wait + `deadline` + `on_timeout` | **none** |
| "when this row changes, notify/index/log" | event trigger + in-process handler | **none** (Go host) |
| "the provider calls us back" | inbound connector webhook | **none** |
| "make a document / call a library" | action without `handler:` + Go function | **none** (Go host) |
| "at 03:00 UTC, call *that* system" | cron trigger | a URL, which usually already exists |

The two "(Go host)" rows depend on the engine running inside a Go program —
see `donat-embedded-go`. On the standalone `donat-server`, those same
declarations deliver to a URL instead, and then a receiver is real work.
**Establish which host you are targeting before promising either.**

## "Run it every night" is usually a process deadline

The reflex is cron. Most of the time the requirement is not "at a fixed time",
it is "if this has not happened within N, do that" — and that is a wait with a
deadline, which needs no receiver on any host:

```yaml
- id: await_customer_confirmation
  wait:
    signal: booking_transition_recorded
    correlate: { booking_id: { state: reserve_slot, field: booking_id } }
    deadline: { state: reserve_slot, field: hold_expires_at }
    next: route_hold_transition
    on_timeout: expire_hold        # ← the timer, in-engine

- id: expire_hold
  command: { name: expire_booking_hold, run_as: booking_worker, next: expired }
```

Expiring a hold, releasing an abandoned checkout, escalating an unanswered
approval, dunning a failed renewal — the petshop implements every one this way.
The work is a command; the schedule is the deadline; nothing polls.

**Ask which it is:** *"is this 'at a particular time', or 'if nothing happens
within a while'?"* The second is far more common and much cheaper.

## A cron schedule that means a local time

A `schedule:` with no `timezone:` is UTC, which is what every existing trigger
is. "09:00 for the customer" is a different thing, and it needs the zone —
otherwise it drifts by an hour twice a year:

```yaml
- name: send_reminders
  webhook: '{{REMINDER_URL}}'
  schedule: "0 9 * * 1-5"
  timezone: Europe/Berlin
  dst:
    skipped_time: fire_after_gap   # or: skip
    repeated_time: fire_at_first   # or: fire_at_second
```

`dst` is **required** with `timezone` and refused without it; metadata that
omits it does not load. That is deliberate: on the spring transition the local
time may not exist and on the autumn one it happens twice, and only the author
knows whether a missed nightly run should be made up late (`fire_after_gap`) or
dropped (`skip`, which is logged, never silent). `repeated_time` picks which of
the two instants is *the* run — it fires once either way.

**Ask:** *"on the night the clocks change, should this run late or not at all?"*

## Event triggers — reacting to a write

Declared on the table, fired after the write commits.

```yaml
event_triggers:
  - name: on_loan_recorded
    definition:
      enable_manual: false
      insert: { columns: "*" }
      update: { columns: [status] }     # only a status change fires it
    retry_conf: { num_retries: 3, interval_sec: 5, timeout_sec: 60 }
    webhook: http://in-process/events   # required by the shape; not dialled in-process
```

In the Go host, a function registered under the trigger's name is called
in-process once the transaction commits — no HTTP, no second service:

```go
donat.On(reg, "on_loan_recorded", func(ctx context.Context, ev donat.Event[gen.Loan]) error {
    return nil // notify, index, emit a metric
})
```

- On `update`, `columns` is the **trigger set** — the event fires only when one
  of those columns changed. `"*"` on a busy table is a firehose.
- `payload` inside an operation spec narrows what is **delivered**, separately
  from what triggers it. Deliver the minimum.
- **Gotcha:** retry field names differ from cron's. Event triggers use
  `interval_sec` / `timeout_sec`; cron uses `retry_interval_seconds` /
  `timeout_seconds`. Copying one into the other silently takes defaults.

Two rules that hold on either host:

**An event trigger is not a way to enforce a rule.** It runs after the write
committed. Anything that must *prevent* a bad write is a permission, a
validator, a constraint or a command guard.

**Two writes that must be atomic are not a trigger either.** If a row must
exist if and only if the engine's write exists, a post-commit handler loses it
on a crash. That is `ExecuteTx` in the Go host, or one command with both steps —
which is the better answer wherever it fits, because it needs no Go at all.

## Inbound webhooks — being called, without a receiver

When a *provider* calls in, you do not build an endpoint. The engine exposes
`/v1/connectors/{instance}/webhooks`, verifies the signature against the
connector's `webhook_secret`, and advances a parked process.

```yaml
# connector
config:
  webhook_secret: { value_from_env: STRIPE_WEBHOOK_SECRET }
```

```yaml
# process — a wait on the provider's event rather than on a signal
- id: await_payment
  wait:
    webhook:
      connector: stripe_live
      trigger: payment_intent_succeeded
      correlate:
        client_reference_id: { input: order_id }
      guard:
        rule: payment_is_paid
        with: { payment_status: { event: payment_status } }
    deadline: 1h
    next: confirmed
    on_timeout: timed_out
```

- `correlate` binds the delivery to **this** instance by a domain key. Without
  it, an authentic webhook advances the wrong order.
- `guard` is a rule over the event's own fields — an authentic, correlated
  delivery saying "failed" must not be read as success.
- The signature is verified before anything is matched. A webhook is a signed
  message, not a trusted caller.

This is the primitive most often replaced by a hand-written receiver. Check it
first. (Connectors are standalone-server only; the embedded host refuses them.)

## Actions — a function, not necessarily a service

An action is a typed GraphQL query or mutation. Where its body lives depends on
one key:

- **without `handler:`** — resolved in-process by a registered Go function.
  Bounded, typed, checked at boot. See `donat-embedded-go`.
- **with `handler:`** — an HTTP service you build and operate. That one is a
  real escalation.

```yaml
actions:
  - name: render_loan_receipt
    definition:
      type: mutation
      arguments: [{ name: loan_id, type: uuid! }]
      output_type: LoanReceipt
    permissions:
      - role: member
```

Three things to get right either way:

1. **`permissions` must not be empty.** An action with no permission entries is
   available to **every role** — the one place in the metadata where absent
   means allowed rather than denied. Always list the roles.
2. **`forward_client_headers: false`** unless there is a reason. Forwarding
   sends the caller's credentials onward.
3. **A handler must never read the database directly.** Every read goes back
   through the API as a declared role, or you have built a second permission
   model. An in-process function is held to the same rule — it reads back
   through the engine.

## Database triggers: the narrow carve-out

A PL/pgSQL trigger is not automatically forbidden, but it is the last resort,
because it is invisible to `donat validate`, binds every writer, and cannot see
the caller's role.

**Acceptable:** mechanical bookkeeping with no domain content — stamping
`updated_at`, maintaining a search vector.

**Not acceptable:** anything a command, validator, permission or constraint can
express. "Grant a role when the row is approved" is a command with two steps.
"Stamp a timestamp on a status transition" is a command step. Written as
triggers, those rules leave the permission model and your partner can no longer
read what governs their business.

**The exception worth naming:** writes donat does not mediate — an auth service
inserting into its own table, a bulk import. Nothing declarative can intercept
those, so a trigger is the only place. Say so in a comment, and keep it to the
minimum.

## Choosing

| What they said | Primitive |
|---|---|
| "if nobody approves in two days, escalate" | process wait + `deadline` + `on_timeout` |
| "release the hold when it expires" | same |
| "when an order is paid, tell the warehouse" | event trigger + handler |
| "when an order is paid, also write our audit row" | one command with both writes, or `ExecuteTx` |
| "Stripe calls us when payment settles" | inbound connector webhook |
| "generate a PDF" | action without `handler:` + Go function |
| "call our legacy system on a schedule" | cron trigger → the URL that system already exposes |

## Talking about it to a non-technical partner

> - Is this "at a particular time", or "if nothing happens within a while"?
> - When it fires, what should happen — and is there already a system that
>   would do it, or would we be building one?
> - If it fires twice by accident, what breaks?
> - The provider calling us — do they sign their messages? (If they don't know,
>   that is a question for them, and it matters.)

The third decides whether the receiver must be idempotent, and it is far
cheaper to ask now.

## Checklist

1. Timer work modelled as a process deadline wherever it fits.
2. Host established before promising an in-process handler.
3. Retry field names correct for the trigger kind.
4. Event trigger update `columns` narrowed to what should really fire it.
5. Delivered payload minimal — it leaves the permission system.
6. Nothing atomic implemented as a post-commit handler.
7. Inbound: `webhook_secret`, `correlate`, `guard`, `deadline`, `on_timeout`.
8. Every action lists its `permissions`.
9. Database triggers only for mechanical bookkeeping or unmediated writes, each
   with a comment saying which.
10. `donat validate` green; a fired trigger observed, not assumed.

## Files to read

- [`examples/petshop/metadata/flows/grooming-booking.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/flows/grooming-booking.yaml)
  — a `deadline` + `on_timeout` replacing a scheduled job
- [`examples/petshop/metadata/flows/checkout-payment.yaml`](https://github.com/donatlabs/donat/blob/main/examples/petshop/metadata/flows/checkout-payment.yaml)
  — timers and routed provider errors in one flow
- [`crates/conformance/tests/process_inbound.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/process_inbound.rs)
  — signed provider delivery advancing a parked wait, end to end
- [`crates/conformance/tests/cron_triggers.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/cron_triggers.rs)
  and [`event_triggers.rs`](https://github.com/donatlabs/donat/blob/main/crates/conformance/tests/event_triggers.rs)
  — the delivery contract CI asserts
- [`examples/lending-golang/handlers.go`](https://github.com/donatlabs/donat/blob/main/examples/lending-golang/handlers.go)
  — in-process event handlers, no webhook
- [`examples/lending-golang/metadata/actions.yaml`](https://github.com/donatlabs/donat/blob/main/examples/lending-golang/metadata/actions.yaml)
  — an action resolved by a Go function instead of a handler
