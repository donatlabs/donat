"""When a shopper cancels an order the store has already charged for.

The money is held by the provider, not by the store, so cancelling is a
conversation with a third party that can stumble, go quiet, or refuse. Every
scenario is one of those behaviours, the cancellation a shopper asks for, and
what must be true afterwards.

Nothing here names a state, a command, a retry policy or a table. A store that
retries the void and a store that reconciles it afterwards are both right; what
neither may do is keep the shopper's money because the first call did not land.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import stays

pytestmark = [pytest.mark.providers, pytest.mark.serial]

#: Longer than the flow's own patience for one call, so the attempt times out
#: rather than merely being slow.
SILENCE_MS = 7000


# -- the worlds --------------------------------------------------------------


def provider_voids(providers) -> None:
    """The ordinary answer: asked to release the hold, the provider does."""


def provider_stumbles_then_voids(providers) -> None:
    """One 5xx, then the provider is itself again."""

    providers.fail(P.VOID, status=500, times=1)


def provider_times_out_then_voids(providers) -> None:
    """The first call never comes back; a later one does."""

    providers.hang(P.VOID, delay_ms=SILENCE_MS, times=1)


def provider_refuses_the_void(providers) -> None:
    """A real answer, and the answer is no: the hold stands."""

    providers.script(P.VOID, times=5, patch={"status": "failed"})


# -- what a shopper does -----------------------------------------------------


def an_authorized_order(shopper, timeout: float) -> dict:
    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=timeout)
    return order


def cancel(shopper, order: dict) -> None:
    shopper.graphql(
        """
        mutation Cancel($order: uuid!, $reason: String!, $request: uuid!) {
          request_authorized_order_cancellation(
            order_id: $order, reason: $reason, request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "reason": "changed my mind",
         "request": d.new_request_id()},
    ).unwrap()


def order_status(shopper, order_id: str) -> str | None:
    for order in d.orders_of(shopper):
        if order["id"] == order_id:
            return order["order_status"]
    return None


def money_still_held(support, order_id: str) -> bool:
    return any(p["status"] == "authorized" for p in d.payments_of(support, order_id))


# -- the provider answers ----------------------------------------------------


def test_a_cancelled_order_gives_the_money_back(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The plain case, and the one every other scenario is measured against."""

    provider_voids(providers)
    order = an_authorized_order(shopper, settle_timeout)

    cancel(shopper, order)

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)
    d.await_payment_status(support, order["id"], {"voided"}, timeout=settle_timeout)


# -- the provider stumbles ---------------------------------------------------


def test_a_stumbling_provider_still_gives_the_money_back(
    shopper, providers, well_stocked, settle_timeout
):
    """One 5xx is not the shopper's problem.

    What the shopper is told, on its own: the cancellation they asked for
    happened. How the store got there — another attempt, or a reconciliation
    afterwards — is the store's business.
    """

    order = an_authorized_order(shopper, settle_timeout)
    provider_stumbles_then_voids(providers)

    cancel(shopper, order)

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)


def test_a_stumble_does_not_leave_the_money_held(
    shopper, support, providers, well_stocked, settle_timeout
):
    """And what the books say, on its own.

    Support reads the payments; a store that gave up after one failed call is
    still holding a shopper's money against an order nobody is going to fulfil,
    and this reading catches that without asking the shopper anything.
    """

    order = an_authorized_order(shopper, settle_timeout)
    provider_stumbles_then_voids(providers)

    cancel(shopper, order)

    d.await_payment_status(support, order["id"], {"voided"}, timeout=settle_timeout)
    assert not money_still_held(support, order["id"]), (
        "the hold outlived the order it was taken for"
    )


# -- the provider goes quiet -------------------------------------------------


def test_a_timed_out_void_is_not_a_lost_void(
    shopper, providers, well_stocked, settle_timeout
):
    """A call that never came back is not an answer.

    Silence says nothing about whether the hold was released, so a store that
    treats it as final has decided something the provider never told it.
    """

    order = an_authorized_order(shopper, settle_timeout)
    provider_times_out_then_voids(providers)

    cancel(shopper, order)

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)


def test_a_timed_out_void_does_not_leave_the_money_held(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The same silence, read from the books instead of from the order."""

    order = an_authorized_order(shopper, settle_timeout)
    provider_times_out_then_voids(providers)

    cancel(shopper, order)

    d.await_payment_status(support, order["id"], {"voided"}, timeout=settle_timeout)
    assert not money_still_held(support, order["id"]), (
        "a call that timed out was allowed to keep the shopper's money"
    )


# -- the provider says no ----------------------------------------------------


def test_a_refused_void_leaves_the_order_as_it_was(
    shopper, providers, well_stocked, settle_timeout
):
    """"No" is an answer, and the store must not improve on it.

    Here to keep the other scenarios honest: a store that simply cancels
    whatever it is asked to cancel passes all of them and is wrong. The hold
    stands, so the order does too.
    """

    order = an_authorized_order(shopper, settle_timeout)
    provider_refuses_the_void(providers)

    cancel(shopper, order)

    # Only the order is read here. The mutation claims the payment row before
    # the process starts, so "still authorized" is not what a refused void
    # looks like in the books — asserting it would assert something false.
    stays(
        lambda: order_status(shopper, order["id"]),
        lambda status: status != "cancelled",
        duration=min(10.0, settle_timeout),
        description="an order cancelled on a void the provider refused",
    )
