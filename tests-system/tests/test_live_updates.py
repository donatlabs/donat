"""Watching the store instead of asking it again.

A shopper who has just paid does not reload the page every second — the browser
holds one connection open and the store pushes the order's new state when it
has one. That connection is a fourth surface next to GraphQL, REST and MCP, and
it is bound by the same walls: what a caller may watch is what a caller may
read.
"""

from __future__ import annotations

import pytest

from petshop_qa import LiveTimeout
from petshop_qa import domain as d
from petshop_qa import providers as P

pytestmark = [pytest.mark.providers, pytest.mark.serial]

ORDER_WATCH = """
    subscription Order($id: uuid!) {
      orders(where: {id: {_eq: $id}}) { id order_status }
    }
"""


def status_in(payload, expected: set[str]) -> bool:
    rows = (payload.get("data") or {}).get("orders") or []
    return bool(rows) and rows[0]["order_status"] in expected


def test_a_shopper_watches_their_order_reach_authorized(
    shopper, providers, well_stocked, settle_timeout
):
    """The order changes while the shopper is looking at it.

    The provider is made slow on purpose so the order is still
    `checkout_started` when the connection opens: the test then proves the
    store *pushed* the change rather than that it happened to be there
    already.
    """

    providers.hang(P.AUTHORIZE, delay_ms=6000, times=1)
    known = {order["id"] for order in d.orders_of(shopper)}
    d.start_checkout(shopper, d.cart_with_one_line(shopper)).unwrap()
    order = d.await_new_order(shopper, known=known, timeout=settle_timeout)

    with shopper.live() as live:
        live.watch(ORDER_WATCH, {"id": order["id"]})
        first = live.await_data(
            lambda payload: status_in(payload, {"checkout_started", "authorized"}),
            timeout=settle_timeout,
            description="the order as it stands when the connection opens",
        )
        if status_in(first, {"authorized"}):
            pytest.skip("the authorization completed before the connection opened")

        settled = live.await_data(
            lambda payload: status_in(payload, {"authorized"}),
            timeout=settle_timeout,
            description="the order becoming authorized, pushed rather than polled",
        )

    assert settled["data"]["orders"][0]["id"] == order["id"]


def test_a_connection_watches_only_what_its_holder_may_read(
    shopper, other_shopper, providers, well_stocked, settle_timeout
):
    """Another shopper's connection is not a way around a row filter.

    The subscription names the order by its identifier, which the other shopper
    could have learnt anywhere. What comes back is what they may see: nothing.
    """

    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)

    with other_shopper.live() as live:
        live.watch(ORDER_WATCH, {"id": order["id"]})
        answer = live.await_data(
            lambda payload: "data" in payload,
            timeout=settle_timeout,
            description="the other shopper's first answer",
        )

    assert answer["data"]["orders"] == [], (
        f"a subscription handed over another shopper's order: {answer}"
    )


def test_the_public_cannot_open_a_window_onto_orders(anonymous):
    """A visitor's schema has no orders in it, on this surface either."""

    with anonymous.live() as live:
        live.watch("subscription { orders { id order_status } }")
        answer = live.await_data(
            lambda payload: True,
            timeout=15,
            description="the store's answer to a visitor",
        )

    errors = answer.get("errors") or []
    assert errors, f"the public was given a live view of orders: {answer}"
    assert errors[0]["extensions"]["code"] == "validation-failed"
    assert "orders" in errors[0]["message"]


def test_the_store_stops_pushing_when_the_client_says_stop(shopper, well_stocked):
    """A closed tab costs the store nothing.

    After `stop` the subscription is over: whatever happens to the data, no
    further frame arrives for it.
    """

    cart_id = d.open_cart(shopper)
    watch = """
        subscription Cart($id: bigint!) {
          cart(where: {id: {_eq: $id}}) { id lines { id quantity } }
        }
    """

    with shopper.live() as live:
        live.watch(watch, {"id": cart_id})
        live.await_data(
            lambda payload: "data" in payload,
            timeout=15,
            description="the cart as it stands",
        )
        live.stop()

        # A change the store would certainly have pushed, had it still been
        # asked to.
        d.add_line(shopper, cart_id, d.IN_STOCK_VARIANT, 2).unwrap()
        after_stop = [frame for frame in live.quiet_for(3.5) if frame.get("type") == "data"]

    assert after_stop == [], f"the store kept pushing after stop: {after_stop}"


def test_one_connection_can_watch_several_things_at_once(shopper, well_stocked):
    """Two windows on one socket, each answered under its own id."""

    cart_id = d.open_cart(shopper)

    with shopper.live() as live:
        live.watch("subscription { product(order_by: {id: asc}) { id slug } }", id="catalogue")
        live.watch(
            "subscription Cart($id: bigint!) { cart(where: {id: {_eq: $id}}) { id status } }",
            {"id": cart_id},
            id="cart",
        )

        answered = set()
        deadline_frames = 0
        while len(answered) < 2 and deadline_frames < 12:
            try:
                frame = live.next_frame(timeout=10)
            except LiveTimeout:
                break
            deadline_frames += 1
            if frame.get("type") == "data":
                answered.add(frame.get("id"))

    assert answered == {"catalogue", "cart"}, (
        f"one connection did not answer both subscriptions: {answered}"
    )
