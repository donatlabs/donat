# One try at releasing the hold, and the first stumble keeps the money

`max_attempts` on the void drops from 3 to 1. The retry classes are untouched —
transport, timeout, 429 and 5xx are still declared retryable — so the flow
reads as if it retries. It just never gets a second attempt.

This is the cheapest possible defect and the hardest to see in review: nothing
is misrouted, no state is missing, no rule is contradicted. A single 5xx from
the provider — the most ordinary thing a payment API does — ends the run with
the shopper's money still held against an order that is being cancelled.

It survives every scenario where the provider answers on the first call, which
is every scenario a suite written from the happy path would contain.

Dies in `provider_stumbles_then_voids`, on
`test_a_stumbling_provider_still_gives_the_money_back` (the shopper's own
orders never reach `cancelled`) and on
`test_a_stumble_does_not_leave_the_money_held` (support reads a payment that is
still holding funds). Two surfaces, one defect: the shopper is not told and the
books are wrong, and either one alone is enough to condemn it.
