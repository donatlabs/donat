"""A shopper changes their mind while the card is still being asked.

This is the hardest moment in a store: the order exists, the provider has been
sent an authorization, and nobody yet knows whether it committed. The store may
not guess. It asks the provider what happened and acts on the answer — release
the shelf if nothing was taken, take the money back if something was.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import stays, until

pytestmark = [pytest.mark.providers, pytest.mark.serial]


def order_in_checkout(shopper, providers, settle_timeout) -> dict:
    """An order whose authorization is still in flight.

    The provider is made slow on purpose, which is the only honest way to stand
    in the window a real cancellation lands in.
    """

    # Every attempt is held, not just the first: a timeout is retryable, so a
    # single held call would be followed by a fast one and the window would
    # close before the shopper could reach for cancel.
    providers.hang(P.AUTHORIZE, delay_ms=8000, times=3)
    before = {order["id"] for order in d.orders_of(shopper)}
    d.start_checkout(shopper, d.cart_with_one_line(shopper)).unwrap()
    order = d.await_new_order(shopper, known=before, timeout=settle_timeout)
    return until(
        lambda: [row for row in d.orders_of(shopper) if row["id"] == order["id"]][0],
        lambda row: row["order_status"] == "checkout_started",
        timeout=settle_timeout,
        description="the order to be waiting on its authorization",
    )


def cancel(shopper, order_id: str):
    return shopper.graphql(
        """
        mutation Cancel($order: uuid!, $request: uuid!) {
          cancel_order(order_id: $order, request_id: $request) { order_id order_status }
        }
        """,
        {"order": order_id, "request": d.new_request_id()},
    )


def lookup_says_authorized(providers, amount_minor: int) -> None:
    """The provider answering that the in-doubt authorization did commit.

    Every lookup in the run answers the same way: the checkout Process makes
    its own once the held authorization times out, and both it and the
    cancellation must see one consistent account of what the provider did.
    """

    providers.script(
        P.LOOKUP,
        times=10,
        # Scoped to the authorization: a cancellation may also ask about the
        # void, and an unscoped answer would tell it the void was authorized.
        when={"resource_kind": "authorization"},
        patch={
            "found": True,
            "terminal_absence_proven": False,
            "outcome": "authorized",
            "provider_mutation_id": f"auth_recovered_{uuid.uuid4().hex[:8]}",
            # Its own event id: the store files every provider event under a
            # unique key, so a lookup that reported an id already recorded
            # would be refused rather than acted on.
            "provider_event_id": f"evt_recovered_{uuid.uuid4().hex[:8]}",
            "provider_reference": f"ref_recovered_{uuid.uuid4().hex[:8]}",
            "amount_minor": amount_minor,
        },
    )


def test_an_authorization_the_provider_did_commit_is_voided(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The in-doubt call turns out to have gone through, so the money goes back.

    This is the branch that exists because a timeout is not evidence: the store
    looks the operation up, finds it did commit, records it, and then voids it.
    Skipping the lookup would leave a shopper charged for a cancelled order.
    """

    order = order_in_checkout(shopper, providers, settle_timeout)
    lookup_says_authorized(providers, order["total_minor"])

    cancel(shopper, order["id"]).unwrap()

    providers.await_call(P.VOID, timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"voided"}, timeout=settle_timeout
    )

    assert payment["status"] == "voided", (
        "an authorization the provider confirms is returned, not forgotten"
    )
    assert providers.count(P.CAPTURE) == 0, "nothing is ever captured on a cancelled order"


def lookup_says_voided(providers) -> None:
    """The provider's account of the void the store could not confirm.

    Scoped to the void lookup: the same endpoint answers for the authorization,
    and the two questions have different answers in this scenario.
    """

    providers.script(
        P.LOOKUP,
        times=10,
        when={"resource_kind": "void"},
        patch={
            "found": True,
            "terminal_absence_proven": False,
            "outcome": "voided",
            "provider_mutation_id": f"void_recovered_{uuid.uuid4().hex[:8]}",
            "provider_event_id": f"evt_void_{uuid.uuid4().hex[:8]}",
            "provider_reference": f"ref_void_{uuid.uuid4().hex[:8]}",
        },
    )


def test_a_void_the_store_could_not_confirm_is_reconciled(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The authorization committed, and then the void call got no answer.

    Neither call can be trusted on its own, so the store asks the provider what
    happened to the void. It says the hold was released, and that is what gets
    recorded — the shopper is not left holding a charge for a cancelled order,
    and the store does not void twice.
    """

    order = order_in_checkout(shopper, providers, settle_timeout)
    lookup_says_authorized(providers, order["total_minor"])
    lookup_says_voided(providers)
    providers.fail(P.VOID, status=503, times=10)

    cancel(shopper, order["id"]).unwrap()

    providers.await_call(P.VOID, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"voided"}, timeout=settle_timeout
    )

    assert payment["status"] == "voided", (
        "a void the provider says it performed is recorded from its own account"
    )
    assert providers.count(P.CAPTURE) == 0, "nothing is captured on a cancelled order"
    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)


def test_a_void_nobody_can_account_for_is_left_alone(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The void call failed and the provider will not say whether it landed.

    The one answer the store must not round: it neither claims the money is
    back nor tries again blindly. The hold stays exactly as the provider last
    confirmed it, for a person to settle.
    """

    order = order_in_checkout(shopper, providers, settle_timeout)
    lookup_says_authorized(providers, order["total_minor"])
    providers.script(
        P.LOOKUP,
        times=10,
        when={"resource_kind": "void"},
        patch={"found": False, "terminal_absence_proven": False, "outcome": "failed"},
    )
    providers.fail(P.VOID, status=503, times=10)

    cancel(shopper, order["id"]).unwrap()

    providers.await_call(P.VOID, timeout=settle_timeout)
    stays(
        lambda: [payment["status"] for payment in d.payments_of(support, order["id"])],
        lambda statuses: "voided" not in statuses,
        duration=4,
        description="an unaccounted void to stay unrecorded",
    )
    assert providers.count(P.CAPTURE) == 0, "an order being cancelled is never captured"
