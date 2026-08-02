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
from petshop_qa import stays, until

pytestmark = pytest.mark.providers


def _checkout(shopper, providers, timeout, *, request_id=None):
    """Start a checkout and return the order the Process created."""

    before = {order["id"] for order in d.orders_of(shopper)}
    d.start_checkout(shopper, d.cart_with_one_line(shopper), request_id).unwrap()
    return d.await_new_order(shopper, known=before, timeout=timeout)


# -- the happy path ----------------------------------------------------------


def test_a_shopper_checks_out_and_the_order_is_authorized(
    shopper, support, providers, well_stocked, settle_timeout
):
    order = _checkout(shopper, providers, settle_timeout)

    settled = d.await_order_status(
        shopper, order["id"], {"authorized"}, timeout=settle_timeout
    )
    payment = d.await_payment_status(
        support, order["id"], {"authorized"}, timeout=settle_timeout
    )

    assert payment["amount_minor"] == settled["total_minor"], (
        "the store asks the provider for exactly what it charged the shopper"
    )
    assert payment["currency"] == settled["currency"]


def test_the_provider_is_asked_once_for_the_order_total(
    shopper, providers, well_stocked, settle_timeout
):
    order = _checkout(shopper, providers, settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

    providers.await_call(P.AUTHORIZE)
    assert providers.count(P.TAX_QUOTE) == 1, "one tax quote per checkout"
    assert providers.count(P.AUTHORIZE) == 1, "one authorization per checkout"

    call = providers.last_call(P.AUTHORIZE)
    assert call["body"]["amount_minor"] == order["total_minor"]
    assert call["body"]["currency"] == "USD"
    assert call["body"]["order_id"] == order["id"]
    # The credential is resolved from the environment, never from metadata.
    assert call["headers"]["authorization"] == "petshop-payment-token"


def test_the_checked_out_cart_is_closed_behind_the_shopper(
    shopper, providers, well_stocked, settle_timeout
):
    cart_id = d.cart_with_one_line(shopper)
    d.start_checkout(shopper, cart_id).unwrap()

    # The Process owns the cart from here, so the close is observed, not assumed.
    until(
        lambda: d.read_cart(shopper, cart_id)["status"],
        lambda status: status != "cart_open",
        timeout=settle_timeout,
        description="the checked-out cart to stop being open",
    )

    # And it cannot be checked out a second time.
    again = d.start_checkout(shopper, cart_id)
    assert again.error_code() == "validation-failed"


# -- the provider says no ----------------------------------------------------


@pytest.mark.serial
def test_a_declined_card_cancels_the_order_and_charges_nothing(
    shopper, support, providers, well_stocked, settle_timeout
):
    providers.decline_authorization()

    order = _checkout(shopper, providers, settle_timeout)

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"failed"}, timeout=settle_timeout
    )

    assert payment["status"] == "failed"
    assert providers.count(P.CAPTURE) == 0, "a declined order is never captured"


@pytest.mark.serial
def test_a_declined_order_puts_the_stock_back(
    shopper, staff, providers, well_stocked, settle_timeout
):
    before = d.stock(staff, d.IN_STOCK_VARIANT)
    providers.decline_authorization()

    order = _checkout(shopper, providers, settle_timeout)
    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)

    after = d.stock(staff, d.IN_STOCK_VARIANT)
    assert after["reserved"] == before["reserved"], (
        "a refused checkout must not keep holding the shelf"
    )
    assert after["on_hand"] == before["on_hand"]


# -- the provider is broken --------------------------------------------------


@pytest.mark.serial
def test_a_failing_provider_leaves_the_order_for_a_human_and_takes_no_money(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The authorization is in doubt, so the store proves it never happened.

    A terminal error is not evidence that the provider skipped the mutation, so
    the Process looks the operation up before deciding. The lookup proves an
    absence, nothing matches `found: true`, and the run ends in
    `authorization_requires_manual_reconciliation`: no money, no authorized
    order, and a case for support rather than a silent success.
    """

    providers.fail(P.AUTHORIZE, status=500, times=10)

    order = _checkout(shopper, providers, settle_timeout)
    providers.await_call(P.LOOKUP)

    lookup = providers.last_call(P.LOOKUP)
    assert lookup["body"]["resource_id"] == order["id"]
    assert lookup["body"]["resource_kind"] == "authorization"

    # Nothing was authorized, and nothing quietly becomes authorized later.
    stays(
        lambda: d.orders_of(shopper)[0]["order_status"],
        lambda status: status not in {"authorized", "paid"},
        duration=3,
        description="an unproven authorization stays unauthorized",
    )
    assert [payment["status"] for payment in d.payments_of(support, order["id"])] == [
        "pending"
    ]


@pytest.mark.serial
def test_a_transient_provider_failure_is_retried(
    shopper, providers, well_stocked, settle_timeout
):
    """One 500, then success: the declared retry policy carries the order.

    The status is retryable because the operation's own `error_map` says so —
    the runtime reads that map, so a deployment does not have to repeat itself
    in the legacy `error_classification` field to be retried.
    """

    providers.fail(P.AUTHORIZE, status=500, times=1)

    order = _checkout(shopper, providers, settle_timeout)

    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    assert providers.count(P.AUTHORIZE) == 2, "the retry policy declares 3 attempts"


def test_the_authorization_carries_its_idempotency_key(
    shopper, providers, well_stocked, settle_timeout
):
    """A provider can only deduplicate a replay if it is given the key.

    The header is named by the operation's provider-idempotent effect, which is
    where the current contract declares it.
    """

    order = _checkout(shopper, providers, settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

    call = providers.last_call(P.AUTHORIZE)
    assert call["headers"].get("idempotency-key"), (
        "a provider-idempotent step sends its fixed idempotency header"
    )


# -- the shopper clicks twice ------------------------------------------------


def test_clicking_pay_twice_creates_one_order_and_one_authorization(
    shopper, providers, well_stocked, settle_timeout
):
    cart_id = d.cart_with_one_line(shopper)
    request_id = d.new_request_id()
    before = {order["id"] for order in d.orders_of(shopper)}

    first = d.start_checkout(shopper, cart_id, request_id)
    second = d.start_checkout(shopper, cart_id, request_id)

    assert first.unwrap() == second.unwrap(), "a replay answers what the first call did"
    order = d.await_new_order(shopper, known=before, timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

    fresh = [o for o in d.orders_of(shopper) if o["id"] not in before]
    assert len(fresh) == 1, "one checkout, one order"
    assert providers.count(P.AUTHORIZE) == 1, "the shopper is asked for money once"


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
    assert providers.count(P.AUTHORIZE) == 1, "and the card is charged once"
