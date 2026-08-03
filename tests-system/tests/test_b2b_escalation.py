"""Buying on account, all the way up the chain.

A small purchase inside the company's credit goes through by itself. A large
one waits for an approver. An approver who says nothing hands it to finance,
and finance saying nothing rejects it. Every rung is a different desk, and the
deadlines are days — so this runs on the fast stand, where the same flow
declares them in seconds.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import stays, until

pytestmark = [pytest.mark.providers, pytest.mark.serial]

ORGANIZATION = "00000000-0000-0000-0000-0000000000c1"

#: The decision table routes a quote automatically only when it is inside this
#: much credit; anything larger is somebody's decision.
AUTOMATIC_CEILING_MINOR = 10000


@pytest.fixture
def buyer(fast_store):
    return fast_store.as_role("b2b_buyer", d.CUSTOMER_ONE)


@pytest.fixture
def shopper(fast_store):
    """The same person, filling their own basket before quoting it."""

    return fast_store.as_role("customer", d.CUSTOMER_ONE)


@pytest.fixture
def approver(fast_store):
    return fast_store.as_role("b2b_approver", "approver-1")


@pytest.fixture
def finance(fast_store):
    return fast_store.as_role("b2b_finance", "finance-1")


def submit(buyer, cart_id: int):
    return buyer.graphql(
        """
        mutation Submit($organization: uuid!, $cart: bigint!, $request: uuid!) {
          submit_quote(organization_id: $organization, cart_id: $cart, request_id: $request) {
            quote_id approval_id total_minor
          }
        }
        """,
        {"organization": ORGANIZATION, "cart": cart_id, "request": d.new_request_id()},
    )


def approval(approver, approval_id: str) -> dict:
    rows = approver.query(
        "query A($id: uuid!) { purchase_approval(where: {id: {_eq: $id}}) { id status } }",
        {"id": approval_id},
    )["purchase_approval"]
    assert rows, f"approval {approval_id} is visible"
    return rows[0]


def settles_at(approver, approval_id: str, expected: set[str], timeout: float) -> dict:
    return until(
        lambda: approval(approver, approval_id),
        lambda row: row["status"] in expected,
        timeout=timeout,
        description=f"approval {approval_id} to reach {sorted(expected)}",
    )


def test_a_small_purchase_inside_the_credit_needs_nobody(
    shopper, buyer, approver, fast_settle_timeout, fast_providers
):
    """One bag of food on account: the company's credit covers it."""

    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=1)).unwrap()["submit_quote"]

    assert quoted["total_minor"] <= AUTOMATIC_CEILING_MINOR, (
        f"this test needs a quote inside the automatic ceiling: {quoted}"
    )
    settled = settles_at(approver, quoted["approval_id"], {"approved"}, fast_settle_timeout)
    assert settled["status"] == "approved", (
        "a purchase inside the credit is approved without a person"
    )


def test_a_large_purchase_waits_for_an_approver(
    shopper, buyer, approver, fast_settle_timeout, fast_providers
):
    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]

    assert quoted["total_minor"] > AUTOMATIC_CEILING_MINOR
    waiting = settles_at(
        approver, quoted["approval_id"], {"awaiting_approver"}, fast_settle_timeout
    )
    assert waiting["status"] == "awaiting_approver"


def test_an_approver_who_says_nothing_hands_it_to_finance(
    shopper, buyer, approver, finance, fast_settle_timeout, fast_providers
):
    """The deadline passes with no decision, so the next desk inherits it."""

    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]
    settles_at(approver, quoted["approval_id"], {"awaiting_approver"}, fast_settle_timeout)

    escalated = settles_at(
        approver, quoted["approval_id"], {"awaiting_finance"}, fast_settle_timeout
    )

    assert escalated["status"] == "awaiting_finance", (
        "an unanswered approval is escalated, not dropped"
    )


def test_finance_approves_what_the_approver_left(
    shopper, buyer, approver, finance, fast_settle_timeout, fast_providers
):
    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]
    settles_at(approver, quoted["approval_id"], {"awaiting_finance"}, fast_settle_timeout)

    finance.graphql(
        """
        mutation Approve($approval: uuid!, $request: uuid!) {
          finance_approve_purchase(approval_id: $approval, request_id: $request) {
            approval_id approval_status
          }
        }
        """,
        {"approval": quoted["approval_id"], "request": d.new_request_id()},
    ).unwrap()

    settled = settles_at(approver, quoted["approval_id"], {"approved"}, fast_settle_timeout)
    assert settled["status"] == "approved"


def test_finance_can_refuse(
    shopper, buyer, approver, finance, fast_settle_timeout, fast_providers
):
    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]
    settles_at(approver, quoted["approval_id"], {"awaiting_finance"}, fast_settle_timeout)

    finance.graphql(
        """
        mutation Reject($approval: uuid!, $request: uuid!) {
          finance_reject_purchase(
            approval_id: $approval, request_id: $request, reason: "over budget"
          ) { approval_id approval_status }
        }
        """,
        {"approval": quoted["approval_id"], "request": d.new_request_id()},
    ).unwrap()

    settled = settles_at(approver, quoted["approval_id"], {"rejected"}, fast_settle_timeout)
    assert settled["status"] == "rejected"


def test_a_purchase_nobody_decides_is_rejected_in_the_end(
    shopper, buyer, approver, finance, fast_settle_timeout, fast_providers
):
    """Two deadlines pass, two desks stay silent: the answer becomes no.

    Nothing is bought on a company's account because everyone was busy.
    """

    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]

    settled = settles_at(
        approver, quoted["approval_id"], {"rejected"}, fast_settle_timeout * 2
    )
    assert settled["status"] == "rejected", (
        "silence at every desk resolves to a refusal, not to an open request"
    )


def test_only_finance_may_take_the_escalated_decision(
    shopper, buyer, approver, fast_settle_timeout, fast_providers
):
    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]
    settles_at(approver, quoted["approval_id"], {"awaiting_finance"}, fast_settle_timeout)

    refused = approver.graphql(
        """
        mutation Approve($approval: uuid!, $request: uuid!) {
          finance_approve_purchase(approval_id: $approval, request_id: $request) {
            approval_id
          }
        }
        """,
        {"approval": quoted["approval_id"], "request": d.new_request_id()},
    )

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == (
        "field 'finance_approve_purchase' not found in type: 'mutation_root'"
    )


def test_an_approver_who_answers_at_once_is_not_escalated_anyway(
    shopper, buyer, approver, finance, fast_settle_timeout, fast_providers
):
    """A decision taken the moment the desk is told to take it must stick.

    The approval row says `awaiting_approver` before the Process opens its
    wait, so an approver who answers immediately answers into that window. If
    the decision were dropped there, the deadline would pass and the purchase
    would escalate — quietly turning a purchase somebody approved back into an
    open request.
    """

    quoted = submit(buyer, d.cart_with_one_line(shopper, quantity=5)).unwrap()["submit_quote"]
    # No pause between seeing the request and answering it: that is exactly the
    # window a real approver clicking straight through would answer in.
    until(
        lambda: approval(approver, quoted["approval_id"]),
        lambda row: row["status"] == "awaiting_approver",
        timeout=fast_settle_timeout,
        description="the approval to reach the approver",
        interval=0,
    )

    approver.graphql(
        """
        mutation Approve($approval: uuid!, $request: uuid!) {
          approve_purchase(approval_id: $approval, request_id: $request) {
            approval_id approval_status
          }
        }
        """,
        {"approval": quoted["approval_id"], "request": d.new_request_id()},
    ).unwrap()

    # Long enough for both escalation deadlines of the fast stand to pass.
    stays(
        lambda: approval(approver, quoted["approval_id"])["status"],
        lambda status: status == "approved",
        duration=fast_settle_timeout,
        description="an approved purchase to stay approved",
    )
