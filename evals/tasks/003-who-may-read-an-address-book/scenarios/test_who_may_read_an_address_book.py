"""Whose address book is it.

Every scenario is two strangers and the same question asked from a different
side: what can each of them see, add, change and remove. Nothing here names a
permission, a filter or a role's YAML — a store that gets the boundary right by
row filters and a store that gets it right some other way are both correct.

What neither may do is let one shopper's address reach another shopper, in
either direction: out, by reading it, or in, by planting it.
"""

from __future__ import annotations

import uuid

import pytest

pytestmark = [pytest.mark.serial]


# -- talking to the address book ---------------------------------------------
#
# Written as plain API calls rather than helpers in `petshop_qa`, because the
# store does not expose this table until a candidate exposes it: there is
# nothing for a helper to wrap in the fixture the candidate is handed.


def add_address(actor, *, label: str, owner: str | None = None):
    """Add an address. `owner` claims a customer id the caller may not own."""

    row = {
        "label": label,
        "line1": "1 Marketplace Row",
        "city": "Bristol",
        "postal_code": "BS1 4DJ",
        "country_code": "GB",
    }
    if owner is not None:
        row["customer_id"] = owner
    return actor.graphql(
        """
        mutation Add($row: customer_address_insert_input!) {
          insert_customer_address_one(object: $row) { id customer_id label }
        }
        """,
        {"row": row},
    )


def addresses_of(actor) -> list[dict]:
    """Every address this caller can see, whoever it belongs to."""

    data = actor.graphql(
        "query Book { customer_address { id customer_id label } }"
    ).unwrap()
    return data["customer_address"] or []


def read_address(actor, address_id) -> dict | None:
    """One address by id — the direct question, not the caller's own list."""

    data = actor.graphql(
        """
        query One($id: Int!) {
          customer_address(where: {id: {_eq: $id}}) { id customer_id label }
        }
        """,
        {"id": address_id},
    ).unwrap()
    rows = data["customer_address"] or []
    return rows[0] if rows else None


def relabel(actor, address_id, label: str):
    return actor.graphql(
        """
        mutation Relabel($id: Int!, $label: String!) {
          update_customer_address(where: {id: {_eq: $id}}, _set: {label: $label}) {
            affected_rows
          }
        }
        """,
        {"id": address_id, "label": label},
    )


def remove(actor, address_id):
    return actor.graphql(
        """
        mutation Remove($id: Int!) {
          delete_customer_address(where: {id: {_eq: $id}}) { affected_rows }
        }
        """,
        {"id": address_id},
    )


def a_label() -> str:
    """Unique per call: these scenarios share a store and must not collide."""

    return f"home-{uuid.uuid4().hex[:8]}"


# -- the worlds ---------------------------------------------------------------


def a_shopper_with_two_addresses(shopper) -> list[dict]:
    return [add_address(shopper, label=a_label()).unwrap()["insert_customer_address_one"]
            for _ in range(2)]


def two_shoppers_with_addresses_each(shopper, other_shopper) -> tuple[dict, dict]:
    mine = add_address(shopper, label=a_label()).unwrap()["insert_customer_address_one"]
    theirs = add_address(other_shopper, label=a_label()).unwrap()["insert_customer_address_one"]
    return mine, theirs


# -- a shopper and their own book ---------------------------------------------


def test_a_shopper_keeps_an_address_book(shopper):
    """The plain case, and the one every other scenario is measured against."""

    added = a_shopper_with_two_addresses(shopper)

    visible = {row["id"] for row in addresses_of(shopper)}
    for row in added:
        assert row["id"] in visible, "a shopper cannot see an address they just added"

    relabel(shopper, added[0]["id"], "work").unwrap()
    assert read_address(shopper, added[0]["id"])["label"] == "work", (
        "a shopper corrected their own address and the correction did not stick"
    )

    remove(shopper, added[1]["id"]).unwrap()
    assert read_address(shopper, added[1]["id"]) is None, (
        "a shopper removed their own address and it is still there"
    )


def test_an_address_belongs_to_whoever_added_it(shopper):
    """A shopper who claims another customer's id still adds to their own book.

    Refusing the request is just as correct as ignoring the claim — what the
    store may not do is take the caller's word for who owns a row.
    """

    # An ordinary address first, unwrapped. Without it this scenario passes on a
    # store that exposes no address book at all: the claimed insert is refused,
    # the scenario reads that as "refused outright, correct" and goes green on
    # the null candidate. Proving the surface works is what makes the refusal
    # below mean something.
    add_address(shopper, label=a_label()).unwrap()

    intended = "customer-2"
    answer = add_address(shopper, label=a_label(), owner=intended)

    if answer.errors:
        return  # refused the claim, which is one of the two right answers

    added = answer.data["insert_customer_address_one"]
    assert added["customer_id"] != intended, (
        "the store let a shopper choose which customer an address belongs to"
    )


# -- two shoppers who are strangers -------------------------------------------


def test_an_address_book_holds_only_its_owners_addresses(shopper, other_shopper):
    """Read from the owner's side: the list is theirs and nothing else."""

    mine, theirs = two_shoppers_with_addresses_each(shopper, other_shopper)

    owners = {row["customer_id"] for row in addresses_of(shopper)}
    assert len(owners) <= 1, (
        f"one shopper's address book contains rows owned by {len(owners)} customers"
    )
    assert theirs["id"] not in {row["id"] for row in addresses_of(shopper)}, (
        "a stranger's address turned up in this shopper's book"
    )


def test_one_shopper_cannot_read_anothers_address(shopper, other_shopper):
    """Read from the intruder's side: asked for directly, it is not there.

    Independent of the scenario above on purpose. That one reads a list and
    could pass on a store that merely orders or paginates it away; this one
    names the row.
    """

    _, theirs = two_shoppers_with_addresses_each(shopper, other_shopper)

    assert read_address(shopper, theirs["id"]) is None, (
        "a shopper read another shopper's address by asking for it by id"
    )


def test_nobody_can_plant_an_address_in_anothers_book(shopper, other_shopper):
    """The same boundary from the other direction: writing into a stranger's book.

    The victim's own list is the reading here, which is what makes this
    independent of the scenario that checks what the *writer* got back.
    """

    before = {row["id"] for row in addresses_of(other_shopper)}
    add_address(shopper, label=a_label(), owner="customer-2")
    after = addresses_of(other_shopper)

    planted = [row for row in after if row["id"] not in before]
    assert not planted, (
        f"{len(planted)} address(es) appeared in a shopper's book that they never added"
    )


def test_one_shopper_cannot_change_anothers_address(shopper, other_shopper):
    """Reading and writing are the same boundary.

    A store that filters on the way out and not on the way in has not drawn a
    boundary; it has drawn a curtain.
    """

    _, theirs = two_shoppers_with_addresses_each(shopper, other_shopper)
    was = read_address(other_shopper, theirs["id"])["label"]

    relabel(shopper, theirs["id"], "taken")

    still = read_address(other_shopper, theirs["id"])
    assert still is not None and still["label"] == was, (
        "a shopper renamed a stranger's address"
    )


def test_one_shopper_cannot_remove_anothers_address(shopper, other_shopper):
    """Deleting is its own reading, and deliberately not folded into the one above.

    A single scenario covering both edit and delete would have been one
    assertion wearing two hats: the same `filter` governs them in the reference
    design, so the day it drifts they fail together and the task's two-killer
    rule would be satisfied only on paper.
    """

    _, theirs = two_shoppers_with_addresses_each(shopper, other_shopper)

    remove(shopper, theirs["id"])

    assert read_address(other_shopper, theirs["id"]) is not None, (
        "a shopper deleted a stranger's address"
    )


# -- the other two kinds of caller --------------------------------------------


def test_support_can_look_up_any_shoppers_address(shopper, other_shopper, support):
    """Support answers "where is my parcel", and that needs the address."""

    mine, theirs = two_shoppers_with_addresses_each(shopper, other_shopper)

    seen = {row["id"] for row in addresses_of(support)}
    assert mine["id"] in seen and theirs["id"] in seen, (
        "support cannot see the addresses deliveries are going to"
    )


def test_a_visitor_reads_no_addresses(anonymous, shopper):
    """Not signed in is not a shopper with an empty book."""

    a_shopper_with_two_addresses(shopper)

    answer = anonymous.graphql("query Book { customer_address { id } }")
    if answer.errors:
        return  # the field is not in the visitor's schema at all, which is right

    assert not (answer.data or {}).get("customer_address"), (
        "a visitor who is not signed in read the delivery addresses"
    )
