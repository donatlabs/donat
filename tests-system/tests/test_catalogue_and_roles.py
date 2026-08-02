"""What each kind of visitor may see and do.

The store has no admin role, so every one of these is a real permission
decision made by the engine for the role the token asserts. A tester's first
job is to prove the walls are where the store says they are.
"""

from __future__ import annotations

import pytest

from petshop_qa import domain as d
from petshop_qa import issue_expired_token, issue_token, issue_token_signed_with


# -- the public catalogue ----------------------------------------------------


def test_the_public_catalogue_shows_only_published_products(anonymous):
    products = d.catalogue(anonymous)

    assert products, "the public catalogue is not empty"
    assert {product["status"] for product in products} == {"published"}
    assert "turtle-heat-lamp" not in {product["slug"] for product in products}, (
        "a draft product is not public"
    )


def test_the_public_catalogue_shows_only_sellable_variants(anonymous):
    for product in d.catalogue(anonymous):
        for variant in product["variants"]:
            assert variant["active"] is True, (
                f"variant {variant['sku']} is inactive but publicly listed"
            )


def test_staff_see_the_drafts_the_public_cannot(staff, anonymous):
    staff_slugs = {product["slug"] for product in d.catalogue(staff)}
    public_slugs = {product["slug"] for product in d.catalogue(anonymous)}

    assert "turtle-heat-lamp" in staff_slugs
    assert staff_slugs > public_slugs, "staff see at least what the public sees, and more"


# -- what a visitor cannot even ask for --------------------------------------


@pytest.mark.parametrize(
    "query, field",
    [
        ("query { cart { id } }", "cart"),
        ("query { orders { id } }", "orders"),
        ("query { customer { customer_id } }", "customer"),
        ("query { payment { id } }", "payment"),
    ],
)
def test_a_visitor_with_no_account_cannot_reach_customer_data(anonymous, query, field):
    response = anonymous.graphql(query)

    assert response.status == 200
    assert response.error_code() == "validation-failed"
    assert response.error_message() == f"field '{field}' not found in type: 'query_root'"


def test_a_shopper_cannot_change_the_catalogue(shopper):
    response = shopper.graphql(
        """
        mutation {
          insert_product_one(
            object: {category_id: 1, slug: "shopper-made-this", title: "Nope", status: "published"}
          ) { id }
        }
        """
    )

    assert response.error_code() == "validation-failed"
    assert response.error_message() == (
        "field 'insert_product_one' not found in type: 'mutation_root'"
    )


# -- one shopper is not another ----------------------------------------------


def test_a_shopper_sees_only_their_own_cart(shopper, other_shopper):
    mine = d.open_cart(shopper)
    d.add_line(shopper, mine, d.IN_STOCK_VARIANT, 1).unwrap()

    theirs = other_shopper.query("query { cart { id } }")["cart"]

    assert mine not in [cart["id"] for cart in theirs], (
        "another shopper's cart is visible to them"
    )


def test_a_shopper_cannot_fill_another_shoppers_cart(shopper, other_shopper):
    mine = d.open_cart(shopper)

    response = d.add_line(other_shopper, mine, d.IN_STOCK_VARIANT, 1)

    assert response.error_code() == "permission-error"
    assert response.error_message() == (
        "check constraint of an insert/update permission has failed"
    )


def test_a_shopper_cannot_check_out_another_shoppers_cart(shopper, other_shopper):
    mine = d.cart_with_one_line(shopper)

    response = d.start_checkout(other_shopper, mine)

    assert response.error_code() == "validation-failed"
    assert "did not find a row" in (response.error_message() or ""), (
        "the entry-point Command refuses a cart the caller does not own"
    )


def test_a_shopper_sees_only_their_own_orders(shopper, other_shopper):
    for order in d.orders_of(shopper):
        assert order["id"] not in {other["id"] for other in d.orders_of(other_shopper)}


# -- what a shopper may not put in the basket --------------------------------


def test_a_draft_product_cannot_be_added_to_a_cart(shopper):
    cart = d.open_cart(shopper)

    # Variant 4 is inactive and belongs to the draft product.
    response = d.add_line(shopper, cart, 4, 1)

    assert response.error_code() == "permission-error"


def test_a_cart_line_is_capped_at_twenty_units_for_shoppers(shopper):
    cart = d.open_cart(shopper)

    refused = d.add_line(shopper, cart, d.IN_STOCK_VARIANT, 21)

    assert refused.error_code() == "validation-failed"
    assert refused.error_message() == "a cart line is limited to 20 units"
    # The cap is the shopper's, and it binds before anything is written.
    assert d.read_cart(shopper, cart)["lines"] == []


def test_twenty_units_are_still_allowed(shopper):
    cart = d.open_cart(shopper)

    d.add_line(shopper, cart, d.IN_STOCK_VARIANT, 20).unwrap()

    assert d.read_cart(shopper, cart)["lines"][0]["quantity"] == 20


# -- the token itself --------------------------------------------------------


def test_an_expired_token_is_refused(store, config):
    stale = store.with_token(issue_expired_token(config, "customer", d.CUSTOMER_ONE), "customer")

    response = stale.graphql("query { cart { id } }")

    assert response.error_code() == "invalid-jwt"
    assert response.error_message() == "Could not verify JWT: JWTExpired"


def test_a_token_signed_with_the_wrong_key_is_refused(store, config):
    forged = store.with_token(
        issue_token_signed_with(config, "not-the-stands-key", "customer", d.CUSTOMER_ONE),
        "customer",
    )

    response = forged.graphql("query { cart { id } }")

    assert response.error_code() == "invalid-jwt"
    assert response.error_message() == "Could not verify JWT: JWSError JWSInvalidSignature"


def test_a_shopper_cannot_promote_themselves_by_asking_for_a_role(store, config):
    """A valid customer token, a header asking for staff. The token decides."""

    climber = store.with_token(issue_token(config, "customer", d.CUSTOMER_ONE), "staff")

    response = climber.graphql("query { customer { customer_id } }")

    assert response.error_code() == "access-denied"
    assert response.error_message() == "Your requested role is not in allowed roles"
