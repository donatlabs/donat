"""What each kind of visitor may see and do.

The store has no admin role, so every one of these is a real permission
decision made by the engine for the role the token asserts. A tester's first
job is to prove the walls are where the store says they are.
"""

from __future__ import annotations

from petshop_qa import domain as d
from petshop_qa import issue_expired_token, issue_token, issue_token_signed_with


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
