# Sealed on the way out, open on the way in

The read filter is untouched and correct — a shopper still sees only their own
addresses. The guards on update and delete are opened. Which rows a shopper may
*change* is now every row in the table.

Worth knowing why both the `filter` and the `check` had to go, because the
first attempt at this defect only opened the filter and the store went on
refusing the edit: the update `check` names the owner column, so it doubles as
a row guard on the result. A careless author opens both together for the same
reason — "the customer owns their own addresses anyway" — and that is the
defect worth having.

What makes it plausible rather than obviously broken is that everything a
single shopper can observe is exactly right, including the boundary they would
think to test, because reading is where people look for a boundary.

It needs a stranger and an id to see at all. A shopper who learns another
shopper's address id — from a shared parcel, a support thread, a sequential
integer — can rename or delete that person's delivery address without ever
being able to read it. Two of the three `open-row-filter` mutants that survived
the whole Petshop suite on this file were exactly these two filters.

Dies in `two_shoppers_with_addresses_each`, on
`test_one_shopper_cannot_change_anothers_address` (the victim's label comes
back changed) and on `test_one_shopper_cannot_remove_anothers_address` (the
victim's row is gone). Those are deliberately two scenarios rather than one
with two assertions: the reference design governs both with the same `filter`,
so folding them together would leave the task resting on a single reading while
appearing to rest on two.
