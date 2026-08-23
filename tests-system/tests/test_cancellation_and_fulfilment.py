"""A void the provider never confirmed is not claimed.

The happy cancellation and fulfilment paths live in the application's own
tests (`examples/petshop/metadata/flows/*_test.yaml`); this failure branch
stays here because it watches a state the provider never resolves.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P

pytestmark = pytest.mark.providers


def authorized_order(shopper, settle_timeout) -> dict:
    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    return d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)


def request_cancellation(shopper, order_id: str):
    return shopper.graphql(
        """
        mutation Cancel($order: uuid!, $request: uuid!) {
          request_authorized_order_cancellation(
            order_id: $order, reason: "changed my mind", request_id: $request
          ) { order_id }
        }
        """,
        {"order": order_id, "request": d.new_request_id()},
    )


# -- cancelling --------------------------------------------------------------


@pytest.mark.serial
def test_a_void_the_provider_never_confirmed_is_not_claimed(
    shopper, support, providers, well_stocked, settle_timeout
):
    """The store may not report money as returned unless the provider said so."""

    order = authorized_order(shopper, settle_timeout)
    providers.fail(P.VOID, status=500, times=10)

    request_cancellation(shopper, order["id"]).unwrap()
    providers.await_call(P.LOOKUP)

    payment = d.payments_of(support, order["id"])[-1]
    assert payment["status"] == "void_in_progress", (
        "an unproven void stays in progress rather than becoming 'voided'"
    )
    current = [o for o in d.orders_of(shopper) if o["id"] == order["id"]][0]
    assert current["order_status"] == "cancellation_requested"
