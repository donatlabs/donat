# "I cannot find it" is read as "it did not happen"

The reconciliation router gains one case: a lookup that comes back `found:
false` is treated as a decline. The order is cancelled and the stock goes back
on the shelf.

That is right exactly when the provider can *prove* the absence, and wrong
otherwise — and this design never looks at the difference. The lookup answers
both `found` and `terminal_absence_proven` for that reason. A provider that is
merely behind, or whose index has not caught up, says "not found" and proves
nothing; cancelling on it releases goods for a card that may well have been
charged.

This is the subtlest of the three, because it passes every world where the
provider is decisive, including the one where absence really is proven.

Dies in `provider_times_out_and_cannot_prove_absence`, on
`test_an_unproven_absence_is_not_a_decline` (the order is cancelled on a
silence) and on `test_no_refusal_is_recorded_that_the_provider_never_gave`
(the books record a refusal nobody made). Two surfaces, either enough on its
own.
