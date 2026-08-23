"""A shopper sends something back, and the money comes back with it.

The return walks four desks — the shopper asks, support approves, the warehouse
receives and inspects — and only then does the store refund. Each step is a
different role, so this is as much an authorization test as a money test.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import stays, until

pytestmark = pytest.mark.providers


def shipped_order(shopper, support, fulfilment, settle_timeout) -> dict:
    """An order that was paid for and left the warehouse."""

    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    fulfilment.graphql(
        """
        mutation Fulfil($order: uuid!, $request: uuid!) {
          start_order_fulfilment(
            order_id: $order, destination_region: "northeast", allocation_request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "request": d.new_request_id()},
    ).unwrap()
    d.await_payment_status(support, order["id"], {"captured"}, timeout=settle_timeout)
    return order


def order_line_of(shopper, order_id: str) -> str:
    orders = shopper.query(
        """
        query Lines($order: uuid!) {
          orders(where: {id: {_eq: $order}}) { id lines { id } }
        }
        """,
        {"order": order_id},
    )["orders"]
    assert orders and orders[0]["lines"], f"order {order_id} has a returnable line"
    return orders[0]["lines"][0]["id"]


def request_return(shopper, order_id: str, order_line_id: str):
    return shopper.graphql(
        """
        mutation Start($order: uuid!, $line: uuid!, $request: uuid!) {
          start_return(
            order_id: $order,
            lines: [{order_line_id: $line, requested_quantity: 1}],
            reason: "wrong size",
            replacement_requested: false,
            request_id: $request
          ) { order_id }
        }
        """,
        {"order": order_id, "line": order_line_id, "request": d.new_request_id()},
    )


def await_return(support, order_id: str, settle_timeout: float) -> dict:
    return until(
        lambda: support.query(
            """
            query Returns($order: uuid!) {
              return_request(where: {order_id: {_eq: $order}}) { id status }
            }
            """,
            {"order": order_id},
        )["return_request"],
        lambda rows: bool(rows),
        timeout=settle_timeout,
        description="the return request to exist",
    )[0]


def return_items(fulfilment, return_id: str) -> list[dict]:
    return fulfilment.query(
        """
        query Items($return: uuid!) {
          return_item(where: {return_request_id: {_eq: $return}}) { id requested_quantity }
        }
        """,
        {"return": return_id},
    )["return_item"]


def test_a_return_label_the_carrier_will_not_issue_stops_the_return(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """Support approved the return, and the carrier cannot produce a label.

    Without a label there is nothing to send the goods back with, so the store
    does not pretend the return is under way — and above all does not refund
    for goods that were never shipped back.
    """

    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    request_return(shopper, order["id"], order_line_of(shopper, order["id"])).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    item = return_items(fulfilment, returned["id"])[0]
    providers.fail(P.RETURN_LABEL, status=503, times=10)
    refunds_before = providers.count(P.REFUND)

    support.graphql(
        """
        mutation Approve($return: uuid!, $item: uuid!, $decision: uuid!) {
          approve_return(
            return_id: $return, lines: [{return_item_id: $item, approved_quantity: 1}],
            decision_id: $decision, note: "approved"
          ) { return_id }
        }
        """,
        {"return": returned["id"], "item": item["id"], "decision": d.new_request_id()},
    ).unwrap()

    # The carrier is asked, and asked again, and the store stops there.
    providers.await_call(P.RETURN_LABEL, timeout=settle_timeout)
    stays(
        lambda: support.query(
            "query R($id: uuid!) { return_request(where: {id: {_eq: $id}}) { id status } }",
            {"id": returned["id"]},
        )["return_request"][0]["status"],
        lambda status: status not in {"received", "inspected", "refunded"},
        duration=4,
        description="a return with no label to go no further",
    )
    assert providers.count(P.REFUND) == refunds_before, (
        "nothing is refunded for goods that were never sent back"
    )
