"""Where the running store is and how to authenticate against it.

The suite never starts anything: it is pointed at a stack that is already up
(`scripts/petshop-stack.sh up`, `docker compose up` in `examples/petshop`, or a
deployed environment) and behaves like an outside tester with an account.
"""

from __future__ import annotations

import os
from dataclasses import dataclass

#: The claims namespace the engine reads session variables from.
CLAIMS_NAMESPACE = "https://donat.io/jwt/claims"

#: The MCP revision the engine speaks at POST /mcp.
MCP_PROTOCOL_VERSION = "2025-06-18"


@dataclass(frozen=True)
class Config:
    """One resolved target: the store, its signing key, its mock providers."""

    base_url: str
    jwt_key: str
    jwt_algorithm: str
    providers_url: str | None
    request_timeout: float
    settle_timeout: float

    @classmethod
    def from_env(cls) -> "Config | None":
        """The configured target, or None when the suite was not pointed at one.

        Absence is not a failure: `PETSHOP_BASE_URL` is how a run opts in, so a
        plain `pytest` with no stack skips instead of reporting false red.
        """

        base_url = os.environ.get("PETSHOP_BASE_URL", "").strip()
        if not base_url:
            return None
        providers_url = os.environ.get("PETSHOP_PROVIDERS_URL", "").strip()
        return cls(
            base_url=base_url.rstrip("/"),
            # Must equal the `key` in the engine's DONAT_GRAPHQL_JWT_SECRET.
            jwt_key=os.environ.get("PETSHOP_JWT_KEY", "petshop-dev-jwt-key-change-me-32bytes+"),
            jwt_algorithm=os.environ.get("PETSHOP_JWT_ALGORITHM", "HS256"),
            providers_url=providers_url.rstrip("/") or None,
            request_timeout=float(os.environ.get("PETSHOP_REQUEST_TIMEOUT", "15")),
            # Durable Processes advance in the background, so every assertion
            # about their outcome polls up to this long before failing.
            settle_timeout=float(os.environ.get("PETSHOP_SETTLE_TIMEOUT", "30")),
        )
