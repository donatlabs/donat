"""The RESTified door, endpoint by endpoint.

A store publishes six named endpoints, each a saved GraphQL operation behind a
URL. That makes them the surface a client without a GraphQL library uses — and
the one most easily left half-tested, because the same data is reachable the
other way.

So: every declared endpoint is exercised, its parameters are shown to bind, and
its answers are compared against the GraphQL door rather than against a guess.
What the two doors must never differ on is who may see what.
"""

from __future__ import annotations

import uuid

import pytest

from petshop_qa import domain as d

#: Every endpoint `rest_endpoints.yaml` declares, with the role it is for and
#: the root the saved operation returns. A new endpoint that nobody exercises
#: shows up here as a missing row.
DECLARED = [
    ("GET", "products", "anonymous", "product"),
    ("GET", "products/dog-kibble", "anonymous", "product"),
    ("GET", "cart", "customer", "cart"),
    ("GET", "orders", "customer", "orders"),
    ("GET", "orders/{order}", "customer", "orders"),
    ("PUT", "cart/lines", "customer", "insert_cart_line"),
]


@pytest.fixture
def an_order(shopper, providers, well_stocked, settle_timeout) -> dict:
    return d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)


def test_every_declared_endpoint_answers(shopper, anonymous, an_order, well_stocked):
    """No endpoint in the store's own list is dead.

    The list is written out here rather than derived, so an endpoint added
    without a test is a failing row and not a silence.
    """

    callers = {"anonymous": anonymous, "customer": shopper}
    cart_id = d.open_cart(shopper)

    for method, path, role, root in DECLARED:
        actor = callers[role]
        body = (
            {"cart_id": cart_id, "variant_id": d.IN_STOCK_VARIANT, "quantity": 1}
            if method == "PUT"
            else None
        )
        answer = actor.rest(method, path.format(order=an_order["id"]), json=body)

        assert answer.status == 200, f"{method} {path}: {answer.status} {answer.text[:200]}"
        assert not answer.json.get("errors"), f"{method} {path}: {answer.text[:200]}"
        assert root in answer.json, f"{method} {path} answered without {root}: {answer.text[:200]}"


# -- parameters ---------------------------------------------------------------


def test_a_path_parameter_selects_the_row_it_names(anonymous):
    catalogue = d.catalogue(anonymous)
    assert len(catalogue) > 1, "this test needs more than one product to choose between"

    for product in catalogue:
        answered = anonymous.rest("GET", f"products/{product['slug']}")
        slugs = [row["slug"] for row in answered.json["product"]]
        assert slugs == [product["slug"]], f"{product['slug']} answered {slugs}"


def test_a_path_that_names_nothing_is_an_empty_answer_not_an_error(anonymous):
    """A slug nobody sells is a question with the answer "none"."""

    answered = anonymous.rest("GET", f"products/{uuid.uuid4()}")

    assert answered.status == 200
    assert answered.json["product"] == []


def test_a_body_binds_to_the_operations_variables(shopper, well_stocked):
    """PUT is an upsert, and it behaves like one twice in a row."""

    cart_id = d.open_cart(shopper)
    line = {"cart_id": cart_id, "variant_id": d.IN_STOCK_VARIANT}

    first = shopper.rest("PUT", "cart/lines", json={**line, "quantity": 2})
    second = shopper.rest("PUT", "cart/lines", json={**line, "quantity": 5})

    assert first.status == 200 and second.status == 200, second.text[:200]
    lines = d.read_cart(shopper, cart_id)["lines"]
    assert [(row["variant_id"], row["quantity"]) for row in lines] == [
        (d.IN_STOCK_VARIANT, 5)
    ], f"the second PUT should have updated the line, not added one: {lines}"


# -- the same walls as the other door -----------------------------------------


def test_an_endpoint_shows_the_caller_only_their_own_rows(shopper, other_shopper, an_order):
    """The saved operation carries no filter for who is asking; the role does."""

    mine = shopper.rest("GET", f"orders/{an_order['id']}")
    theirs = other_shopper.rest("GET", f"orders/{an_order['id']}")

    assert [row["id"] for row in mine.json["orders"]] == [an_order["id"]]
    assert theirs.json["orders"] == [], (
        f"another shopper read the order through REST: {theirs.text[:200]}"
    )


def test_the_public_gets_no_cart_and_no_orders(anonymous):
    for path in ("cart", "orders"):
        answered = anonymous.rest("GET", path)

        assert answered.status == 200, answered.text[:200]
        errors = answered.json.get("errors") or []
        assert errors, f"the public was served {path}: {answered.text[:200]}"
        assert errors[0]["extensions"]["code"] == "validation-failed"


def test_a_validator_applies_to_the_rest_door_too(shopper, well_stocked):
    """The twenty-unit cap is a property of the store, not of GraphQL."""

    cart_id = d.open_cart(shopper)

    refused = shopper.rest(
        "PUT",
        "cart/lines",
        json={"cart_id": cart_id, "variant_id": d.IN_STOCK_VARIANT, "quantity": 21},
    )

    assert refused.json.get("errors"), f"REST let a shopper past the cap: {refused.text[:200]}"
    assert d.read_cart(shopper, cart_id)["lines"] == []


def test_a_shopper_cannot_write_into_another_shoppers_cart_through_rest(
    shopper, other_shopper, well_stocked
):
    victim_cart = d.open_cart(other_shopper)

    written = shopper.rest(
        "PUT",
        "cart/lines",
        json={"cart_id": victim_cart, "variant_id": d.IN_STOCK_VARIANT, "quantity": 1},
    )

    errors = written.json.get("errors") or []
    assert errors, f"a shopper wrote into another cart through REST: {written.text[:200]}"
    assert errors[0]["extensions"]["code"] == "permission-error", (
        f"the refusal is the permission's, not something else: {errors[0]}"
    )
    assert d.read_cart(other_shopper, victim_cart)["lines"] == []


# -- what a client can rely on ------------------------------------------------


@pytest.mark.parametrize("method", ["POST", "DELETE", "PATCH", "OPTIONS", "HEAD"])
def test_an_endpoint_answers_only_the_method_it_declares(anonymous, method):
    answered = anonymous.rest(method, "products")

    assert answered.status == 405, f"{method} /products answered {answered.status}"


def test_both_doors_refuse_the_same_things_the_same_way(shopper, an_order):
    """A REST client and a GraphQL client are told the same story.

    Both are the same saved operation, so a difference here would mean one door
    validating something the other does not — which is how a caller ends up
    trusting the wrong one.
    """

    over_rest = shopper.rest("GET", "orders/not-a-uuid")
    over_graphql = shopper.graphql(
        "query O($id: uuid!) { orders(where: {id: {_eq: $id}}) { id } }", {"id": "not-a-uuid"}
    )

    assert over_rest.status == over_graphql.status == 200, "both doors answer, neither crashes"
    rest_code = (over_rest.json.get("errors") or [{}])[0].get("extensions", {}).get("code")
    assert rest_code == over_graphql.error_code(), (
        f"REST said {rest_code}, GraphQL said {over_graphql.error_code()}"
    )

    # A missing variable is refused before anything is executed, on both doors.
    incomplete = shopper.rest("PUT", "cart/lines", json={"cart_id": 1})
    assert (incomplete.json.get("errors") or [{}])[0]["extensions"]["code"] == "validation-failed"


def test_a_rest_error_is_a_graphql_error_with_an_http_200(shopper):
    """The shape a REST client has to expect, stated once.

    These endpoints are saved GraphQL operations, so a refusal comes back as a
    GraphQL error envelope with HTTP 200 — not as a 4xx. Only the routing
    layer (unknown path, wrong method) answers with a status code.
    """

    refused = shopper.rest("GET", "orders/not-a-uuid")

    assert refused.status == 200
    assert set(refused.json) == {"errors"}, f"unexpected envelope: {refused.text[:200]}"
    assert {"extensions", "message"} <= set(refused.json["errors"][0])

    routing = shopper.rest("GET", "no-such-endpoint")
    assert routing.status == 404, "an unknown route is a routing answer, not a GraphQL one"
