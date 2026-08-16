#!/usr/bin/env python3
"""Fill `.env.example`'s blanks with fresh secrets, on stdout.

Every name and every comment comes from `.env.example`, so this file decides
nothing about the shape of the configuration — it only supplies values for the
lines that ship empty. A name added there and not here is reported rather than
left blank, because a blank is what stops the stack from starting.
"""

import base64
import re
import secrets
import string
import sys

ALNUM = string.ascii_letters + string.digits


def alnum(length: int) -> str:
    return "".join(secrets.choice(ALNUM) for _ in range(length))


def b64(size: int) -> str:
    return base64.b64encode(secrets.token_bytes(size)).decode()


# The provider's policy wants an upper, a lower and a digit; the rest is length.
def password() -> str:
    return f"Donat-{alnum(12)}-{secrets.randbelow(9000) + 1000}"


VALUES = {
    "DONAT_ADMIN_PASSWORD": password,
    "DONAT_DB_PASSWORD": lambda: alnum(32),
    "DONAT_IDP_ENC_KEY": lambda: b64(32),
    "DONAT_IDP_RAFT_SECRET": lambda: alnum(32),
    "DONAT_IDP_API_SECRET": lambda: alnum(32),
    "DONAT_S3_KEY": lambda: alnum(20),
    "DONAT_S3_SECRET": lambda: alnum(40),
    "DONAT_FILE_SIGNING_SECRET": lambda: b64(32),
}


def main() -> int:
    with open(".env.example", encoding="utf-8") as handle:
        template = handle.read()

    missing = []
    out = []
    for line in template.splitlines():
        match = re.fullmatch(r"([A-Z0-9_]+)=", line)
        if not match:
            out.append(line)
            continue
        name = match.group(1)
        if name not in VALUES:
            missing.append(name)
            out.append(line)
            continue
        out.append(f"{name}={VALUES[name]()}")

    if missing:
        print(
            f"generate-env: no value for {', '.join(missing)} — add it to "
            "scripts/generate-env.py",
            file=sys.stderr,
        )
        return 1

    print("\n".join(out))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
