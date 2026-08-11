# Cancelling an order the store has already charged for

The store takes the money when a shopper checks out: the payment provider puts
a hold on their card and the order reads `authorized`. Sometimes the shopper
changes their mind before the goods leave the warehouse. What is missing is the
step that undoes the charge — releasing the hold and closing the order.

Build it.

## What already exists

- `request_authorized_order_cancellation` is the mutation the storefront calls.
  It proves the caller owns the order, claims the row set for the void, then
  starts a durable process named **`authorized_order_cancellation`** with
  `order_id`, `owner_user_id`, `payment_id`, `authorization_id` and `reason`,
  keyed for idempotency on the caller's `request_id`. That name and those
  inputs are fixed: read the command file — it is the contract, not this
  paragraph.
- The commands this needs are declared and working — voiding a payment,
  recording what the provider said, finishing the cancellation, releasing the
  reserved stock. Read them before you design anything.
- The payment provider is declared as a connector: the operation that releases
  a hold, the operation that asks what happened to an earlier request, their
  responses, and their failure modes.

## What the shopper and support must see

- The shopper polls their own orders. An order whose hold was released reads
  `cancelled`.
- Support can read the payment behind an order: `voided` once the money is
  released. Until then the money is still with the provider, whatever the
  claim the mutation has already written says.
- A cancellation the provider refuses leaves the order as it was — the shopper
  is not told their money is coming back when it is not, and the order does
  not read `cancelled`.

## The rules the business will hold you to

1. **A stumble is not an answer.** The provider can be briefly unreachable, be
   slow, or answer 429 or 5xx. None of those mean the hold still stands, and
   none of them may cost the shopper their refund.
2. **Silence is not an answer either.** A call that times out proves nothing
   about whether the hold was released. Deciding either way on silence is
   deciding something the provider never told you — but *asking again* is not
   deciding. Releasing a hold twice costs nobody anything, unlike charging
   twice, so the store is free to keep asking until it gets an answer. Handing
   a slow provider to a human is a decision too, and the shopper pays for it.
3. **"No" is an answer.** If the provider refuses the void, the money is still
   held, and the order must not read `cancelled`.
4. **Never release twice.** A shopper who clicks cancel twice, or a retry
   inside the store, must not produce two voids.
5. **Leave a person a case, not a mess.** When the store genuinely cannot
   establish what happened, the order must end up somewhere support can pick it
   up — never silently cancelled, never silently stuck.

## How you will be judged

By what a shopper and support see afterwards, in each of the ways the provider
can behave. Not by which states you chose, how you named them, or whether you
recover by retrying or by reconciling afterwards — those are yours to decide.

You may write `flows/authorized-order-cancellation.yaml` and add its include to
`flows.yaml`. Nothing else in the store is yours to change.
