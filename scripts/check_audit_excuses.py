#!/usr/bin/env python3
"""Which excused advisories no longer apply.

`.cargo/audit.toml` excuses an advisory by name with a reachability argument,
under one condition: "an entry that has become removable is removed". Nothing
checks that condition — `cargo audit` is silent about an ignore it never
needed, so an excuse outlives the dependency it was written for and the file
slowly stops meaning anything.

This runs `cargo audit` against the same lockfile *without* the config (from
another working directory, which is where cargo-audit looks for it) and
reports every excused id the unconfigured run did not raise. Exit 1 when any
excuse is stale, so the nightly run can open an issue; 0 otherwise.

Usage:
    check_audit_excuses.py [--no-fetch]
"""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / ".cargo" / "audit.toml"
LOCKFILE = ROOT / "Cargo.lock"


def excused() -> list[str]:
    with CONFIG.open("rb") as f:
        return list(tomllib.load(f).get("advisories", {}).get("ignore", []))


def raised(no_fetch: bool) -> set[str]:
    """Every advisory id cargo-audit raises for the lockfile, ignores unapplied."""
    args = ["cargo", "audit", "--json", "--quiet", "--file", str(LOCKFILE)]
    if no_fetch:
        args.append("--no-fetch")
    with tempfile.TemporaryDirectory() as elsewhere:
        proc = subprocess.run(args, cwd=elsewhere, capture_output=True, text=True, check=False)
    # cargo-audit exits 1 when it found something, which here is the point.
    if not proc.stdout.strip():
        raise RuntimeError(f"cargo audit produced no report:\n{proc.stderr.strip()}")
    report = json.loads(proc.stdout)
    ids = {entry["advisory"]["id"] for entry in report["vulnerabilities"]["list"]}
    for warnings in report.get("warnings", {}).values():
        ids.update(entry["advisory"]["id"] for entry in warnings if entry.get("advisory"))
    return ids


def stale(excuses: list[str], live: set[str]) -> list[str]:
    return [advisory for advisory in excuses if advisory not in live]


def self_test() -> int:
    assert stale(["A", "B"], {"B"}) == ["A"]
    assert stale(["A"], {"A", "C"}) == []
    assert stale([], set()) == []
    print("self-test ok")
    return 0


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()
    try:
        excuses = excused()
        live = raised(no_fetch="--no-fetch" in argv)
    except (RuntimeError, OSError, KeyError, json.JSONDecodeError) as err:
        print(f"audit excuses: {err}", file=sys.stderr)
        return 2
    gone = stale(excuses, live)
    print(f"{len(excuses)} advisories excused in {CONFIG.relative_to(ROOT)}, {len(live)} raised without it")
    if not gone:
        print("every excuse still applies")
        return 0
    print("excused but no longer raised — remove from .cargo/audit.toml:")
    for advisory in gone:
        print(f"  {advisory}")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
