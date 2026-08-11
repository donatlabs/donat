# The row filter is opened, and every shopper reads every address book

`filter: {}` on the shopper's read. Every other part of the declaration is
untouched and correct: the columns are right, the writes are still bounded to
the owner, support still sees everything it should. Only the question "which
rows are yours" now answers "all of them".

This is the single most common shape in the mutant sweep and the least visible.
It changes no behaviour the owner can notice — their own addresses are all
still there, still editable, still theirs — so every scenario written from one
customer's point of view passes. It takes a second customer in the world to see
anything at all, and a suite that tests a feature rather than a boundary never
puts one there. Across the corpus this operator survived 14 times in 22, three
of them on this very file.

What it costs is not a feature. It is every shopper's home address, readable by
every other shopper, through the ordinary API with no privilege and no trick.

Dies in `two_shoppers_with_addresses_each`, on
`test_an_address_book_holds_only_its_owners_addresses` (the owner's own list
now contains rows belonging to more than one customer) and on
`test_one_shopper_cannot_read_anothers_address` (the stranger's row, asked for
by id, comes back). Two readings — a list and a lookup — because a store that
merely ordered or paginated the list away would pass the first alone.
