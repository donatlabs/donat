#!/usr/bin/env python3
"""Create the operator this deployment signs in as.

Not `deploy/bootstrap/users.json`, where this used to live and could not stay:
that file requires a `password` field — the provider refuses to parse it
without one — so declaring the operator there meant either committing a working
password or a stack that panics on first start with `missing field
"password"`. It panicked. Only on a *first* start, which is why it survived so
long: the bootstrap files apply solely to an empty database, so every stack
already running had been created before the password was taken out.

Nor the provider's own bootstrap admin. That account exists for the provider,
and it carries the provider's own roles — `admin`, `rauthy_admin`. Making it
the operator too puts a role this deployment never declared at the front of the
token's list, which is what `x-donat-default-role` reads. The operator here has
exactly one role, `support`: the one `deploy/metadata` writes permissions
against and the one the panel asserts.

So the account is created through the provider's admin API with the engine's
own key, from the environment, on every `up`. Idempotent: an account that
already exists gets its password and role brought back in line, because
editing `.env` and starting the stack should do what it looks like it does.
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
ROLE = os.environ.get("OPERATOR_ROLE", "support")
PASSWORD = os.environ["OPERATOR_PASSWORD"]
DEADLINE = time.monotonic() + 120


class Refused(Exception):
    def __init__(self, status: int, message: str):
        super().__init__(message)
        self.status = status
        self.message = message


def call(method: str, path: str, body: dict | None = None):
    request = urllib.request.Request(
        f"{API}{path}",
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={"Authorization": KEY, "Accept": "application/json"}
        | ({"Content-Type": "application/json"} if body is not None else {}),
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            payload = response.read().decode()
    except urllib.error.HTTPError as error:
        detail = error.read().decode()
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


def create() -> int:
    call(
        "POST",
        "/users",
        {
            "email": EMAIL,
            "given_name": "Olive",
            "family_name": "Operator",
            "language": "en",
            "roles": [ROLE],
            "groups": [],
            "user_expires": None,
        },
    )
    # The provider creates an account without a password and emails a link to
    # set one. There is no mail server here and the password is already known,
    # so it is put in directly — the same call the account's own screen makes.
    found = next(user for user in call("GET", "/users") if user["email"] == EMAIL)
    set_password(found["id"], [ROLE])
    print(f"created {EMAIL} with {ROLE}")
    return 0


def set_password(user_id: str, roles: list[str]) -> None:
    detail = call("GET", f"/users/{user_id}")
    call(
        "PUT",
        f"/users/{user_id}",
        {
            "email": detail["email"],
            "given_name": detail.get("given_name"),
            "family_name": detail.get("family_name"),
            "roles": roles,
            "groups": detail.get("groups", []),
            "enabled": True,
            "email_verified": True,
            "password": PASSWORD,
        },
    )


def main() -> int:
    users = wait_for_the_provider()
    found = next((user for user in users if user["email"] == EMAIL), None)
    if found is None:
        return create()

    detail = call("GET", f"/users/{found['id']}")
    roles = list(detail.get("roles") or [])
    if ROLE not in roles:
        roles = roles + [ROLE]
    try:
        set_password(found["id"], roles)
    except Refused as refused:
        # The provider keeps a short history and will not take a password back
        # that is already in it — which, on a second run with an unchanged
        # `.env`, means the account already has this one.
        if refused.status == 400 and "used passwords" in refused.message:
            print(f"{EMAIL} already has this password")
            return 0
        print(f"could not provision {EMAIL}: {refused.message}", file=sys.stderr)
        return 1

    print(f"{EMAIL} can sign in, with {ROLE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
