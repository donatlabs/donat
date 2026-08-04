"""After the money is authorized: cancelling it, or shipping and taking it.

Both directions are durable Processes with a provider in the middle, so both
are watched from outside: what the shopper's order says, what support sees of
the money, and what the provider was actually asked to do.
"""

from __future__ import annotations

from urllib.parse import unquote

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

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


def start_fulfilment(worker, order_id: str):
    return worker.graphql(
        """
        mutation Fulfil($order: uuid!, $request: uuid!) {
          start_order_fulfilment(
            order_id: $order, destination_region: "northeast", allocation_request_id: $request
          ) { order_id }
        }
        """,
        {"order": order_id, "request": d.new_request_id()},
    )


# -- cancelling --------------------------------------------------------------


def test_cancelling_an_authorized_order_voids_the_money(
    shopper, support, providers, well_stocked, settle_timeout
):
    order = authorized_order(shopper, settle_timeout)

    request_cancellation(shopper, order["id"]).unwrap()

    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"voided"}, timeout=settle_timeout
    )

    assert payment["status"] == "voided", "an unshipped order gives the money back"
    assert providers.count(P.CAPTURE) == 0, "nothing was ever captured"

    void = providers.last_call(P.VOID)
    assert payment["id"] in unquote(void["path"]), "the void names the payment it releases"


def test_cancelling_releases_the_reserved_stock(
    shopper, staff, providers, well_stocked, settle_timeout
):
    before = d.stock(staff, d.IN_STOCK_VARIANT)
    order = authorized_order(shopper, settle_timeout)

    request_cancellation(shopper, order["id"]).unwrap()
    d.await_order_status(shopper, order["id"], {"cancelled"}, timeout=settle_timeout)

    after = until(
        lambda: d.stock(staff, d.IN_STOCK_VARIANT),
        lambda level: level["reserved"] == before["reserved"],
        timeout=settle_timeout,
        description="the cancelled order to release its reservation",
    )
    assert after["on_hand"] == before["on_hand"], "cancelling sells nothing"


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


# -- shipping and taking the money ------------------------------------------


def test_shipping_an_order_captures_the_authorized_money(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    order = authorized_order(shopper, settle_timeout)

    start_fulfilment(fulfilment, order["id"]).unwrap()

    d.await_payment_status(support, order["id"], {"captured"}, timeout=settle_timeout)

    providers.await_call(P.CAPTURE)
    capture = providers.last_call(P.CAPTURE)
    # A capture takes the value that actually shipped, which is at most what
    # was authorized — never more, whatever the shipment contained.
    assert 0 < capture["body"]["amount_minor"] <= order["total_minor"], (
        f"captured {capture['body']['amount_minor']} against an authorization "
        f"of {order['total_minor']}"
    )
    assert capture["body"]["currency"] == "USD"
    assert providers.count(P.CAPTURE) == 1, "one shipment, one capture"


def test_a_shipment_carries_the_carriers_tracking_number(
    shopper, fulfilment, providers, well_stocked, settle_timeout
):
    order = authorized_order(shopper, settle_timeout)
    start_fulfilment(fulfilment, order["id"]).unwrap()

    shipment = until(
        lambda: fulfilment.query(
            """
            query Shipments($order: uuid!) {
              shipment(where: {order_id: {_eq: $order}}) {
                id status tracking_number
              }
            }
            """,
            {"order": order["id"]},
        )["shipment"],
        lambda rows: bool(rows) and rows[0]["status"] == "shipped",
        timeout=settle_timeout,
        description="the shipment to be labelled and shipped",
    )[0]

    label = providers.last_call(P.SHIPMENT_LABEL)
    assert label["body"]["shipment_id"] == shipment["id"], (
        "the label was bought for this shipment"
    )
    assert shipment["tracking_number"], (
        "a shipped parcel carries the tracking number the carrier issued"
    )


def test_only_fulfilment_may_start_fulfilment(shopper, providers, well_stocked, settle_timeout):
    order = authorized_order(shopper, settle_timeout)

    refused = start_fulfilment(shopper, order["id"])

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'start_order_fulfilment' not found in type: 'mutation_root'"
    )
