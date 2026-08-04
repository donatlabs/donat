"""How much of the store's flows the suite actually walked.

Not a test — a measurement, and the only one here that reads the database. It
compares the states each Petshop flow declares against the states the stands'
Process journals record, so "the suite is green" can be followed by "and it
went down these branches".

    tests-system/stack.sh up && tests-system/stack.sh up-fast
    cd tests-system && .venv/bin/python -m pytest
    .venv/bin/python coverage.py

Both stands count: the deadline branches only ever run on the fast one. Run it
after a full suite, on stands the suite has just used — it reads what is in the
journals, not what could be there.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).resolve().parent
FLOWS = HERE.parent / "examples" / "petshop" / "metadata" / "flows"

#: One database per stand, both in the compose Postgres.
DATABASES = ("petshop_system", "petshop_fast")

#: Every state a flow reached, from its transitions and from where its
#: instances currently stand.
VISITED = """
select distinct i.process_name, t.state
  from (select instance_id, from_state as state from donat.process_transition_logs
        union all
        select instance_id, to_state from donat.process_transition_logs) t
  join donat.process_instances i on i.id = t.instance_id
 where t.state is not null
union
select process_name, current_state from donat.process_instances
"""

#: `name:` at the top level, and the `- id:` of each declared state. Read with
#: two regexes rather than a YAML parser: this script is meant to run from the
#: suite's own virtualenv, which carries no YAML dependency.
FLOW_NAME = re.compile(r"(?m)^name:\s*(\S+)\s*$")
STATE_ID = re.compile(r"(?m)^  - id:\s*(\S+)\s*$")


def visited() -> set[tuple[str, str]]:
    seen: set[tuple[str, str]] = set()
    for database in DATABASES:
        result = subprocess.run(
            [
                "docker", "compose", "-f", str(HERE / "docker-compose.yml"),
                "exec", "-T", "postgres",
                "psql", "-U", "postgres", "-d", database, "-At", "-F", "\t", "-c", VISITED,
            ],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            print(f"  (no journal in {database}: {result.stderr.strip().splitlines()[-1:]})")
            continue
        for line in result.stdout.splitlines():
            if "\t" in line:
                process, state = line.split("\t", 1)
                seen.add((process, state))
    return seen


def main() -> int:
    seen = visited()
    total = covered = 0
    missing: dict[str, list[str]] = {}
    for path in sorted(FLOWS.glob("*.yaml")):
        text = path.read_text()
        name = FLOW_NAME.search(text)
        if not name:
            print(f"  (no flow name in {path.name})")
            continue
        for state in STATE_ID.findall(text):
            total += 1
            if (name.group(1), state) in seen:
                covered += 1
            else:
                missing.setdefault(name.group(1), []).append(state)

    print(f"{covered}/{total} states ({covered * 100 // total}%)")
    for flow in sorted(missing):
        print(f"  {flow}: {', '.join(missing[flow])}")
    # Four defensive defaults are unreachable by construction; see README.
    return 0


if __name__ == "__main__":
    sys.exit(main())
