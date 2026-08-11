# An outcome nobody recognised is recorded as success

The router over the provider's answer keeps its three known cases — authorized,
declined, failed — and sends everything else to `record_authorization` instead
of to a person.

The provider's outcome vocabulary is wider than those three: a card can come
back `challenged`, waiting on the shopper to complete a step the store never
asked for. Under this design the store books that as money held. The order
reads `authorized`, the warehouse may pick it, and the provider is holding
nothing.

A default branch that means "success" is the most common way a state machine
lies, and it is invisible on the happy path — every scenario that returns a
recognised status passes.

Dies in `provider_challenges_the_card`, on
`test_an_authorized_order_has_money_behind_it` (the books show money that is
not held) and on `test_a_challenged_card_is_not_money_in_the_till` (the
shopper's order reads paid). Two surfaces, either enough on its own.
