"""Tokens the suite issues for itself.

The store has no admin role: every request runs as one explicit role, and the
role travels in a signed token rather than in a trusted header. The suite signs
its own tokens with the stack's key, which is how a real client would arrive —
and it keeps the tests honest, because a token that claims a role it is not
allowed to assert is rejected by the engine, not by the test.
"""

from __future__ import annotations

import time

import jwt

from .config import CLAIMS_NAMESPACE, Config


def issue_token(
    config: Config,
    role: str,
    user_id: str | None = None,
    *,
    allowed_roles: list[str] | None = None,
    lifetime_seconds: int = 900,
    claims_namespace: str = CLAIMS_NAMESPACE,
) -> str:
    """A bearer token asserting `role`, optionally on behalf of `user_id`.

    `allowed_roles` defaults to the single requested role: a token minted for a
    customer cannot be replayed as staff by adding a header.
    """

    now = int(time.time())
    session: dict[str, object] = {
        "x-donat-default-role": role,
        "x-donat-allowed-roles": allowed_roles if allowed_roles is not None else [role],
    }
    if user_id is not None:
        # The only session variable the Petshop permissions read.
        session["x-donat-user-id"] = user_id
    payload = {
        "sub": user_id or role,
        "iat": now,
        "exp": now + lifetime_seconds,
        claims_namespace: session,
    }
    return jwt.encode(payload, config.jwt_key, algorithm=config.jwt_algorithm)


def issue_expired_token(config: Config, role: str, user_id: str | None = None) -> str:
    """A token that was valid a minute ago — for the rejection cases."""

    now = int(time.time())
    payload = {
        "sub": user_id or role,
        "iat": now - 3600,
        "exp": now - 60,
        CLAIMS_NAMESPACE: {
            "x-donat-default-role": role,
            "x-donat-allowed-roles": [role],
            **({"x-donat-user-id": user_id} if user_id else {}),
        },
    }
    return jwt.encode(payload, config.jwt_key, algorithm=config.jwt_algorithm)


def issue_token_signed_with(
    config: Config, key: str, role: str, user_id: str | None = None
) -> str:
    """A well-formed token signed with the wrong key."""

    forged = Config(
        base_url=config.base_url,
        jwt_key=key,
        jwt_algorithm=config.jwt_algorithm,
        providers_url=config.providers_url,
        request_timeout=config.request_timeout,
        settle_timeout=config.settle_timeout,
    )
    return issue_token(forged, role, user_id)
