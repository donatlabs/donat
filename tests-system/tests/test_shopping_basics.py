"""Browsing the shop and changing your mind about a basket.

Everything before the money: paging through a catalogue, sorting it, filtering
it, counting it, and editing the basket you filled. These are the reads and
writes a shopper makes dozens of times per order, and the ones a permission
mistake shows up in first — an aggregate that counts drafts leaks the same fact
as a query that lists them.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d

DRAFT_SLUG = "turtle-heat-lamp"


# -- browsing the catalogue --------------------------------------------------


def all_variants(actor) -> list[dict]:
    return actor.query(
        "query { product_variant(order_by: {id: asc}) { id sku } }"
    )["product_variant"]


def test_paging_the_catalogue_repeats_nothing_and_drops_nothing(anonymous):
    """Two pages of one, then the rest: the same list, cut in two."""

    everything = all_variants(anonymous)
    assert len(everything) >= 3, f"this test needs a catalogue to page through: {everything}"

    page = anonymous.query(
        "query { product_variant(order_by: {id: asc}, limit: 2) { id sku } }"
    )["product_variant"]
    rest = anonymous.query(
        "query { product_variant(order_by: {id: asc}, limit: 2, offset: 2) { id sku } }"
    )["product_variant"]

    assert page == everything[:2], f"the first page is the start of the list: {page}"
    assert rest == everything[2:4], f"the offset page continues where it left off: {rest}"
    assert not {row["id"] for row in page} & {row["id"] for row in rest}, (
        "a shopper paging through the shop is never shown the same thing twice"
    )


def test_the_catalogue_sorts_the_way_it_was_asked_to(anonymous):
    ascending = anonymous.query(
        "query { product(order_by: {slug: asc}) { slug } }"
    )["product"]
    descending = anonymous.query(
        "query { product(order_by: {slug: desc}) { slug } }"
    )["product"]

    assert [row["slug"] for row in ascending] == sorted(row["slug"] for row in ascending)
    assert descending == list(reversed(ascending)), (
        f"desc is asc read backwards: {descending} vs {ascending}"
    )


def test_a_filter_selects_exactly_what_it_names(anonymous):
    everything = {product["slug"] for product in d.catalogue(anonymous)}
    wanted = sorted(everything)[:1]

    selected = anonymous.query(
        "query Pick($slugs: [String!]) { product(where: {slug: {_in: $slugs}}) { slug } }",
        {"slugs": wanted},
    )["product"]
    excluded = anonymous.query(
        "query Drop($slugs: [String!]) { product(where: {slug: {_nin: $slugs}}) { slug } }",
        {"slugs": wanted},
    )["product"]

    assert [row["slug"] for row in selected] == wanted
    assert {row["slug"] for row in excluded} == everything - set(wanted), (
        "what a filter excludes is exactly the rest of what the shopper may see"
    )


def test_a_filter_cannot_reach_past_what_the_role_may_see(anonymous, staff):
    """Naming a draft by hand does not produce it.

    A permission is not a default the caller can argue with: the public
    catalogue is the whole world the public has, and asking for a row outside
    it answers nothing rather than answering it.
    """

    assert any(product["slug"] == DRAFT_SLUG for product in staff.query(
        "query { product { slug status } }"
    )["product"]), "this test needs the seeded draft product"

    named = anonymous.query(
        "query Pick($slug: String) { product(where: {slug: {_eq: $slug}}) { slug status } }",
        {"slug": DRAFT_SLUG},
    )["product"]

    assert named == [], f"a draft was reachable by naming it: {named}"


def test_the_count_counts_only_what_the_shopper_may_see(anonymous, staff):
    """An aggregate is a read, and it obeys the same walls as the rows.

    A count that included drafts would tell the public how many products the
    shop is preparing — the same leak as listing them, one number at a time.
    """

    public = anonymous.query(
        "query { product_aggregate { aggregate { count } } product { id } }"
    )
    counted = public["product_aggregate"]["aggregate"]["count"]

    assert counted == len(public["product"]), (
        f"the count disagrees with the rows it counts: {counted} vs {len(public['product'])}"
    )
    for_staff = staff.query("query { product_aggregate { aggregate { count } } }")
    assert for_staff["product_aggregate"]["aggregate"]["count"] > counted, (
        "staff see the drafts, so their count is the larger one — otherwise this "
        "test proves nothing about the public's"
    )


def test_one_row_by_key_is_the_same_row_the_list_shows(anonymous):
    first = d.catalogue(anonymous)[0]

    by_key = anonymous.query(
        "query One($id: Int!) { product_by_pk(id: $id) { id slug } }", {"id": first["id"]}
    )["product_by_pk"]

    assert by_key["slug"] == first["slug"]


def test_a_draft_is_not_reachable_by_its_key_either(anonymous, staff):
    draft = [
        product
        for product in staff.query("query { product { id slug status } }")["product"]
        if product["slug"] == DRAFT_SLUG
    ][0]

    by_key = anonymous.query(
        "query One($id: Int!) { product_by_pk(id: $id) { id slug } }", {"id": draft["id"]}
    )["product_by_pk"]

    assert by_key is None, f"the by-key door is not wider than the list: {by_key}"


# -- changing your mind about a basket ---------------------------------------


def test_a_shopper_changes_how_many_they_want(shopper, well_stocked):
    cart_id = d.cart_with_one_line(shopper, quantity=2)
    line = d.read_cart(shopper, cart_id)["lines"][0]

    shopper.graphql(
        """
        mutation Change($id: bigint!, $quantity: Int!) {
          update_cart_line(where: {id: {_eq: $id}}, _set: {quantity: $quantity}) {
            affected_rows
          }
        }
        """,
        {"id": line["id"], "quantity": 5},
    ).unwrap()

    assert d.read_cart(shopper, cart_id)["lines"][0]["quantity"] == 5


def test_the_twenty_unit_cap_holds_on_the_second_thought_too(shopper, well_stocked):
    """The ceiling is on the basket, not on the first way of filling it.

    A cap enforced only when a line is created would be no cap at all: add one,
    then raise it to a hundred.
    """

    cart_id = d.cart_with_one_line(shopper, quantity=1)
    line = d.read_cart(shopper, cart_id)["lines"][0]

    refused = shopper.graphql(
        """
        mutation Change($id: bigint!, $quantity: Int!) {
          update_cart_line(where: {id: {_eq: $id}}, _set: {quantity: $quantity}) {
            affected_rows
          }
        }
        """,
        {"id": line["id"], "quantity": 21},
    )

    assert refused.errors, "a shopper raised a line past the cap by editing it"
    assert d.read_cart(shopper, cart_id)["lines"][0]["quantity"] == 1, (
        "and the basket is what it was"
    )


def test_a_shopper_takes_something_back_out(shopper, well_stocked):
    cart_id = d.cart_with_one_line(shopper)
    line = d.read_cart(shopper, cart_id)["lines"][0]

    shopper.graphql(
        "mutation Drop($id: bigint!) { delete_cart_line(where: {id: {_eq: $id}}) { affected_rows } }",
        {"id": line["id"]},
    ).unwrap()

    assert d.read_cart(shopper, cart_id)["lines"] == [], "the basket is empty again"


def test_a_shopper_cannot_edit_another_shoppers_basket(shopper, other_shopper, well_stocked):
    cart_id = d.cart_with_one_line(shopper, quantity=2)
    line = d.read_cart(shopper, cart_id)["lines"][0]

    reached = other_shopper.graphql(
        """
        mutation Change($id: bigint!) {
          update_cart_line(where: {id: {_eq: $id}}, _set: {quantity: 9}) { affected_rows }
        }
        """,
        {"id": line["id"]},
    )
    removed = other_shopper.graphql(
        "mutation Drop($id: bigint!) { delete_cart_line(where: {id: {_eq: $id}}) { affected_rows } }",
        {"id": line["id"]},
    )

    assert reached.value("data/update_cart_line/affected_rows", 0) == 0, (
        f"another shopper's line was edited: {reached.text[:200]}"
    )
    assert removed.value("data/delete_cart_line/affected_rows", 0) == 0, (
        f"another shopper's line was removed: {removed.text[:200]}"
    )
    assert d.read_cart(shopper, cart_id)["lines"][0]["quantity"] == 2, (
        "and the owner's basket is untouched"
    )


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
