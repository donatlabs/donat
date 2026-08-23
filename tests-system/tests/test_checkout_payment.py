"""Checkout, from the basket to the money — and every way it can go wrong.

The shopper calls one entry-point Command; the durable checkout Process quotes,
taxes, creates the order and asks the provider for an authorization. A tester
watches the outcome from outside: the order the shopper sees, the payment
support sees, the shelf, and what the provider was actually sent.
"""

from __future__ import annotations

import threading

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P

pytestmark = pytest.mark.providers


def test_two_racing_checkouts_of_one_cart_produce_one_order(
    shopper, providers, well_stocked, settle_timeout
):
    """Two tabs, two distinct requests, one basket."""

    cart_id = d.cart_with_one_line(shopper)
    before = {order["id"] for order in d.orders_of(shopper)}
    answers: list = []

    def press_pay():
        answers.append(d.start_checkout(shopper, cart_id))

    racers = [threading.Thread(target=press_pay) for _ in range(2)]
    for racer in racers:
        racer.start()
    for racer in racers:
        racer.join()

    accepted = [answer for answer in answers if not answer.errors]
    assert accepted, f"at least one checkout is accepted: {answers}"

    order = d.await_new_order(shopper, known=before, timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

    fresh = [o for o in d.orders_of(shopper) if o["id"] not in before]
    assert len(fresh) == 1, f"one cart, one order — got {fresh}"
    assert len(providers.calls_about(P.AUTHORIZE, order_id=order["id"])) == 1, (
        "and the card is charged once"
    )
