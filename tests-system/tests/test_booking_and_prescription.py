"""Two services the store sells besides goods: a grooming slot and a
veterinarian's opinion.

Both are holds on something scarce — a groomer's morning, a regulated line on an
order — so both are about who may confirm, and what happens to the hold if
nobody does.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d
from petshop_qa import until

pytestmark = pytest.mark.providers


# -- grooming ---------------------------------------------------------------


def hold_a_slot(shopper, *, resource: str | None = None, slot: str | None = None):
    return shopper.graphql(
        """
        mutation Hold(
          $resource: uuid!, $slot: String!, $starts: timestamptz!,
          $expires: timestamptz!, $request: uuid!
        ) {
          start_grooming_booking(
            service_resource_id: $resource,
            slot_key: $slot,
            starts_at: $starts,
            hold_expires_at: $expires,
            request_id: $request
          ) { slot_key }
        }
        """,
        {
            "resource": resource or str(uuid.uuid4()),
            "slot": slot or f"2030-01-02T{uuid.uuid4().hex[:2]}:00",
            "starts": "2030-01-02T10:00:00Z",
            "expires": "2030-01-01T10:00:00Z",
            "request": d.new_request_id(),
        },
    )


def booking_for(shopper, slot_key: str, settle_timeout: float) -> dict:
    return until(
        lambda: shopper.query(
            """
            query Bookings($slot: String!) {
              grooming_booking(where: {slot_key: {_eq: $slot}}) {
                id slot_key status customer_id
              }
            }
            """,
            {"slot": slot_key},
        )["grooming_booking"],
        lambda rows: bool(rows),
        timeout=settle_timeout,
        description=f"the booking for {slot_key} to exist",
    )[0]


def test_a_shopper_holds_a_slot_and_confirms_it(shopper, providers, settle_timeout):
    held = hold_a_slot(shopper).unwrap()["start_grooming_booking"]
    booking = booking_for(shopper, held["slot_key"], settle_timeout)

    shopper.graphql(
        """
        mutation Confirm($booking: uuid!, $request: uuid!) {
          confirm_booking(booking_id: $booking, request_id: $request) { booking_id }
        }
        """,
        {"booking": booking["id"], "request": d.new_request_id()},
    ).unwrap()

    confirmed = until(
        lambda: booking_for(shopper, held["slot_key"], settle_timeout),
        lambda row: row["status"] in {"confirmed", "completed"},
        timeout=settle_timeout,
        description="the booking to be confirmed",
    )
    assert confirmed["status"] in {"confirmed", "completed"}


def cancel_booking(shopper, booking_id: str):
    return shopper.graphql(
        """
        mutation Cancel($booking: uuid!, $request: uuid!) {
          cancel_booking(booking_id: $booking, request_id: $request, reason: "released")
            { booking_id }
        }
        """,
        {"booking": booking_id, "request": d.new_request_id()},
    )


@pytest.mark.serial
def test_a_double_booking_stays_that_shoppers_problem(
    shopper, other_shopper, providers, well_stocked, settle_timeout
):
    """One shopper's scheduling conflict does not stop anybody else.

    The second hold is accepted by the Command and then refused by the database
    — one groomer cannot be in two places at ten. That refusal ends the losing
    Process and nothing else: the slot keeps exactly one booking, and the store
    goes on taking orders while it happens.
    """

    resource = str(uuid.uuid4())
    slot = f"2030-03-04T10:{uuid.uuid4().hex[:2]}"
    hold_a_slot(shopper, resource=resource, slot=slot).unwrap()
    held = booking_for(shopper, slot, settle_timeout)

    try:
        hold_a_slot(other_shopper, resource=resource, slot=slot)

        order = d.checkout_to_order(
            shopper, d.cart_with_one_line(shopper), timeout=settle_timeout
        )
        d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)

        bookings = shopper.query(
            """
            query Bookings($slot: String!) {
              grooming_booking(where: {slot_key: {_eq: $slot}}) { id customer_id status }
            }
            """,
            {"slot": slot},
        )["grooming_booking"]
        assert [booking["id"] for booking in bookings] == [held["id"]], (
            f"the slot kept exactly one booking: {bookings}"
        )
    finally:
        cancel_booking(shopper, held["id"])


def test_a_shopper_sees_only_their_own_bookings(shopper, other_shopper, providers, settle_timeout):
    held = hold_a_slot(shopper).unwrap()["start_grooming_booking"]
    booking_for(shopper, held["slot_key"], settle_timeout)

    assert other_shopper.query(
        """
        query Bookings($slot: String!) {
          grooming_booking(where: {slot_key: {_eq: $slot}}) { id }
        }
        """,
        {"slot": held["slot_key"]},
    )["grooming_booking"] == []


# -- prescriptions ----------------------------------------------------------


def order_line_needing_review(shopper, settle_timeout) -> str:
    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    d.await_order_status(shopper, order["id"], {"authorized"}, timeout=settle_timeout)
    lines = shopper.query(
        """
        query Lines($order: uuid!) { orders(where: {id: {_eq: $order}}) { lines { id } } }
        """,
        {"order": order["id"]},
    )["orders"][0]["lines"]
    return lines[0]["id"]


def ask_for_review(shopper, order_line_id: str):
    return shopper.graphql(
        """
        mutation Review($line: uuid!, $deadline: timestamptz!, $request: uuid!) {
          start_prescription_review(
            order_line_id: $line, review_deadline: $deadline, request_id: $request
          ) { order_line_id }
        }
        """,
        {"line": order_line_id, "deadline": "2030-01-01T00:00:00Z", "request": d.new_request_id()},
    )


def request_for(actor, order_line_id: str, settle_timeout: float) -> dict:
    return until(
        lambda: actor.query(
            """
            query Requests($line: uuid!) {
              prescription_request(where: {order_line_id: {_eq: $line}}) { id status }
            }
            """,
            {"line": order_line_id},
        )["prescription_request"],
        lambda rows: bool(rows),
        timeout=settle_timeout,
        description="the prescription request to exist",
    )[0]


def test_a_veterinarian_approves_a_prescription_and_the_line_is_released(
    shopper, reviewer, providers, well_stocked, settle_timeout
):
    line_id = order_line_needing_review(shopper, settle_timeout)
    ask_for_review(shopper, line_id).unwrap()
    request = request_for(reviewer, line_id, settle_timeout)

    reviewer.graphql(
        """
        mutation Approve($request: uuid!, $decision: uuid!) {
          approve_prescription(
            prescription_id: $request, decision_id: $decision, review_note: "fit to dispense"
          ) { prescription_id }
        }
        """,
        {"request": request["id"], "decision": d.new_request_id()},
    ).unwrap()

    decided = until(
        lambda: request_for(reviewer, line_id, settle_timeout),
        lambda row: row["status"] != "submitted",
        timeout=settle_timeout,
        description="the prescription to be decided",
    )
    assert decided["status"] == "approved"


def test_a_veterinarian_can_refuse(shopper, reviewer, providers, well_stocked, settle_timeout):
    line_id = order_line_needing_review(shopper, settle_timeout)
    ask_for_review(shopper, line_id).unwrap()
    request = request_for(reviewer, line_id, settle_timeout)

    reviewer.graphql(
        """
        mutation Reject($request: uuid!, $decision: uuid!) {
          reject_prescription(
            prescription_id: $request, decision_id: $decision, review_note: "not appropriate"
          ) { prescription_id }
        }
        """,
        {"request": request["id"], "decision": d.new_request_id()},
    ).unwrap()

    decided = until(
        lambda: request_for(reviewer, line_id, settle_timeout),
        lambda row: row["status"] != "submitted",
        timeout=settle_timeout,
        description="the prescription to be decided",
    )
    assert decided["status"] == "rejected"


def test_a_shopper_cannot_approve_their_own_prescription(
    shopper, reviewer, providers, well_stocked, settle_timeout
):
    line_id = order_line_needing_review(shopper, settle_timeout)
    ask_for_review(shopper, line_id).unwrap()
    request = request_for(reviewer, line_id, settle_timeout)

    refused = shopper.graphql(
        """
        mutation Approve($request: uuid!, $decision: uuid!) {
          approve_prescription(
            prescription_id: $request, decision_id: $decision, review_note: "please"
          ) { prescription_id }
        }
        """,
        {"request": request["id"], "decision": d.new_request_id()},
    )

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'approve_prescription' not found in type: 'mutation_root'"
    )
