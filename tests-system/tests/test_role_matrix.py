"""Every desk reaches its own work, and nobody else's.

Written as a matrix on purpose. A store grows roles and commands faster than it
grows tests, and one forgotten permission is how a warehouse clerk ends up able
to approve refunds. This asks the same question of every pair.
"""

from __future__ import annotations

import pytest

#: Which role each worker command belongs to. Everything not listed against a
#: command must not even see it in its schema — Donat hides what a role cannot
#: call, so the check is "is the field there at all".
OWNERS = {
    # returns
    "start_return": {"customer"},
    "approve_return": {"support"},
    "reject_return": {"support"},
    "receive_return": {"fulfilment"},
    "record_return_inspection": {"fulfilment"},
    "create_exchange": {"fulfilment"},
    # money
    "start_payment_reconciliation": {"reconciliation_worker"},
    "record_chargeback": {"payment_worker"},
    "capture_payment": {"payment_worker"},
    "void_authorization": {"payment_worker"},
    # fulfilment
    "start_order_fulfilment": {"fulfilment"},
    "record_delivery": {"fulfilment"},
    "mark_order_packed": {"fulfilment"},
    # marketplace
    "start_vendor_payout": {"marketplace_worker"},
    "split_vendor_orders": {"marketplace_worker"},
    "record_vendor_acceptance": {"vendor"},
    # b2b
    "submit_quote": {"b2b_buyer"},
    "approve_purchase": {"b2b_approver"},
    "reject_purchase": {"b2b_approver"},
    "finance_approve_purchase": {"b2b_finance"},
    "finance_reject_purchase": {"b2b_finance"},
    # care
    "approve_prescription": {"veterinary_reviewer"},
    "reject_prescription": {"veterinary_reviewer"},
    "start_grooming_booking": {"customer"},
    "confirm_booking": {"customer"},
    "record_no_show": {"groomer"},
    # subscriptions
    "start_subscription_renewal": {"subscription_worker"},
    # A plan is the shopper's to stop, and the worker's to pause on their
    # behalf — the dunning ladder pauses a plan nobody could charge.
    "cancel_subscription": {"customer", "subscription_worker"},
    "pause_subscription": {"customer", "subscription_worker"},
    # operations
    "record_notification_delivery": {"notification_worker"},
    "resolve_fraud_review": {"support"},
}

ROLES = [
    "anonymous",
    "customer",
    "staff",
    "support",
    "fulfilment",
    "payment_worker",
    "reconciliation_worker",
    "marketplace_worker",
    "payout_worker",
    "vendor",
    "b2b_buyer",
    "b2b_approver",
    "b2b_finance",
    "veterinary_reviewer",
    "prescription_worker",
    "groomer",
    "booking_worker",
    "subscription_worker",
    "notification_worker",
]

MUTATION_FIELDS = 'query { __type(name: "mutation_root") { fields { name } } }'


@pytest.fixture(scope="module")
def callable_by_role(store) -> dict[str, set[str]]:
    """What each role can call, asked of the store once."""

    surface: dict[str, set[str]] = {}
    for role in ROLES:
        actor = store.anonymous() if role == "anonymous" else store.as_role(role, "matrix-user")
        data = actor.graphql(MUTATION_FIELDS).data or {}
        fields = ((data.get("__type") or {}).get("fields")) or []
        surface[role] = {field["name"] for field in fields}
    return surface


@pytest.mark.parametrize("command", sorted(OWNERS))
def test_a_command_is_offered_to_the_roles_that_own_it(command, callable_by_role):
    for role in OWNERS[command]:
        assert command in callable_by_role[role], (
            f"{role} cannot call {command}, which is its own work"
        )


@pytest.mark.parametrize("command", sorted(OWNERS))
def test_a_command_is_hidden_from_every_other_role(command, callable_by_role):
    intruders = {
        role
        for role in ROLES
        if role not in OWNERS[command] and command in callable_by_role[role]
    }

    assert not intruders, f"{command} is also offered to {sorted(intruders)}"


def test_the_public_is_offered_no_mutation_at_all(callable_by_role):
    """An unauthenticated visitor browses; it does not act."""

    assert callable_by_role["anonymous"] == set(), (
        f"the public can call {sorted(callable_by_role['anonymous'])}"
    )


def test_every_role_can_read_something(store):
    """A role that can see nothing is a deployment mistake, not a wall."""

    for role in ROLES:
        actor = store.anonymous() if role == "anonymous" else store.as_role(role, "matrix-user")
        data = actor.graphql('query { __type(name: "query_root") { fields { name } } }').data or {}
        fields = ((data.get("__type") or {}).get("fields")) or []
        assert fields, f"{role} has no query surface at all"


# -- the shop advertises its search operators (from test_search.py) ------------


def test_the_shop_advertises_that_it_can_be_searched(anonymous):
    """A search nobody can discover is a search nobody uses.

    Generated clients, IDEs and agents learn what a store can do by reading its
    schema. If `_ilike` works but is not published, every one of them concludes
    the catalogue cannot be searched — and a hand-written query is the only way
    in, which is exactly the knowledge a declarative API is supposed to remove.
    """

    published = anonymous.query(
        """
        query {
          text: __type(name: "String_comparison_exp") { inputFields { name } }
          number: __type(name: "Int_comparison_exp") { inputFields { name } }
        }
        """
    )
    text = {field["name"] for field in published["text"]["inputFields"]}
    number = {field["name"] for field in published["number"]["inputFields"]}

    assert {"_like", "_ilike", "_nlike", "_nilike"} <= text, (
        f"the store answers _ilike but does not offer it: {sorted(text)}"
    )
    assert not {"_like", "_ilike"} & number, (
        f"a pattern on a number is not a filter the store can honour: {sorted(number)}"
    )


# -- mcp lists only what a role may touch (from test_surface_parity.py) --------


# -- the walls are the same height on every door ----------------------------


def test_mcp_lists_only_the_tables_a_role_may_touch(anonymous, staff):
    public = anonymous.mcp_tool("list_tables", {})
    inside = staff.mcp_tool("list_tables", {})

    public_tables = {row["name"] for row in public.value("result/structuredContent/tables", [])}
    staff_tables = {row["name"] for row in inside.value("result/structuredContent/tables", [])}

    assert public_tables, f"the public sees the catalogue: {public.text[:200]}"
    assert "orders" not in public_tables
    assert staff_tables > public_tables, "staff see at least what the public sees, and more"
