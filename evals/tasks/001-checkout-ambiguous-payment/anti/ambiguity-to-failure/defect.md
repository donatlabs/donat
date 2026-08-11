# The ambiguous route gives up instead of finding out

An authorization that ends in a timeout or an exhausted retry is sent straight
to the state a human picks up. Nothing ever asks the provider what it did.

This is a defensible-looking design — it never claims anything it cannot prove,
which is rule 3 — and it is still wrong, because rule 4 only allows a person to
be handed the problem when the outcome *cannot* be established. Here it can:
the provider will answer a read-only lookup. A shopper whose card was charged
gets an order that never becomes `authorized`, while the money sits at the
provider.

The compiler forces the defect to be honest. Leaving the reconciliation states
in place while routing past them is rejected — a state unreachable from
`start_at` is a compile error — so the patch removes them, which is exactly
what this design amounts to.

Dies in `provider_times_out_after_charging`, on
`test_a_charge_the_store_never_learned_about` (the shopper is told the sale
failed while the provider holds their money) and on
`test_an_ambiguous_charge_is_investigated_not_written_off` (nothing is left
for a person to look at). Two surfaces, either enough on its own.
