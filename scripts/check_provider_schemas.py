#!/usr/bin/env python3
"""Compare this workspace's declared connector surface against each provider's
own published API schema.

Why this exists
---------------

Every connector test in `crates/connectors` runs against a local stub whose
expectations were written from the same reading of the provider's documentation
as the connector itself. That proves the connector does what we told it to. It
cannot prove we told it the right thing: a misread path or a misspelled query
parameter is written into both halves at once, the test is green, and the
provider answers 404.

A provider's own published schema is independent of our reading, needs no
credential, and is admitted as ground truth by ADR 037. This tool holds the two
side by side and reports every declared operation the schema cannot account
for.

What a finding means
--------------------

A finding is a *question*, not a verdict. Schemas are routinely incomplete,
version-shifted, or split across documents, so an unmatched operation may be
our bug or may be a gap in the schema. The tool says which operations to go and
read the documentation for; a human resolves each one against that
documentation, never against this output.

`uncovered` is the honest column: a connector with no published schema is
unverifiable by this method, and saying so is the point.

Usage
-----

    cargo test -p donat-connectors --features testing --test provider_schema
    # with DONAT_DECLARATIONS_OUT=<path> to write the dump, then:
    python3 scripts/check_provider_schemas.py --declarations <path>

Exit status is 0 unless --strict is given, because an unmatched operation is a
prompt to read documentation rather than a build failure.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys
import urllib.error
import urllib.request

REPO = pathlib.Path(__file__).resolve().parent.parent
REGISTRY = REPO / "scripts" / "provider_schemas.json"
DEFAULT_CACHE = REPO / "target" / "provider-schemas"
USER_AGENT = "donat-connector-schema-audit"

# `{id}`, and also the `{Sid}` half of a segment like `{Sid}.json`.
PLACEHOLDER = re.compile(r"\{[^{}]*\}")


# ---------------------------------------------------------------------------
# Fetching
# ---------------------------------------------------------------------------


def fetch(url: str, cache: pathlib.Path, refresh: bool) -> bytes | None:
    """The schema bytes, from the cache when it has them.

    Cached on disk because these documents are large — GitHub's is tens of
    megabytes — and because a run that re-reads the same bytes is a run whose
    findings are comparable with the last one.
    """
    cache.mkdir(parents=True, exist_ok=True)
    key = "".join(c if c.isalnum() else "_" for c in url)[-150:]
    path = cache / key
    if path.exists() and not refresh:
        return path.read_bytes()
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=120) as response:
            body = response.read()
    except (urllib.error.URLError, TimeoutError) as error:
        print(f"  ! could not fetch {url}: {error}", file=sys.stderr)
        return None
    path.write_bytes(body)
    return body


def parse(body: bytes) -> dict | None:
    try:
        return json.loads(body)
    except json.JSONDecodeError:
        pass
    try:
        import yaml
    except ImportError:
        print("  ! a YAML schema needs PyYAML (pip install pyyaml)", file=sys.stderr)
        return None
    try:
        return yaml.safe_load(body)
    except yaml.YAMLError as error:
        print(f"  ! unparseable schema: {error}", file=sys.stderr)
        return None


# ---------------------------------------------------------------------------
# Matching
# ---------------------------------------------------------------------------


def normalise(path: str) -> tuple[str, ...]:
    """A path as comparable segments, every `{placeholder}` reduced to `*`.

    Parameter *names* are deliberately discarded: the provider calls it
    `{issue_number}` and we may call it `{issue}`, and that difference is a
    naming choice, not a disagreement about the surface.
    """
    segments = []
    for segment in path.strip("/").split("/"):
        # A placeholder is replaced wherever it sits inside the segment, not
        # only when it is the whole of it: Twilio spells a resource read
        # `/Calls/{Sid}.json`, so a rule that only collapsed a whole segment
        # left `{Sid}.json` and `{sid}.json` looking like different surfaces.
        segments.append(PLACEHOLDER.sub("*", segment))
    return tuple(segments)


def server_prefixes(schema: dict) -> list[tuple[str, ...]]:
    """The path components each declared server contributes.

    A schema routinely puts the version in the server URL (`.../v1`) and starts
    every path after it, while our declaration carries the whole path from the
    origin. Without this, every OpenAI operation would read as unmatched.
    """
    prefixes: list[tuple[str, ...]] = []
    for server in schema.get("servers") or []:
        url = server.get("url") if isinstance(server, dict) else None
        if not url:
            continue
        without_scheme = url.split("://", 1)[-1]
        path = without_scheme.split("/", 1)[1] if "/" in without_scheme else ""
        components = normalise(path)
        if components and components != ("",):
            prefixes.append(components)
    # Swagger 2.0 spells it differently.
    base = schema.get("basePath")
    if isinstance(base, str):
        components = normalise(base)
        if components and components != ("",):
            prefixes.append(components)
    return prefixes


def is_discovery(schema: dict) -> bool:
    """Whether this is a Google API Discovery document rather than OpenAPI.

    Google does not publish OpenAPI for its workspace APIs; it publishes this,
    which carries the same two facts the audit needs — the path and the HTTP
    method — under different keys.
    """
    return "discoveryVersion" in schema or (
        "resources" in schema and "servicePath" in schema
    )


def discovery_index(schema: dict) -> dict[tuple[str, ...], set[str]]:
    """The Discovery document's methods, as the same path → methods table.

    A method's `path` is relative to `servicePath`, and our declarations carry
    the whole path from the origin, so the two are joined here — the Discovery
    equivalent of an OpenAPI server prefix.
    """
    table: dict[tuple[str, ...], set[str]] = {}
    prefix = normalise(schema.get("servicePath") or "")
    prefix = tuple(part for part in prefix if part)

    def walk(node: dict) -> None:
        for method in (node.get("methods") or {}).values():
            if not isinstance(method, dict):
                continue
            path, verb = method.get("path"), method.get("httpMethod")
            if not isinstance(path, str) or not isinstance(verb, str):
                continue
            table.setdefault(prefix + normalise(path), set()).add(verb.upper())
        # Discovery nests resources inside resources.
        for child in (node.get("resources") or {}).values():
            if isinstance(child, dict):
                walk(child)

    walk(schema)
    return table


def index(schema: dict) -> dict[tuple[str, ...], set[str]]:
    """Every path the schema publishes, mapped to the methods it allows."""
    if is_discovery(schema):
        return discovery_index(schema)
    table: dict[tuple[str, ...], set[str]] = {}
    paths = schema.get("paths")
    if not isinstance(paths, dict):
        return table
    prefixes = server_prefixes(schema)
    for path, item in paths.items():
        if not isinstance(item, dict):
            continue
        methods = {
            key.upper()
            for key in item
            if key.lower()
            in {"get", "head", "post", "put", "patch", "delete", "options", "trace"}
        }
        if not methods:
            continue
        components = normalise(path)
        if prefixes:
            # Only as seen from the origin. Indexing the bare path *as well*
            # would accept a declaration that had dropped the version segment —
            # `/models` where the provider serves `/v1/models` — which is
            # exactly the class of mistake this audit exists to catch. Every
            # match in the current register survives the stricter rule, so the
            # looser one was buying nothing.
            for prefix in prefixes:
                table.setdefault(prefix + components, set()).update(methods)
        else:
            table.setdefault(components, set()).update(methods)
    return table


def lookup(
    table: dict[tuple[str, ...], set[str]], components: tuple[str, ...]
) -> set[str] | None:
    """The methods the schema allows on this path, exact match preferred.

    A schema placeholder also matches a literal we compiled in, because a
    placeholder position accepts any value: Gmail publishes
    `users/{userId}/messages` and this workspace declares `users/me/messages`,
    where `me` is a documented value of that very parameter. Exact keys are
    tried first, so a genuinely distinct static route — `/users/settings`
    beside `/users/{id}` — still resolves to itself rather than to the
    placeholder.
    """
    exact = table.get(components)
    if exact is not None:
        return exact
    matched: set[str] = set()
    for key, methods in table.items():
        if len(key) != len(components):
            continue
        if all(k == c or k == "*" for k, c in zip(key, components)):
            matched |= methods
    return matched or None


def verdict(
    table: dict[tuple[str, ...], set[str]], path: str, method: str
) -> tuple[str, str]:
    """How the schema accounts for one declared operation."""
    components = normalise(path)
    allowed = lookup(table, components)
    if allowed is None:
        return ("path-absent", "no path of this shape in the schema")
    if method.upper() not in allowed:
        return (
            "method-absent",
            f"schema allows {', '.join(sorted(allowed))} on this path",
        )
    return ("ok", "")


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--declarations",
        required=True,
        type=pathlib.Path,
        help="the JSON written by the provider_schema test",
    )
    parser.add_argument("--cache", type=pathlib.Path, default=DEFAULT_CACHE)
    parser.add_argument(
        "--refresh", action="store_true", help="re-fetch instead of using the cache"
    )
    parser.add_argument("--module", help="check one module only")
    parser.add_argument(
        "--strict",
        action="store_true",
        help="exit non-zero when a declared operation is unaccounted for",
    )
    args = parser.parse_args()

    declarations = json.loads(args.declarations.read_text())
    registry = {
        key: value
        for key, value in json.loads(REGISTRY.read_text()).items()
        if not key.startswith("//")
    }

    # A registry key that matches no module is the quietest possible failure:
    # the connector it was meant to cover simply reports as uncovered, which
    # reads like "no schema exists" rather than "this file has a typo". It
    # happened once already — `box_platform` for a module named `box`.
    known = {module["module"] for module in declarations["modules"]}
    stray = sorted(set(registry) - known)
    if stray:
        print(
            f"error: {REGISTRY.name} names modules that do not exist: "
            f"{', '.join(stray)}",
            file=sys.stderr,
        )
        return 2

    covered, uncovered, findings, unreachable = [], [], [], []
    total_ops = matched_ops = 0

    for module in declarations["modules"]:
        name = module["module"]
        if args.module and name != args.module:
            continue
        operations = module["operations"]
        entry = registry.get(name)
        if entry is None:
            uncovered.append((name, len(operations)))
            continue

        # A provider may publish one document per API rather than one for all
        # of them — PayPal ships Orders, Billing and Reporting separately — so
        # an entry may name several and their tables are merged.
        urls = entry["url"] if isinstance(entry["url"], list) else [entry["url"]]
        merged: dict[tuple[str, ...], set[str]] = {}
        failed = False
        for url in urls:
            body = fetch(url, args.cache, args.refresh)
            one = parse(body) if body else None
            if not one:
                failed = True
                continue
            for key, methods in index(one).items():
                merged.setdefault(key, set()).update(methods)
        schema = None if failed and not merged else merged
        if not schema:
            # Registered but unreadable is not the same as unregistered. Left in
            # `uncovered` alone it would read as "no schema exists", and
            # `--strict` would pass because a schema nobody could fetch produced
            # no findings — the audit going quiet exactly when it stopped working.
            unreachable.append(name)
            uncovered.append((name, len(operations)))
            continue
        table = merged
        if not table:
            print(f"  ! {name}: schema published no paths", file=sys.stderr)
            unreachable.append(name)
            uncovered.append((name, len(operations)))
            continue

        module_findings = []
        for operation in operations:
            total_ops += 1
            state, detail = verdict(table, operation["path_template"], operation["method"])
            if state == "ok":
                matched_ops += 1
            else:
                module_findings.append((operation, state, detail))
        covered.append((name, len(operations), len(module_findings)))
        findings.extend((name, *item) for item in module_findings)

    # ---- report -----------------------------------------------------------
    print("=" * 78)
    print("Declared connector surface vs. each provider's published schema")
    print("=" * 78)

    print(f"\nChecked {len(covered)} connectors, {total_ops} executable operations.")
    if total_ops:
        print(f"Accounted for by the schema: {matched_ops}/{total_ops}")

    if covered:
        print("\n-- checked ------------------------------------------------")
        for name, count, bad in sorted(covered, key=lambda row: -row[2]):
            mark = "ok " if bad == 0 else "??"
            print(f"  {mark} {name:<18} {count - bad}/{count} accounted for")

    if findings:
        print("\n-- operations to read the documentation for ----------------")
        print("   (a schema gap and our bug look identical here; go and read)")
        current = None
        for name, operation, state, detail in findings:
            if name != current:
                print(f"\n  {name}")
                current = name
            print(f"    {operation['method']:<6} {operation['path_template']}")
            print(f"           {operation['id']} — {state}: {detail}")

    if uncovered:
        print("\n-- no published schema, so unverifiable this way -----------")
        total_unverifiable = sum(count for _, count in uncovered)
        for name, count in sorted(uncovered):
            print(f"     {name:<18} {count} operations")
        print(f"\n  {len(uncovered)} connectors, {total_unverifiable} operations.")
        print("  These need a recorded response or a live smoke test instead.")

    if unreachable:
        print(
            f"\nwarning: {len(unreachable)} registered schema(s) could not be read: "
            f"{', '.join(sorted(unreachable))}",
            file=sys.stderr,
        )
    if args.strict and (findings or unreachable):
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
