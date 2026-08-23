"""The awkward inputs, the second door, and two people reaching at once.

Nothing here is a business flow. These are the things a store meets anyway:
a client sending nonsense, an agent writing through MCP, a Relay endpoint
nobody remembered to check, and two shoppers wanting the last bag of food.
"""

from __future__ import annotations

import threading

import pytest

from petshop_qa import domain as d
from petshop_qa import providers as P

pytestmark = pytest.mark.providers


# -- MCP writes --------------------------------------------------------------


def test_an_agent_writes_through_mcp_under_its_own_role(shopper, well_stocked):
    """The generic `insert` tool is the same permission as the GraphQL one."""

    cart_id = d.open_cart(shopper)

    written = shopper.mcp_tool(
        "insert",
        {
            "table": "cart_line",
            "objects": [
                {"cart_id": cart_id, "variant_id": d.IN_STOCK_VARIANT, "quantity": 3}
            ],
            "returning": ["id", "quantity"],
        },
    )

    assert written.status == 200, written.text
    assert written.value("result/isError") is not True, written.text
    lines = d.read_cart(shopper, cart_id)["lines"]
    assert [(line["variant_id"], line["quantity"]) for line in lines] == [
        (d.IN_STOCK_VARIANT, 3)
    ], "what the agent wrote is what the shopper sees"


def test_mcp_obeys_the_same_validator_as_graphql(shopper, well_stocked):
    """The per-role ceiling on a cart line is not a GraphQL-only rule."""

    cart_id = d.open_cart(shopper)

    refused = shopper.mcp_tool(
        "insert",
        {
            "table": "cart_line",
            "objects": [
                {"cart_id": cart_id, "variant_id": d.IN_STOCK_VARIANT, "quantity": 21}
            ],
            "returning": ["id"],
        },
    )

    assert refused.value("result/isError") is True or refused.value("error") is not None, (
        f"MCP let a shopper past the twenty-unit cap: {refused.text[:300]}"
    )
    assert d.read_cart(shopper, cart_id)["lines"] == []


def test_an_agent_cannot_write_a_table_its_role_may_not_touch(shopper):
    refused = shopper.mcp_tool(
        "insert",
        {
            "table": "product",
            "objects": [{"category_id": 1, "slug": "agent-made", "title": "No", "status": "published"}],
            "returning": ["id"],
        },
    )

    assert refused.value("result/isError") is True or refused.value("error") is not None, (
        f"an agent wrote the catalogue: {refused.text[:300]}"
    )


# -- the Relay door ----------------------------------------------------------


def test_relay_serves_the_same_catalogue_as_graphql(anonymous):
    over_graphql = {product["slug"] for product in d.catalogue(anonymous)}

    over_relay = anonymous._post(
        "/v1/relay",
        {"query": "query { product(order_by: {id: asc}) { id slug } }"},
        None,
        summary="relay catalogue",
    )

    assert over_relay.status == 200, over_relay.text
    assert {product["slug"] for product in over_relay.unwrap()["product"]} == over_graphql


def test_relay_applies_the_same_permissions(anonymous):
    refused = anonymous._post(
        "/v1/relay", {"query": "query { orders { id } }"}, None, summary="relay orders"
    )

    assert refused.error_code() == "validation-failed", (
        "the second door is not a way around the first one's walls"
    )


# -- inputs a client should not get away with --------------------------------


@pytest.mark.parametrize(
    "quantity, why",
    [
        (0, "a line of nothing is not an order"),
        (-3, "a negative quantity would credit the shopper"),
    ],
)
def test_a_cart_line_needs_a_real_quantity(shopper, quantity, why):
    cart_id = d.open_cart(shopper)

    refused = d.add_line(shopper, cart_id, d.IN_STOCK_VARIANT, quantity)

    assert refused.errors, why
    assert d.read_cart(shopper, cart_id)["lines"] == []


def test_a_variant_that_does_not_exist_is_refused(shopper):
    cart_id = d.open_cart(shopper)

    refused = d.add_line(shopper, cart_id, 999_999, 1)

    assert refused.errors, "a cart line points at something the store sells"


def test_a_malformed_query_is_a_client_error_not_a_crash(shopper):
    broken = shopper.graphql("query { product(order_by: ) { id } }")

    assert broken.status == 200, "a bad query is answered, not dropped"
    assert broken.errors, "and it is refused"


def test_an_unknown_field_names_itself(shopper):
    unknown = shopper.graphql("query { product { unicorn_count } }")

    assert unknown.error_code() == "validation-failed"
    assert "unicorn_count" in (unknown.error_message() or "")


def test_a_very_long_string_is_handled_rather_than_fatal(shopper):
    """A megabyte in a text column is accepted, and the store stays itself.

    Nothing declares a ceiling on a customer's own name — `text` has none, and
    no validator adds one — so this pins the behaviour that exists: the write
    is answered, the store keeps serving, and the value comes back whole. A
    store that wants a limit has to say so, and then this test changes.
    """

    long_name = "a" * 1_000_000

    written = shopper.graphql(
        "mutation Rename($name: String!) { update_customer(where: {}, _set: {name: $name}) { affected_rows } }",
        {"name": long_name},
    )

    assert written.status == 200, written.text
    if not written.errors:
        stored = shopper.query("query { customer { name } }")["customer"][0]["name"]
        assert len(stored) == len(long_name), "what was stored is what comes back"
    # And the store is still answering ordinary questions afterwards.
    shopper.query("query { customer { customer_id } }")
    shopper.graphql(
        "mutation Restore($name: String!) { update_customer(where: {}, _set: {name: $name}) { affected_rows } }",
        {"name": "Alice Buyer"},
    ).unwrap()


# -- two people, one shelf ---------------------------------------------------


def test_two_shoppers_cannot_both_take_the_last_unit(
    store, staff, shopper, other_shopper, providers, settle_timeout
):
    """The shelf holds one; two carts want it."""

    variant = d.IN_STOCK_VARIANT
    level = d.stock(staff, variant)
    staff.graphql(
        """
        mutation Restock($variant: Int!, $on_hand: Int!) {
          update_inventory_stock(where: {variant_id: {_eq: $variant}},
                                 _set: {on_hand: $on_hand}) { affected_rows }
        }
        """,
        {"variant": variant, "on_hand": level["reserved"] + 1},
    ).unwrap()

    carts = {
        "one": (shopper, d.cart_with_one_line(shopper, variant)),
        "two": (other_shopper, d.cart_with_one_line(other_shopper, variant)),
    }
    answers: dict[str, object] = {}

    def check_out(name: str) -> None:
        actor, cart_id = carts[name]
        answers[name] = d.start_checkout(actor, cart_id)

    racers = [threading.Thread(target=check_out, args=(name,)) for name in carts]
    for racer in racers:
        racer.start()
    for racer in racers:
        racer.join()

    # Whatever each caller was told, the shelf may not go negative and the
    # store may not sell the same unit twice.
    after = d.stock(staff, variant)
    assert after["on_hand"] >= 0 and after["reserved"] >= 0
    assert after["reserved"] <= after["on_hand"], (
        f"the last unit was promised twice: {after}"
    )


# -- one shopper's trouble is not everybody's --------------------------------


def test_an_order_waiting_on_a_provider_does_not_hold_up_another_shopper(
    shopper, other_shopper, staff, support, providers, settle_timeout
):
    """One order stuck mid-flight must not be every shopper's wait.

    The store runs every order through one durable queue. If that queue served
    one instance at a time, an order waiting on a provider that will not answer
    would be the whole store's latency — and nothing on any surface would say
    so, because every request still answers `200`.

    So: hold one shopper's authorization on a provider that does not reply, and
    then take another shopper from cart to authorized while the first is still
    waiting.
    """

    d.ensure_stock(staff, d.IN_STOCK_VARIANT)
    # Exactly one held answer, so only the first shopper's call is caught by it.
    providers.hang(P.AUTHORIZE, delay_ms=9000, times=1)
    known = {order["id"] for order in d.orders_of(shopper)}

    d.start_checkout(shopper, d.cart_with_one_line(shopper)).unwrap()
    stuck = d.await_new_order(shopper, known=known, timeout=settle_timeout)
    # The held call has been made: from here the first order is in flight and
    # not going anywhere for nine seconds.
    providers.await_call(P.AUTHORIZE, timeout=settle_timeout)

    served = d.checkout_to_order(
        other_shopper, d.cart_with_one_line(other_shopper), timeout=settle_timeout
    )
    d.await_order_status(other_shopper, served["id"], {"authorized"}, timeout=settle_timeout)

    # The second shopper is served, and the first was still waiting when it
    # happened — which is the whole point.
    assert [row for row in d.orders_of(shopper) if row["id"] == stuck["id"]][0][
        "order_status"
    ] not in {"paid", "cancelled"}, "the parked order was never actually in flight"
    assert d.payments_of(support, served["id"])[-1]["status"] == "authorized", (
        "the served order's money moved while another order was waiting"
    )
