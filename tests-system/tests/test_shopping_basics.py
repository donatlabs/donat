"""Browsing the shop and changing your mind about a basket.

Everything before the money: paging through a catalogue, sorting it, filtering
it, counting it, and editing the basket you filled. These are the reads and
writes a shopper makes dozens of times per order, and the ones a permission
mistake shows up in first — an aggregate that counts drafts leaks the same fact
as a query that lists them.
"""

from __future__ import annotations


def test_a_query_may_ask_for_as_many_fields_as_it_likes(anonymous):
    """A wide selection is an ordinary query, not an attack.

    A dashboard, a wide table, an export — all of them ask for more fields at
    once than a hand-written query would, and the answer must be the data in
    the order it was asked for.
    """

    width = 120
    asked = [f"f{index}" for index in range(width)]
    query = "query { product(order_by: {id: asc}) { %s } }" % " ".join(
        f"{alias}: slug" for alias in asked
    )

    rows = anonymous.query(query)["product"]

    assert rows, "the catalogue is not empty"
    assert list(rows[0].keys()) == asked, (
        "a wide answer keeps the order the query asked in: "
        f"{list(rows[0].keys())[:4]}…{list(rows[0].keys())[-1]}"
    )
    assert set(rows[0].values()) == {rows[0]["f0"]}, "and every alias is the same column"

    # The same width through a relationship, where each row is built again.
    nested = anonymous.query(
        "query { product(order_by: {id: asc}) { variants { %s } } }"
        % " ".join(f"{alias}: sku" for alias in asked)
    )["product"]
    assert list(nested[0]["variants"][0].keys()) == asked

    # And the same width over an aggregate, whose values belong to the query
    # level the object is built in — a row set of its own would put them one
    # level down, where the database refuses them.
    counted = anonymous.query(
        "query { product_aggregate { aggregate { %s } } }"
        % " ".join(f"{alias}: count" for alias in asked)
    )["product_aggregate"]["aggregate"]
    assert list(counted.keys()) == asked
    assert len(set(counted.values())) == 1, "every alias counts the same thing"
