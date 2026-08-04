"""Money leaving the store, and money the store is not sure about.

A payout cycle pays vendors for what they sold; a reconciliation asks the
provider what it thinks happened to a payment. Both are back-office work with
no shopper in the room, so both are checked through worker roles only.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P
from petshop_qa import until

pytestmark = pytest.mark.providers

#: The one cycle `vendor_payout_candidate` reports against.
PAYOUT_CYCLE = "00000000-0000-0000-0000-000000000001"


# -- vendor payouts ---------------------------------------------------------


def shipped_order_with_vendor_line(shopper, support, fulfilment, marketplace_worker, settle_timeout):
    """An order whose line has been claimed by a vendor and accepted."""

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

    line = shopper.query(
        """
        query Lines($order: uuid!) {
          orders(where: {id: {_eq: $order}}) { lines { id } }
        }
        """,
        {"order": order["id"]},
    )["orders"][0]["lines"][0]

    marketplace_worker.graphql(
        """
        mutation Claim(
          $order: uuid!, $line: uuid!, $vendor: uuid!, $offer: uuid!, $gross: bigint!
        ) {
          insert_vendor_order(objects: [{
            order_id: $order, order_line_id: $line, vendor_id: $vendor, offer_id: $offer,
            line_sequence: 1, product_category: "services", gross_minor: $gross,
            currency: "USD", commission_tier: "standard", commission_bps: 1000,
            status: "accepted"
          }]) { affected_rows }
        }
        """,
        {
            "order": order["id"],
            "line": line["id"],
            "vendor": str(uuid.uuid4()),
            "offer": str(uuid.uuid4()),
            "gross": order["total_minor"],
        },
    ).unwrap()
    return order


def test_a_payout_cycle_pays_the_vendor_that_sold_the_line(
    shopper, support, fulfilment, marketplace_worker, providers, well_stocked, settle_timeout
):
    if support.query(
        "query { vendor_payout(limit: 1) { id } }"
    )["vendor_payout"]:
        pytest.skip(
            "this stand has already run its payout cycle. The cycle id is fixed in "
            "vendor_payout_candidate and nothing marks a paid candidate as settled, "
            "so a second run collides on vendor_payout_payout_key_key — run "
            "tests-system/stack.sh provision (or raise a fresh stand) first"
        )

    shipped_order_with_vendor_line(
        shopper, support, fulfilment, marketplace_worker, settle_timeout
    )
    # The candidate view names one cycle: a payout run is "everything accepted
    # and not yet paid", not an arbitrary batch the caller invents.
    cycle = PAYOUT_CYCLE

    marketplace_worker.graphql(
        """
        mutation Payout($cycle: uuid!, $request: uuid!) {
          start_vendor_payout(payout_cycle_id: $cycle, request_id: $request) { payout_cycle_id }
        }
        """,
        {"cycle": cycle, "request": d.new_request_id()},
    ).unwrap()

    providers.await_call(P.PAYOUT)
    paid = providers.last_call(P.PAYOUT)
    assert paid["body"]["vendor_id"], "the payout names the vendor being paid"
    assert paid["headers"]["authorization"] == "petshop-payout-token"

    settled = until(
        lambda: support.query(
            """
            query Payouts { vendor_payout(order_by: {id: desc}, limit: 5) { id status } }
            """
        )["vendor_payout"],
        lambda rows: bool(rows) and rows[0]["status"] in {"paid", "settled", "completed"},
        timeout=settle_timeout,
        description="the payout to be recorded as paid",
    )[0]
    assert settled["status"] in {"paid", "settled", "completed"}


def test_only_marketplace_may_start_a_payout(shopper, support, providers):
    for actor in (shopper, support):
        refused = actor.graphql(
            """
            mutation Payout($cycle: uuid!, $request: uuid!) {
              start_vendor_payout(payout_cycle_id: $cycle, request_id: $request) {
                payout_cycle_id
              }
            }
            """,
            {"cycle": str(uuid.uuid4()), "request": d.new_request_id()},
        )
        assert refused.error_code() == "validation-failed", (
            f"{actor.label} must not be able to move money to vendors"
        )


# -- payment reconciliation -------------------------------------------------


def open_reconciliation(reconciliation_worker, payment_id: str):
    return reconciliation_worker.graphql(
        """
        mutation Reconcile($payment: uuid!, $event: String!, $request: uuid!) {
          start_payment_reconciliation(
            payment_id: $payment,
            provider_event_id: $event,
            expected_status: authorized,
            request_id: $request
          ) { payment_id }
        }
        """,
        {
            "payment": payment_id,
            "event": f"evt_reconcile_{uuid.uuid4().hex[:8]}",
            "request": d.new_request_id(),
        },
    )


def authorized_payment(shopper, support, settle_timeout) -> dict:
    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    return d.await_payment_status(support, order["id"], {"authorized"}, timeout=settle_timeout)


def reconciliation_outcome(support, payment_id: str, settle_timeout: float) -> dict:
    return until(
        lambda: support.query(
            """
            query Reconciliations($payment: uuid!) {
              payment_reconciliation(where: {payment_id: {_eq: $payment}}) { id status }
            }
            """,
            {"payment": payment_id},
        )["payment_reconciliation"],
        lambda rows: bool(rows) and rows[0]["status"] != "pending",
        timeout=settle_timeout,
        description="the reconciliation to reach an outcome",
    )[0]


@pytest.mark.serial
def test_a_provider_that_agrees_settles_the_case_without_support(
    shopper, support, reconciliation_worker, providers, well_stocked, settle_timeout
):
    payment = authorized_payment(shopper, support, settle_timeout)
    # The store does not tell the provider what it expects, so a stand-in that
    # agrees has to be told the amount the payment actually carries.
    providers.script(P.RECONCILE, patch={"amount_minor": payment["amount_minor"]})

    open_reconciliation(reconciliation_worker, payment["id"]).unwrap()

    providers.await_call(P.RECONCILE)
    assert reconciliation_outcome(support, payment["id"], settle_timeout)["status"] == "matched"


@pytest.mark.serial
def test_a_provider_that_disagrees_is_escalated_to_support(
    shopper, support, reconciliation_worker, providers, well_stocked, settle_timeout
):
    """A provider answer that does not match is never taken at face value."""

    payment = authorized_payment(shopper, support, settle_timeout)
    providers.script(P.RECONCILE, patch={"amount_minor": payment["amount_minor"] + 1})

    open_reconciliation(reconciliation_worker, payment["id"]).unwrap()

    providers.await_call(P.RECONCILE)
    settled = reconciliation_outcome(support, payment["id"], settle_timeout)
    assert settled["status"] == "review_required", (
        "a disagreement about money is a person's decision, not the engine's"
    )


def test_a_shopper_cannot_reconcile_their_own_payment(
    shopper, support, providers, well_stocked, settle_timeout
):
    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    payment = d.await_payment_status(
        support, order["id"], {"authorized", "pending"}, timeout=settle_timeout
    )

    refused = shopper.graphql(
        """
        mutation Reconcile($payment: uuid!, $request: uuid!) {
          start_payment_reconciliation(
            payment_id: $payment, provider_event_id: "evt_self",
            expected_status: authorized, request_id: $request
          ) { payment_id }
        }
        """,
        {"payment": payment["id"], "request": d.new_request_id()},
    )

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'start_payment_reconciliation' not found in type: 'mutation_root'"
    )
