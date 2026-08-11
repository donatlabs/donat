#!/usr/bin/env python3
"""Did this change to the skills help? A paired reading of two arms.

    python3 evals/agent.py 001-checkout-ambiguous-payment --attempts 3 --label before
    # edit plugins/donat/skills/...
    python3 evals/agent.py 001-checkout-ambiguous-payment --attempts 3 --skills plugin --label after
    python3 evals/compare.py before after

Absolute scores and paired scores answer different questions, and the second is
the one a tuning loop needs. "pass@1 went from 0.667 to 1.0" on three attempts
is two thirds of noise; "of the scenarios that disagreed between the arms, nine
of ten went the new way" is a measurement, on exactly the same evidence.

The pairing is by (task, attempt index, scenario). Task difficulty and scenario
difficulty are common to both arms and cancel; what is left is the arm. This is
the standard remedy when comparing two systems on a small set, and it is the
reason a corpus of two tasks can still tell you something.

What it is *not*: the attempts themselves are not paired samples — each one is a
fresh session with its own randomness — so this measures the arms on matched
work, not the same work done twice. Read it as evidence about the change, and
keep the absolute number for a held-out split that no tuning ever touched.
"""

from __future__ import annotations

import argparse
import collections
import json
import math
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import run as harness  # noqa: E402

WORKSPACES = pathlib.Path(
    __import__("os").environ.get("EVAL_WORKSPACE_ROOT",
                                 pathlib.Path.home() / ".cache" / "donat-evals")
) / "agent"


def load_arm(label: str) -> list[dict]:
    """Every recorded attempt that carried this label, newest run per attempt."""

    found: dict[tuple[str, int], dict] = {}
    for record in sorted(WORKSPACES.glob("*--runs.jsonl")):
        for line in record.read_text().splitlines():
            if not line.strip():
                continue
            row = json.loads(line)
            if row.get("arm") != label:
                continue
            # Later lines win: a re-run of attempt 2 replaces the earlier one.
            found[(row["task"], row["attempt"])] = row
    return [found[key] for key in sorted(found)]


def exact_binomial_ci(successes: int, total: int, level: float = 0.95) -> list[float]:
    """Clopper-Pearson, by bisection on the exact binomial tails.

    A normal approximation on eleven observations is not an interval, it is a
    decoration. The counts here are small enough to sum the distribution
    directly, which is both exact and short enough to check by eye — a
    continued-fraction beta was tried first and returned a lower bound above its
    own upper bound, which is the kind of error a statistic hides well.
    """

    if total == 0:
        return [0.0, 1.0]
    alpha = 1 - level

    def at_least(p: float) -> float:
        return sum(math.comb(total, k) * p ** k * (1 - p) ** (total - k)
                   for k in range(successes, total + 1))

    def at_most(p: float) -> float:
        return sum(math.comb(total, k) * p ** k * (1 - p) ** (total - k)
                   for k in range(0, successes + 1))

    def bisect(target, predicate) -> float:
        low, high = 0.0, 1.0
        for _ in range(80):
            mid = (low + high) / 2
            if predicate(mid) < target:
                low = mid
            else:
                high = mid
        return (low + high) / 2

    # Lower: the p at which seeing this many or more is already unlikely —
    # `at_least` rises with p, so it crosses alpha/2 from below. Upper: the p at
    # which seeing this many or fewer is unlikely; `at_most` *falls* with p, so
    # the crossing to bisect on is its complement against 1 - alpha/2. Getting
    # that tail the wrong way round yields an upper bound below the estimate,
    # which is what the first version of this function did.
    lower = 0.0 if successes == 0 else bisect(alpha / 2, at_least)
    upper = 1.0 if successes == total else bisect(1 - alpha / 2, lambda p: 1 - at_most(p))
    return [round(lower, 3), round(upper, 3)]


def compare(before: str, after: str) -> int:
    left, right = load_arm(before), load_arm(after)
    if not left or not right:
        sys.exit(f"nothing recorded for {before if not left else after!r} — run "
                 f"agent.py with --label first")

    index = {(r["task"], r["attempt"]): r for r in right}
    pairs = [(a, index[(a["task"], a["attempt"])]) for a in left
             if (a["task"], a["attempt"]) in index]
    if not pairs:
        sys.exit("the two arms share no (task, attempt) — pair them on the same tasks "
                 "and the same number of attempts, or the comparison is not paired")

    print(f"== {before} → {after}   {len(pairs)} matched attempt(s)")
    # A paired comparison assumes both arms answered the same question. Editing
    # a prompt — to fix an ambiguity, say — silently ends that, and the labels
    # go on looking comparable.
    drifted = sorted({a["task"] for a, b in pairs
                      if a.get("task_hash") and b.get("task_hash")
                      and a["task_hash"] != b["task_hash"]})
    if drifted:
        print(f"   WARNING: the task itself changed between the arms "
              f"({', '.join(drifted)}). These are two different questions; the "
              f"comparison below is not paired in the way it looks.")
    unmatched = len(left) + len(right) - 2 * len(pairs)
    if unmatched:
        print(f"   {unmatched} attempt(s) had no partner and are excluded")

    gained, lost = [], []
    per_task = collections.defaultdict(lambda: [0, 0])
    for a, b in pairs:
        for name, was in (a.get("scenarios") or {}).items():
            now = (b.get("scenarios") or {}).get(name)
            if now is None or was == now:
                continue
            moved = (a["task"], a["attempt"], name)
            if was != "pass" and now == "pass":
                gained.append(moved)
                per_task[a["task"]][0] += 1
            elif was == "pass" and now != "pass":
                lost.append(moved)
                per_task[a["task"]][1] += 1

    # Before any scenario arithmetic: the outcome each attempt landed on. An
    # attempt that stopped at a gate has *no* scenario verdicts, so scenario
    # pairing drops it — and drops its partner's failures with it. The first
    # run of this comparison read "+2 -0, the new arm is better" while quietly
    # hiding that the new arm had failed to build once. A reading that improves
    # by discarding its own worst attempt is not a reading.
    moves = collections.Counter()
    for a, b in pairs:
        moves[(harness.score_one(a), harness.score_one(b))] += 1
    print("\nwhere the attempts landed:")
    for (was, now), count in sorted(moves.items()):
        arrow = "  =  " if was == now else "  ->  "
        print(f"  {count} x  {was}{arrow}{now}")
    blind = sum(count for (was, now), count in moves.items()
                if (was == "unbuilt") != (now == "unbuilt"))
    if blind:
        print(f"  {blind} attempt(s) built in one arm and not the other: their "
              f"scenarios cannot be paired and are excluded below")

    discordant = len(gained) + len(lost)
    print(f"\nscenario verdicts that moved: {discordant}")
    if not discordant:
        print("  none — on this evidence the two arms are the same store-builder")
        return 0

    share = len(gained) / discordant
    interval = exact_binomial_ci(len(gained), discordant)
    print(f"  {len(gained)} to the new arm, {len(lost)} to the old")
    print(f"  share favouring {after}: {share:.2f} ci95 {interval}")
    if interval[0] > 0.5:
        print(f"  → {after} is better on this evidence")
    elif interval[1] < 0.5:
        print(f"  → {after} is WORSE on this evidence")
    else:
        print("  → not separated: the interval spans a coin flip, so this is a "
              "direction to keep testing, not a result")

    print("\nby task:")
    for task, (up, down) in sorted(per_task.items()):
        print(f"  {task:<40} +{up} -{down}")

    if lost:
        print(f"\nwhat the new arm broke ({len(lost)}):")
        for task, attempt, name in lost[:10]:
            print(f"  {task} #{attempt}  {name}")

    print("\nwhat to write into a skill — scenarios still failing in both arms:")
    stuck = collections.Counter(
        name for a, b in pairs
        for name, was in (a.get("scenarios") or {}).items()
        if was != "pass" and (b.get("scenarios") or {}).get(name) not in (None, "pass"))
    for name, count in stuck.most_common(8):
        print(f"  {count:>2}×  {name}")
    if not stuck:
        print("  none")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("before")
    parser.add_argument("after")
    args = parser.parse_args()
    return compare(args.before, args.after)


if __name__ == "__main__":
    raise SystemExit(main())
