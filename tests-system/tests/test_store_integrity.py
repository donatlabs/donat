"""Statements that must be true of the whole store, not of one scenario.

These run against whatever the stand has accumulated — every order, payment,
refund and shelf the other tests left behind. A scenario test says "this flow
did the right thing"; these say "and nothing anywhere is now impossible".
"""

from __future__ import annotations

import pytest

pytestmark = pytest.mark.providers

#: Enough rows to be meaningful without pulling the whole store into memory.
SAMPLE = 500


def payments(support) -> list[dict]:
    return support.query(
        """
        query Payments($limit: Int!) {
          payment(order_by: {created_at: desc}, limit: $limit) {
            id order_id status amount_minor currency
          }
        }
        """,
        {"limit": SAMPLE},
    )["payment"]


def orders(support) -> list[dict]:
    return support.query(
        """
        query Orders($limit: Int!) {
          orders(order_by: {created_at: desc}, limit: $limit) {
            id order_status total_minor currency
          }
        }
        """,
        {"limit": SAMPLE},
    )["orders"]


def test_no_shelf_is_oversold(staff):
    levels = staff.query(
        "query { inventory_stock { variant_id on_hand reserved } }"
    )["inventory_stock"]

    assert levels, "the store has stock rows"
    for level in levels:
        assert level["on_hand"] >= 0, f"negative stock for variant {level['variant_id']}"
        assert level["reserved"] >= 0, f"negative reservation for variant {level['variant_id']}"
        assert level["reserved"] <= level["on_hand"], (
            f"variant {level['variant_id']} has more reserved than it holds: {level}"
        )


def test_every_payment_is_a_positive_amount_in_one_currency(support):
    for payment in payments(support):
        assert payment["amount_minor"] > 0, f"payment {payment['id']} charges nothing: {payment}"
        assert payment["currency"] == "USD", f"payment {payment['id']} left the price list"


def test_every_payment_status_is_one_the_store_defines(support):
    known = {
        "pending",
        "authorized",
        "captured",
        "failed",
        "voided",
        "void_in_progress",
        "refunded",
        "partially_refunded",
        "charged_back",
    }

    seen = {payment["status"] for payment in payments(support)}

    assert seen <= known, f"unknown payment states in the store: {sorted(seen - known)}"


def test_no_order_is_both_paid_for_and_given_back(support):
    """One order never holds a captured payment and a voided one at once."""

    by_order: dict[str, set[str]] = {}
    for payment in payments(support):
        by_order.setdefault(payment["order_id"], set()).add(payment["status"])

    for order_id, states in by_order.items():
        assert not ({"captured"} <= states and {"voided"} <= states), (
            f"order {order_id} was both captured and voided: {sorted(states)}"
        )


def test_a_cancelled_order_never_keeps_the_money(support):
    cancelled = {order["id"] for order in orders(support) if order["order_status"] == "cancelled"}

    for payment in payments(support):
        if payment["order_id"] in cancelled:
            assert payment["status"] not in {"captured"}, (
                f"cancelled order {payment['order_id']} still holds a capture: {payment}"
            )


def test_no_refund_exceeds_the_payment_it_came_from(support):
    refunds = support.query(
        """
        query Refunds($limit: Int!) {
          refund(order_by: {created_at: desc}, limit: $limit) {
            id payment_id amount_minor status
          }
        }
        """,
        {"limit": SAMPLE},
    )["refund"]
    charged = {payment["id"]: payment["amount_minor"] for payment in payments(support)}

    for refund in refunds:
        if refund["payment_id"] in charged:
            assert refund["amount_minor"] <= charged[refund["payment_id"]], (
                f"refund {refund['id']} gives back more than was taken: {refund}"
            )
        assert refund["amount_minor"] > 0, f"refund {refund['id']} returns nothing"


