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


def test_no_header_is_a_way_in(store):
    """A role comes from a verified token. No header can add one.

    This engine has no admin role and no shared secret: the two mechanisms that
    name a role are a verified JWT and an authentication hook. Headers that look
    authoritative — the admin secret this engine used to accept, a bare role
    header, the Hasura spellings of either — are just headers, and a token
    granting one role is not widened by any of them.
    """

    for pretender in (
        {"X-Donat-Admin-Secret": "guessed"},
        {"X-Donat-Role": "staff"},
        {"X-Hasura-Admin-Secret": "guessed"},
    ):
        alone = raw_graphql(store, "query { customer { customer_id } }", pretender).json()
        assert alone.get("errors"), f"{pretender} alone reached customer data: {alone}"

    anonymous_token = token(
        store, {"x-donat-default-role": "anonymous", "x-donat-allowed-roles": ["anonymous"]}
    )

    # A header that looks like a credential is not one: it is ignored, and the
    # request runs as the token says it does.
    with_secrets = raw_graphql(
        store,
        "query { product { id status } }",
        {
            "authorization": f"Bearer {anonymous_token}",
            "X-Donat-Admin-Secret": "guessed",
            "X-Hasura-Admin-Secret": "guessed",
        },
    ).json()
    assert all(row["status"] == "published" for row in with_secrets["data"]["product"]), (
        f"a header widened what the public may see: {with_secrets}"
    )

    # `X-Donat-Role` is the one header the engine reads, and it only *picks*
    # among the roles a token already granted. Naming one it did not is
    # refused outright rather than ignored — stricter than being ignored, and
    # the difference an operator sees is an error instead of silence.
    as_staff = raw_graphql(
        store,
        "query { product { id status } }",
        {"authorization": f"Bearer {anonymous_token}", "X-Donat-Role": "staff"},
    ).json()
    assert as_staff.get("errors"), f"a header granted a role the token did not: {as_staff}"
    assert "allowed roles" in as_staff["errors"][0]["message"].lower(), as_staff


# -- putting SQL where a value goes ------------------------------------------


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
