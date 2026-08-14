#!/usr/bin/env python3
"""Give the bootstrap operator the password from `.env`.

The provider's bootstrap files are committed, so a password in one of them is
a password everybody who clones this repository shares — the same thing
`.env.example` exists to avoid. So `users.json` declares who the operator is
and nothing about how they prove it, and this puts the secret in afterwards,
through the provider's own admin API with the engine's own key.

It runs on every `up`, so editing `.env` and starting the stack does what it
looks like it does. Running it twice with the *same* password is not an error
either — the provider refuses to reuse a recent password, and "that password is
already this account's" is the outcome this wanted.
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request

API = os.environ["IDP_API"].rstrip("/")
KEY = os.environ["IDP_ADMIN_KEY"]
EMAIL = os.environ.get("OPERATOR_EMAIL", "operator@example.com")
PASSWORD = os.environ["OPERATOR_PASSWORD"]
DEADLINE = time.monotonic() + 120


class Refused(Exception):
    """The provider answered, and said no. Its words, not a stack trace."""

    def __init__(self, status: int, message: str) -> None:
        super().__init__(f"the identity provider answered {status}: {message}")
        self.status = status
        self.message = message


def call(method: str, path: str, body: dict | None = None) -> object:
    request = urllib.request.Request(
        f"{API}{path}",
        method=method,
        headers={"Authorization": KEY, "Content-Type": "application/json"},
        data=json.dumps(body).encode() if body is not None else None,
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            payload = response.read()
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")
        try:
            detail = json.loads(detail).get("message", detail)
        except json.JSONDecodeError:
            pass
        raise Refused(error.code, detail) from None
    return json.loads(payload) if payload else None


def wait_for_the_provider() -> list:
    """The provider is still writing its bootstrap when this starts."""
    while True:
        try:
            return call("GET", "/users")
        except (urllib.error.URLError, TimeoutError, ConnectionError, Refused) as error:
            if time.monotonic() > DEADLINE:
                raise SystemExit(f"the identity provider never answered: {error}")
            time.sleep(2)


def main() -> int:
    users = wait_for_the_provider()
    found = next((user for user in users if user["email"] == EMAIL), None)
    if found is None:
        # Bootstrap applies only to an empty database; an existing one that
        # predates this account is a real state, not an error to hide.
        print(f"no {EMAIL} at the provider — nothing to set", file=sys.stderr)
        return 1

    detail = call("GET", f"/users/{found['id']}")
    try:
        call(
            "PUT",
            f"/users/{found['id']}",
            {
                "email": detail["email"],
                "given_name": detail["given_name"],
                "family_name": detail.get("family_name"),
                "roles": detail.get("roles", []),
                "groups": detail.get("groups", []),
                "enabled": True,
                "email_verified": True,
                "password": PASSWORD,
            },
        )
    except Refused as refused:
        # The provider keeps a short history and will not take a password back
        # that is already in it — which, on a second run with an unchanged
        # `.env`, means the account already has this one.
        if refused.status == 400 and "used passwords" in refused.message:
            print(f"{EMAIL} already has this password")
            return 0
        print(f"could not set the password for {EMAIL}: {refused.message}", file=sys.stderr)
        return 1

    print(f"{EMAIL} can sign in")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
