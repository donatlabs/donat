"""A company buys on account, and somebody has to say yes first."""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import until

pytestmark = pytest.mark.providers

#: Provisioned by the stand: an organization the first customer buys for.
ORGANIZATION = "00000000-0000-0000-0000-0000000000c1"


def submit_quote(buyer, cart_id: int, organization: str = ORGANIZATION):
    return buyer.graphql(
        """
        mutation Submit($organization: uuid!, $cart: bigint!, $request: uuid!) {
          submit_quote(organization_id: $organization, cart_id: $cart, request_id: $request) {
            quote_id approval_id total_minor
          }
        }
        """,
        {"organization": organization, "cart": cart_id, "request": d.new_request_id()},
    )


def approve(approver, approval_id: str):
    return approver.graphql(
        """
        mutation Approve($approval: uuid!, $request: uuid!) {
          approve_purchase(approval_id: $approval, request_id: $request) {
            approval_id approval_status
          }
        }
        """,
        {"approval": approval_id, "request": d.new_request_id()},
    )


def approval_of(approver, approval_id: str) -> dict:
    rows = approver.query(
        """
        query Approval($id: uuid!) {
          purchase_approval(where: {id: {_eq: $id}}) { id status }
        }
        """,
        {"id": approval_id},
    )["purchase_approval"]
    assert rows, f"approval {approval_id} is visible to the approver"
    return rows[0]


def test_a_quote_on_account_waits_for_an_approver(
    shopper, b2b_buyer, b2b_approver, well_stocked, providers, settle_timeout
):
    # The buyer fills the basket as themselves; the quote is submitted for the
    # organization they belong to.
    cart_id = d.cart_with_one_line(shopper, quantity=5)

    quoted = submit_quote(b2b_buyer, cart_id).unwrap()["submit_quote"]

    assert quoted["total_minor"] > 0
    # The approval is recorded immediately and routed to a person by the
    # Process; until it is routed, nobody can act on it.
    assert approval_of(b2b_approver, quoted["approval_id"])["status"] in {
        "submitted",
        "awaiting_approver",
    }
    routed = until(
        lambda: approval_of(b2b_approver, quoted["approval_id"]),
        lambda approval: approval["status"] == "awaiting_approver",
        timeout=settle_timeout,
        description="the approval to be routed to an approver",
    )
    assert routed["status"] == "awaiting_approver"


def test_an_approver_approves_and_the_purchase_is_committed(
    shopper, b2b_buyer, b2b_approver, well_stocked, settle_timeout, providers
):
    cart_id = d.cart_with_one_line(shopper, quantity=5)
    quoted = submit_quote(b2b_buyer, cart_id).unwrap()["submit_quote"]
    until(
        lambda: approval_of(b2b_approver, quoted["approval_id"]),
        lambda approval: approval["status"] == "awaiting_approver",
        timeout=settle_timeout,
        description="the approval to be routed to an approver",
    )

    decided = approve(b2b_approver, quoted["approval_id"]).unwrap()["approve_purchase"]

    assert decided["approval_status"] == "approved"
    assert approval_of(b2b_approver, quoted["approval_id"])["status"] == "approved"


def test_a_shopper_cannot_approve_a_purchase(
    shopper, b2b_buyer, b2b_approver, well_stocked, providers
):
    cart_id = d.cart_with_one_line(shopper, quantity=5)
    quoted = submit_quote(b2b_buyer, cart_id).unwrap()["submit_quote"]

    refused = approve(shopper, quoted["approval_id"])

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'approve_purchase' not found in type: 'mutation_root'"
    )


def test_a_buyer_cannot_quote_for_an_organization_they_do_not_belong_to(
    other_shopper, store, well_stocked, providers
):
    """The second customer is in no organization at all."""

    outsider = store.as_role("b2b_buyer", d.CUSTOMER_TWO)
    cart_id = d.cart_with_one_line(other_shopper, quantity=2)

    refused = submit_quote(outsider, cart_id)

    assert refused.errors, "membership is what makes a quote possible"
    assert refused.error_code() == "validation-failed"
