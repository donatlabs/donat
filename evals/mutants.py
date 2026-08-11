#!/usr/bin/env python3
"""Seed one business defect at a time into the checked-in Petshop.

    python3 evals/mutants.py list           # what would be generated, and where
    python3 evals/mutants.py generate       # write evals/mutants/<id>/

The question this corpus answers is not "how good is an agent" but "**what
would we notice**". Petshop is a store the black-box suite has exercised for
months; each mutant is that store with exactly one thing made wrong. Every
mutant then lands in one of three places, and all three are results:

  killed by the compiler  the invariant is enforced statically — the best
                          outcome donat can have, and the thesis of the
                          knowledgebase's "move invariants into the compiler"
  killed by the tests     the suite noticed a store that deploys and misbehaves
  survived                it deploys, it misbehaves, and nothing said a word

Survivors are the output that matters: each one is a hole in the behavioural
suite, and a ready-made anti-oracle for an eval task.

Operators are deliberately line-based rather than YAML round-trips: the patch
has to be small enough for a person to read and agree that it is one defect.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
PETSHOP = REPO / "examples" / "petshop" / "metadata"
OUT = REPO / "evals" / "mutants"

#: Which black-box modules are worth running for a mutant in this file. A
#: mutant is only "survived" if the tests that own its behaviour all ran and
#: all passed, so this map is what makes a 200-mutant sweep affordable —
#: survivors are re-checked against the whole suite before anyone believes them.
COVERAGE = {
    "flows/checkout-payment.yaml": [
        "test_checkout_payment", "test_reconciliation_branches",
        "test_provider_failure_branches", "test_store_integrity",
    ],
    "flows/checkout-cancellation.yaml": [
        "test_checkout_cancellation", "test_store_integrity",
    ],
    "flows/authorized-order-cancellation.yaml": [
        "test_cancellation_and_fulfilment", "test_checkout_cancellation",
        "test_store_integrity",
    ],
    "flows/partial-fulfilment.yaml": [
        "test_cancellation_and_fulfilment", "test_store_integrity",
    ],
    "flows/return-refund.yaml": [
        "test_returns_and_refunds", "test_refund_reconciliation",
        "test_store_integrity",
    ],
    "flows/subscription-renewal.yaml": [
        "test_subscriptions", "test_dunning_reconciliation", "test_store_integrity",
    ],
    "flows/b2b-order-approval.yaml": [
        "test_b2b_approval", "test_b2b_escalation",
    ],
    "flows/vendor-payout.yaml": ["test_marketplace_and_reconciliation"],
    "flows/grooming-booking.yaml": ["test_booking_and_prescription"],
    "flows/prescription-review.yaml": ["test_booking_and_prescription"],
    "flows/payment-reconciliation.yaml": [
        "test_provider_failure_branches", "test_refund_reconciliation",
        "test_marketplace_and_reconciliation", "test_store_integrity",
    ],
    "commands/checkout/": [
        "test_checkout_payment", "test_shopping_basics", "test_store_integrity",
    ],
    "commands/payments/": [
        "test_checkout_payment", "test_reconciliation_branches",
        "test_refund_reconciliation", "test_store_integrity",
    ],
    "commands/returns/": ["test_returns_and_refunds", "test_store_integrity"],
    "commands/b2b/": ["test_b2b_approval", "test_b2b_escalation"],
    "commands/booking/": ["test_booking_and_prescription"],
    "commands/prescription/": ["test_booking_and_prescription"],
    "commands/fulfilment/": [
        "test_cancellation_and_fulfilment", "test_shopping_basics",
        "test_store_integrity",
    ],
    "commands/marketplace/": [
        "test_marketplace_and_reconciliation", "test_store_integrity",
    ],
    "commands/subscriptions/": ["test_subscriptions", "test_dunning_reconciliation"],
    "commands/operations/": [
        "test_reconciliation_branches", "test_provider_failure_branches",
        "test_store_integrity",
    ],
    "rules.yaml": [
        "test_checkout_payment", "test_returns_and_refunds", "test_b2b_approval",
        "test_store_integrity",
    ],
    "databases/default/tables/": [
        "test_role_matrix", "test_attacks", "test_catalogue_and_roles",
        "test_search", "test_shopping_basics",
    ],
}

#: Modules that need the seconds-scale stand and skip themselves without it.
#: A skipped test is not a passing test, so a mutant whose only coverage is
#: here is reported as uncovered rather than as a survivor.
NEEDS_FAST_STAND = {"test_time_based_branches", "test_dunning_reconciliation"}


def coverage_for(relative: str) -> list[str]:
    if relative in COVERAGE:
        return COVERAGE[relative]
    for prefix, modules in COVERAGE.items():
        if prefix.endswith("/") and relative.startswith(prefix):
            return modules
    return []


# -- operators ---------------------------------------------------------------
#
# Each yields (site, description, mutated text). `site` only has to be unique
# within one file and stable across runs, because it becomes part of the id.


def op_ambiguity_to_failure(text: str):
    """Route every error out of a request straight to the flow's failure.

    The store stops finding out what the provider did. This is the defect the
    engine exists to prevent, generalised over every connector request.
    """

    fail = re.search(r"^  - id: (\S+)\n    fail:", text, re.M)
    if not fail:
        return
    target = fail.group(1)
    for index, block in enumerate(re.finditer(r"(?m)^      on_error:\n(?:^ {8,}.*\n)+", text)):
        original = block.group(0)
        if f"next: {target}" in original and original.count("next:") == original.count(f"next: {target}"):
            continue  # already goes nowhere else
        rewritten = re.sub(r"next: \S+", f"next: {target}", original)
        yield f"on_error{index}", "every error route gives up instead of reconciling", \
            text[:block.start()] + rewritten + text[block.end():]


def op_default_to_first_case(text: str):
    """Treat an outcome nobody recognised as the first case's outcome.

    A `default` that means success is the most common way a state machine lies.
    """

    for index, block in enumerate(re.finditer(
            r"(?m)^    when:\n      cases:\n((?:^ {8}.*\n)+)^      default: (\S+)\n", text)):
        first = re.search(r"next: (\S+)", block.group(1))
        if not first or first.group(1) == block.group(2):
            continue
        rewritten = block.group(0).replace(f"      default: {block.group(2)}\n",
                                           f"      default: {first.group(1)}\n")
        yield f"when{index}", "an unrecognised outcome takes the first case's branch", \
            text[:block.start()] + rewritten + text[block.end():]


def op_swap_case_targets(text: str):
    """Send each of the first two cases to the other's state."""

    for index, block in enumerate(re.finditer(
            r"(?m)^    when:\n      cases:\n((?:^ {8}.*\n)+)", text)):
        targets = re.findall(r"next: (\S+)", block.group(1))
        if len(targets) < 2 or targets[0] == targets[1]:
            continue
        body = block.group(1)
        body = body.replace(f"next: {targets[0]}", "next: __SWAP__", 1)
        body = body.replace(f"next: {targets[1]}", f"next: {targets[0]}", 1)
        body = body.replace("next: __SWAP__", f"next: {targets[1]}", 1)
        yield f"cases{index}", "two decision branches are wired to each other's outcome", \
            text[:block.start()] + "    when:\n      cases:\n" + body + text[block.end():]


def op_single_attempt(text: str):
    """One try at a provider that is briefly unreachable."""

    for index, match in enumerate(re.finditer(r"(?m)^(\s+)max_attempts: ([2-9]\d*)\n", text)):
        yield f"attempts{index}", "a transient provider failure is no longer retried", \
            text[:match.start()] + f"{match.group(1)}max_attempts: 1\n" + text[match.end():]


def op_no_retry_on_timeout(text: str):
    """Stop treating a timeout as worth retrying."""

    for index, match in enumerate(re.finditer(r"(?m)^(\s+)retry_on: \[([^\]]*timeout[^\]]*)\]\n", text)):
        kinds = [k.strip() for k in match.group(2).split(",") if k.strip() != "timeout"]
        if not kinds:
            continue
        yield f"retryon{index}", "a timed-out call is treated as final", \
            text[:match.start()] + f"{match.group(1)}retry_on: [{', '.join(kinds)}]\n" + text[match.end():]


def op_persist_before_match_off(text: str):
    """A signal committed before the wait is receptive is dropped (ADR 034)."""

    for index, match in enumerate(re.finditer(r"(?m)^(\s+)persist_before_match: true\n", text)):
        yield f"persist{index}", "an early signal is dropped instead of being taken", \
            text[:match.start()] + f"{match.group(1)}persist_before_match: false\n" + text[match.end():]


def op_drop_assert(text: str):
    """Remove a declared business guard from a command."""

    for index, block in enumerate(re.finditer(
            r"(?m)^  - name: \S+\n    assert:\n(?:^ {6,}.*\n)+", text)):
        yield f"assert{index}", "a business rule the command asserts is no longer checked", \
            text[:block.start()] + text[block.end():]


def op_require_found_false(text: str):
    """Stop insisting the row a command reads actually exists."""

    for index, match in enumerate(re.finditer(r"(?m)^(\s+)require_found: true\n", text)):
        yield f"found{index}", "a missing row no longer refuses the command", \
            text[:match.start()] + f"{match.group(1)}require_found: false\n" + text[match.end():]


def op_require_affected_false(text: str):
    """Stop insisting the write a command makes actually landed."""

    for index, match in enumerate(re.finditer(r"(?m)^(\s+)require_affected: true\n", text)):
        yield f"affected{index}", "a write that changed nothing is accepted", \
            text[:match.start()] + f"{match.group(1)}require_affected: false\n" + text[match.end():]


def op_drop_state_guard(text: str):
    """Drop the status a command selects by, so it acts on the wrong row.

    `by: { id: ..., status: { literal: pending } }` becomes `by: { id: ... }`:
    the command now runs against a row in any state.
    """

    for index, match in enumerate(re.finditer(
            r"(?m)^(\s+)(?:status|order_status|state): \{ literal: \S+ \}\n", text)):
        before = text[:match.start()].rstrip("\n").rsplit("\n", 1)[-1]
        if "by:" not in text[max(0, match.start() - 400):match.start()]:
            continue
        if before.strip().startswith("set:"):
            continue
        yield f"guard{index}", "the state a command requires is no longer part of its lookup", \
            text[:match.start()] + text[match.end():]


def op_open_row_filter(text: str):
    """Let a role see every row of a table instead of its own."""

    for index, block in enumerate(re.finditer(
            r"(?m)^(\s+)filter:\n((?:^\s*#.*\n|^ {8,}.*\n)+)", text)):
        indent = block.group(1)
        if block.group(2).strip() in ("{}", ""):
            continue
        yield f"filter{index}", "a role's row filter is removed: it sees everything", \
            text[:block.start()] + f"{indent}filter: {{}}\n" + text[block.end():]


def op_flip_comparison(text: str):
    """Move a rule's boundary by one."""

    swaps = {">=": ">", "<=": "<"}
    for index, match in enumerate(re.finditer(r"(>=|<=)", text)):
        symbol = match.group(1)
        yield f"cmp{index}", f"a rule boundary changes from `{symbol}` to `{swaps[symbol]}`", \
            text[:match.start()] + swaps[symbol] + text[match.end():]


OPERATORS = {
    "ambiguity-to-failure": (op_ambiguity_to_failure, ("flows/",)),
    "default-to-first-case": (op_default_to_first_case, ("flows/",)),
    "swap-case-targets": (op_swap_case_targets, ("flows/",)),
    "single-attempt": (op_single_attempt, ("flows/",)),
    "no-retry-on-timeout": (op_no_retry_on_timeout, ("flows/",)),
    "early-signal-dropped": (op_persist_before_match_off, ("flows/",)),
    "drop-assert": (op_drop_assert, ("commands/",)),
    "require-found-false": (op_require_found_false, ("commands/",)),
    "require-affected-false": (op_require_affected_false, ("commands/",)),
    "drop-state-guard": (op_drop_state_guard, ("commands/",)),
    "open-row-filter": (op_open_row_filter, ("databases/default/tables/",)),
    "flip-comparison": (op_flip_comparison, ("rules.yaml",)),
}

#: How many mutants one operator may take from one file. Ten identical-looking
#: defects in one flow measure the same hole ten times; the cap spends the
#: sweep's hours on variety instead.
PER_FILE_CAP = 3


def sources() -> list[pathlib.Path]:
    return sorted(p for p in PETSHOP.rglob("*.yaml") if p.is_file())


def enumerate_mutants(limit: int | None = None) -> list[dict]:
    """Every mutant this corpus can produce, in a stable order.

    Round-robin over operators rather than operator-by-operator, so that any
    prefix of the list — a sample, or a sweep that ran out of night — is a
    spread across every kind of defect rather than 200 of the first one.
    """

    by_operator: dict[str, list[dict]] = {name: [] for name in OPERATORS}
    for path in sources():
        relative = str(path.relative_to(PETSHOP))
        text = path.read_text()
        for name, (operator, scopes) in OPERATORS.items():
            if not any(relative.startswith(scope) or relative == scope for scope in scopes):
                continue
            taken = 0
            for site, description, mutated in operator(text):
                if taken >= PER_FILE_CAP:
                    break
                if mutated == text:
                    continue
                taken += 1
                identifier = f"{name}--{relative.replace('/', '-').removesuffix('.yaml')}--{site}"
                by_operator[name].append({
                    "id": identifier,
                    "split": split_of(identifier),
                    "operator": name,
                    "file": relative,
                    "site": site,
                    "defect": description,
                    "coverage": coverage_for(relative),
                    "text": mutated,
                })

    ordered, rows = [], list(by_operator.values())
    for index in range(max((len(r) for r in rows), default=0)):
        for row in rows:
            if index < len(row):
                ordered.append(row[index])
    return ordered[:limit] if limit else ordered


def patch_for(path: pathlib.Path, mutated: str, destination: pathlib.Path) -> None:
    scratch = destination.parent / "scratch.yaml"
    scratch.write_text(mutated)
    diff = subprocess.run(
        ["git", "diff", "--no-index", "--no-prefix", "--", str(path), str(scratch)],
        capture_output=True, text=True,
    ).stdout
    relative = path.relative_to(PETSHOP)
    lines = diff.splitlines(keepends=True)
    lines[0] = f"diff --git a/{relative} b/{relative}\n"
    for index, line in enumerate(lines[:6]):
        if line.startswith("--- ") or line.startswith("+++ "):
            lines[index] = line[:4] + str(relative) + "\n"
    destination.write_text("".join(lines))
    scratch.unlink()


def split_of(identifier: str) -> str:
    """dev or holdout, decided by the mutant's own name.

    Tasks get written from the dev third; the holdout third is never read while
    authoring them. Detection measured on mutants you designed tasks around is
    detection measured on your own answer key — the split is what keeps the
    sensitivity number meaning anything a year from now.
    """

    digest = hashlib.sha256(identifier.encode()).hexdigest()
    return "holdout" if int(digest[:8], 16) % 3 == 0 else "dev"


def generate(limit: int | None) -> int:
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir(parents=True)
    catalogue = []
    for mutant in enumerate_mutants(limit):
        place = OUT / mutant["id"]
        place.mkdir(parents=True)
        patch_for(PETSHOP / mutant["file"], mutant.pop("text"), place / "defect.patch")
        uncovered = [m for m in mutant["coverage"] if m not in NEEDS_FAST_STAND]
        mutant["covered"] = bool(uncovered)
        (place / "mutant.json").write_text(json.dumps(mutant, indent=2) + "\n")
        catalogue.append(mutant)
    (OUT / "catalogue.json").write_text(json.dumps(catalogue, indent=2) + "\n")
    print(f"{len(catalogue)} mutants in {OUT}")
    for name in OPERATORS:
        count = sum(1 for m in catalogue if m["operator"] == name)
        print(f"  {name:<24} {count:>4}")
    blind = [m for m in catalogue if not m["covered"]]
    if blind:
        # By source file, not as a bare count. A count reads as a property of
        # the store — "nothing tests this" — when it is just as often a hole in
        # COVERAGE above. `commands/booking/` sat unmapped through a whole
        # 200-mutant sweep and five real defects were filed as uncovered while
        # test_booking_and_prescription was sitting there owning that behaviour.
        by_file = {}
        for mutant in blind:
            by_file[mutant["file"]] = by_file.get(mutant["file"], 0) + 1
        print(f"\n{len(blind)} mutant(s) have no coverage on the ordinary stand "
              f"and will be reported as uncovered, never as survivors:")
        for path, count in sorted(by_file.items(), key=lambda kv: -kv[1]):
            print(f"  {path:<52} {count:>3}")
        print("  — if a module in tests-system/tests/ does own one of these, "
              "COVERAGE is missing an entry, not the suite")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    listing = sub.add_parser("list")
    listing.add_argument("--limit", type=int)
    writing = sub.add_parser("generate")
    writing.add_argument("--limit", type=int, default=200)
    args = parser.parse_args()

    if args.command == "list":
        for mutant in enumerate_mutants(args.limit):
            print(f"{mutant['id']:<70} {mutant['defect']}")
        return 0
    return generate(args.limit)


if __name__ == "__main__":
    sys.exit(main())
