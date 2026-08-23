#!/usr/bin/env bash
# Run one loop job, unattended, in a worktree of its own.
#
# A job is a skill under .claude/skills/<job>/ that knows it is running
# unattended and ends its output with one machine-readable line (see
# fix-advisories). This script gives it what it needs and nothing else: a
# fresh worktree cut from origin/main, a time limit, a turn limit, a log, and
# one line in the run journal. The job's only way to leave a mark on the
# repository is a pull request; the worktree is removed when it ends, whatever
# happened.
#
#   scripts/loop.sh <job>
#
# State lives outside the repository, in $DONAT_LOOP_STATE
# (~/.local/state/donat-loops): the journal runs.jsonl, one log per run, the
# worktrees while they run, and a Cargo target directory shared across nights
# so a build is incremental rather than cold. Nothing here is committed.
set -euo pipefail

job="${1:?usage: scripts/loop.sh <job>}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
state="${DONAT_LOOP_STATE:-$HOME/.local/state/donat-loops}"
base="${DONAT_LOOP_BASE:-origin/main}"
timeout_s="${DONAT_LOOP_TIMEOUT:-2700}"
max_turns="${DONAT_LOOP_MAX_TURNS:-60}"
mkdir -p "$state/worktrees" "$state/logs"

# One loop at a time: two jobs rebuilding the same target directory at once
# would only make both slower, and two pull requests for one advisory is the
# failure this exists to prevent.
exec 9>"$state/.lock"
if ! flock -n 9; then
  echo "loop: another job is running (lock: $state/.lock)" >&2
  exit 0
fi

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
branch="loop/$job-$stamp"
worktree="$state/worktrees/$job-$stamp"
log="$state/logs/$stamp-$job.log"
result="$state/logs/$stamp-$job.json"

cleanup() {
  # The worktree goes whatever happened; the branch stays only if it was
  # pushed, because then a pull request refers to it.
  git -C "$repo" worktree remove --force "$worktree" 2>/dev/null || true
  if ! git -C "$repo" ls-remote --exit-code --heads origin "$branch" >/dev/null 2>&1; then
    git -C "$repo" branch -D "$branch" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if [ "$base" = "origin/main" ]; then
  git -C "$repo" fetch -q origin main
fi
git -C "$repo" worktree add -q -b "$branch" "$worktree" "$base"
echo "loop: $job on $branch ($base) — log $log"

started=$(date +%s)
status=0
run_agent=1

# A job may carry a cheap precheck that decides whether the model is worth
# waking. `.claude/skills/<job>/precheck.sh` runs in the worktree with the same
# PATH and target directory the agent would have; its exit code is the whole
# contract: 0 means "nothing to do, do not spend a model on it", 10 means
# "there is work, run the agent", anything else is an error the run stops on.
# For fix-advisories this is `cargo audit` (free, seconds) instead of a
# half-dollar of model to be told the tree is clean. A job with no precheck
# always runs the agent — the earlier, simpler behaviour.
precheck="$repo/.claude/skills/$job/precheck.sh"
if [ -f "$precheck" ]; then
  echo "loop: $job precheck…"
  pc=0
  ( cd "$worktree"; export CARGO_TARGET_DIR="$state/target"; bash "$precheck" ) >>"$log" 2>&1 || pc=$?
  case "$pc" in
    0)  run_agent=0; printf '{"result":"NOTHING TO DO (precheck)","num_turns":0,"total_cost_usd":0}' >"$result" ;;
    10) : ;;  # work exists — fall through to the agent
    *)  run_agent=0; status=$pc
        printf '{"result":"NEEDS HUMAN: precheck exit %s (see log)","num_turns":0,"total_cost_usd":0}' "$pc" >"$result" ;;
  esac
fi

if [ "$run_agent" -eq 1 ]; then
  (
    cd "$worktree"
    export CARGO_TARGET_DIR="$state/target"
    timeout --signal=TERM --kill-after=60 "$timeout_s" \
      claude -p "/$job" --max-turns "$max_turns" --output-format json
  ) >"$result" 2>"$log" || status=$?
fi
seconds=$(( $(date +%s) - started ))

# The job's verdict is its last line; cost and turns come from the JSON
# envelope when there is one. A killed or crashed run has neither, and is
# recorded as such rather than skipped — silence in the journal would read as
# "nothing happened".
python3 - "$job" "$branch" "$stamp" "$seconds" "$status" "$result" "$log" "$state/runs.jsonl" <<'EOF'
import json, sys, re
job, branch, stamp, seconds, status, result_path, log_path, journal = sys.argv[1:]
entry = {"ts": stamp, "job": job, "branch": branch, "seconds": int(seconds), "exit": int(status)}
try:
    data = json.load(open(result_path))
    text = data.get("result") or ""
    entry["turns"] = data.get("num_turns")
    entry["cost_usd"] = data.get("total_cost_usd")
    entry["session"] = data.get("session_id")
except Exception:
    text = ""
last = next((l.strip() for l in reversed(text.splitlines()) if l.strip()), "")
verdict = re.match(r"(NOTHING TO DO|PR OPENED|NEEDS HUMAN|ALREADY OPEN)\b", last)
entry["outcome"] = verdict.group(1) if verdict else ("timeout" if int(status) == 124 else "unknown")
entry["last_line"] = last[:300]
url = re.search(r"https://\S+", last)
if url and verdict and verdict.group(1) in ("PR OPENED", "ALREADY OPEN"):
    entry["pr"] = url.group(0)
entry["log"] = log_path
with open(journal, "a") as f:
    f.write(json.dumps(entry, ensure_ascii=False) + "\n")
print(f"loop: {job} → {entry['outcome']} in {seconds}s" + (f" — {entry['pr']}" if "pr" in entry else ""))
if entry["outcome"] in ("unknown", "timeout"):
    print(f"loop: see {log_path}", file=sys.stderr)
    sys.exit(1)
EOF
