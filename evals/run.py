#!/usr/bin/env python3
"""Run one candidate solution of one eval task, or verify a task's own honesty.

    python3 evals/run.py run 001-checkout-ambiguous-payment oracle
    python3 evals/run.py run 001-checkout-ambiguous-payment anti/success-default
    python3 evals/run.py run 001-checkout-ambiguous-payment /path/to/an/answer
    python3 evals/run.py verify-oracles [task ...]

A candidate is an overlay: the files it wrote, dropped onto the task's fixture.
The stand is raised the way the example documents its own deploy — engine DDL,
store DDL, Process revisions, then serve — from the engine built in this working
tree.

`migrate`, `validate` and `deploy` are gates: they say where a candidate fell
over and contribute nothing to the score. Only the scenarios score.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import re
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

REPO = pathlib.Path(__file__).resolve().parent.parent
EVALS = REPO / "evals"
TASKS = EVALS / "tasks"
STATE = EVALS / ".state"
COMPOSE = ["docker", "compose", "-f", str(EVALS / "docker-compose.yml")]

PORT = int(os.environ.get("EVAL_PORT", "8090"))
BASE_URL = os.environ.get("EVAL_BASE_URL", f"http://127.0.0.1:{PORT}")
PROVIDERS_URL = os.environ.get("EVAL_PROVIDERS_URL", "http://127.0.0.1:8097")
PG_HOST_URL = os.environ.get(
    "EVAL_PG_URL", "postgresql://postgres:postgres@127.0.0.1:15435/donat_evals"
)
JWT_KEY = "petshop-dev-jwt-key-change-me-32bytes+"
ADMIN_SECRET = "eval-secret"

#: Phases in the order they run. A candidate that fails one never reaches the
#: next, and the report says which.
GATES = ("compose", "migrate", "validate", "deploy")

#: Phases that are not gates but can still stop a candidate being read. A
#: regression suite that never produced a report has *not* said the store is
#: intact; scoring that silence as a pass is the same mistake as scoring a
#: missing junit file as green scenarios, one layer further out.
PHASES = GATES + ("regression",)


class Failed(Exception):
    """A phase this candidate did not get past."""

    def __init__(self, phase: str, detail: str):
        super().__init__(f"{phase}: {detail}")
        self.phase = phase
        self.detail = detail


# -- task loading ------------------------------------------------------------


def load_task(name: str) -> dict:
    try:
        import yaml
    except ModuleNotFoundError:  # pragma: no cover - environment problem
        sys.exit("evals/run.py needs PyYAML (pip install pyyaml)")
    directory = TASKS / name
    if not directory.is_dir():
        sys.exit(f"no such task: {name} (looked in {TASKS})")
    task = yaml.safe_load((directory / "task.yaml").read_text())
    task["dir"] = directory
    task.setdefault("split", "dev")
    if task["split"] not in ("dev", "holdout"):
        sys.exit(f"{name}: split must be dev or holdout, not {task['split']!r}")
    return task


def tasks_in(split: str) -> list[str]:
    """Task names on one side of the split, in order."""

    return sorted(
        directory.name for directory in TASKS.iterdir()
        if (directory / "task.yaml").exists() and load_task(directory.name)["split"] == split
    )


# -- building the metadata a candidate is judged on --------------------------


def compose_metadata(task: dict, candidate: str | None, destination: pathlib.Path) -> None:
    """Fixture, minus what the task removed, plus what the candidate wrote.

    A `candidate` of None composes the bare fixture — the store with the missing
    piece missing, which is what an agent is handed.
    """

    if destination.exists():
        shutil.rmtree(destination)
    base = REPO / task["fixture"]["base"]
    # The same rehosting the Petshop stands do: object-storage addresses have to
    # name published ports, because the engine runs on the host.
    subprocess.run(
        [sys.executable, str(REPO / "tests-system" / "fast_metadata.py"),
         str(base), str(destination), "--rehost"],
        check=True, capture_output=True, text=True,
    )

    for entry in task["fixture"].get("remove", []):
        if "#" in entry:
            relative, line = entry.split("#", 1)
            path = destination / relative
            kept = [l for l in path.read_text().splitlines(keepends=True)
                    if line not in l]
            path.write_text("".join(kept))
        else:
            (destination / entry).unlink()

    overlay, patch = (None, None) if candidate is None else resolve_candidate(task, candidate)
    if overlay is not None:
        for source in sorted(p for p in overlay.rglob("*") if p.is_file()):
            target = destination / source.relative_to(overlay)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    if patch is not None:
        applied = subprocess.run(
            ["patch", "-p0", "-s", "-i", str(patch)],
            cwd=destination, capture_output=True, text=True,
        )
        if applied.returncode != 0:
            # A patch that no longer applies means the oracle moved under it.
            # That is a corpus defect, not a candidate failure.
            raise Failed("compose", f"{patch.name} no longer applies: {applied.stderr.strip()}")


def resolve_candidate(task: dict, candidate: str):
    """Where the candidate's files come from: an overlay, a patch, or both."""

    directory = task["dir"]
    if candidate == "fixture":
        return None, None
    if candidate == "oracle":
        return directory / task["oracle"], None
    if candidate.startswith("anti/"):
        name = candidate.split("/", 1)[1]
        if name not in task.get("anti", {}):
            sys.exit(f"{task['id']} declares no anti-oracle {name!r}")
        # An anti-oracle is the oracle with one defect: same overlay, one patch.
        return directory / task["oracle"], directory / "anti" / name / "defect.patch"
    path = pathlib.Path(candidate).expanduser().resolve()
    if not path.is_dir():
        sys.exit(f"a candidate is a directory of the files it wrote: {candidate}")
    return path, None


# -- the stand ---------------------------------------------------------------


def engine_binary() -> pathlib.Path:
    # A non-login shell (editor terminal, make) may not have rustup on PATH.
    cargo = shutil.which("cargo") or str(pathlib.Path.home() / ".cargo" / "bin" / "cargo")
    subprocess.run(
        [cargo, "build", "--manifest-path", str(REPO / "Cargo.toml"),
         "-p", "donat-server", "--bin", "donat"],
        check=True,
    )
    return REPO / "target" / "debug" / "donat"


def engine_env(database_url: str) -> dict:
    env = child_env({
        "DONAT_GRAPHQL_DATABASE_URL": database_url,
        "PETSHOP_PAYMENT_BASE_URL": PROVIDERS_URL,
        "PETSHOP_PAYMENT_API_TOKEN": "petshop-payment-token",
        "PETSHOP_TAX_BASE_URL": PROVIDERS_URL,
        "PETSHOP_TAX_API_TOKEN": "petshop-tax-token",
        "DONAT_MOCK_CARRIER_BASE_URL": PROVIDERS_URL,
        "DONAT_MOCK_CARRIER_TOKEN": "petshop-carrier-token",
        "PETSHOP_NOTIFICATION_BASE_URL": PROVIDERS_URL,
        "PETSHOP_NOTIFICATION_API_TOKEN": "petshop-notification-token",
        "PETSHOP_PAYOUT_BASE_URL": PROVIDERS_URL,
        "PETSHOP_PAYOUT_API_TOKEN": "petshop-payout-token",
        "PETSHOP_S3_KEY": "petshopminio",
        "PETSHOP_S3_SECRET": "petshopminiosecret",
        "PETSHOP_FILE_SIGNING_SECRET": "petshop-file-signing-secret",
    })
    return env


def services_up() -> None:
    # A gate the report advertises has to be able to *fail* as that gate. With
    # check=True a docker failure left run_candidate as a CalledProcessError
    # traceback, so `compose` could only ever read "pass" or never be reached.
    done = subprocess.run(COMPOSE + ["up", "-d", "--wait"], capture_output=True, text=True)
    if done.returncode != 0:
        raise Failed("compose", (done.stderr or done.stdout).strip()[-2000:]
                     or f"docker compose up exited {done.returncode}")


def fresh_database(name: str) -> str:
    """One database per candidate: nothing carries over between runs."""

    psql = COMPOSE + ["exec", "-T", "postgres", "psql", "-q", "-U", "postgres"]
    subprocess.run(psql + ["-c", f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE)'],
                   check=True, capture_output=True, text=True)
    subprocess.run(psql + ["-c", f'CREATE DATABASE "{name}"'],
                   check=True, capture_output=True, text=True)
    return PG_HOST_URL.rsplit("/", 1)[0] + "/" + name


def run_gate(phase: str, argv: list[str], env: dict) -> str:
    done = subprocess.run(argv, env=env, capture_output=True, text=True)
    if done.returncode != 0:
        # Both streams: `validate` lists the inconsistencies on stdout and only
        # says how many there were on stderr, so either alone is useless.
        said = "\n".join(part for part in (done.stdout.strip(), done.stderr.strip()) if part)
        raise Failed(phase, said[-4000:])
    return done.stdout


def store_answers(url: str) -> bool:
    request = urllib.request.Request(
        f"{url}/v1/graphql", data=b'{"query":"{ __typename }"}',
        headers={"content-type": "application/json"}, method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=3):
            return True
    except (urllib.error.URLError, OSError):
        return False


def provision(database: str) -> None:
    with open(REPO / "tests-system" / "provision.sql", "rb") as sql:
        done = subprocess.run(
            COMPOSE + ["exec", "-T", "postgres", "psql", "-v", "ON_ERROR_STOP=1",
                       "-q", "-U", "postgres", "-d", database],
            stdin=sql, capture_output=True, text=True,
        )
    # Seed data that did not land is a deploy failure, not a crash: the stand
    # would come up and every scenario would fail on an empty store.
    if done.returncode != 0:
        raise Failed("deploy", (done.stderr or done.stdout).strip()[-2000:]
                     or f"provision.sql exited {done.returncode}")


def instances(database: str) -> list[tuple[str, str]]:
    read = subprocess.run(
        COMPOSE + ["exec", "-T", "postgres", "psql", "-q", "-t", "-A", "-F", " ",
                   "-U", "postgres", "-d", database,
                   "-c", "select source_name, id from donat.process_instances"],
        capture_output=True, text=True,
    )
    if read.returncode != 0:
        return []
    rows = [line.split(" ") for line in read.stdout.split("\n") if " " in line]
    return [(row[0], row[1]) for row in rows]


# -- one run -----------------------------------------------------------------


def owning_modules(task: dict) -> set[str]:
    """Which Petshop test modules are about the piece this task removes.

    SWE-bench takes its baseline from the repository *before* the fix, which
    works because that repository still builds. Ours does not: remove a flow and
    the commands that start it no longer validate, so the fixture has no stand
    to measure. The baseline therefore comes from the oracle — a store that
    works — and the modules that own the removed files are held apart from it.

    They have to be. A candidate may implement the task differently and still be
    right, and Petshop's own tests for that behaviour were written against the
    reference design. Counting those as regressions is the mechanism by which
    benchmarks reject correct answers; keeping them as a separate, unscored
    reading is what lets us see the difference without punishing it.
    """

    sys.path.insert(0, str(EVALS))
    import mutants  # noqa: PLC0415 - the coverage map lives with the generator

    modules: set[str] = set()
    for entry in task["fixture"].get("remove", []):
        modules.update(mutants.coverage_for(entry.split("#", 1)[0]))
    return modules


def write_baseline(task: dict, verdicts: dict, fixture: dict | None = None) -> None:
    """What a correct store passes, split from what is the task's own subject.

    Regenerated by `verify-oracles`, committed with the task, and reviewed like
    any other part of it: a baseline that shrinks means a correct answer now
    does less of the store than it used to, which is a change to what the task
    is asking, not a detail.
    """

    green = [node for node, verdict in verdicts.items() if verdict == "pass"]
    if fixture is not None:
        # The exact split, when the fixture deploys: a test that fails without
        # the missing piece and passes with it *is* the task's subject —
        # SWE-bench's FAIL_TO_PASS, computed rather than inferred. Everything
        # else the correct store passes is the store the candidate must not
        # break. The module map is a blunt instrument next to this: on task 003
        # it consigned the whole of `test_search` and `test_shopping_basics` to
        # "the task's own subject" because they touch some table, leaving 115
        # tests guarded where this leaves more than twice that.
        subject = sorted(n for n in green if fixture.get(n) != "pass")
        keep = sorted(n for n in green if fixture.get(n) == "pass")
        how = "the fixture and the oracle, test by test"
    else:
        owned = owning_modules(task)
        keep = sorted(n for n in green if n.split("::", 1)[0] not in owned)
        subject = sorted(n for n in green if n.split("::", 1)[0] in owned)
        how = "the coverage map, because the fixture does not deploy"
    skipped = sorted(node for node, verdict in verdicts.items() if verdict == "skip")
    failing = sorted(node for node, verdict in verdicts.items() if verdict == "fail")
    baseline_path(task).write_text(json.dumps({
        "note": f"Computed by verify-oracles from {how} — do not hand-edit.",
        "pass_to_pass": keep,
        "reference_behaviour": subject,
        "red_on_the_oracle": failing,
        "skipped": skipped,
    }, indent=2) + "\n")
    print(f"  baseline: {len(keep)} test(s) a candidate must not break, "
          f"{len(subject)} about the task's own subject (reported, never scored), "
          f"{len(failing)} red even on the oracle, {len(skipped)} skipped")


def baseline_path(task: dict) -> pathlib.Path:
    return task["dir"] / "pass-to-pass.json"


def broke_the_store(task: dict, verdicts: dict) -> list[str]:
    """Which tests passed on the fixture and fail on this candidate.

    The split is computed, never declared. Whatever the null candidate cannot
    pass belongs to the hole the task cuts — those are the task's own business,
    the FAIL_TO_PASS half — and everything the fixture *does* pass is the store
    the candidate is not allowed to break. Declaring the list by hand would rot
    the first time a scenario or a fixture moved.
    """

    path = baseline_path(task)
    if not path.exists():
        # No baseline means the guard cannot fire at all, and a guard that
        # cannot fire must not look like a guard that found nothing. The caller
        # turns this into a phase failure rather than a silent clean bill.
        raise Failed("regression", (
            f"{task['id']} has no {path.name}: run `verify-oracles {task['dir'].name}` "
            f"to record what a correct store passes. Until then nothing can be said "
            f"about whether a candidate broke the store."))
    baseline = json.loads(path.read_text())["pass_to_pass"]
    return sorted(node for node in baseline if verdicts.get(node) == "fail")


def baseline_not_reached(task: dict, verdicts: dict) -> list[str]:
    """Guarded tests that produced no verdict at all this run.

    Absent is not failing, so these are deliberately kept out of the regression
    count — but they are not nothing either. A baseline promising 211 tests
    while 190 ran is a guard quietly guarding less than it claims, and the
    usual cause is a module the candidate broke badly enough that pytest could
    not collect it, or a baseline that has rotted against a renamed test.
    """

    baseline = json.loads(baseline_path(task).read_text())["pass_to_pass"]
    return sorted(node for node in baseline if node not in verdicts)


def run_candidate(task: dict, candidate: str | None, *, keep: bool = False,
                  select: str | None = None, regression: str | None = None) -> dict:
    """Score one candidate. `None` runs the bare fixture — the null candidate.

    `regression` runs the store's own black-box suite against the same stand:
    `"always"` for the null candidate, whose failures *define* the baseline, and
    `"if-clean"` for a real answer, where it only earns its twelve minutes once
    the task's own scenarios are green — an answer that did not build the thing
    is already not a pass, and what else it broke changes nothing.
    """

    candidate = candidate if candidate is not None else "fixture"
    slug = candidate.replace("/", "-")
    work = STATE / f"{task['dir'].name}--{slug}"
    metadata = work / "metadata"
    work.mkdir(parents=True, exist_ok=True)
    log = work / "engine.log"
    report = {"task": task["id"], "candidate": candidate, "gates": {}, "scenarios": {}}

    engine = engine_binary()
    if store_answers(BASE_URL):
        sys.exit(f"something already answers at {BASE_URL}; stop it or set EVAL_PORT")

    process = None
    try:
        services_up()
        report["gates"]["compose"] = "pass"
        compose_metadata(task, candidate, metadata)

        # Both the task and the candidate, because `run 001 oracle` and
        # `run 002 oracle` used to resolve to the same `eval_oracle` — and the
        # second run's DROP DATABASE ... WITH (FORCE) would tear down the
        # first's engine mid-scenario. Hashed tail: two long answer paths are
        # identical after truncation.
        import hashlib
        stem = f"{task['dir'].name}_{slug}".replace('-', '_')
        digest = hashlib.sha256(stem.encode()).hexdigest()[:8]
        database = f"eval_{stem[:48]}_{digest}"[:60]
        env = engine_env(fresh_database(database))

        run_gate("migrate", [str(engine), "migrate", "--migrations-dir", str(REPO / "migrations")], env)
        run_gate("migrate", [str(engine), "migrate", "--migrations-dir",
                             str(REPO / "examples" / "petshop" / "migrations")], env)
        report["gates"]["migrate"] = "pass"

        run_gate("validate", [str(engine), "validate", "--metadata-dir", str(metadata),
                              "--source", "default"], env)
        report["gates"]["validate"] = "pass"

        run_gate("deploy", [str(engine), "migrate", "--migrations-dir", str(REPO / "migrations"),
                            "--metadata-dir", str(metadata), "--source", "default"], env)
        provision(database)

        serving = dict(env)
        serving.update({
            "DONAT_PORT": str(PORT),
            "DONAT_METADATA_DIR": str(metadata),
            "DONAT_GRAPHQL_ADMIN_SECRET": ADMIN_SECRET,
            "DONAT_GRAPHQL_UNAUTHORIZED_ROLE": "anonymous",
            "DONAT_GRAPHQL_JWT_SECRET": json.dumps({"type": "HS256", "key": JWT_KEY}),
            "RUST_LOG": os.environ.get("RUST_LOG", "donat=info"),
        })
        with open(log, "wb") as sink:
            process = subprocess.Popen([str(engine), "--metadata-dir", str(metadata)],
                                       env=serving, stdout=sink, stderr=sink)
        for _ in range(60):
            if process.poll() is not None:
                raise Failed("deploy", f"the engine exited during start-up; see {log}")
            if store_answers(BASE_URL):
                break
            time.sleep(1)
        else:
            raise Failed("deploy", f"the engine never answered; see {log}")
        report["gates"]["deploy"] = "pass"

        report["scenarios"] = run_scenarios(task, work, select)
        clean = bool(report["scenarios"]) and all(
            verdict == "pass" for verdict in report["scenarios"].values())
        if regression == "always" or (regression == "if-clean" and clean):
            report["regression"] = run_regression(task, work)
            # `always` is the baseline-generating run: there is nothing to
            # compare against yet, and asking would be circular.
            if regression != "always":
                report["regressions"] = broke_the_store(task, report["regression"])
                report["unreached"] = baseline_not_reached(task, report["regression"])
        report["history"] = verify_histories(engine, database, env)
        try:
            report["coverage"] = graph_coverage(metadata, database)
        except Exception as broken:  # noqa: BLE001 - a report, never a verdict
            # Coverage is reported and never scored, so it has no business
            # deciding whether an attempt counts.
            report["coverage_error"] = f"{type(broken).__name__}: {broken}"
    except Failed as failure:
        report["gates"][failure.phase] = "fail"
        report["failure"] = failure.detail
    finally:
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:  # pragma: no cover - stubborn engine
                process.kill()
        if not keep:
            shutil.rmtree(metadata, ignore_errors=True)

    for phase in PHASES:
        report["gates"].setdefault(phase, "skipped")
    return report


#: The four places one attempt can land. They are kept apart on purpose: a run
#: that never deployed and a run that deployed and behaved wrongly are both
#: "not a pass", but averaging them together hides which of the two a model
#: actually does, and they call for opposite fixes.
OUTCOMES = ("voided", "unbuilt", "wrong", "regressed", "pass")


def score_one(report: dict) -> str:
    """Which of OUTCOMES this attempt is.

    `voided` outranks everything: an answer that got there by editing a file
    the task closed is not a result at all, in either direction. Scoring it as
    a failure would be just as wrong as scoring it as a pass — it measures the
    harness's containment, not the model's ability.
    """

    conduct = report.get("conduct") or {}
    if conduct and not conduct.get("clean", True):
        return "voided"
    if any(report["gates"].get(gate) != "pass" for gate in GATES):
        return "unbuilt"
    # A regression phase that failed means the store was never checked. That is
    # not evidence of a clean candidate, and it must not read as one.
    if report["gates"].get("regression") == "fail":
        return "regressed"
    verdicts = report.get("scenarios") or {}
    if not verdicts:
        return "unbuilt"
    # A skipped scenario is not a passing one — the same rule the mutant sweep
    # applies to uncovered behaviour. A run that quietly skips half the task
    # must not read as a solved one.
    if any(verdict != "pass" for verdict in verdicts.values()):
        return "wrong"
    # SWE-bench's PASS_TO_PASS half: it built the missing piece and broke
    # something else. Kept apart from `wrong` because it is a different
    # failure with a different fix — the model understood the task and not
    # the blast radius — and because folding it into `wrong` would make an
    # answer that breaks the store indistinguishable from one that never
    # built anything.
    if report.get("regressions"):
        return "regressed"
    # Guarded tests that produced no verdict at all. A candidate that breaks a
    # module badly enough that pytest cannot collect it leaves its tests absent
    # rather than failing — breaking forty hard would otherwise score better
    # than breaking one softly.
    if report.get("unreached"):
        return "regressed"
    return "pass"


def wilson(passed: int, total: int, z: float = 1.96) -> list[float]:
    """A 95% interval that stays inside [0, 1] at these sample sizes.

    The normal approximation is useless here — at 3 attempts it produces bounds
    outside the unit line and a zero-width interval when every attempt agrees.
    Wilson's is the standard remedy and degrades honestly: 3 of 3 reads
    [0.44, 1.0], not [1.0, 1.0].
    """

    if total == 0:
        return [0.0, 1.0]
    phat = passed / total
    denominator = 1 + z * z / total
    centre = (phat + z * z / (2 * total)) / denominator
    spread = z * math.sqrt(phat * (1 - phat) / total + z * z / (4 * total * total)) / denominator
    return [round(max(0.0, centre - spread), 3), round(min(1.0, centre + spread), 3)]


def tally(reports: list[dict]) -> dict:
    """pass@1 over attempts, with the denominators best practice asks for.

    Voided attempts leave the denominator: a benchmark that counts containment
    failures as model failures rewards a harness that leaks. They are reported
    beside the score, never inside it, because a rising void rate invalidates
    the score above it.
    """

    outcomes = [score_one(report) for report in reports]
    counted = [outcome for outcome in outcomes if outcome != "voided"]
    passed = sum(1 for outcome in counted if outcome == "pass")
    graded = [r for r, o in zip(reports, outcomes) if o != "voided"]
    verdicts = [v for r in graded for v in (r.get("scenarios") or {}).values()]
    scenario_total = len(verdicts)
    scenario_pass = sum(1 for v in verdicts if v == "pass")
    return {
        "attempts": len(outcomes),
        "voided": outcomes.count("voided"),
        "scored": len(counted),
        "pass@1": None if not counted else round(passed / len(counted), 3),
        # τ-bench's point: an agent that passes 90% of the time is only 57%
        # reliable over eight tries, and a store nobody can rebuild twice is not
        # a store anyone can deploy. pass^k is *every* attempt passing.
        "pass^k": None if not counted else int(passed == len(counted)),
        # Never a bare decimal. On three attempts the interval is nearly the
        # whole unit line, and printing 0.667 without it invites a reader to
        # believe a rate that was never measured.
        "ci95": None if not counted else wilson(passed, len(counted)),
        # Resolution, for the loop that tunes something. A task-level rate at
        # k=3 moves in thirds; the same three attempts carry thirty scenario
        # verdicts, and an edit that takes an attempt from six of ten to nine of
        # ten is invisible in the first and obvious in the second. It is a
        # diagnostic, never the headline: an answer that satisfies most branches
        # and abandons the rest is not most of a working store.
        "scenario_pass": scenario_pass,
        "scenario_total": scenario_total,
        "scenario_rate": None if not scenario_total else round(
            scenario_pass / scenario_total, 3),
        "scenario_ci95": None if not scenario_total else wilson(
            scenario_pass, scenario_total),
        "unbuilt": outcomes.count("unbuilt"),
        "wrong": outcomes.count("wrong"),
        "regressed": outcomes.count("regressed"),
        "usd": round(sum((r.get("cost") or {}).get("usd", 0.0) for r in reports), 4),
    }


#: Variables that silently point a child at somebody else's stand. `PETSHOP_FAST_*`
#: is the one that bites: the fast Petshop stand is a *different, pristine*
#: store, and `tests-system`'s own `fast_config` fixture reads it straight from
#: the environment. A developer with `eval "$(./stack.sh env)"` in their shell
#: would have the fast-stand tests pass against the pristine store while the
#: stand under test was never touched — a mutant judged `survived` on tests that
#: never saw it. `DONAT_DATABASE_URL` is the other: the engine prefers it over
#: `DONAT_GRAPHQL_DATABASE_URL`, so an inherited one sends `migrate`, `validate`
#: and `verify-history` to a different database than the one being served.
STAND_LEAKS = (
    "PETSHOP_FAST_BASE_URL", "PETSHOP_FAST_PROVIDERS_URL",
    "DONAT_DATABASE_URL", "DONAT_METADATA_DIR",
)


def child_env(overrides: dict) -> dict:
    """The parent environment, minus anything that addresses another stand."""

    env = {key: value for key, value in os.environ.items() if key not in STAND_LEAKS}
    env.update(overrides)
    return env


def run_scenarios(task: dict, work: pathlib.Path, select: str | None = None) -> dict:
    """Every scenario's own verdict, by test name — not one number.

    `select` is a pytest -k expression, for the authoring loop: when the
    question is "does this new scenario kill this one anti-oracle", raising a
    stand and running the other six scenarios answers nothing and costs the
    most of the run. Verification of a whole task never passes it.
    """

    venv = REPO / "tests-system" / ".venv" / "bin" / "python"
    python = str(venv) if venv.exists() else sys.executable
    outcome = work / "scenarios.json"
    # The work directory is keyed on (task, candidate) and reused, so last
    # run's report is sitting here. If pytest produces none this time — a
    # collection error, a missing venv, a killed process — reading a stale
    # green XML would score the previous candidate as this one, and the
    # "no verdicts" guard below would never fire.
    outcome.with_suffix(".xml").unlink(missing_ok=True)
    env = child_env({
        "PETSHOP_BASE_URL": BASE_URL,
        "PETSHOP_PROVIDERS_URL": PROVIDERS_URL,
        "PETSHOP_JWT_KEY": JWT_KEY,
    })
    done = subprocess.run(
        [python, "-m", "pytest", str(task["dir"] / task["scenarios"]),
         "-p", "no:cacheprovider", "-q", "--tb=short",
         *(["-k", select] if select else []),
         f"--junit-xml={outcome.with_suffix('.xml')}"],
        cwd=str(EVALS), env=env, capture_output=True, text=True,
    )
    (work / "scenarios.log").write_text(done.stdout + done.stderr)
    verdicts = junit_verdicts(outcome.with_suffix(".xml"))
    if not verdicts:
        # No report at all — pytest itself did not run (a missing venv, a
        # collection error). Silence here reads as "nothing failed", which is
        # how a null candidate came to look like a passing one.
        raise Failed("scenarios", (done.stdout + done.stderr).strip()[-400:]
                     or "pytest produced no report")
    return verdicts


#: Petshop modules that cannot run on this stand at all — no object store, and
#: a timing test that measures the machine rather than the store. The sweep
#: imports this one rather than keeping its own: two copies drift, and the copy
#: that keeps running a stack-broken module turns every mutant it touches into
#: a false "the tests caught it".
UNSERVICEABLE = {"test_file_attachments", "test_file_attacks", "test_performance"}


def run_regression(task: dict, work: pathlib.Path) -> dict:
    """The store's own suite, against the candidate's stand. Node ids, not names.

    This is SWE-bench's PASS_TO_PASS, and it exists for the same reason: a patch
    that satisfies the issue and breaks the repository is not a fix. The task's
    own scenarios can only say the missing piece was built; they say nothing
    about the twenty other flows the candidate could have broken on the way, and
    an answer that ships a working checkout on top of a broken subscription
    renewal would score a clean pass here without it.

    These tests are also the strongest evidence available, precisely because
    nobody wrote them for this: they are the product's black-box suite, written
    against Petshop months before the corpus existed and unable to be tuned to
    an answer.
    """

    venv = REPO / "tests-system" / ".venv" / "bin" / "python"
    python = str(venv) if venv.exists() else sys.executable
    report = work / "regression.xml"
    report.unlink(missing_ok=True)
    env = child_env({
        "PETSHOP_BASE_URL": BASE_URL,
        "PETSHOP_PROVIDERS_URL": PROVIDERS_URL,
        "PETSHOP_JWT_KEY": JWT_KEY,
    })
    modules = sorted(
        f"tests/{path.stem}.py"
        for path in (REPO / "tests-system" / "tests").glob("test_*.py")
        if path.stem not in UNSERVICEABLE
    )
    done = subprocess.run(
        [python, "-m", "pytest", *modules, "-p", "no:cacheprovider", "-q", "--tb=line",
         f"--junit-xml={report}"],
        cwd=str(REPO / "tests-system"), env=env, capture_output=True, text=True,
    )
    (work / "regression.log").write_text(done.stdout + done.stderr)
    verdicts = junit_node_verdicts(report)
    if not verdicts:
        raise Failed("regression", (done.stdout + done.stderr).strip()[-400:]
                     or "the regression suite produced no report")
    return verdicts


def junit_node_verdicts(path: pathlib.Path) -> dict:
    """`module::test` for every case, because bare names collide across modules.

    The scenarios are read by bare name — `caught_by` names them that way and
    the sweep's judge looks them up that way. A whole-suite reading cannot
    afford that: 281 tests across 25 modules do reuse names, and a collision
    that silently drops one is a regression nobody sees.
    """

    import xml.etree.ElementTree as ET

    if not path.exists():
        return {}
    verdicts = {}
    for case in ET.parse(path).getroot().iter("testcase"):
        state = "pass"
        for child in case:
            if child.tag in ("failure", "error"):
                state = "fail"
            elif child.tag == "skipped":
                state = "skip"
        module = (case.get("classname") or "").split(".")[-1]
        verdicts[f"{module}::{case.get('name')}"] = state
    return verdicts


def junit_verdicts(path: pathlib.Path) -> dict:
    import xml.etree.ElementTree as ET

    if not path.exists():
        return {}
    verdicts = {}
    for case in ET.parse(path).getroot().iter("testcase"):
        state = "pass"
        for child in case:
            if child.tag in ("failure", "error"):
                state = "fail"
            elif child.tag == "skipped":
                state = "skip"
        # Bare names, because that is what `caught_by` and the sweep's judge
        # look up. But two modules may name a test the same thing, and
        # last-write-wins would let a pass overwrite a real kill — publishing a
        # mutant as a survivor that was in fact caught. On a collision the
        # worse verdict wins.
        name = case.get("name")
        rank = {"fail": 2, "skip": 1, "pass": 0}
        if rank[state] >= rank.get(verdicts.get(name, "pass"), 0):
            verdicts[name] = state
    return verdicts


def graph_coverage(metadata: pathlib.Path, database: str) -> dict:
    """How much of the candidate's own Process graph the scenarios reached.

    Coverage of the *oracle's* graph would say nothing about a candidate that
    designed the flow differently; coverage of the graph it actually built
    answers "did we exercise what this application is". Reported, never scored:
    a coverage number in the reward pushes authors to write tests that walk
    states instead of tests that assert outcomes.
    """

    import yaml

    declared: dict[str, set[str]] = {}
    for path in sorted((metadata / "flows").glob("*.yaml")) if (metadata / "flows").is_dir() else []:
        try:
            document = yaml.safe_load(path.read_text())
        except yaml.YAMLError:
            continue
        if not isinstance(document, dict) or document.get("kind") != "process":
            continue
        # Every read here is defensive on purpose. This walks whatever the
        # candidate left in `flows/`, including files it never included and the
        # validator therefore never saw — a scratch file with `kind: process`
        # and no `name` used to raise a KeyError that `run_candidate` does not
        # catch, losing the whole attempt to a diagnostic that is not scored.
        name = document.get("name")
        if not isinstance(name, str):
            continue
        declared[name] = {
            state["id"] for state in document.get("states", [])
            if isinstance(state, dict) and isinstance(state.get("id"), str)
        }
    if not declared:
        return {}

    read = subprocess.run(
        COMPOSE + ["exec", "-T", "postgres", "psql", "-q", "-t", "-A", "-F", " ",
                   "-U", "postgres", "-d", database, "-c",
                   "select distinct i.process_name, coalesce(t.to_state, t.from_state) "
                   "from donat.process_transition_logs t "
                   "join donat.process_instances i "
                   "  on i.source_name = t.source_name and i.id = t.instance_id "
                   "where coalesce(t.to_state, t.from_state) is not null"],
        capture_output=True, text=True,
    )
    if read.returncode != 0:
        return {}
    visited: dict[str, set[str]] = {}
    for line in read.stdout.splitlines():
        if " " not in line:
            continue
        process, state = line.rsplit(" ", 1)
        visited.setdefault(process.strip(), set()).add(state.strip())

    # Only processes the scenarios actually entered. A store the size of Petshop
    # has dozens of flows a task never touches, and listing every unvisited
    # state of every one of them buries the number that matters under noise.
    coverage = {}
    for process, states in declared.items():
        seen = visited.get(process, set()) & states
        if seen:
            coverage[process] = {
                "states": f"{len(seen)}/{len(states)}",
                "unvisited": sorted(states - seen),
            }
    return coverage


def verify_histories(engine: pathlib.Path, database: str, env: dict) -> dict:
    """Every instance the scenarios created must have a coherent history.

    Free, and independent of whatever design the candidate chose: the engine's
    own read-only check, run over each instance the run produced.
    """

    checked, broken = 0, []
    for source, instance in instances(database):
        checked += 1
        done = subprocess.run(
            [str(engine), "process", "verify-history", "--source", source,
             "--instance", instance],
            env=env, capture_output=True, text=True,
        )
        if done.returncode != 0:
            broken.append(instance)
    return {"checked": checked, "broken": broken}


# -- reporting ---------------------------------------------------------------


def print_report(report: dict) -> None:
    gates = "  ".join(f"{gate}:{report['gates'][gate]}" for gate in GATES)
    print(f"\n{report['candidate']}  [{gates}]")
    if "failure" in report:
        # Enough of the gate's own words to act on. A one-line summary of a
        # validation failure is exactly the diagnostic that tells you nothing.
        detail = (report["failure"] or "").splitlines()
        print("  stopped:")
        for line in detail[:8]:
            print(f"    {line}")
        if len(detail) > 8:
            print(f"    … {len(detail) - 8} more lines")
    for name, verdict in sorted(report.get("scenarios", {}).items()):
        mark = {"pass": "ok  ", "fail": "FAIL", "skip": "skip"}[verdict]
        print(f"  {mark}  {name}")
    broken = report.get("regressions")
    if broken:
        print(f"  BROKE THE STORE: {len(broken)} test(s) that pass on the fixture:")
        for node in broken[:10]:
            print(f"    {node}")
        if len(broken) > 10:
            print(f"    … {len(broken) - 10} more")
    elif report.get("regression"):
        kept = sum(1 for v in report["regression"].values() if v == "pass")
        print(f"  regression: {kept} of the store's own tests still pass")
    missing = report.get("unreached")
    if missing:
        print(f"  {len(missing)} guarded test(s) never ran this time — the guard "
              f"covered less than its baseline claims:")
        for node in missing[:5]:
            print(f"    {node}")
    for process, seen in (report.get("coverage") or {}).items():
        print(f"  graph: {process} {seen['states']} states reached"
              + (f", unvisited: {', '.join(seen['unvisited'])}" if seen["unvisited"] else ""))
    history = report.get("history")
    if history and history["checked"]:
        state = "ok" if not history["broken"] else f"BROKEN {history['broken']}"
        print(f"  history: {history['checked']} instances, {state}")


def killers_of(expectation: dict) -> list[str]:
    """The scenarios an anti-oracle must trip. One name or a list of them."""

    caught = (expectation or {}).get("caught_by")
    if caught is None:
        return []
    return [caught] if isinstance(caught, str) else list(caught)


def check_declarations(task: dict) -> list[str]:
    """A field the harness never reads is documentation pretending to be a
    contract. These are the checks that keep `task.yaml` honest."""

    problems = []
    scenarios = "\n".join(path.read_text() for path in
                          sorted((task["dir"] / task["scenarios"]).glob("test_*.py")))
    for world in task.get("worlds", []):
        if f"def {world}(" not in scenarios:
            problems.append(f"{task['id']}: world `{world}` is declared but no scenario "
                            f"defines it")
    for name, expectation in task.get("anti", {}).items():
        world = expectation.get("world")
        if world and world not in task.get("worlds", []):
            problems.append(f"{task['id']}/{name}: dies in `{world}`, which the task "
                            f"does not declare")
        killers = killers_of(expectation)
        # Two, because one is a single point of failure for the whole task: a
        # scenario quietly weakened by a fixture change takes the task's ability
        # to tell right from wrong with it, and nothing goes red to say so.
        if len(killers) < 2:
            problems.append(f"{task['id']}/{name}: only {len(killers)} scenario(s) kill it — "
                            f"a task whose discrimination rests on one assertion stops "
                            f"discriminating the moment that assertion drifts")
        for killer in killers:
            if f"def {killer}(" not in scenarios:
                problems.append(f"{task['id']}/{name}: is supposed to trip "
                                f"`{killer}`, which no scenario defines")
    # A file the oracle overlays wholesale, that the fixture only *edited*, is a
    # copy that rots. Both oracles ship the whole of `flows.yaml` to put back one
    # `!include` line; the day Petshop gains a flow, that stale copy silently
    # drops it from every oracle and anti-oracle stand, and the only symptom is a
    # shrinking baseline nobody reads as a cause.
    base = REPO / task["fixture"]["base"]
    edited = {entry.split("#", 1)[0] for entry in task["fixture"].get("remove", [])
              if "#" in entry}
    overlay = task["dir"] / task["oracle"]
    for relative in sorted(edited):
        shipped, original = overlay / relative, base / relative
        if not shipped.exists() or not original.exists():
            continue
        if shipped.read_text() != original.read_text():
            problems.append(
                f"{task['id']}: oracle/{relative} has drifted from "
                f"{task['fixture']['base']}/{relative} — the overlay is a whole-file "
                f"copy of something the fixture only edits, so it now hides changes "
                f"made to the store")
    if not task.get("canary"):
        problems.append(f"{task['id']}: no canary — a task that leaks into a training "
                        f"set should be detectable afterwards")
    return problems


def check_stability(task: dict, runs: int) -> list[str]:
    """The same oracle, several times, and every scenario must agree with itself.

    The one remaining failure mode that would corrupt every number above it: a
    scenario that is load-sensitive rather than wrong looks exactly like a
    candidate that got it wrong, and it does so at random. Petshop already has
    tests like that — the concurrency ones pass alone and fail under parallel
    load — so a task built without checking is one unlucky scheduling away from
    silently marking correct answers wrong.
    """

    seen: dict[str, set[str]] = {}
    for attempt in range(runs):
        report = run_candidate(task, "oracle")
        print_report(report)
        if not report["scenarios"]:
            return [f"{task['id']}: the oracle did not reach the scenarios on run "
                    f"{attempt + 1} ({report.get('failure', 'unknown')[:160]})"]
        for name, verdict in report["scenarios"].items():
            seen.setdefault(name, set()).add(verdict)
    unstable = {name: sorted(verdicts) for name, verdicts in seen.items()
                if len(verdicts) > 1}
    return [f"{task['id']}/{name}: gave {verdicts} over {runs} identical runs — "
            f"a scenario that disagrees with itself scores noise"
            for name, verdicts in unstable.items()]


def check_declarations_against_prompt(task: dict) -> list[str]:
    """Names a scenario reads that the brief never mentions.

    Underspecification is the most common defect in benchmark tasks, and it is
    invisible from inside: the author knows what they meant. This is the cheap
    mechanical half — every status literal and provider path a scenario asserts
    on, looked for in the prompt. It cannot judge whether a *rule* was stated,
    only whether a word the scenarios depend on appears at all, so it warns
    rather than fails.

    Task 001 was checking two things it never asked for when this was written.
    """

    prompt = (task["dir"] / task["prompt"]).read_text().lower()
    scenarios = "\n".join(
        path.read_text() for path in (task["dir"] / task["scenarios"]).rglob("*.py"))
    # Only the state literals a scenario demands the store reach — `{"cancelled"}`
    # and friends. Column names were tried too and were pure noise: a brief says
    # "the order reads cancelled", not `order_status`, and the column is
    # discoverable from the schema anyway. A check with false positives gets
    # ignored, and an ignored check is worse than none.
    wanted = set(re.findall(r'{"([a-z_]+)"}', scenarios))
    missing = sorted(word for word in wanted
                     if len(word) > 3 and word.replace("_", " ") not in prompt
                     and word not in prompt)
    return [f"{task['id']}: scenarios read `{word}` and the prompt never says it"
            for word in missing]


def verify_oracles(names: list[str], only_anti: str | None = None,
                   stability: int = 0) -> int:
    """The task's own honesty check: does it distinguish right from plausible?

    Runs with no agent, no network and no API key. The oracle must score every
    scenario; each anti-oracle must reach the scenarios and die on the exact
    assertion it was written to trip.
    """

    problems = []
    for name in names:
        task = load_task(name)
        print(f"== {task['id']}")
        problems.extend(check_declarations(task))
        # A warning, not a problem: it reads words, not meaning, so it can only
        # say a scenario depends on a term the brief never uses. That is enough
        # to have caught both of task 001's unstated requirements.
        for warning in check_declarations_against_prompt(task):
            print(f"  warning: {warning}")

        if stability:
            problems.extend(check_stability(task, stability))
            continue

        if only_anti:
            expectation = task.get("anti", {}).get(only_anti)
            if expectation is None:
                print(f"  {task['id']} declares no anti-oracle {only_anti!r}")
                continue
            wanted = killers_of(expectation)
            report = run_candidate(task, f"anti/{only_anti}", select=" or ".join(wanted))
            print_report(report)
            if not report["scenarios"]:
                problems.append(
                    f"{task['id']}/{only_anti}: never reached the scenarios "
                    f"({report.get('failure', 'no verdicts')}) — that is a broken "
                    f"patch, not a weak suite")
                continue
            survived = [n for n in wanted if report["scenarios"].get(n) != "fail"]
            if survived:
                problems.append(f"{task['id']}/{only_anti}: survived {', '.join(survived)}")
            continue

        # The null candidate: the fixture with nothing added. A task that this
        # satisfies is asking for nothing. It is also where PASS_TO_PASS comes
        # from — whatever the store can still do with the piece missing is what
        # a candidate must not break — so this run carries the whole suite.
        # If the fixture deploys, its suite reading is what makes the baseline
        # exact. Most fixtures do not — remove a flow and the commands that
        # start it stop validating — so this is opportunistic, not required.
        nothing = run_candidate(task, "fixture", regression="always")
        print_report(nothing)
        if all(nothing["gates"][gate] == "pass" for gate in GATES) and nothing[
            "scenarios"
        ] and all(verdict == "pass" for verdict in nothing["scenarios"].values()):
            problems.append(f"{task['id']}: doing nothing already passes it")

        # The oracle carries the whole Petshop suite: a working store is the
        # only baseline available, because the fixture does not deploy.
        oracle = run_candidate(task, "oracle", regression="always")
        print_report(oracle)
        if oracle.get("regression"):
            write_baseline(task, oracle["regression"], nothing.get("regression"))
        if any(v != "pass" for v in oracle["scenarios"].values()) or not oracle["scenarios"]:
            problems.append(f"{task['id']}: the oracle does not pass its own scenarios")

        caught = 0
        declared = task.get("anti", {})
        for anti, expectation in declared.items():
            report = run_candidate(task, f"anti/{anti}")
            print_report(report)
            expected = killers_of(expectation)
            verdicts = report["scenarios"]
            survived = [name for name in expected if verdicts.get(name) != "fail"]
            if not verdicts:
                problems.append(
                    f"{task['id']}/{anti}: never reached the scenarios "
                    f"({report.get('failure', 'unknown')[:200]}) — it measures the gate, not the suite"
                )
            elif survived:
                problems.append(
                    f"{task['id']}/{anti}: survived {', '.join(survived)} "
                    f"(saw {[verdicts.get(n, 'no such scenario') for n in survived]}) — "
                    f"the suite is weaker than the task claims"
                )
            else:
                caught += 1
        if declared:
            print(f"\n{task['id']}: business-case detection power "
                  f"{caught}/{len(declared)}")

    if problems:
        print("\nthe corpus is not honest yet:")
        for problem in problems:
            print(f"  - {problem}")
        return 1
    print("\nevery oracle passes and every anti-oracle dies where it should")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    one = sub.add_parser("run", help="run one candidate against one task")
    one.add_argument("task")
    one.add_argument("candidate")
    one.add_argument("-k", "--only", default=None,
                     help="a pytest -k expression: run only some scenarios")
    one.add_argument("--keep", action="store_true",
                     help="leave the composed metadata in evals/.state for inspection")
    every = sub.add_parser("verify-oracles", help="check that the tasks measure anything")
    every.add_argument("tasks", nargs="*")
    every.add_argument("--stability", type=int, default=0, metavar="K",
                       help="run the oracle K times; every scenario must agree with itself")
    every.add_argument("--anti", default=None,
                       help="check one anti-oracle only, and skip the null candidate")
    args = parser.parse_args()

    STATE.mkdir(parents=True, exist_ok=True)
    if args.command == "run":
        report = run_candidate(load_task(args.task), args.candidate,
                               keep=args.keep, select=args.only)
        print_report(report)
        # `score_one` treats a skip as wrong — "an unrun test is not a passing
        # test" — and this exit code has to agree with it. A provider fixture
        # that skips the whole file would otherwise report success for a
        # candidate nothing tested.
        failed = any(v != "pass" for v in report["scenarios"].values())
        return 1 if ("failure" in report or failed) else 0

    # `tasks_in` filters on a readable task.yaml, so a stray directory — .state,
    # an editor backup, a work in progress — no longer crashes the whole run in
    # `load_task`.
    names = args.tasks or tasks_in("dev") + tasks_in("holdout")
    return verify_oracles(names, only_anti=args.anti, stability=args.stability)


if __name__ == "__main__":
    sys.exit(main())
