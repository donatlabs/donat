"""The store with somebody hostile in front of it.

Not malformed input — that is `test_edges_and_races.py`. This is a caller who
knows how the engine works and is trying to get past it: forging who they are,
widening a mutation until it catches somebody else's rows, putting SQL where a
value goes, and asking for shapes meant to make the store fall over.

Every case here ends the same way: the attempt is refused or scoped down, and
the shop is still serving afterwards.
"""

from __future__ import annotations

import time

import jwt
import pytest
import requests

from petshop_qa import domain as d

pytestmark = pytest.mark.providers

CLAIMS = "https://donat.io/jwt/claims"

#: What a shopper who is not the attacker owns, read by a desk that sees both.
OTHER_CUSTOMER = d.CUSTOMER_TWO


def raw_graphql(store, query: str, headers: dict, variables: dict | None = None):
    """One request with headers of the test's own choosing.

    The suite's client mints a correct token for a role; these cases are about
    what happens when the headers do not agree with each other.
    """

    return requests.post(
        f"{store.config.base_url}/v1/graphql",
        json={"query": query, "variables": variables or {}},
        headers={"content-type": "application/json", **headers},
        timeout=store.config.request_timeout,
    )


def token(store, claims: dict, *, secret: str | None = None, algorithm: str = "HS256") -> str:
    return jwt.encode(
        {"sub": "attacker", CLAIMS: claims, "exp": int(time.time()) + 300},
        secret if secret is not None else store.config.jwt_key,
        algorithm=algorithm,
    )


def customer_names(support) -> dict[str, str]:
    return {
        row["customer_id"]: row["name"]
        for row in support.query("query { customer { customer_id name } }")["customer"]
    }


# -- forging who you are -----------------------------------------------------


def test_a_header_cannot_re_label_who_you_are(store, support):
    """The session's identity comes from the signed token, not from a header.

    If `X-Donat-User-Id` could be set by the caller, every per-customer
    permission in the store would be decoration: one header and a shopper reads
    anybody's orders.
    """

    mine = token(
        store,
        {
            "x-donat-default-role": "customer",
            "x-donat-allowed-roles": ["customer"],
            "x-donat-user-id": d.CUSTOMER_ONE,
        },
    )

    answer = raw_graphql(
        store,
        "query { customer { customer_id name } }",
        {"authorization": f"Bearer {mine}", "X-Donat-User-Id": OTHER_CUSTOMER},
    )

    seen = answer.json()["data"]["customer"]
    assert [row["customer_id"] for row in seen] == [d.CUSTOMER_ONE], (
        f"a header re-labelled the caller: {seen}"
    )
    # And the other shopper's row is untouched and unread.
    assert customer_names(support)[OTHER_CUSTOMER] == "Bob Buyer"


def test_a_header_cannot_promote_you_to_a_role_your_token_does_not_grant(store):
    mine = token(
        store,
        {"x-donat-default-role": "customer", "x-donat-allowed-roles": ["customer"]},
    )

    answer = raw_graphql(
        store,
        "query { product { id status } }",
        {"authorization": f"Bearer {mine}", "X-Donat-Role": "staff"},
    )

    body = answer.json()
    assert body.get("errors"), f"a header granted a role the token does not: {body}"
    assert body["errors"][0]["extensions"]["code"] == "access-denied"


def test_a_role_the_token_does_grant_is_the_callers_to_choose(store):
    """The other half of the rule: allowed roles really are allowed.

    Without this the test above would pass on a store that refused every
    header, which is a different store.
    """

    mine = token(
        store,
        {
            "x-donat-default-role": "customer",
            "x-donat-allowed-roles": ["customer", "staff"],
            "x-donat-user-id": d.CUSTOMER_ONE,
        },
    )

    answer = raw_graphql(
        store,
        "query { product { id status } }",
        {"authorization": f"Bearer {mine}", "X-Donat-Role": "staff"},
    )

    assert not answer.json().get("errors"), answer.text[:200]


@pytest.mark.parametrize(
    "description, headers_for",
    [
        (
            "a signature made with another key",
            lambda store: {
                "authorization": "Bearer "
                + token(
                    store,
                    {"x-donat-default-role": "staff", "x-donat-allowed-roles": ["staff"]},
                    secret="not-the-stores-key-not-the-stores-key",
                )
            },
        ),
        (
            "no signature at all",
            lambda store: {
                "authorization": "Bearer "
                + jwt.encode(
                    {
                        "sub": "attacker",
                        CLAIMS: {
                            "x-donat-default-role": "staff",
                            "x-donat-allowed-roles": ["staff"],
                        },
                        "exp": int(time.time()) + 300,
                    },
                    None,
                    algorithm="none",
                )
            },
        ),
        (
            "a token that expired",
            lambda store: {
                "authorization": "Bearer "
                + jwt.encode(
                    {
                        "sub": "attacker",
                        CLAIMS: {
                            "x-donat-default-role": "staff",
                            "x-donat-allowed-roles": ["staff"],
                        },
                        "exp": int(time.time()) - 60,
                    },
                    store.config.jwt_key,
                    algorithm="HS256",
                )
            },
        ),
    ],
)
def test_a_token_the_store_did_not_issue_buys_nothing(store, description, headers_for):
    answer = raw_graphql(store, "query { product { id status } }", headers_for(store))

    body = answer.json()
    assert body.get("errors"), f"{description}: {body}"
    assert body["errors"][0]["extensions"]["code"] in {"invalid-jwt", "access-denied"}, (
        f"{description}: {body['errors'][0]}"
    )


def test_the_admin_secret_is_not_a_way_in(store):
    """`X-Donat-Admin-Secret` authenticates a request; it is not a role.

    This engine has no admin role at all, so the secret cannot be a shortcut to
    one: with it and nothing else, the caller is the public.
    """

    alone = raw_graphql(
        store, "query { customer { customer_id } }", {"X-Donat-Admin-Secret": "guessed"}
    ).json()
    assert alone.get("errors"), f"the secret alone reached customer data: {alone}"

    anonymous_token = token(
        store, {"x-donat-default-role": "anonymous", "x-donat-allowed-roles": ["anonymous"]}
    )
    with_secret = raw_graphql(
        store,
        "query { product { id status } }",
        {"authorization": f"Bearer {anonymous_token}", "X-Donat-Admin-Secret": "guessed"},
    ).json()
    assert all(row["status"] == "published" for row in with_secret["data"]["product"]), (
        f"the secret widened what the public may see: {with_secret}"
    )


# -- widening a mutation -----------------------------------------------------


def test_an_unfiltered_update_still_only_reaches_your_own_row(shopper, support):
    """`where: {}` means "everything I am allowed to touch", not "everything".

    A shopper editing their own name with no filter is ordinary; the same
    request renaming every customer in the store would be the whole database.
    """

    before = customer_names(support)
    assert len(before) > 1, "this test needs a second customer to fail to touch"

    affected = shopper.graphql(
        'mutation { update_customer(where: {}, _set: {name: "Renamed By Test"}) { affected_rows } }'
    ).unwrap()["update_customer"]["affected_rows"]

    try:
        assert affected == 1, f"an unfiltered update reached {affected} rows"
        after = customer_names(support)
        assert after[OTHER_CUSTOMER] == before[OTHER_CUSTOMER], (
            "another shopper was renamed by a request that never named them"
        )
    finally:
        shopper.graphql(
            "mutation Restore($name: String!) { update_customer(where: {}, _set: {name: $name}) { affected_rows } }",
            {"name": before[d.CUSTOMER_ONE]},
        ).unwrap()


def test_an_unfiltered_delete_cannot_empty_the_store(shopper, other_shopper, well_stocked):
    d.cart_with_one_line(shopper)
    other_cart = d.cart_with_one_line(other_shopper)

    removed = shopper.graphql(
        "mutation { delete_cart_line(where: {}) { affected_rows } }"
    ).unwrap()["delete_cart_line"]["affected_rows"]

    assert removed == 1, f"an unfiltered delete reached {removed} rows"
    assert d.read_cart(other_shopper, other_cart)["lines"], (
        "another shopper's basket was emptied by a delete that never named it"
    )


def test_a_shopper_has_no_mutation_for_what_the_store_decides(shopper):
    """Order and payment state are the Process's to write, not a caller's.

    A shopper who could set `order_status` would not need to pay for anything.
    """

    for attempt in (
        'mutation { update_orders(where: {}, _set: {order_status: "authorized"}) { affected_rows } }',
        "mutation { delete_orders(where: {}) { affected_rows } }",
        "mutation { delete_payment(where: {}) { affected_rows } }",
    ):
        refused = shopper.graphql(attempt)
        assert refused.error_code() == "validation-failed", (
            f"the mutation exists for a shopper: {attempt} -> {refused.text[:160]}"
        )


def test_a_shopper_cannot_write_their_own_payment_paid(shopper, support, well_stocked, settle_timeout):
    """A payment says whether money moved. Only the store may say it did.

    The shopper's own commands do move a payment — to `cancellation_requested`
    or `void_in_progress`, which is a shopper asking to stop one. Everything
    else about it, above all `captured`, is the answer a provider gave, and a
    caller who could write it would have bought the order for nothing.
    """

    order = d.checkout_to_order(shopper, d.cart_with_one_line(shopper), timeout=settle_timeout)
    # Let the store finish moving the payment itself, so what is measured after
    # the attempts is the attempts and not the checkout still running.
    payment = d.await_payment_status(
        support, order["id"], {"authorized"}, timeout=settle_timeout
    )

    for column, value in (("status", '"captured"'), ("amount_minor", "1")):
        refused = shopper.graphql(
            "mutation Rewrite($id: uuid!) { update_payment(where: {id: {_eq: $id}}, "
            f"_set: {{{column}: {value}}}) {{ affected_rows }} }}",
            {"id": payment["id"]},
        )
        assert refused.errors, (
            f"a shopper rewrote payment.{column}: {refused.text[:200]}"
        )

    after = [row for row in d.payments_of(support, order["id"]) if row["id"] == payment["id"]][0]
    assert after["status"] == payment["status"] and after["amount_minor"] == payment["amount_minor"], (
        f"the payment moved anyway: {after} was {payment}"
    )


# -- putting SQL where a value goes ------------------------------------------


def test_an_enum_argument_is_an_enum_not_a_fragment_of_sql(anonymous):
    refused = anonymous.graphql(
        '{ product(order_by: {slug: "asc; DROP TABLE product"}) { id } }'
    )

    assert refused.error_code() == "validation-failed"
    assert d.catalogue(anonymous), "the catalogue is still there"


def test_an_agent_cannot_smuggle_sql_through_a_tool_argument(shopper):
    """The MCP tools take a table and columns by name, not a query.

    An agent is a caller like any other, and the generic tools are the widest
    argument surface in the store.
    """

    for arguments in (
        {"table": 'customer"; DROP TABLE product; --', "columns": ["customer_id"]},
        {"table": "product", "columns": ["id) , (SELECT current_user"]},
        {"table": "payment", "columns": ["id"]},
    ):
        answer = shopper.mcp_tool("select", arguments)
        refused = answer.value("result/isError") is True or answer.value("error") is not None
        assert refused, f"a tool argument was obeyed: {arguments} -> {answer.text[:200]}"

    assert d.catalogue(shopper), "the shop still answers afterwards"


def test_a_rest_parameter_carrying_a_payload_is_read_as_a_value(anonymous):
    for payload in ("../../v1/graphql", "%27%20OR%20%271%27%3D%271", "1;SELECT%201"):
        answer = anonymous.rest("GET", f"products/{payload}")
        assert answer.status in {200, 400, 404}, f"{payload}: {answer.status} {answer.text[:160]}"
        if answer.status == 200:
            assert answer.value("product", []) == [], (
                f"a path payload selected rows: {payload} -> {answer.text[:160]}"
            )

    assert d.catalogue(anonymous), "the shop still answers afterwards"


# -- shapes meant to hurt ----------------------------------------------------


def test_a_query_shaped_to_hurt_is_answered_or_refused_but_never_fatal(anonymous):
    """Wide, deep and absurd, one after another.

    A store is allowed to refuse any of these. What it may not do is stop
    serving, hang, or answer with something other than GraphQL.
    """

    shapes = {
        "wide": "{ %s }" % " ".join(f"a{i}: product {{ id }}" for i in range(120)),
        "deep": "{ product { variants { product { variants { product { variants { id } } } } } } }",
        "absurd limit": "{ product(limit: 2147483647) { id } }",
        "negative limit": "{ product(limit: -1) { id } }",
        "huge offset": "{ product(offset: 2147483647) { id } }",
    }

    for name, query in shapes.items():
        started = time.monotonic()
        answer = anonymous.graphql(query)
        assert answer.status == 200, f"{name}: HTTP {answer.status} {answer.text[:160]}"
        assert answer.data is not None or answer.errors, f"{name}: {answer.text[:160]}"
        assert time.monotonic() - started < 20, f"{name}: took too long to answer"

    assert d.catalogue(anonymous), "the shop is still serving after all of it"


def test_a_batch_of_operations_is_refused_clearly(store, anonymous):
    """Not supported is a fine answer; an unhandled one is not."""

    answer = requests.post(
        f"{store.config.base_url}/v1/graphql",
        json=[{"query": "{ product { id } }"}, {"query": "{ product { id } }"}],
        headers={"content-type": "application/json", **anonymous._headers()},
        timeout=store.config.request_timeout,
    )

    assert answer.status_code == 200, answer.text[:200]
    assert answer.json().get("errors"), f"a batch body was silently accepted: {answer.text[:200]}"
    assert d.catalogue(anonymous), "the shop still answers afterwards"


# -- taking somebody else's turn ---------------------------------------------


def test_replaying_another_shoppers_request_id_does_not_hand_over_their_answer(
    shopper, other_shopper, providers, settle_timeout
):
    """An idempotency key is scoped to who used it.

    If it were global, knowing somebody's request id would return their result
    — an order, a booking, a payment — to whoever asked second.
    """

    request_id = d.new_request_id()
    slot = f"2031-03-04T09:{request_id[:2]}"
    arguments = {
        "resource": d.new_request_id(),
        "slot": slot,
        "starts": "2031-03-04T09:00:00Z",
        "expires": "2031-03-04T08:00:00Z",
        "request": request_id,
    }
    mutation = """
        mutation Hold(
          $resource: uuid!, $slot: String!, $starts: timestamptz!,
          $expires: timestamptz!, $request: uuid!
        ) {
          start_grooming_booking(
            service_resource_id: $resource, slot_key: $slot, starts_at: $starts,
            hold_expires_at: $expires, request_id: $request
          ) { slot_key }
        }
    """

    mine = shopper.graphql(mutation, arguments).unwrap()
    theirs = other_shopper.graphql(mutation, arguments)

    assert mine["start_grooming_booking"]["slot_key"] == slot
    if not theirs.errors:
        # A second caller may be allowed to hold their own slot; what they may
        # not get is the first caller's booking.
        booked = shopper.query(
            "query B($slot: String!) { grooming_booking(where: {slot_key: {_eq: $slot}}) { id } }",
            {"slot": slot},
        )["grooming_booking"]
        assert len(booked) <= 1, f"a replayed key produced a second booking: {booked}"
    assert other_shopper.query(
        "query B($slot: String!) { grooming_booking(where: {slot_key: {_eq: $slot}}) { id } }",
        {"slot": slot},
    )["grooming_booking"] == [], "another shopper was handed the first one's booking"
