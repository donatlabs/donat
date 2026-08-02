"""A shopper sends something back, and the money comes back with it.

The return walks four desks — the shopper asks, support approves, the warehouse
receives and inspects — and only then does the store refund. Each step is a
different role, so this is as much an authorization test as a money test.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

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


# -- the whole way back ------------------------------------------------------


def test_an_approved_return_is_received_inspected_and_refunded(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    line_id = order_line_of(shopper, order["id"])

    request_return(shopper, order["id"], line_id).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    item = return_items(fulfilment, returned["id"])[0]

    support.graphql(
        """
        mutation Approve($return: uuid!, $item: uuid!, $decision: uuid!) {
          approve_return(
            return_id: $return,
            lines: [{return_item_id: $item, approved_quantity: 1}],
            decision_id: $decision,
            note: "approved"
          ) { return_id status }
        }
        """,
        {"return": returned["id"], "item": item["id"], "decision": d.new_request_id()},
    ).unwrap()


    fulfilment.graphql(
        """
        mutation Receive($return: uuid!, $item: uuid!, $receipt: uuid!) {
          receive_return(
            return_id: $return,
            lines: [{return_item_id: $item, received_quantity: 1}],
            receipt_id: $receipt,
            received_at: "2030-01-02T00:00:00Z"
          ) { return_id status }
        }
        """,
        {"return": returned["id"], "item": item["id"], "receipt": d.new_request_id()},
    ).unwrap()

    refund_amount = 1000
    fulfilment.graphql(
        """
        mutation Inspect($return: uuid!, $item: uuid!, $inspection: uuid!, $amount: bigint!) {
          record_return_inspection(
            return_id: $return,
            lines: [{return_item_id: $item, inspected_quantity: 1}],
            inspection: accepted,
            refund_amount_minor: $amount,
            inspection_id: $inspection,
            note: "as described"
          ) { return_id status }
        }
        """,
        {
            "return": returned["id"],
            "item": item["id"],
            "inspection": d.new_request_id(),
            "amount": refund_amount,
        },
    ).unwrap()

    # The provider is asked to refund, and the store records what came back.
    providers.await_call(P.REFUND)
    refund_call = providers.last_call(P.REFUND)
    assert refund_call["body"]["amount_minor"] == refund_amount

    refunded = until(
        lambda: support.query(
            """
            query Refunds($return: uuid!) {
              refund(where: {return_request_id: {_eq: $return}}) { id amount_minor status }
            }
            """,
            {"return": returned["id"]},
        )["refund"],
        lambda rows: bool(rows) and rows[0]["status"] == "refunded",
        timeout=settle_timeout,
        description="the refund to be recorded as refunded",
    )[0]
    assert refunded["amount_minor"] == refund_amount, (
        "the store refunds exactly what the inspection allowed"
    )


@pytest.mark.serial
def test_a_receipt_the_warehouse_records_immediately_is_not_lost(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """The warehouse scans receipt and inspection the moment approval lands.

    Nothing is paced here on purpose: the receipt is recorded before the return
    Process has reached the wait that expects it. The wait declares
    `persist_before_match`, so the signal is held for it and the refund still
    happens — a warehouse that works quickly is not a reason to strand a
    shopper's money.
    """

    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    request_return(shopper, order["id"], order_line_of(shopper, order["id"])).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    item = return_items(fulfilment, returned["id"])[0]

    for mutation, extra in (
        (
            """mutation A($return: uuid!, $item: uuid!, $id: uuid!) {
                 approve_return(return_id: $return,
                   lines: [{return_item_id: $item, approved_quantity: 1}],
                   decision_id: $id, note: "approved") { return_id } }""",
            support,
        ),
        (
            """mutation R($return: uuid!, $item: uuid!, $id: uuid!) {
                 receive_return(return_id: $return,
                   lines: [{return_item_id: $item, received_quantity: 1}],
                   receipt_id: $id, received_at: "2030-01-02T00:00:00Z") { return_id } }""",
            fulfilment,
        ),
        (
            """mutation I($return: uuid!, $item: uuid!, $id: uuid!) {
                 record_return_inspection(return_id: $return,
                   lines: [{return_item_id: $item, inspected_quantity: 1}],
                   inspection: accepted, refund_amount_minor: 1000,
                   inspection_id: $id, note: "as described") { return_id } }""",
            fulfilment,
        ),
    ):
        extra.graphql(
            mutation,
            {"return": returned["id"], "item": item["id"], "id": d.new_request_id()},
        ).unwrap()

    providers.await_call(P.REFUND, timeout=settle_timeout)


# -- and the ways it is refused ---------------------------------------------


def test_a_shopper_cannot_return_someone_elses_order(
    shopper, other_shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    line_id = order_line_of(shopper, order["id"])

    refused = request_return(other_shopper, order["id"], line_id)

    assert refused.errors, "a return may only be started by the order's owner"
    assert refused.error_code() == "validation-failed"


def test_the_warehouse_cannot_approve_its_own_returns(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """Approval is support's decision; receiving is the warehouse's."""

    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    request_return(shopper, order["id"], order_line_of(shopper, order["id"])).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    item = return_items(fulfilment, returned["id"])[0]

    refused = fulfilment.graphql(
        """
        mutation Approve($return: uuid!, $item: uuid!, $decision: uuid!) {
          approve_return(
            return_id: $return,
            lines: [{return_item_id: $item, approved_quantity: 1}],
            decision_id: $decision,
            note: "self-approved"
          ) { return_id }
        }
        """,
        {"return": returned["id"], "item": item["id"], "decision": d.new_request_id()},
    )

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'approve_return' not found in type: 'mutation_root'"
    )


def test_nothing_is_refunded_before_the_goods_are_inspected(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    request_return(shopper, order["id"], order_line_of(shopper, order["id"])).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    item = return_items(fulfilment, returned["id"])[0]

    support.graphql(
        """
        mutation Approve($return: uuid!, $item: uuid!, $decision: uuid!) {
          approve_return(
            return_id: $return,
            lines: [{return_item_id: $item, approved_quantity: 1}],
            decision_id: $decision,
            note: "approved"
          ) { return_id }
        }
        """,
        {"return": returned["id"], "item": item["id"], "decision": d.new_request_id()},
    ).unwrap()

    assert providers.count(P.REFUND) == 0, (
        "an approved return is a promise to inspect, not a promise to pay"
    )
    assert support.query(
        """
        query Refunds($return: uuid!) {
          refund(where: {return_request_id: {_eq: $return}}) { id }
        }
        """,
        {"return": returned["id"]},
    )["refund"] == []
