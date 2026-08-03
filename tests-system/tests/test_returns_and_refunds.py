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


# -- the two dispositions that are not a refund ------------------------------


def walk_to_inspection(shopper, support, fulfilment, settle_timeout, *, replacement: bool):
    """A return approved and received, waiting to be inspected."""

    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    line_id = order_line_of(shopper, order["id"])
    shopper.graphql(
        """
        mutation Start($order: uuid!, $line: uuid!, $replacement: Boolean!, $request: uuid!) {
          start_return(
            order_id: $order,
            lines: [{order_line_id: $line, requested_quantity: 1}],
            reason: "wrong size",
            replacement_requested: $replacement,
            request_id: $request
          ) { order_id }
        }
        """,
        {
            "order": order["id"],
            "line": line_id,
            "replacement": replacement,
            "request": d.new_request_id(),
        },
    ).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    item = return_items(fulfilment, returned["id"])[0]

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
    fulfilment.graphql(
        """
        mutation Receive($return: uuid!, $item: uuid!, $receipt: uuid!) {
          receive_return(
            return_id: $return, lines: [{return_item_id: $item, received_quantity: 1}],
            receipt_id: $receipt, received_at: "2030-01-02T00:00:00Z"
          ) { return_id }
        }
        """,
        {"return": returned["id"], "item": item["id"], "receipt": d.new_request_id()},
    ).unwrap()
    return order, returned, item


def inspect(fulfilment, returned: dict, item: dict, decision: str, amount: int = 1000):
    return fulfilment.graphql(
        """
        mutation Inspect(
          $return: uuid!, $item: uuid!, $inspection: ReturnInspection!,
          $amount: bigint!, $id: uuid!
        ) {
          record_return_inspection(
            return_id: $return, lines: [{return_item_id: $item, inspected_quantity: 1}],
            inspection: $inspection, refund_amount_minor: $amount,
            inspection_id: $id, note: "inspected"
          ) { return_id status }
        }
        """,
        {
            "return": returned["id"],
            "item": item["id"],
            "inspection": decision,
            "amount": amount,
            "id": d.new_request_id(),
        },
    )


def test_a_shopper_who_asked_for_a_replacement_gets_an_exchange(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """The warehouse marks the inspection an exchange, and goods go back out.

    The disposition is the warehouse's call, not the shopper's: asking for a
    replacement is a request, and `exchange` is the inspection agreeing to it.
    """

    order, returned, item = walk_to_inspection(
        shopper, support, fulfilment, settle_timeout, replacement=True
    )
    refunds_before = providers.count(P.REFUND)

    inspect(fulfilment, returned, item, "exchange", amount=0).unwrap()

    settled = until(
        lambda: support.query(
            "query R($id: uuid!) { return_request(where: {id: {_eq: $id}}) { id status } }",
            {"id": returned["id"]},
        )["return_request"],
        lambda rows: bool(rows) and rows[0]["status"] not in {"inspected", "received"},
        timeout=settle_timeout,
        description="the return to reach its disposition",
    )[0]

    assert settled["status"] == "exchanged", (
        f"a replacement request is answered with goods, not money: {settled}"
    )
    assert providers.count(P.REFUND) == refunds_before, "an exchange refunds nothing"


def test_goods_the_warehouse_rejects_are_not_refunded(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """What came back is not what was sold, so no money goes back."""

    order, returned, item = walk_to_inspection(
        shopper, support, fulfilment, settle_timeout, replacement=False
    )
    refunds_before = providers.count(P.REFUND)

    inspect(fulfilment, returned, item, "rejected", amount=0).unwrap()

    settled = until(
        lambda: support.query(
            "query R($id: uuid!) { return_request(where: {id: {_eq: $id}}) { id status } }",
            {"id": returned["id"]},
        )["return_request"],
        lambda rows: bool(rows) and rows[0]["status"] not in {"inspected", "received"},
        timeout=settle_timeout,
        description="the rejected return to settle",
    )[0]

    assert settled["status"] in {"rejected", "rejected_after_inspection"}, (
        f"a rejected inspection ends the return: {settled}"
    )
    assert providers.count(P.REFUND) == refunds_before, "nothing is refunded for rejected goods"


def test_support_can_refuse_a_return_outright(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """The desk that approves returns can also say no, and then it ends there.

    A refused request never reaches the warehouse and never reaches the
    provider: the goods stay with the shopper and the money stays with the
    store.
    """

    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    request_return(shopper, order["id"], order_line_of(shopper, order["id"])).unwrap()
    returned = await_return(support, order["id"], settle_timeout)
    refunds_before = providers.count(P.REFUND)

    support.graphql(
        """
        mutation Reject($return: uuid!, $decision: uuid!) {
          reject_return(return_id: $return, reason: "outside the window", decision_id: $decision) {
            return_id status
          }
        }
        """,
        {"return": returned["id"], "decision": d.new_request_id()},
    ).unwrap()

    settled = until(
        lambda: support.query(
            "query R($id: uuid!) { return_request(where: {id: {_eq: $id}}) { id status } }",
            {"id": returned["id"]},
        )["return_request"],
        lambda rows: bool(rows) and rows[0]["status"] != "requested",
        timeout=settle_timeout,
        description="support's refusal to settle",
    )[0]

    assert settled["status"] == "rejected", f"a refused return is refused: {settled}"
    assert providers.count(P.REFUND) == refunds_before, "a refused return refunds nothing"
    assert return_items(fulfilment, returned["id"]), (
        "the request is kept on file rather than erased"
    )


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
