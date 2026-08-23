"""Looking for something in the shop.

A shopper types a few letters and expects the right products back — matched
case-insensitively, across title and description, and never past the shelf they
are allowed to see. Search is also where a text pattern reaches SQL most
directly, so what a shopper types must stay a search term and never become
part of the query.
"""

from __future__ import annotations


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
