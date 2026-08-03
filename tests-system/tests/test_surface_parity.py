"""The same store, through three different doors.

GraphQL, the RESTified endpoints and MCP are separate transports over one
permission system. A tester's question is whether the door changes the answer —
it must not, neither the rows nor the refusals.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d


# -- the catalogue, three ways ----------------------------------------------


def test_rest_and_graphql_publish_the_same_catalogue(anonymous):
    over_graphql = {product["slug"] for product in d.catalogue(anonymous)}

    over_rest = anonymous.rest("GET", "products")
    assert over_rest.status == 200, over_rest.text
    assert {product["slug"] for product in over_rest.json["product"]} == over_graphql


def test_mcp_and_graphql_publish_the_same_catalogue(anonymous):
    over_graphql = {product["slug"] for product in d.catalogue(anonymous)}

    over_mcp = anonymous.mcp_tool(
        "query", {"table": "product", "columns": ["id", "slug"], "order_by": {"id": "asc"}}
    )
    assert over_mcp.status == 200, over_mcp.text
    rows = over_mcp.value("result/structuredContent/rows")
    assert rows is not None, over_mcp.text
    assert {row["slug"] for row in rows} == over_graphql


def test_one_product_reads_the_same_by_slug(anonymous):
    over_rest = anonymous.rest("GET", "products/dog-kibble")

    assert over_rest.status == 200
    product = over_rest.json["product"][0]
    assert product["slug"] == "dog-kibble"
    assert product["title"] == "Dog Kibble"


# -- a shopper's own data, three ways ---------------------------------------


def test_a_shopper_reads_the_same_order_everywhere(
    shopper, providers, well_stocked, settle_timeout
):
    """One order, asked for by id on all three surfaces.

    Compared by id rather than by whole listings: the shopper's order list
    keeps growing while a durable Process commits, so two listings taken a
    moment apart legitimately differ, and the MCP query tool answers at most
    100 rows.
    """

    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)

    over_graphql = {row["id"] for row in d.orders_of(shopper)}
    over_rest = shopper.rest("GET", f"orders/{order['id']}")
    over_mcp = shopper.mcp_tool(
        "query",
        {"table": "orders", "columns": ["id", "order_status"],
         "where": {"id": {"_eq": order["id"]}}},
    )

    assert order["id"] in over_graphql
    assert over_rest.status == 200, over_rest.text
    assert over_rest.json["orders"][0]["id"] == order["id"], over_rest.text
    assert [row["id"] for row in over_mcp.value("result/structuredContent/rows")] == [
        order["id"]
    ]


def test_a_shopper_writes_through_rest_and_reads_it_over_graphql(shopper, well_stocked):
    cart_id = d.open_cart(shopper)

    written = shopper.rest(
        "PUT",
        "cart/lines",
        json={"cart_id": cart_id, "variant_id": d.IN_STOCK_VARIANT, "quantity": 2},
    )

    assert written.status == 200, written.text
    assert not written.errors, written.text
    lines = d.read_cart(shopper, cart_id)["lines"]
    assert [(line["variant_id"], line["quantity"]) for line in lines] == [
        (d.IN_STOCK_VARIANT, 2)
    ]


# -- the walls are the same height on every door ----------------------------


def test_the_public_cannot_read_orders_on_any_surface(anonymous):
    over_graphql = anonymous.graphql("query { orders { id } }")
    over_rest = anonymous.rest("GET", "orders")
    over_mcp = anonymous.mcp_tool("query", {"table": "orders", "columns": ["id"]})

    assert over_graphql.error_code() == "validation-failed"
    assert over_rest.errors, "the REST route runs the same permission check"
    assert over_rest.error_code() == "validation-failed"
    assert over_mcp.status == 200
    assert over_mcp.value("result/isError") is True or over_mcp.value("error") is not None, (
        f"MCP must refuse what GraphQL refuses: {over_mcp.text[:300]}"
    )


def test_mcp_lists_only_the_tables_a_role_may_touch(anonymous, staff):
    public = anonymous.mcp_tool("list_tables", {})
    inside = staff.mcp_tool("list_tables", {})

    public_tables = {row["name"] for row in public.value("result/structuredContent/tables", [])}
    staff_tables = {row["name"] for row in inside.value("result/structuredContent/tables", [])}

    assert public_tables, f"the public sees the catalogue: {public.text[:200]}"
    assert "orders" not in public_tables
    assert staff_tables > public_tables, "staff see at least what the public sees, and more"


# -- the documented HTTP contract -------------------------------------------


def test_an_unknown_rest_route_is_404(anonymous):
    missing = anonymous.rest("GET", "not-a-route")

    assert missing.status == 404
    assert missing.json == {"code": "not-found", "error": "endpoint not found"}


def test_a_known_route_with_the_wrong_method_is_405(anonymous):
    wrong = anonymous.rest("POST", "products", json={})

    assert wrong.status == 405
    assert wrong.json["code"] == "method-not-allowed"
    assert "GET" in wrong.json["error"]
