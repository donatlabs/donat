# Everything is retried except the one failure that proves nothing

`timeout` is removed from `retry_on`. Transport errors, 429s and 5xxs are still
retried three times; only a call that never came back is treated as final.

The inversion is exact. A 5xx is the provider telling you it failed — the one
case where you know where you stand. A timeout is the provider telling you
nothing at all: the hold may have been released, or the request may never have
arrived. Of the four classes, timeout is the one that most needs another look,
and this defect is the only one that does not get it.

A store built this way looks careful. It has a retry ladder, backoff, jitter
and an idempotency key, and it uses all of them — on the failures that were
never in doubt.

Dies in `provider_times_out_then_voids`, on
`test_a_timed_out_void_is_not_a_lost_void` (the order never reaches
`cancelled`) and on `test_a_timed_out_void_does_not_leave_the_money_held`
(support still reads a live hold). The shopper's view and the books, each
enough on its own.
