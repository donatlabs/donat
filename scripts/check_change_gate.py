#!/usr/bin/env python3
"""The change gate: what a diff has to say out loud before it merges.

CLAUDE.md states the rules in prose — fixtures are ground truth, snapshots are
reviewed and never blind-accepted, there is no admin role. Prose is read by
whoever reads it. This script is the mechanical twin of the few rules that can
be checked from a diff alone, so that an agent (or a person) optimising for a
green build cannot take the short cuts that make a build green without making
the code right.

Two kinds of finding:

* A **hard** finding fails the gate and nothing in the PR description excuses
  it: a pending `.snap.new` committed, or one of the names the BLOCKING RULE
  forbids reappearing in engine sources.
* An **excusable** finding is a change that is sometimes right and always
  worth a sentence: an existing fixture or snapshot rewritten, the toolchain
  bumped, an advisory excused, a `sleep` added to a conformance test, a test
  ignored or removed, a plugin skill edited. Each kind is excused by one line
  in the PR description, `gate:<kind> <reason>`, and the gate prints the exact
  line it is missing. The reason is for the reviewer; the gate only checks
  that the change was named.

Adding a *new* fixture or snapshot is free — that is how the TDD loop starts.
Changing an existing one is what needs a reason.

Usage:
    check_change_gate.py --base <ref> [--head <ref>] [--body-file <path>]
    check_change_gate.py --self-test

The PR description is read from `--body-file`, else from `$GATE_PR_BODY`,
else it is empty — so a local run lists every marker the PR will need.
Exit 0 when the gate passes, 1 on findings, 2 on a usage error.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile


# ---------------------------------------------------------------------------
# What the gate watches
# ---------------------------------------------------------------------------

FIXTURES_ROOT = "crates/conformance/fixtures/"
CONFORMANCE_ROOT = "crates/conformance/"
SKILLS_ROOT = "plugins/donat/skills/"
# The application-test runner waits on the journal; a `sleep` added here is
# the same short cut as one in a conformance suite.
TESTKIT_ROOT = "crates/testkit/"
TOOLCHAIN = "rust-toolchain.toml"
AUDIT_CONFIG = ".cargo/audit.toml"
SERVER_SRC = "crates/server/src/"

# Names the BLOCKING RULE "No Admin Role" retired. Engine sources may not
# mention them again; tests that prove they are ignored, and the conformance
# harness that applies `run_sql` setup fixtures to its own database, may.
# `\b` keeps `DONAT_OIDC_ADMIN_ROLE` (the identity provider's own role, not
# ours) from matching.
FORBIDDEN_IN_ENGINE = (
    re.compile(r"\bADMIN_ROLE\b"),
    re.compile(r"DONAT_GRAPHQL_ADMIN_SECRET"),
    re.compile(r"x-donat-admin-secret", re.IGNORECASE),
)
FORBIDDEN_IN_SERVER = (re.compile(r"\brun_sql\b"),)

ADVISORY_ID = re.compile(r"RUSTSEC-\d{4}-\d+")
SLEEP_CALL = re.compile(r"\bsleep\s*\(")
IGNORE_ATTR = re.compile(r"#\[\s*ignore\b")
TEST_ATTR = re.compile(r"^\s*#\[\s*(?:tokio::)?test\b")
MARKER_LINE = re.compile(r"^\s*(?:[-*]\s+)?gate:([a-z-]+)\s+(.*\S)\s*$", re.MULTILINE)

# One sentence per excusable kind: what the reviewer is owed.
KINDS = {
    "fixtures": "why the conformance ground truth changes (a documented known-diff, or the fixture was wrong and how you know)",
    "snapshots": "what behaviour changed and why the new SQL/output is right (reviewed with `cargo insta review`)",
    "toolchain": "which release and what it reformats or newly warns on",
    "audit-ignore": "the reachability argument and why no upgrade exists (see .cargo/audit.toml)",
    "timing": "why the test has to wait on time rather than on an event, and what bounds the wait",
    "ignored-test": "why the test cannot run by default and what runs it",
    "removed-test": "which tests went and what covers their property now",
    "skills": "a measurement that the edit helps a skill (a paired benchmark arm, where one exists), or why the edit needs none",
}


@dataclass(frozen=True)
class FileChange:
    status: str  # A, M, D, R, T
    path: str  # the path after the change (the old path for D)
    old_path: str | None = None  # for renames


@dataclass
class Diff:
    files: list[FileChange] = field(default_factory=list)
    added: dict[str, list[str]] = field(default_factory=dict)
    removed: dict[str, list[str]] = field(default_factory=dict)


@dataclass(frozen=True)
class Finding:
    kind: str  # a key of KINDS, or "hard"
    path: str
    detail: str


# ---------------------------------------------------------------------------
# Reading the diff
# ---------------------------------------------------------------------------


def _git(repo: Path, *args: str) -> str:
    proc = subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed:\n{proc.stderr.strip()}")
    return proc.stdout


def collect(repo: Path, base: str, head: str) -> Diff:
    """Everything the gate needs, from two git invocations.

    `base...head` diffs against the merge base, which is what a pull request
    is: the branch's own changes, not whatever landed on the base since.
    """
    diff = Diff()
    status_out = _git(repo, "diff", "--name-status", "-M", f"{base}...{head}")
    for line in status_out.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        code = parts[0][0]
        if code == "R":
            diff.files.append(FileChange("R", parts[2], parts[1]))
        elif code in "AMDT":
            diff.files.append(FileChange(code, parts[1]))
        # C (copy) and U (unmerged) do not occur with these flags.

    patch = _git(repo, "diff", "-U0", "-M", f"{base}...{head}")
    old_path: str | None = None
    new_path: str | None = None
    for line in patch.splitlines():
        if line.startswith("--- "):
            old_path = None if line == "--- /dev/null" else _strip_prefix(line[4:], "a/")
        elif line.startswith("+++ "):
            new_path = None if line == "+++ /dev/null" else _strip_prefix(line[4:], "b/")
        elif line.startswith("+") and new_path is not None:
            diff.added.setdefault(new_path, []).append(line[1:])
        elif line.startswith("-") and old_path is not None:
            diff.removed.setdefault(old_path, []).append(line[1:])
    return diff


def _strip_prefix(path: str, prefix: str) -> str:
    if path.startswith('"'):
        raise RuntimeError(f"the gate does not handle quoted paths: {path}")
    return path[len(prefix) :] if path.startswith(prefix) else path


# ---------------------------------------------------------------------------
# The rules
# ---------------------------------------------------------------------------


def _is_engine_source(path: str) -> bool:
    return (
        path.startswith("crates/")
        and "/src/" in path
        and not path.startswith(CONFORMANCE_ROOT)
        and path.endswith(".rs")
    )


def evaluate(diff: Diff) -> list[Finding]:
    findings: list[Finding] = []
    present = [f for f in diff.files if f.status != "D"]
    rewritten = [f for f in diff.files if f.status in "MDRT"]

    # Hard: a pending snapshot is an unreviewed one by definition.
    for f in present:
        if f.path.endswith(".snap.new"):
            findings.append(Finding("hard", f.path, "pending insta snapshot committed; review it with `cargo insta review`"))

    # Hard: the retired names.
    for path, lines in diff.added.items():
        patterns: tuple[re.Pattern[str], ...] = ()
        if _is_engine_source(path):
            patterns += FORBIDDEN_IN_ENGINE
        if path.startswith(SERVER_SRC) and path.endswith(".rs"):
            patterns += FORBIDDEN_IN_SERVER
        for pattern in patterns:
            for text in lines:
                if pattern.search(text):
                    findings.append(Finding("hard", path, f"`{pattern.pattern}` reappears in engine sources: {text.strip()}"))
                    break

    # Excusable: existing ground truth rewritten. New files are free.
    for f in rewritten:
        touched = (f.old_path or f.path, f.path)
        if any(p.startswith(FIXTURES_ROOT) for p in touched):
            findings.append(Finding("fixtures", f.path, f"existing fixture {f.status}"))
        if any(p.endswith(".snap") for p in touched):
            findings.append(Finding("snapshots", f.path, f"existing snapshot {f.status}"))

    for f in diff.files:
        if TOOLCHAIN in (f.path, f.old_path):
            findings.append(Finding("toolchain", f.path, f"toolchain pin {f.status}"))
        if f.path.startswith(SKILLS_ROOT) or (f.old_path or "").startswith(SKILLS_ROOT):
            findings.append(Finding("skills", f.path, f"plugin skill {f.status}"))

    for text in diff.added.get(AUDIT_CONFIG, []):
        match = ADVISORY_ID.search(text)
        if match:
            findings.append(Finding("audit-ignore", AUDIT_CONFIG, f"{match.group(0)} excused"))

    for path, lines in diff.added.items():
        if (path.startswith(CONFORMANCE_ROOT) or path.startswith(TESTKIT_ROOT)) and path.endswith(".rs"):
            for text in lines:
                if SLEEP_CALL.search(text):
                    findings.append(Finding("timing", path, text.strip()))
        if path.endswith(".rs"):
            for text in lines:
                if IGNORE_ATTR.search(text):
                    findings.append(Finding("ignored-test", path, text.strip()))

    removed_tests = sum(
        1 for path, lines in diff.removed.items() if path.endswith(".rs") for t in lines if TEST_ATTR.search(t)
    )
    added_tests = sum(
        1 for path, lines in diff.added.items() if path.endswith(".rs") for t in lines if TEST_ATTR.search(t)
    )
    if removed_tests > added_tests:
        findings.append(Finding("removed-test", "(workspace)", f"{removed_tests - added_tests} fewer `#[test]` than before"))

    return findings


def markers_in(body: str) -> dict[str, str]:
    return {kind: reason for kind, reason in MARKER_LINE.findall(body)}


def judge(findings: list[Finding], body: str) -> tuple[list[Finding], dict[str, str], dict[str, str]]:
    """Split findings into what fails and what the description excused.

    Returns (failing, excused_kind -> reason, stale_kind -> reason).
    """
    markers = markers_in(body)
    failing = [f for f in findings if f.kind == "hard" or f.kind not in markers]
    kinds_found = {f.kind for f in findings}
    excused = {k: r for k, r in markers.items() if k in kinds_found}
    stale = {k: r for k, r in markers.items() if k not in kinds_found}
    return failing, excused, stale


def report(findings: list[Finding], body: str, out=sys.stdout) -> int:
    failing, excused, stale = judge(findings, body)
    if not findings:
        print("change gate: nothing to declare", file=out)
    for f in findings:
        state = "FAIL" if f in failing else "excused"
        print(f"[{state}] {f.kind:13} {f.path}: {f.detail}", file=out)
    for kind, reason in excused.items():
        print(f"  gate:{kind} — {reason}", file=out)
    for kind, reason in stale.items():
        print(f"[stale] gate:{kind} in the description, but nothing in the diff needs it ({reason})", file=out)
    missing = sorted({f.kind for f in failing if f.kind != "hard"})
    if missing:
        print("\nAdd to the pull request description, one line each:", file=out)
        for kind in missing:
            print(f"  gate:{kind} <{KINDS[kind]}>", file=out)
    if any(f.kind == "hard" for f in failing):
        print("\nHard findings are not excusable; the change itself has to go.", file=out)
    return 1 if failing else 0


# ---------------------------------------------------------------------------
# Self-test: a throwaway repository with one of everything
# ---------------------------------------------------------------------------


def _write(root: Path, rel: str, text: str) -> None:
    path = root / rel
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def _commit(repo: Path, message: str) -> None:
    _git(repo, "add", "-A")
    _git(repo, "-c", "user.name=gate", "-c", "user.email=gate@example", "commit", "-q", "-m", message)


def self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp)
        _git(repo, "init", "-q")
        _write(repo, f"{FIXTURES_ROOT}basic/query.yaml", "status: 200\n")
        _write(repo, f"{FIXTURES_ROOT}basic/moved.yaml", "status: 204  # distinct, so the rename is unambiguous\n")
        _write(repo, "crates/sqlgen/tests/snapshots/select.snap", "SELECT 1\n")
        _write(repo, TOOLCHAIN, '[toolchain]\nchannel = "1.89.0"\n')
        _write(repo, AUDIT_CONFIG, "[advisories]\nignore = [\n]\n")
        _write(repo, f"{CONFORMANCE_ROOT}tests/commands.rs", "#[test]\nfn a() {}\n\n#[tokio::test]\nasync fn b() {}\n")
        _write(repo, f"{CONFORMANCE_ROOT}src/lib.rs", '// harness applies "run_sql" ops\n')
        _write(repo, f"{SERVER_SRC}auth.rs", "pub fn role() {}\n")
        _write(repo, f"{SERVER_SRC}config.rs", "pub const IDP: &str = \"x\";\n")
        _write(repo, f"{SKILLS_ROOT}add-table/SKILL.md", "# add-table\n")
        _write(repo, "crates/schema/src/lib.rs", "pub fn schema() {}\n")
        _commit(repo, "base")
        _git(repo, "branch", "-f", "base")

        # One of everything the gate watches, plus the things it must ignore.
        _write(repo, f"{FIXTURES_ROOT}basic/query.yaml", "status: 400\n")  # fixtures (M)
        _write(repo, f"{FIXTURES_ROOT}basic/new.yaml", "status: 201\n")  # free (A)
        _git(repo, "mv", f"{FIXTURES_ROOT}basic/moved.yaml", f"{FIXTURES_ROOT}basic/renamed.yaml")  # fixtures (R)
        _write(repo, "crates/sqlgen/tests/snapshots/select.snap", "SELECT 2\n")  # snapshots (M)
        _write(repo, "crates/sqlgen/tests/snapshots/fresh.snap", "SELECT 3\n")  # free (A)
        _write(repo, "crates/sqlgen/tests/snapshots/pending.snap.new", "SELECT 4\n")  # hard
        _write(repo, TOOLCHAIN, '[toolchain]\nchannel = "1.90.0"\n')  # toolchain
        _write(repo, AUDIT_CONFIG, '[advisories]\nignore = [\n  "RUSTSEC-2025-0001",\n]\n')  # audit-ignore
        _write(
            repo,
            f"{CONFORMANCE_ROOT}tests/commands.rs",
            "#[test]\n#[ignore]\nfn a() { std::thread::sleep(d); }\n",
        )  # timing, ignored-test, removed-test (2 -> 1)
        _write(repo, f"{TESTKIT_ROOT}src/runner.rs", "fn poll() { std::thread::sleep(d); }\n")  # timing
        _write(repo, f"{CONFORMANCE_ROOT}src/lib.rs", '// harness applies "run_sql" ops\n"run_sql" => {}\n')  # ignored: harness
        _write(repo, f"{SERVER_SRC}auth.rs", 'pub fn role() { let _ = ADMIN_ROLE; }\nfn h() { "run_sql" }\n')  # hard x2
        _write(repo, f"{SERVER_SRC}config.rs", "pub const IDP: &str = \"DONAT_OIDC_ADMIN_ROLE\";\n")  # ignored: not our name
        _write(repo, f"{SKILLS_ROOT}add-table/SKILL.md", "# add-table\nmore\n")  # skills
        _write(repo, "crates/schema/src/lib.rs", "pub fn schema() { let _ = DONAT_GRAPHQL_ADMIN_SECRET; }\n")  # hard
        _commit(repo, "head")

        diff = collect(repo, "base", "HEAD")
        findings = evaluate(diff)
        by_kind: dict[str, list[Finding]] = {}
        for f in findings:
            by_kind.setdefault(f.kind, []).append(f)

        failures: list[str] = []

        def expect(cond: bool, what: str) -> None:
            if not cond:
                failures.append(what)

        hard = sorted(f.path for f in by_kind.get("hard", []))
        expect(
            hard == sorted([
                "crates/sqlgen/tests/snapshots/pending.snap.new",
                f"{SERVER_SRC}auth.rs",  # ADMIN_ROLE
                f"{SERVER_SRC}auth.rs",  # run_sql
                "crates/schema/src/lib.rs",
            ]),
            f"hard findings: {hard}",
        )
        fixtures = sorted(f.path for f in by_kind.get("fixtures", []))
        expect(
            fixtures == [f"{FIXTURES_ROOT}basic/query.yaml", f"{FIXTURES_ROOT}basic/renamed.yaml"],
            f"fixtures: {fixtures} (new.yaml must be free, the rename must count)",
        )
        snaps = [f.path for f in by_kind.get("snapshots", [])]
        expect(snaps == ["crates/sqlgen/tests/snapshots/select.snap"], f"snapshots: {snaps} (fresh.snap must be free)")
        expect(len(by_kind.get("toolchain", [])) == 1, "toolchain change not seen")
        expect([f.detail for f in by_kind.get("audit-ignore", [])] == ["RUSTSEC-2025-0001 excused"], "advisory not seen")
        timing = sorted(f.path for f in by_kind.get("timing", []))
        expect(
            timing == [f"{CONFORMANCE_ROOT}tests/commands.rs", f"{TESTKIT_ROOT}src/runner.rs"],
            f"sleep in a conformance test or the testkit runner not seen: {timing}",
        )
        expect(len(by_kind.get("ignored-test", [])) == 1, "#[ignore] not seen")
        expect([f.detail for f in by_kind.get("removed-test", [])] == ["1 fewer `#[test]` than before"], "net removed test not seen")
        expect(len(by_kind.get("skills", [])) == 1, "skill edit not seen")
        expect(
            not any("conformance/src" in f.path or "config.rs" in f.path for f in by_kind.get("hard", [])),
            "the harness's run_sql or the IdP's DONAT_OIDC_ADMIN_ROLE was flagged",
        )

        # Every excusable kind excused: only the hard findings remain.
        body = "\n".join(f"gate:{kind} because of reasons" for kind in KINDS)
        failing, excused, stale = judge(findings, body)
        expect(all(f.kind == "hard" for f in failing), f"excused kinds still failing: {[f.kind for f in failing]}")
        expect(set(excused) == set(KINDS), f"excused: {sorted(excused)}")
        expect(not stale, f"stale: {stale}")

        # A marker with nothing behind it is reported, not failed.
        failing, excused, stale = judge([Finding("timing", "x.rs", "sleep")], "- gate:timing waits on the cron tick\ngate:toolchain unused")
        expect(not failing and excused == {"timing": "waits on the cron tick"} and stale == {"toolchain": "unused"}, "marker parsing")

        # Nothing changed: nothing to declare.
        expect(evaluate(collect(repo, "base", "base")) == [], "an empty diff produced findings")

        if failures:
            for line in failures:
                print(f"self-test FAILED: {line}", file=sys.stderr)
            return 1
        print("self-test ok")
        return 0


# ---------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    if "--self-test" in argv:
        return self_test()
    base = head = body_file = None
    it = iter(argv)
    for arg in it:
        if arg == "--base":
            base = next(it, None)
        elif arg == "--head":
            head = next(it, None)
        elif arg == "--body-file":
            body_file = next(it, None)
        else:
            print(__doc__.split("Usage:")[1].split("\n\n")[0].strip(), file=sys.stderr)
            return 2
    if not base:
        print("usage: check_change_gate.py --base <ref> [--head <ref>] [--body-file <path>] | --self-test", file=sys.stderr)
        return 2
    if body_file:
        body = Path(body_file).read_text()
    else:
        body = os.environ.get("GATE_PR_BODY", "")
    repo = Path(__file__).resolve().parent.parent
    try:
        diff = collect(repo, base, head or "HEAD")
    except RuntimeError as err:
        print(f"change gate: {err}", file=sys.stderr)
        return 2
    return report(evaluate(diff), body)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
