#!/usr/bin/env python3
"""Give a task to an agent, keep what it wrote, and score it.

    EVAL_AGENT_CMD='claude --dangerously-skip-permissions -p "$(cat PROMPT.md)"' \
        python3 evals/agent.py 001-checkout-ambiguous-payment --attempts 3

The agent is not part of the task. Whatever builds the answer is a process
contract and nothing more:

  * it starts in a workspace containing `PROMPT.md` and `metadata/`, which is
    the task's fixture — the store with the missing piece missing;
  * it may write anything under `metadata/`;
  * whatever it leaves behind there when it exits is the answer.

That is the whole interface, so Claude Code, Codex or a plain API loop are
interchangeable and none of them can be told apart by the task.

The scenarios and the oracle are never in the workspace: a suite the candidate
can read is a suite the candidate can satisfy without building anything.
"""

from __future__ import annotations

import argparse
import filecmp
import hashlib
import json
import os
import pathlib
import shutil
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import run as harness  # noqa: E402  (the harness lives beside this file)

#: Outside the repository, deliberately. The oracle for this task is the flow
#: the checked-in Petshop ships, so an agent working anywhere under the repo
#: can read the answer — and an agent that can read the answer measures
#: nothing. The workspace holds the fixture and the brief, and stands alone.
WORKSPACES = pathlib.Path(
    os.environ.get("EVAL_WORKSPACE_ROOT", pathlib.Path.home() / ".cache" / "donat-evals")
) / "agent"

#: The default scaffold writes a machine-readable record of its own run, which
#: is where the token and dollar figures come from. Any other agent works too —
#: it just reports no cost unless it leaves an `agent-output.json` behind.
DEFAULT_COMMAND = (
    'claude --dangerously-skip-permissions --output-format json '
    '-p "$(cat PROMPT.md)" | tee agent-output.json'
)

#: Files the adapter itself puts in the workspace: not the agent going astray.
ADAPTER_FILES = {"metadata", "answer", "PROMPT.md", "fixture-reference",
                 "agent-output.json", ".claude"}

#: The plugin whose skills this repository ships. Naming it here, rather than
#: leaving it to whatever the developer happens to have installed, is the whole
#: point: the skill set is a *variable of the run*, recorded with the result,
#: and two runs are only comparable if it is the same or deliberately different.
REPO_SKILLS = harness.REPO / "plugins" / "donat" / "skills"


def task_fingerprint(task: dict) -> str:
    """What the task *was* when this attempt ran.

    The skill set is recorded because it is the thing being tuned; the task has
    to be recorded for the same reason, from the other side. Fixing an
    underspecified prompt is a change to the question, and two arms asked
    different questions are not a paired comparison however carefully they were
    labelled. This is the guard that notices.
    """

    digest = hashlib.sha256()
    # The question, and nothing else. Earlier this hashed the whole directory,
    # which meant recording a measured `bare_baseline` in `task.yaml` — pure
    # bookkeeping, invisible to any candidate — invalidated every comparison
    # that had ever been made. A guard that fires on annotations gets switched
    # off, and then it is not there for the change that matters.
    parts: list[pathlib.Path] = [task["dir"] / task["prompt"]]
    parts += sorted(p for p in (task["dir"] / task["scenarios"]).rglob("*.py"))
    for path in parts:
        if not path.is_file():
            continue
        digest.update(str(path.relative_to(task["dir"])).encode())
        digest.update(path.read_bytes())
    # What the candidate starts from and may write: change either and it is a
    # different exercise, however similar the prose stayed.
    digest.update(json.dumps(task.get("fixture"), sort_keys=True).encode())
    digest.update(json.dumps(task.get("writable"), sort_keys=True).encode())
    return digest.hexdigest()[:12]


def install_help() -> pathlib.Path:
    """Put `donat help` on the agent's PATH, and nothing else the engine can do.

    The help surface is read out of the binary's own declarations — every
    connector with its operations, and the local capabilities — with no
    database, no network and no metadata. It is documentation that cannot drift
    from the build, which is exactly what an agent writing connector activities
    needs and exactly what it did not have.

    Everything else the binary offers still fails closed: `validate`, `migrate`
    and the process commands all refuse without `DONAT_GRAPHQL_DATABASE_URL`,
    which this workspace does not have. And it is a copy rather than a link to
    the build, because the path would otherwise say where this repository is —
    and an agent that can find the repository can read the oracle.
    """

    binary = harness.engine_binary()
    shared = WORKSPACES / "bin"
    shared.mkdir(parents=True, exist_ok=True)
    installed = shared / "donat"
    if not installed.exists() or installed.stat().st_mtime < binary.stat().st_mtime:
        shutil.copy2(binary, installed)
    return shared


def install_skills(place: pathlib.Path, skills: pathlib.Path | None) -> dict:
    """Put a skill set in the workspace, and record exactly which one.

    Skills are the thing being tuned, so they cannot be ambient. A run that
    inherits whatever the developer has installed is not reproducible and, worse,
    is not comparable to the run before it — which is the only measurement that
    tells you whether an edit to a skill helped.

    The identity is a hash of the content, not a version string: a version
    people forget to bump is how two different skill sets come to share a name.
    """

    if skills is None:
        return {"name": "none", "hash": "none", "files": 0}
    if not skills.is_dir():
        sys.exit(f"no such skill set: {skills}")
    target = place / ".claude" / "skills"
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(skills, target)
    digest = hashlib.sha256()
    files = sorted(p for p in skills.rglob("*") if p.is_file())
    for path in files:
        digest.update(str(path.relative_to(skills)).encode())
        digest.update(path.read_bytes())
    return {"name": "donat-plugin" if skills == REPO_SKILLS else skills.name,
            "hash": digest.hexdigest()[:12], "files": len(files)}


def cost_of(place: pathlib.Path) -> dict:
    """Tokens and dollars, when the agent left a record of them.

    Cost is a first-class metric, not a footnote: an answer that scores 1.0 for
    ten times the tokens is a different product decision.
    """

    record = place / "agent-output.json"
    if not record.exists():
        return {}
    try:
        payload = json.loads(record.read_text())
    except json.JSONDecodeError:
        return {}
    if isinstance(payload, list):
        payload = next((entry for entry in reversed(payload)
                        if isinstance(entry, dict) and entry.get("type") == "result"), {})
    if not isinstance(payload, dict):
        return {}
    usage = payload.get("usage") or {}
    tokens = sum(int(usage.get(key, 0) or 0) for key in
                 ("input_tokens", "output_tokens",
                  "cache_creation_input_tokens", "cache_read_input_tokens"))
    money = {"usd": payload.get("total_cost_usd"), "turns": payload.get("num_turns")}
    if tokens:
        money["total_tokens"] = tokens
        money["output_tokens"] = usage.get("output_tokens")
    return {key: value for key, value in money.items() if value is not None}


def workspace_for(task: dict, attempt: int) -> pathlib.Path:
    place = WORKSPACES / f"{task['dir'].name}--{attempt}"
    if place.exists():
        shutil.rmtree(place)
    (place / "metadata").parent.mkdir(parents=True, exist_ok=True)
    return place


def lay_out(task: dict, place: pathlib.Path) -> None:
    """The fixture, and the brief. Nothing else."""

    harness.compose_metadata(task, None, place / "metadata")
    prompt = (task["dir"] / task["prompt"]).read_text()
    writable = "\n".join(f"- `metadata/{path}`" for path in task["writable"])
    # The canary rides in the brief so that if this task ever turns up in a
    # training set, it is detectable there afterwards.
    (place / "PROMPT.md").write_text(
        f"<!-- {task['canary']} -->\n\n{prompt}\n\n---\n\n"
        f"The store's metadata is in `metadata/`. Write your answer there; the "
        f"files this task expects you to add or change are:\n\n{writable}\n\n"
        f"`donat help` is on your PATH. It prints this build's own reference for "
        f"every connector and every local capability, read out of the binary "
        f"rather than out of documentation that can drift: `donat help` for the "
        f"contents, `donat help connectors`, `donat help <name>` for one in "
        f"full.\n\n"
        f"There is no database here, so `donat validate`, `donat migrate` and "
        f"the process commands will refuse to run: the answer has to be right "
        f"by reading, not by trying. Do not write tests.\n"
    )


def collect(task: dict, place: pathlib.Path) -> tuple[pathlib.Path, dict]:
    """Everything the agent changed under metadata/, as an overlay.

    Also the conduct record: which files it touched, and whether any of them
    were outside what the task said it could write. An answer that got there by
    editing something it was told not to is not an answer.
    """

    answer = place / "answer"
    if answer.exists():
        shutil.rmtree(answer)
    fixture = place / "fixture-reference"
    harness.compose_metadata(task, None, fixture)

    touched = []
    for produced in sorted(p for p in (place / "metadata").rglob("*") if p.is_file()):
        relative = produced.relative_to(place / "metadata")
        original = fixture / relative
        if original.exists() and filecmp.cmp(original, produced, shallow=False):
            continue
        target = answer / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(produced, target)
        touched.append(str(relative))

    removed = [str(p.relative_to(fixture)) for p in fixture.rglob("*")
               if p.is_file() and not (place / "metadata" / p.relative_to(fixture)).exists()]
    shutil.rmtree(fixture, ignore_errors=True)
    answer.mkdir(parents=True, exist_ok=True)

    allowed = set(task.get("writable", []))
    conduct = {
        "touched": touched,
        "deleted": removed,
        "outside_writable": sorted(set(touched) - allowed),
        "stray": sorted(str(p.relative_to(place)) for p in place.iterdir()
                        if p.name not in ADAPTER_FILES),
    }
    conduct["clean"] = not (conduct["outside_writable"] or conduct["deleted"])
    print(f"  the agent changed {len(touched)} file(s) under metadata/"
          + ("" if conduct["clean"] else "  — CONDUCT: "
             f"outside writable {conduct['outside_writable']}, deleted {conduct['deleted']}"))
    return answer, conduct


def budget_seconds(task: dict) -> int:
    declared = str(task.get("budget", {}).get("wall_clock", "20m")).strip()
    if declared.endswith("m"):
        return int(declared[:-1]) * 60
    if declared.endswith("h"):
        return int(declared[:-1]) * 3600
    return int(declared.rstrip("s"))


#: Where a read would be a leak. The oracle for every task is the flow the
#: checked-in Petshop ships, so a single `cat` of the repository is a perfect
#: answer with a spotless write-side conduct record.
GROUND_TRUTH = (str(harness.REPO / "examples" / "petshop"), str(harness.TASKS))


def transcript_of(place: pathlib.Path, record: dict) -> pathlib.Path | None:
    """The agent's own log of what it did, found by the session it reported.

    `--output-format json` returns the final message and the bill, not the tool
    calls. Claude Code keeps the full transcript under `~/.claude/projects`; we
    locate it by session id rather than by reconstructing the directory name,
    because that encoding is not ours to depend on.
    """

    session = record.get("session_id")
    if not session:
        return None
    root = pathlib.Path.home() / ".claude" / "projects"
    return next(root.glob(f"*/{session}.jsonl"), None) if root.is_dir() else None


def scan_for_leaks(place: pathlib.Path) -> dict:
    """Did the agent read anything outside the workspace it was given?

    The write side of conduct has always been checked. The read side is the one
    that matters more here and was never looked at: an answer copied from the
    oracle writes only declared files and scores a clean pass. Recent work on
    benchmark validity scans agent transcripts for exactly this — "ground truth
    access" is its own named flaw category — and the same scan is available to
    us because the record is already on disk.

    A finding is not automatically a void. It is evidence, reported with the
    attempt, so that "the corpus assumes good faith" stops being a paragraph in
    a README and becomes a number that can be checked.
    """

    verdict = {"scanned": False, "tool_calls": 0, "ground_truth_reads": [],
               "outside_workspace": []}
    record = {}
    output = place / "agent-output.json"
    if output.exists():
        try:
            record = json.loads(output.read_text())
        except json.JSONDecodeError:
            record = {}
    if isinstance(record, list):
        record = next((r for r in reversed(record)
                       if isinstance(r, dict) and r.get("type") == "result"), {})
    transcript = transcript_of(place, record if isinstance(record, dict) else {})
    if transcript is None or not transcript.exists():
        return verdict

    verdict["scanned"] = True
    workspace = str(place)
    for line in transcript.read_text(errors="replace").splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        content = ((row.get("message") or {}).get("content")) or []
        if not isinstance(content, list):
            continue
        for part in content:
            if not (isinstance(part, dict) and part.get("type") == "tool_use"):
                continue
            verdict["tool_calls"] += 1
            blob = json.dumps(part.get("input") or {})
            if any(path in blob for path in GROUND_TRUTH):
                verdict["ground_truth_reads"].append(f"{part.get('name')}: {blob[:200]}")
            elif str(harness.REPO) in blob and workspace not in blob:
                verdict["outside_workspace"].append(f"{part.get('name')}: {blob[:200]}")
    return verdict


def read_twice_if_all_red(task: dict, answer: pathlib.Path) -> dict:
    """One reading, and a second one when the first says *everything* failed.

    A stand that deploys and then does not serve fails every scenario at once
    with all four gates green — which is exactly what a hopeless answer looks
    like. The two were indistinguishable until an attempt that read 10/10 red
    read 8/10 on a rerun: the answer was wrong either way, so the score was
    unharmed, but the same fault lands on a *correct* answer just as easily and
    there would be nothing in the record to show it.

    Total failure is rare enough to pay for a second stand, and cheap to judge:
    if the two readings agree, the answer really is that bad and the second
    reading costs one stand; if they disagree, the first was the harness
    talking and only the second is evidence.
    """

    report = harness.run_candidate(task, str(answer), regression="if-clean")
    verdicts = report.get("scenarios") or {}
    gates_green = all(report["gates"].get(gate) == "pass" for gate in harness.GATES)
    if not verdicts or not gates_green:
        return report
    if any(verdict == "pass" for verdict in verdicts.values()):
        return report

    print("  every scenario failed on a stand that came up green — reading again")
    second = harness.run_candidate(task, str(answer), regression="if-clean")
    agreed = (second.get("scenarios") or {}) == verdicts
    second["reread"] = {
        "why": "every scenario failed with all gates green",
        "first": verdicts,
        "agreed": agreed,
    }
    if not agreed:
        print("  the two readings disagree — the first was the stand, not the answer")
    return second


def attempt(task: dict, index: int, command: str,
            skills: pathlib.Path | None = None) -> dict:
    place = workspace_for(task, index)
    lay_out(task, place)
    installed = install_skills(place, skills)
    print(f"\n-- attempt {index}: {command}\n   in {place}"
          f"\n   skills: {installed['name']} ({installed['hash']}, "
          f"{installed['files']} file(s))")

    tools = install_help()
    environment = harness.child_env({"PATH": f"{tools}:{os.environ.get('PATH', '')}"})

    started = time.monotonic()
    timeout = budget_seconds(task)
    # Its own process group, so the budget can actually be enforced. Killing
    # the `sh -c` alone leaves the model and its pipe running: it goes on
    # editing `metadata/` while `collect()` walks it — the answer is then a
    # torn snapshot — and it keeps spending through the remaining attempts.
    running = subprocess.Popen(command, shell=True, cwd=place, start_new_session=True,
                               env=environment)
    try:
        exit_code, timed_out = running.wait(timeout=timeout), False
    except subprocess.TimeoutExpired:
        exit_code, timed_out = None, True
        for signal_to_send in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(os.getpgid(running.pid), signal_to_send)
                running.wait(timeout=15)
                break
            except (ProcessLookupError, PermissionError):
                break
            except subprocess.TimeoutExpired:
                continue
    spent = time.monotonic() - started
    print(f"   agent finished in {spent:.0f}s"
          + (" (out of budget)" if timed_out else f" (exit {exit_code})"))

    answer, conduct = collect(task, place)
    report = read_twice_if_all_red(task, answer)
    report.update({
        "attempt": index,
        "agent_seconds": round(spent, 1),
        "agent_exit": exit_code,
        "agent_timed_out": timed_out,
        "workspace": str(place),
        "conduct": conduct,
        "skills": installed,
        "task_hash": task_fingerprint(task),
        "leaks": scan_for_leaks(place),
        "cost": cost_of(place),
    })
    if not conduct["clean"]:
        # A voided attempt is not a failed one: it says nothing about whether
        # the agent can build the thing, only that this run cannot be read.
        report["voided"] = "the answer touched files the task did not open"
    harness.print_report(report)
    leaks = report.get("leaks") or {}
    if leaks.get("ground_truth_reads"):
        print(f"  LEAK: {len(leaks['ground_truth_reads'])} tool call(s) touched the "
              f"oracle's own files — this attempt is evidence about the harness, "
              f"not about the model")
        for line in leaks["ground_truth_reads"][:3]:
            print(f"    {line}")
    elif leaks.get("scanned"):
        outside = len(leaks.get("outside_workspace", []))
        print(f"  transcript: {leaks['tool_calls']} tool call(s), no read of the "
              f"oracle" + (f", {outside} outside the workspace" if outside else ""))
    if report.get("cost"):
        money = report["cost"]
        print(f"  cost: {money.get('total_tokens', '?')} tokens"
              + (f", ${money['usd']:.2f}" if money.get("usd") is not None else ""))
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("task")
    parser.add_argument("--attempts", type=int, default=1,
                        help="how many independent tries (pass^k needs several)")
    parser.add_argument("--command", default=os.environ.get("EVAL_AGENT_CMD", DEFAULT_COMMAND),
                        help="the agent invocation, run in the workspace")
    parser.add_argument("--skills", default=None, metavar="DIR",
                        help="a skill set to install in the workspace: a directory of "
                             "SKILL.md folders, `plugin` for this repository's own, or "
                             "omitted for the bare model (the baseline every other run "
                             "is read against)")
    parser.add_argument("--holdout", action="store_true",
                        help="permit a task marked `split: holdout` — for the periodic "
                             "absolute reading, never for a tuning iteration")
    parser.add_argument("--label", default=None,
                        help="name this arm of a comparison, e.g. `plugin-v3`; defaults "
                             "to the skill set's name")
    args = parser.parse_args()

    skills = None
    if args.skills == "plugin":
        skills = REPO_SKILLS
    elif args.skills:
        skills = pathlib.Path(args.skills).expanduser().resolve()

    task = harness.load_task(args.task)
    if task["split"] == "holdout" and not args.holdout:
        sys.exit(
            f"{task['id']} is a holdout task. Held-out tasks exist so that one number "
            f"in this corpus is not tuned on; reading them during a tuning loop is how "
            f"that number stops meaning anything. Pass --holdout if this is the "
            f"periodic absolute measurement and not an iteration.")
    WORKSPACES.mkdir(parents=True, exist_ok=True)
    record = WORKSPACES / f"{task['dir'].name}--runs.jsonl"
    arm = args.label or ("donat-plugin" if skills else "none")
    reports = []
    for index in range(1, args.attempts + 1):
        # Written as it happens, not at the end. Attempt three raising — a
        # stale engine on the port, a diagnostic that throws — used to discard
        # attempts one and two along with the money they cost. The sweep
        # learned this the same way and fixed it the same way.
        report = attempt(task, index, args.command, skills)
        report["arm"] = arm
        reports.append(report)
        with record.open("a") as sink:
            sink.write(json.dumps(report) + "\n")

    # One decomposition, shared with everything else that scores a run: an
    # attempt that never built and an attempt that built and behaved wrongly are
    # both "not a pass", and averaging them together hides which one a model
    # actually does.
    score = harness.tally(reports)
    print(f"\n{task['id']}: pass@1 {score['pass@1']} "
          f"ci95 {score['ci95']}  pass^k {score['pass^k']} "
          f"({score['scored']} scored of {score['attempts']} attempts)")
    # The sensitive one. At k=3 a task-level rate moves in thirds and cannot
    # see a skill edit that took an attempt from six scenarios to nine; the
    # scenario rate has ten times the resolution and is what a tuning loop
    # should watch.
    if score["scenario_rate"] is not None:
        print(f"  scenarios {score['scenario_pass']}/{score['scenario_total']} "
              f"= {score['scenario_rate']} ci95 {score['scenario_ci95']}")
    print(f"  unbuilt {score['unbuilt']}   wrong {score['wrong']}   "
          f"regressed {score['regressed']}   voided {score['voided']}" + ("   <- the score above is only as good "
          "as the containment under it" if score["voided"] else ""))
    if score["usd"]:
        print(f"  cost ${score['usd']:.2f} total, "
              f"${score['usd'] / max(score['attempts'], 1):.2f} per attempt")
    if score["attempts"] < 3:
        print("  (k<3: a shape, not a score — one attempt cannot separate a model "
              "from a lucky sample)")

    print(f"run record: {record}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
