"""Providers that answer badly, rather than not at all.

A refusal is not a timeout: the call arrived, the provider replied, and what it
said was no — or something the store cannot act on. Each of these branches
exists so that an unusable answer never becomes a guess about money.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import stays

pytestmark = [pytest.mark.providers, pytest.mark.serial]


def undetermined_lookup(providers, times: int = 10) -> None:
    """The provider that cannot say whether the operation happened.

    Neither found nor a proven absence: the one answer the store must not
    round to either side.
    """

    providers.script(
        P.LOOKUP,
        times=times,
        patch={"found": False, "terminal_absence_proven": False, "outcome": "failed"},
    )


# -- answers the store cannot act on ----------------------------------------


def test_an_undetermined_authorization_is_left_for_a_person(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The provider will not say. Nobody guesses; support gets the case."""

    providers.hang(P.AUTHORIZE, delay_ms=9000, times=3)
    undetermined_lookup(providers)
    known = {order["id"] for order in d.orders_of(shopper)}

    d.start_checkout(shopper, d.cart_with_one_line(shopper)).unwrap()
    order = d.await_new_order(shopper, known=known, timeout=settle_timeout)

    providers.await_call(P.LOOKUP, timeout=settle_timeout)
    stays(
        lambda: [row for row in d.orders_of(shopper) if row["id"] == order["id"]][0][
            "order_status"
        ],
        lambda status: status not in {"authorized", "paid"},
        duration=4,
        description="an undetermined authorization stays unauthorized",
    )
    assert [payment["status"] for payment in d.payments_of(support, order["id"])] == [
        "pending"
    ], "no payment state is invented from an answer the provider would not give"


def test_a_void_the_provider_refuses_leaves_the_order_as_it_was(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The provider says the hold stands, so the store does not pretend it fell."""

    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

    # A successful call whose outcome is a refusal, which is not a void.
    providers.script(P.VOID, times=5, patch={"status": "failed"})

    shopper.graphql(
        """
        mutation Cancel($order: uuid!, $reason: String!, $request: uuid!) {
          request_authorized_order_cancellation(
            order_id: $order, reason: $reason, request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "reason": "changed my mind", "request": d.new_request_id()},
    ).unwrap()

    providers.await_call(P.VOID, timeout=settle_timeout)
    stays(
        lambda: d.payments_of(support, order["id"])[-1]["status"],
        lambda status: status != "voided",
        duration=4,
        description="a refused void is not recorded as a void",
    )
    current = [row for row in d.orders_of(shopper) if row["id"] == order["id"]][0]
    assert current["order_status"] == "cancellation_requested", (
        f"the order waits for a person rather than moving on: {current}"
    )


# -- reconciliation ----------------------------------------------------------


def test_a_reconciliation_the_provider_cannot_serve_waits_for_support(
    shopper, support, reconciliation_worker, providers, well_stocked, settle_timeout
):
    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"authorized"}, timeout=settle_timeout
    )
    providers.fail(P.RECONCILE, status=503, times=10)

    reconciliation_worker.graphql(
        """
        mutation Reconcile($payment: uuid!, $event: String!, $request: uuid!) {
          start_payment_reconciliation(
            payment_id: $payment, provider_event_id: $event,
            expected_status: authorized, request_id: $request
          ) { payment_id }
        }
        """,
        {
            "payment": payment["id"],
            "event": f"evt_{uuid.uuid4().hex[:8]}",
            "request": d.new_request_id(),
        },
    ).unwrap()

    providers.await_call(P.RECONCILE, timeout=settle_timeout)

    # A reconciliation exists to record what the provider said. It said
    # nothing, so the store records nothing — and above all does not decide the
    # payment's fate on its own.
    stays(
        lambda: support.query(
            """
            query R($payment: uuid!) {
              payment_reconciliation(where: {payment_id: {_eq: $payment}}) { id status }
            }
            """,
            {"payment": payment["id"]},
        )["payment_reconciliation"],
        lambda rows: all(row["status"] != "matched" for row in rows),
        duration=5,
        description="an unreachable provider never produces a match",
    )
    assert d.payments_of(support, order["id"])[-1]["status"] == "authorized", (
        "the payment is exactly as it was before the attempt"
    )
