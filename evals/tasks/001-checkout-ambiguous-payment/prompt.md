# Checkout payment for the pet store

The store already sells things: customers fill a cart, staff keep stock, and
support answers for the money. What is missing is the step between a full cart
and an order the warehouse can pick — taking the payment.

Build it.

## What already exists

- `start_checkout` is the command the storefront calls. It proves the caller
  owns an open cart, then starts a durable process named **`checkout_payment`**
  with `cart_id` and `request_id`. That name and those inputs are fixed: the
  storefront is already written against them.
- The commands checkout needs are declared and working — quoting a cart,
  opening the order, recording an authorization, finishing a declined
  checkout. Read them before you design anything.
- The store buys two services from outside: a tax service that prices a cart,
  and a payment provider that authorizes a card. Their operations, their
  responses and their failure modes are declared in the connectors.

## What the customer and support must see

- The customer polls their own orders. An order that got paid for reads
  `authorized`; one that did not reads `cancelled`.
- Support can read the payment behind an order: `authorized` when the money is
  held, `failed` when it is not, together with the amount. The amount recorded
  is the amount the provider actually took — which matters in the one case
  where the store learns it late, from the provider rather than from its own
  arithmetic.
- The process ends by reporting `order_id`, `payment_id` and the payment
  outcome.

## The rules the business will hold you to

1. **Never take money twice.** A customer who clicks pay twice, or a retry
   inside the store, must not produce two charges. The provider deduplicates on
   a key it is sent, so every attempt at one order's payment has to carry the
   same one — a second attempt under a fresh key is a second charge, however
   the store thinks of it.
2. **Retry what is worth retrying.** A provider that is briefly unreachable,
   slow, or answers 429 or 5xx should not lose the sale.
3. **Never claim what you cannot prove.** The provider can take the money and
   then fail to answer — a timed-out call proves nothing about whether the card
   was charged. An order must never be treated as paid unless the money is
   really there, and must never be treated as unpaid unless the provider says
   so.
4. **When the provider cannot say, a person decides.** If the outcome cannot be
   established, the checkout must stop somewhere a human can pick it up, with
   the goods still held — not quietly succeed and not quietly cancel.
5. **A declined card releases the stock** it was holding.

## How this will be judged

By running the store. A shopper checks out while the payment provider behaves
in five different ways — including taking the money and going silent — and the
store's account of the money must agree with the provider's, every time.
