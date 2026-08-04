"""Looking for something in the shop.

A shopper types a few letters and expects the right products back — matched
case-insensitively, across title and description, and never past the shelf they
are allowed to see. Search is also where a text pattern reaches SQL most
directly, so what a shopper types must stay a search term and never become
part of the query.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d

SEARCH = """
    query Search($pattern: String) {
      product(
        where: {_or: [{title: {_ilike: $pattern}}, {description: {_ilike: $pattern}}]}
        order_by: {slug: asc}
      ) { slug title }
    }
"""


def search(actor, term: str) -> list[str]:
    """What a shop's search box does with what was typed into it."""

    return [
        product["slug"]
        for product in actor.query(SEARCH, {"pattern": f"%{term}%"})["product"]
    ]


def test_a_search_finds_the_product_by_part_of_its_name(anonymous):
    assert search(anonymous, "Kibble") == ["dog-kibble"]


def test_a_search_does_not_care_about_case(anonymous):
    """`_ilike` is the difference between a search box and a riddle."""

    assert search(anonymous, "kibble") == search(anonymous, "KIBBLE") == ["dog-kibble"]
    # And the case-sensitive operator really is the other one, so the choice
    # above is a choice and not an accident.
    exact_case = anonymous.query(
        "query C($p: String) { product(where: {title: {_like: $p}}) { slug } }",
        {"p": "%kibble%"},
    )["product"]
    assert exact_case == [], "_like is case-sensitive; _ilike is what a search box wants"


def test_a_search_looks_in_the_description_too(anonymous):
    """Nobody types the title exactly. `scratch` is only in the description."""

    assert search(anonymous, "scratch") == ["cat-scratcher"]


def test_a_search_that_matches_nothing_says_so(anonymous):
    assert search(anonymous, "aquarium filter") == []


def test_a_search_never_reaches_the_drafts(anonymous, staff):
    """The shop's own words for an unpublished product stay unpublished.

    A search that ignored the read permission would be the widest leak in the
    store: a shopper could recover an entire draft catalogue a letter at a time.
    """

    draft = [
        product
        for product in staff.query("query { product { slug title status } }")["product"]
        if product["status"] == "draft"
    ]
    assert draft, "this test needs the seeded draft product"
    term = draft[0]["title"].split()[0]

    assert search(staff, term) == [draft[0]["slug"]], (
        "staff, who may see drafts, do find it — otherwise the next line proves nothing"
    )
    assert search(anonymous, term) == [], "the public found a draft by searching for it"


@pytest.mark.parametrize(
    "term, why",
    [
        ("'", "a quote would end the string literal if it were not escaped"),
        ("' OR '1'='1", "the textbook injection"),
        ("\\", "a backslash escapes the escape"),
        ("%' OR title LIKE '%", "an injection wearing a wildcard"),
        ('"; DROP TABLE product; --', "a statement terminator inside a term"),
    ],
)
def test_an_injection_typed_into_the_search_box_stays_a_search_term(anonymous, term, why):
    """What reaches SQL is a value, never syntax.

    Each of these is something a shopper could type. None may break out of the
    pattern: the answer is an ordinary empty result — not an error, not the
    whole shop, and not a shop that stopped working afterwards.
    """

    found = anonymous.query(SEARCH, {"pattern": f"%{term}%"})["product"]

    assert found == [], f"{why}: {term!r} matched {[row['slug'] for row in found]}"
    assert d.catalogue(anonymous), f"{why}: the shop stopped answering after {term!r}"


def test_pattern_syntax_is_pattern_syntax_and_still_cannot_cross_a_permission(
    anonymous, staff
):
    """`%` and `_` are the operator's own language, not letters to search for.

    An application putting a search box in front of `_ilike` has to escape what
    the shopper types; the engine passes the pattern through, which is what
    makes `_ilike` a pattern operator at all. This pins that — and pins the
    part that is not the application's business: the widest pattern expressible
    still returns only what the role may see.
    """

    public = {product["slug"] for product in d.catalogue(anonymous)}

    everything = {row["slug"] for row in anonymous.query(SEARCH, {"pattern": "%"})["product"]}
    single = anonymous.query(SEARCH, {"pattern": "%_%"})["product"]

    assert everything == public, (
        f"a bare wildcard is the whole catalogue the role may see: {everything}"
    )
    assert {row["slug"] for row in single} <= public, "and `_` reaches no further"
    for_staff = {row["slug"] for row in staff.query(SEARCH, {"pattern": "%"})["product"]}
    assert for_staff > public, (
        "staff see more than the public through the same wildcard — otherwise the "
        "assertion above proves nothing about the wall"
    )


def test_the_store_answers_a_search_rather_than_failing_on_it(anonymous):
    """Whatever is typed, the shop stays a shop."""

    for term in ("", "%%%%", "'" * 20, "\\%_", "ünïcödé", "🐈"):
        answer = anonymous.graphql(SEARCH, {"pattern": f"%{term}%"})
        assert answer.status == 200, f"{term!r}: {answer.text[:200]}"
        assert not answer.errors, f"{term!r}: {answer.errors}"
    # Still serving afterwards.
    assert d.catalogue(anonymous), "the catalogue is still readable after all that"


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
