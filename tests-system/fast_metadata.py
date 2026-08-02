"""Build a copy of the Petshop metadata whose clocks run in seconds.

Several branches of the store only happen when time passes: a grooming hold
expires, a veterinarian misses the review deadline, support never answers a
return, a failed subscription renewal waits a day and tries again. Those
periods are declared as days, so no test can reach them by waiting.

This rewrites the declared periods — and only those — into seconds, so the same
flows can be walked end to end on a stand of their own. Nothing else about the
metadata changes: the states, commands, rules and permissions under test are
the ones the store actually ships.

    python3 tests-system/fast_metadata.py <source-metadata> <destination>
"""

from __future__ import annotations

import pathlib
import re
import shutil
import sys

#: Declared periods become this many seconds. Long enough that a test can act
#: before the deadline when it means to, short enough to wait out when it does
#: not.
DEADLINE_SECONDS = 12

#: The dunning ladder's own delays, keyed by the decision table's output field.
DUNNING_SECONDS = 3

PERIOD = re.compile(r"(?m)^(?P<indent>\s*)deadline:\s*(?P<value>[0-9]+(?:ms|s|m|h|d))\s*$")
INLINE_PERIOD = re.compile(r"deadline:\s*([0-9]+(?:ms|s|m|h|d))")
DELAY_SECONDS = re.compile(r"delay_seconds:\s*(?P<value>[0-9]{2,})")


def shorten(text: str) -> tuple[str, int]:
    """Rewrite declared periods; report how many were changed."""

    changes = 0

    def block(match: re.Match[str]) -> str:
        nonlocal changes
        changes += 1
        return f"{match.group('indent')}deadline: {DEADLINE_SECONDS}s"

    def inline(match: re.Match[str]) -> str:
        nonlocal changes
        changes += 1
        return f"deadline: {DEADLINE_SECONDS}s"

    def delay(match: re.Match[str]) -> str:
        nonlocal changes
        # A zero delay is the ladder's "give up now" row, and it stays zero.
        if match.group("value") == "0":
            return match.group(0)
        changes += 1
        return f"delay_seconds: {DUNNING_SECONDS}"

    text = PERIOD.sub(block, text)
    text = INLINE_PERIOD.sub(inline, text)
    text = DELAY_SECONDS.sub(delay, text)
    return text, changes


def build(source: pathlib.Path, destination: pathlib.Path) -> int:
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination)
    changed = 0
    for path in destination.rglob("*.yaml"):
        text = path.read_text()
        rewritten, count = shorten(text)
        if count:
            path.write_text(rewritten)
            changed += count
    return changed


if __name__ == "__main__":
    source = pathlib.Path(sys.argv[1])
    destination = pathlib.Path(sys.argv[2])
    count = build(source, destination)
    print(f"shortened {count} declared periods into {destination}")
