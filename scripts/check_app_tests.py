#!/usr/bin/env python3
"""A table that grants a role something has a test beside it that proves the
refusal.

    check_app_tests.py <metadata-dir> [--baseline <file>]
    check_app_tests.py --self-test

For every `*.yaml` under `<metadata-dir>/databases/*/tables/` that declares a
permission (`select_permissions:`, `insert_permissions:`, ...), a sibling
`<name>_test.yaml` with at least one test case must exist. A permission is
only proven by the request it turns away, and a test that lives beside the
permission is the one a reviewer sees in the same diff.

The baseline file lists, one per line relative to the metadata dir, the tables
that had no test when this gate was introduced. It is a ratchet: a table in
the baseline may stay untested, a table outside it may not, and a baseline
entry that has gained a test must be removed — the gate fails while it is
still listed, so the list only ever shrinks.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

PERMISSION_KEYS = re.compile(
    r"^\s*(select|insert|update|delete)_permissions:\s*$", re.MULTILINE
)
TEST_SUFFIX = "_test.yaml"


def tables_with_permissions(metadata_dir: Path) -> list[Path]:
    found = []
    for path in sorted(metadata_dir.glob("databases/*/tables/*.yaml")):
        if path.name.endswith(TEST_SUFFIX):
            continue
        if PERMISSION_KEYS.search(path.read_text(encoding="utf-8")):
            found.append(path)
    return found


def has_test_cases(test_file: Path) -> bool:
    # `tests:` followed by at least one `- name:` entry. The runner validates
    # the shape; this gate only asks whether something is there to run.
    text = test_file.read_text(encoding="utf-8")
    return re.search(r"^tests:\s*$", text, re.MULTILINE) is not None and (
        re.search(r"^\s*-\s*name:", text, re.MULTILINE) is not None
    )


def load_baseline(path: Path | None) -> set[str]:
    if path is None or not path.exists():
        return set()
    return {
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }


def check(metadata_dir: Path, baseline_path: Path | None) -> list[str]:
    baseline = load_baseline(baseline_path)
    problems = []
    seen = set()
    for table in tables_with_permissions(metadata_dir):
        rel = table.relative_to(metadata_dir).as_posix()
        seen.add(rel)
        test = table.with_name(table.stem + TEST_SUFFIX)
        tested = test.exists() and has_test_cases(test)
        if tested and rel in baseline:
            problems.append(
                f"{rel}: now has {test.name}; remove it from the baseline "
                f"({baseline_path}) so the ratchet holds"
            )
        elif not tested and rel not in baseline:
            problems.append(
                f"{rel}: declares permissions but has no {test.name} "
                "with a test case beside it"
            )
    for stale in sorted(baseline - seen):
        problems.append(
            f"{stale}: in the baseline but no such table declares permissions; "
            "remove it"
        )
    return problems


def self_test() -> int:
    failures = []

    def case(name: str, files: dict[str, str], baseline: str | None, expect_problems: int):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "metadata"
            for rel, content in files.items():
                path = root / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            baseline_path = None
            if baseline is not None:
                baseline_path = Path(tmp) / "baseline.txt"
                baseline_path.write_text(baseline, encoding="utf-8")
            problems = check(root, baseline_path)
            if len(problems) != expect_problems:
                failures.append(f"{name}: expected {expect_problems} problem(s), got {problems}")

    table = "table:\n  name: t\nselect_permissions:\n  - role: r\n"
    tests = "tests:\n  - name: r sees nothing\n    steps: []\n"
    T = "databases/default/tables/"
    case("untested table fails", {T + "public_t.yaml": table}, None, 1)
    case("tested table passes", {T + "public_t.yaml": table, T + "public_t_test.yaml": tests}, None, 0)
    case("empty test file does not count", {T + "public_t.yaml": table, T + "public_t_test.yaml": "tests: []\n"}, None, 1)
    case("no permissions needs no test", {T + "public_t.yaml": "table:\n  name: t\n"}, None, 0)
    case("baseline excuses", {T + "public_t.yaml": table}, T + "public_t.yaml\n", 0)
    case("baseline must shrink", {T + "public_t.yaml": table, T + "public_t_test.yaml": tests}, T + "public_t.yaml\n", 1)
    case("stale baseline entry", {T + "public_t.yaml": "table:\n  name: t\n"}, T + "public_t.yaml\n", 1)

    for line in failures:
        print(f"self-test FAILED: {line}", file=sys.stderr)
    if not failures:
        print("self-test ok")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("metadata_dir", nargs="?", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.metadata_dir is None:
        parser.error("a metadata directory is required")
    problems = check(args.metadata_dir, args.baseline)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        print(
            f"\n{len(problems)} table(s) need a test beside them; see scripts/check_app_tests.py",
            file=sys.stderr,
        )
        return 1
    print(f"app tests: every permission-bearing table under {args.metadata_dir} has a test beside it")
    return 0


if __name__ == "__main__":
    sys.exit(main())
