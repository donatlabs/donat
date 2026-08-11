#!/usr/bin/env python3
"""Run the mutant corpus against the store's own black-box suite, in parallel.

    python3 evals/sweep.py --workers 8              # the whole corpus
    python3 evals/sweep.py --limit 24               # a spread across operators
    python3 evals/sweep.py --only drop-assert       # one operator

Each mutant lands in one of four places:

  compiler   `validate` or the Process deploy refused it — the invariant is
             enforced statically, which is the best answer donat can give
  tests      it deployed and the black-box suite noticed
  survived   it deployed, the tests that own its behaviour all ran, all passed,
             and the store is wrong — a hole, and a ready-made anti-oracle
  uncovered  nothing owns its behaviour on this stand; not a survivor, because
             an unrun test is not a passing test

Survivors are re-checked against the **whole** suite before they are believed:
the per-mutant subset is what makes 200 stands affordable, and it is also the
one thing that could invent a survivor that is not real.

Every worker owns a whole stand — its own database, its own engine port, its
own mock providers — because a scripted provider answer claimed by another
worker's durable work is indistinguishable from a store misbehaving.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import queue
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import run as harness  # noqa: E402

MUTANTS = harness.EVALS / "mutants"
RESULTS = harness.EVALS / ".state" / "sweep"
PETSHOP = harness.REPO / "examples" / "petshop"
TEMPLATE_DB = "eval_template"

#: What a survivor is re-checked against: the whole suite minus the modules
#: this stack cannot serve. File attachments need an object store the eval
#: compose deliberately does not run, and those tests would fail for reasons
#: that have nothing to do with the mutant — turning a real survivor into a
#: false "the tests caught it".
#: One list, defined in the harness. See its comment for why not two.
UNSERVICEABLE = harness.UNSERVICEABLE


def whole_suite() -> list[str]:
    return sorted(
        path.stem for path in (harness.REPO / "tests-system" / "tests").glob("test_*.py")
        if path.stem not in UNSERVICEABLE
    )

_print_lock = threading.Lock()


def say(message: str) -> None:
    with _print_lock:
        print(message, flush=True)


# -- the shared database template --------------------------------------------


def psql(*arguments: str, database: str = "postgres", stdin=None):
    return subprocess.run(
        harness.COMPOSE + ["exec", "-T", "postgres", "psql", "-v", "ON_ERROR_STOP=1",
                           "-q", "-U", "postgres", "-d", database, *arguments],
        capture_output=stdin is None, text=stdin is None, stdin=stdin, check=True,
    )


def build_template(engine: pathlib.Path) -> None:
    """Migrate and stock one database, then clone it per mutant.

    Every mutant shares the same DDL and the same opening stock; only the
    Process revisions differ. Doing the migration 200 times would be most of
    the sweep's wall clock.
    """

    say("==> building the template database")
    psql("-c", f'DROP DATABASE IF EXISTS "{TEMPLATE_DB}" WITH (FORCE)')
    psql("-c", f'CREATE DATABASE "{TEMPLATE_DB}"')
    url = harness.PG_HOST_URL.rsplit("/", 1)[0] + "/" + TEMPLATE_DB
    env = harness.engine_env(url)
    for migrations in (harness.REPO / "migrations", PETSHOP / "migrations"):
        subprocess.run([str(engine), "migrate", "--migrations-dir", str(migrations)],
                       env=env, check=True, capture_output=True)
    with open(harness.REPO / "tests-system" / "provision.sql", "rb") as sql:
        psql(database=TEMPLATE_DB, stdin=sql)


def clone_template(name: str) -> str:
    psql("-c", f'DROP DATABASE IF EXISTS "{name}" WITH (FORCE)')
    psql("-c", f'CREATE DATABASE "{name}" TEMPLATE "{TEMPLATE_DB}"')
    return harness.PG_HOST_URL.rsplit("/", 1)[0] + "/" + name


# -- one worker's stand ------------------------------------------------------


def providers_answer(url: str) -> bool:
    """Whether *something* is serving on the mock-providers port.

    Used twice and for opposite reasons: to refuse a port somebody else holds,
    and to wait for our own to come up. Both readings are only sound because
    the caller also checks that its own child is still alive.
    """

    try:
        urllib.request.urlopen(f"{url}/", timeout=1)
        return True
    except Exception:
        return False


class Stand:
    """An engine and a set of mock providers on ports nobody else uses."""

    def __init__(self, index: int, engine: pathlib.Path):
        self.index = index
        self.engine = engine
        self.port = 8200 + index
        self.providers_port = 8300 + index
        self.base_url = f"http://127.0.0.1:{self.port}"
        self.providers_url = f"http://127.0.0.1:{self.providers_port}"
        self.place = RESULTS / f"worker-{index}"
        self.place.mkdir(parents=True, exist_ok=True)
        self.providers = None

    def refuse_if_taken(self) -> None:
        """Never test an engine this worker did not start.

        An aborted sweep left four engines holding these ports for hours; the
        next control run happily drove one of them, found its database gone,
        and reported 143 red tests. A stand that answers is only ours if we
        started it — the Petshop stack script has always known this.
        """

        if harness.store_answers(self.base_url):
            raise RuntimeError(
                f"something already answers at {self.base_url} — a stale engine from "
                f"an earlier run? Stop it before sweeping.")
        # The providers port needs the same guard, and for a worse reason. If a
        # stale mock holds it, our Popen fails to bind and dies, the readiness
        # probe below succeeds against the *foreign* process, and the worker
        # runs happily against a provider whose scripted failures it does not
        # own — while `self.providers` points at a dead pid, so stop() never
        # reclaims the real one.
        if providers_answer(self.providers_url):
            raise RuntimeError(
                f"something already answers at {self.providers_url} — stale mock "
                f"providers from an earlier run? Stop them before sweeping.")

    def start_providers(self) -> None:
        environment = harness.child_env({
            "PETSHOP_PROVIDERS_PORT": str(self.providers_port),
            "PETSHOP_PROVIDERS_CONTROL": "1",
        })
        self.providers = subprocess.Popen(
            [sys.executable, str(PETSHOP / "mock-providers" / "providers.py")],
            env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        for _ in range(30):
            if self.providers.poll() is not None:
                raise RuntimeError(
                    f"worker {self.index}: mock providers exited during start-up "
                    f"(exit {self.providers.returncode}) — port {self.providers_port} taken?")
            if providers_answer(self.providers_url):
                return
            time.sleep(0.5)
        raise RuntimeError(f"worker {self.index}: mock providers never answered")

    def stop(self) -> None:
        for child in (self.providers,):
            if child is not None and child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    child.kill()


def compose_mutant(mutant: dict, destination: pathlib.Path) -> None:
    """Petshop plus one defect. Retried once: the first sweep lost six mutants
    to a full disk, and a transient copy failure is not a verdict."""

    last = ""
    for attempt in (1, 2):
        if destination.exists():
            shutil.rmtree(destination)
        copied = subprocess.run(
            [sys.executable, str(harness.REPO / "tests-system" / "fast_metadata.py"),
             str(PETSHOP / "metadata"), str(destination), "--rehost"],
            capture_output=True, text=True,
        )
        if copied.returncode == 0:
            break
        last = (copied.stderr or copied.stdout).strip()[-300:]
        time.sleep(2 * attempt)
    else:
        raise harness.Failed("compose", f"could not lay out the metadata: {last}")

    patch = MUTANTS / mutant["id"] / "defect.patch"
    applied = subprocess.run(["patch", "-p0", "-s", "-i", str(patch)],
                             cwd=destination, capture_output=True, text=True)
    if applied.returncode != 0:
        raise harness.Failed("compose", f"patch did not apply: {applied.stderr.strip()}")


def serve(stand: Stand, metadata: pathlib.Path, env: dict, log: pathlib.Path):
    serving = dict(env)
    serving.update({
        "DONAT_PORT": str(stand.port),
        "DONAT_METADATA_DIR": str(metadata),
        "DONAT_GRAPHQL_ADMIN_SECRET": harness.ADMIN_SECRET,
        "DONAT_GRAPHQL_UNAUTHORIZED_ROLE": "anonymous",
        "DONAT_GRAPHQL_JWT_SECRET": json.dumps({"type": "HS256", "key": harness.JWT_KEY}),
        "RUST_LOG": "donat=warn",
    })
    with open(log, "wb") as sink:
        process = subprocess.Popen([str(stand.engine), "--metadata-dir", str(metadata)],
                                   env=serving, stdout=sink, stderr=sink)
    for _ in range(60):
        if process.poll() is not None:
            raise harness.Failed("serve", f"the engine exited during start-up; see {log}")
        if harness.store_answers(stand.base_url):
            return process
        time.sleep(1)
    process.terminate()
    raise harness.Failed("serve", f"the engine never answered; see {log}")


def run_suite(stand: Stand, modules: list[str] | str, *, stop_early: bool) -> dict:
    """Run the store's own black-box tests against this stand."""

    venv = harness.REPO / "tests-system" / ".venv" / "bin" / "python"
    python = str(venv) if venv.exists() else sys.executable
    if isinstance(modules, str):
        targets = [modules]
    else:
        targets = [f"tests/{module}.py" for module in modules]
    report = stand.place / "suite.xml"
    report.unlink(missing_ok=True)
    environment = harness.child_env({
        "PETSHOP_BASE_URL": stand.base_url,
        "PETSHOP_PROVIDERS_URL": stand.providers_url,
        "PETSHOP_JWT_KEY": harness.JWT_KEY,
    })
    command = [python, "-m", "pytest", *targets, "-p", "no:cacheprovider", "-q",
               "--tb=no", f"--junit-xml={report}"]
    if stop_early:
        command.append("-x")
    done = subprocess.run(command, cwd=str(harness.REPO / "tests-system"), env=environment,
                          capture_output=True, text=True, timeout=3600)
    (stand.place / "suite.log").write_text(done.stdout + done.stderr)
    if not report.exists() or not report.stat().st_size:
        # pytest died before it could write a report — a collection error, an
        # internal error, a killed process. Silence here used to surface as an
        # XML ParseError with no cause attached.
        tail = (done.stdout + done.stderr).strip().splitlines()[-6:]
        raise harness.Failed("suite", "pytest wrote no report: " + " / ".join(tail))
    return harness.junit_verdicts(report)


def run_suite_by_node(stand: Stand, modules: list[str] | str, *, stop_early: bool) -> dict:
    """The same run, read as `module::test` instead of by bare name.

    Bare names are what `judge` and `caught_by` look up, so the mutant path
    keeps them. The control cannot: 281 tests across 25 modules do reuse names,
    so collisions undercount the total and — worse — a reported red name does
    not say which module to put in UNSERVICEABLE, which is the documented
    remedy.
    """

    run_suite(stand, modules, stop_early=stop_early)
    return harness.junit_node_verdicts(stand.place / "suite.xml")


def judge(verdicts: dict) -> tuple[str, str | None]:
    killed = [name for name, verdict in verdicts.items() if verdict == "fail"]
    if killed:
        return "tests", killed[0]
    if not any(verdict == "pass" for verdict in verdicts.values()):
        return "uncovered", None
    return "survived", None


def examine(stand: Stand, mutant: dict, *, confirm: bool) -> dict:
    started = time.monotonic()
    metadata = stand.place / "metadata"
    result = {"id": mutant["id"], "operator": mutant["operator"], "file": mutant["file"],
              "defect": mutant["defect"], "worker": stand.index}
    engine_process = None
    try:
        compose_mutant(mutant, metadata)
        env = harness.engine_env(clone_template(f"eval_w{stand.index}"))
        env["PETSHOP_PAYMENT_BASE_URL"] = stand.providers_url
        env["PETSHOP_TAX_BASE_URL"] = stand.providers_url
        env["DONAT_MOCK_CARRIER_BASE_URL"] = stand.providers_url
        env["PETSHOP_NOTIFICATION_BASE_URL"] = stand.providers_url
        env["PETSHOP_PAYOUT_BASE_URL"] = stand.providers_url

        harness.run_gate("validate", [str(stand.engine), "validate", "--metadata-dir",
                                      str(metadata), "--source", "default"], env)
        harness.run_gate("deploy", [str(stand.engine), "migrate", "--migrations-dir",
                                    str(harness.REPO / "migrations"), "--metadata-dir",
                                    str(metadata), "--source", "default"], env)
        # After the gates, before the engine. `validate` and `deploy` are real
        # verdicts for an uncovered mutant — a quarter of this corpus never
        # reaches a test at all — but serving a store nothing is going to drive
        # is a whole stand spent to print one word.
        modules = [m for m in mutant["coverage"]]
        if not modules:
            result["outcome"] = "uncovered"
        else:
            engine_process = serve(stand, metadata, env, stand.place / "engine.log")
            verdicts = run_suite(stand, modules, stop_early=True)
            outcome, killer = judge(verdicts)
            result["outcome"], result["killed_by"] = outcome, killer
            result["ran"] = len(verdicts)
            if outcome == "survived" and confirm:
                # The subset is a guess about what owns this behaviour. Before
                # anyone calls it a hole, ask the whole suite.
                verdicts = run_suite(stand, whole_suite(), stop_early=True)
                outcome, killer = judge(verdicts)
                result["outcome"] = outcome
                result["killed_by"] = killer
                result["confirmed_against"] = "whole suite"
                result["ran"] = len(verdicts)
    except harness.Failed as failure:
        # Only a refusal by the engine is a verdict about the mutant. A
        # workspace that could not be laid out, or a pytest that never ran, is
        # the harness having a bad day and must never be counted as detection.
        # `serve` is deliberately absent from this tuple. An engine that will
        # not boot — a lost port, an OOM, a loaded box — used to arrive here as
        # a `deploy` failure and be published as "the invariant is enforced
        # statically". A stand having a bad day is never evidence about a mutant.
        result["outcome"] = "compiler" if failure.phase in ("validate", "deploy") else "error"
        result["refused_at"] = failure.phase
        result["detail"] = failure.detail.splitlines()[-1][:300] if failure.detail else ""
    except Exception as unexpected:  # pragma: no cover - harness trouble, not a verdict
        result["outcome"] = "error"
        result["detail"] = f"{type(unexpected).__name__}: {unexpected}"[:300]
    finally:
        if engine_process is not None and engine_process.poll() is None:
            engine_process.send_signal(signal.SIGTERM)
            try:
                engine_process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                engine_process.kill()
        shutil.rmtree(metadata, ignore_errors=True)
    result["seconds"] = round(time.monotonic() - started, 1)
    return result


# -- the sweep ---------------------------------------------------------------


def control() -> int:
    """Run the pristine store through the identical path.

    Without this, "the tests caught it" is unproven: a test that fails on this
    stack for its own reasons — a missing object store, a flake — kills every
    mutant it touches and reads exactly like detection. Anything red here is
    noise that must be excluded from every verdict, and the first full sweep
    was wrong for precisely this reason.
    """

    engine = harness.engine_binary()
    subprocess.run(harness.COMPOSE + ["up", "-d", "--wait"], check=True, capture_output=True)
    build_template(engine)
    stand = Stand(0, engine)
    metadata = stand.place / "metadata"
    process = None
    # Both inside the try, as in `work()`: a failure between starting the mock
    # providers and entering the block would otherwise leave a process holding
    # port 8300 with no stop() to reap it — which is how the stale providers
    # this function now refuses got there in the first place.
    try:
        stand.refuse_if_taken()
        stand.start_providers()
        if metadata.exists():
            shutil.rmtree(metadata)
        subprocess.run(
            [sys.executable, str(harness.REPO / "tests-system" / "fast_metadata.py"),
             str(PETSHOP / "metadata"), str(metadata), "--rehost"],
            check=True, capture_output=True, text=True)
        env = harness.engine_env(clone_template("eval_control"))
        for name in ("PETSHOP_PAYMENT_BASE_URL", "PETSHOP_TAX_BASE_URL",
                     "DONAT_MOCK_CARRIER_BASE_URL", "PETSHOP_NOTIFICATION_BASE_URL",
                     "PETSHOP_PAYOUT_BASE_URL"):
            env[name] = stand.providers_url
        harness.run_gate("validate", [str(engine), "validate", "--metadata-dir",
                                      str(metadata), "--source", "default"], env)
        harness.run_gate("deploy", [str(engine), "migrate", "--migrations-dir",
                                    str(harness.REPO / "migrations"), "--metadata-dir",
                                    str(metadata), "--source", "default"], env)
        process = serve(stand, metadata, env, stand.place / "engine.log")
        verdicts = run_suite_by_node(stand, whole_suite(), stop_early=False)
    finally:
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGTERM)
            try:
                process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                process.kill()
        stand.stop()

    red = sorted(name for name, verdict in verdicts.items() if verdict == "fail")
    skipped = sum(1 for verdict in verdicts.values() if verdict == "skip")
    (RESULTS / "control.json").write_text(json.dumps(
        {"failing": red, "skipped": skipped, "total": len(verdicts)}, indent=2) + "\n")
    print(f"\ncontrol: {len(verdicts)} tests, {len(red)} failing, {skipped} skipped")
    for name in red:
        print(f"  RED  {name}")
    if red:
        print("\nEvery verdict that leans on these is noise. Add them to "
              "UNSERVICEABLE, or fix the stack, before believing a sweep.")
    # Non-zero on a red control, for the same reason `main` exits non-zero on a
    # sweep that judged nothing: `make evals-control && make evals-sweep` has to
    # stop here rather than read success and die in the sweep's precondition.
    return 1 if red else 0


def load_corpus(limit: int | None, only: str | None) -> list[dict]:
    # The corpus is generated, not checked in, so on a fresh clone this file
    # simply is not there. The friendly message downstream only fires when the
    # catalogue exists and is empty; without this, a first run is a traceback.
    index = MUTANTS / "catalogue.json"
    if not index.exists():
        return []
    catalogue = json.loads(index.read_text())
    if only:
        catalogue = [m for m in catalogue if m["operator"] == only]
    return catalogue[:limit] if limit else catalogue


def require_headroom(gigabytes: float = 4.0) -> None:
    """Refuse to start on a nearly full disk.

    The first full sweep lost six mutants to a box at 99%, and a lost mutant
    reads like a judged one unless somebody checks the error column.
    """

    usage = shutil.disk_usage(harness.REPO)
    free = usage.free / 1e9
    if free < gigabytes:
        sys.exit(f"only {free:.1f} GB free; a sweep needs room for its stands "
                 f"and databases. Free some space first.")
    if free < 2 * gigabytes:
        say(f"warning: {free:.1f} GB free — tight for {gigabytes * 2:.0f} GB of comfort")


def already_done() -> list[dict]:
    """Verdicts an earlier sweep got far enough to write."""

    record = RESULTS / "results.jsonl"
    if not record.exists():
        return []
    out = {}
    for line in record.read_text().splitlines():
        line = line.strip()
        if line:
            try:
                result = json.loads(line)
            except json.JSONDecodeError:
                continue      # a line half-written when the sweep was killed
            out[result["id"]] = result
    return list(out.values())


def sweep(corpus: list[dict], workers: int, confirm: bool) -> list[dict]:
    require_headroom()
    engine = harness.engine_binary()
    subprocess.run(harness.COMPOSE + ["up", "-d", "--wait"], check=True, capture_output=True)
    build_template(engine)

    pending: queue.Queue = queue.Queue()
    for mutant in corpus:
        pending.put(mutant)
    results, done = [], [0]

    def work(index: int) -> None:
        stand = Stand(index, engine)
        try:
            stand.refuse_if_taken()
            stand.start_providers()
        except Exception as problem:
            # Inside the try, because start_providers can raise *after* spawning:
            # the orphan would hold port 83xx and the next sweep would adopt it
            # as its own mock provider.
            stand.stop()
            say(f"worker {index} never came up: {problem}")
            return
        try:
            while True:
                try:
                    mutant = pending.get_nowait()
                except queue.Empty:
                    return
                result = examine(stand, mutant, confirm=confirm)
                with _print_lock:
                    results.append(result)
                    # Written here, not at the end: this sweep was killed at
                    # 136/200 and took an hour of verdicts with it, because the
                    # record was only produced once every worker had finished.
                    with (RESULTS / "results.jsonl").open("a") as sink:
                        sink.write(json.dumps(result) + "\n")
                    done[0] += 1
                    mark = {"compiler": "compiler", "tests": "tests   ",
                            "survived": "SURVIVED", "uncovered": "uncovered",
                            "error": "ERROR   "}[result["outcome"]]
                    print(f"[{done[0]:>3}/{len(corpus)}] {mark}  {result['id']}"
                          f"  ({result['seconds']}s)", flush=True)
        finally:
            stand.stop()

    threads = [threading.Thread(target=work, args=(index,), daemon=True)
               for index in range(workers)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    return results


def summarise(results: list[dict]) -> None:
    from collections import Counter

    counts = Counter(r["outcome"] for r in results)
    total = len(results)
    judged = counts["compiler"] + counts["tests"] + counts["survived"]
    print("\n" + "=" * 72)
    print(f"{total} mutants")
    for outcome in ("compiler", "tests", "survived", "uncovered", "error"):
        if counts[outcome]:
            print(f"  {outcome:<10} {counts[outcome]:>4}")
    if judged:
        caught = counts["compiler"] + counts["tests"]
        print(f"\ncaught {caught}/{judged} of the mutants anything could judge "
              f"({100 * caught / judged:.1f}%)")
        print(f"  of those, {counts['compiler']} never deployed at all")

    print("\nby operator:")
    for operator in sorted({r["operator"] for r in results}):
        row = [r for r in results if r["operator"] == operator]
        breakdown = Counter(r["outcome"] for r in row)
        print(f"  {operator:<24} " + "  ".join(
            f"{outcome}:{breakdown[outcome]}" for outcome in
            ("compiler", "tests", "survived", "uncovered", "error") if breakdown[outcome]))

    survivors = [r for r in results if r["outcome"] == "survived"]
    if survivors:
        print(f"\n{len(survivors)} survivor(s) — each one is a hole in the suite "
              f"and an anti-oracle waiting to be written:")
        for row in survivors:
            print(f"  {row['id']}\n      {row['defect']}  [{row['file']}]")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--workers", type=int,
                        default=min(8, max(1, (os.cpu_count() or 4) - 2)))
    parser.add_argument("--limit", type=int)
    parser.add_argument("--only", help="one operator name")
    parser.add_argument("--resume", action="store_true",
                        help="skip mutants already in results.jsonl")
    parser.add_argument("--no-confirm", action="store_true",
                        help="skip the whole-suite re-check of survivors (faster, less honest)")
    parser.add_argument("--control", action="store_true",
                        help="run the pristine store: anything red here is noise, not detection")
    args = parser.parse_args()

    if args.control:
        RESULTS.mkdir(parents=True, exist_ok=True)
        return control()

    corpus = load_corpus(args.limit, args.only)
    if not corpus:
        sys.exit("no mutants: run `python3 evals/mutants.py generate` first")
    RESULTS.mkdir(parents=True, exist_ok=True)

    # A sweep is only readable against a clean control: a test that is red on
    # the pristine store kills every mutant it touches and reads exactly like
    # detection. This is not advice, it is a precondition.
    record = RESULTS / "control.json"
    if not record.exists():
        sys.exit("no control on file: run `python3 evals/sweep.py --control` first")
    baseline = json.loads(record.read_text())
    if baseline["failing"]:
        sys.exit(f"the control is red on {len(baseline['failing'])} test(s) — "
                 f"fix the stack or exclude them; a sweep now would count noise "
                 f"as detection:\n  " + "\n  ".join(baseline["failing"]))
    if args.resume:
        finished = {result["id"] for result in already_done()}
        before = len(corpus)
        corpus = [mutant for mutant in corpus if mutant["id"] not in finished]
        print(f"resuming: {before - len(corpus)} already on file, {len(corpus)} to go")
    elif (RESULTS / "results.jsonl").exists():
        # A scoped run is a spot check, not a new corpus: keeping the record
        # under a name of its own stops `ONLY=` or `LIMIT=` from throwing away
        # an overnight 200-mutant run and then printing a corpus-shaped table
        # computed from twenty rows.
        if args.only or args.limit:
            record = RESULTS / "results.jsonl"
            kept = RESULTS / "results-full.jsonl"
            if kept.exists():
                # The second scoped run must not overwrite what the first one
                # preserved: that replaces the overnight corpus with twenty rows
                # from a spot check, while still printing "the full record is
                # preserved". Whatever is already set aside is the corpus.
                print(f"scoped run: {kept} already holds a preserved record — "
                      f"leaving it alone and starting this one fresh")
                record.unlink()
            else:
                record.replace(kept)
                print(f"scoped run: the full record is preserved at {kept}")
        else:
            (RESULTS / "results.jsonl").unlink()
    if not corpus:
        summarise(already_done())
        return 0
    print(f"{len(corpus)} mutants, {args.workers} workers "
          f"(control: {baseline['total']} tests, all green)")

    started = time.monotonic()
    results = sweep(corpus, args.workers, confirm=not args.no_confirm)
    if not results:
        # Every worker died on start-up. Exiting 0 here told `make evals-sweep`
        # that a sweep judging nothing had succeeded.
        print(f"no mutant was judged: all {args.workers} worker(s) failed to start")
        return 1
    record = RESULTS / "results.jsonl"
    everything = {result["id"]: result for result in already_done()}
    everything.update({result["id"]: result for result in results})
    with record.open("w") as sink:
        for result in sorted(everything.values(), key=lambda r: r["id"]):
            sink.write(json.dumps(result) + "\n")
    summarise(list(everything.values()))
    print(f"\n{(time.monotonic() - started) / 60:.1f} minutes; record: {record}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
