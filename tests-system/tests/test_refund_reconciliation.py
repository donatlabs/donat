"""Refunds and voids the provider does not confirm cleanly.

The last branch of the money story: the store asked for the shopper's money
back, the call failed, and it has to find out whether the provider sent it
anyway. Giving up would keep money that is not the store's; assuming would
give it back twice.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import stays, until

pytestmark = [pytest.mark.providers, pytest.mark.serial]


def lookup_answers(providers, outcome: str, *, times: int = 12) -> None:
    """The provider's account of an operation, with its own identifiers."""

    for _ in range(times):
        providers.script(
            P.LOOKUP,
            times=1,
            patch={
                "found": True,
                "terminal_absence_proven": False,
                "outcome": outcome,
                "provider_mutation_id": f"op_{uuid.uuid4().hex[:10]}",
                "provider_event_id": f"evt_{uuid.uuid4().hex[:10]}",
                "provider_reference": f"ref_{uuid.uuid4().hex[:10]}",
                "amount_minor": 1000,
            },
        )



def walk_to_inspection(shopper, support, fulfilment, settle_timeout):
    """A return approved, received, and ready for the warehouse's verdict."""

    order = shipped_order(shopper, support, fulfilment, settle_timeout)
    line_id = shopper.query(
        "query L($order: uuid!) { orders(where: {id: {_eq: $order}}) { lines { id } } }",
        {"order": order["id"]},
    )["orders"][0]["lines"][0]["id"]
    shopper.graphql(
        """
        mutation Start($order: uuid!, $line: uuid!, $request: uuid!) {
          start_return(
            order_id: $order, lines: [{order_line_id: $line, requested_quantity: 1}],
            reason: "wrong size", replacement_requested: false, request_id: $request
          ) { order_id }
        }
        """,
        {"order": order["id"], "line": line_id, "request": d.new_request_id()},
    ).unwrap()
    returned = until(
        lambda: support.query(
            "query R($order: uuid!) { return_request(where: {order_id: {_eq: $order}}) { id status } }",
            {"order": order["id"]},
        )["return_request"],
        lambda rows: bool(rows),
        timeout=settle_timeout,
        description="the return request to exist",
    )[0]
    item = fulfilment.query(
        "query I($return: uuid!) { return_item(where: {return_request_id: {_eq: $return}}) { id } }",
        {"return": returned["id"]},
    )["return_item"][0]

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


def accept(fulfilment, returned, item, amount: int = 1000):
    return fulfilment.graphql(
        """
        mutation Inspect($return: uuid!, $item: uuid!, $amount: bigint!, $id: uuid!) {
          record_return_inspection(
            return_id: $return, lines: [{return_item_id: $item, inspected_quantity: 1}],
            inspection: accepted, refund_amount_minor: $amount,
            inspection_id: $id, note: "as described"
          ) { return_id status }
        }
        """,
        {
            "return": returned["id"],
            "item": item["id"],
            "amount": amount,
            "id": d.new_request_id(),
        },
    )


def refund_state(support, returned) -> list[dict]:
    return support.query(
        """
        query F($return: uuid!) {
          refund(where: {return_request_id: {_eq: $return}}) { id status amount_minor }
        }
        """,
        {"return": returned["id"]},
    )["refund"]


def test_a_refund_that_did_go_through_is_recorded_not_repeated(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """The refund call failed; the lookup says the money already went back."""

    order, returned, item = walk_to_inspection(shopper, support, fulfilment, settle_timeout)
    providers.fail(P.REFUND, status=500, times=10)
    lookup_answers(providers, "refunded")

    accept(fulfilment, returned, item).unwrap()

    providers.await_call(P.LOOKUP, timeout=settle_timeout)
    recorded = until(
        lambda: refund_state(support, returned),
        lambda rows: bool(rows) and rows[0]["status"] == "refunded",
        timeout=settle_timeout,
        description="the reconciled refund to be recorded",
    )[0]

    assert recorded["amount_minor"] == 1000, (
        "the store records the amount the provider says it returned"
    )
    # And it does not send the money a second time.
    stays(
        lambda: len(refund_state(support, returned)),
        lambda count: count == 1,
        duration=3,
        description="one refund, however many calls it took to establish it",
    )


def test_a_refund_the_provider_refused_pays_nobody(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """A successful call whose answer is a refusal is not a refund."""

    order, returned, item = walk_to_inspection(shopper, support, fulfilment, settle_timeout)
    providers.script(P.REFUND, times=5, patch={"status": "failed"})

    accept(fulfilment, returned, item).unwrap()

    providers.await_call(P.REFUND, timeout=settle_timeout)
    stays(
        lambda: refund_state(support, returned),
        lambda rows: all(row["status"] != "refunded" for row in rows),
        duration=4,
        description="a refused refund is never recorded as paid",
    )


def test_a_refund_nobody_can_account_for_waits_for_a_person(
    shopper, support, fulfilment, providers, well_stocked, settle_timeout
):
    """Neither confirmed nor proven absent: the store stops and asks."""

    order, returned, item = walk_to_inspection(shopper, support, fulfilment, settle_timeout)
    providers.fail(P.REFUND, status=500, times=10)
    providers.script(
        P.LOOKUP,
        times=12,
        patch={"found": False, "terminal_absence_proven": False, "outcome": "failed"},
    )

    accept(fulfilment, returned, item).unwrap()

    providers.await_call(P.LOOKUP, timeout=settle_timeout)
    stays(
        lambda: refund_state(support, returned),
        lambda rows: all(row["status"] != "refunded" for row in rows),
        duration=4,
        description="an unaccounted refund is never claimed as done",
    )


# -- a return label the carrier will not issue (from test_returns_and_refunds.py) ---


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
