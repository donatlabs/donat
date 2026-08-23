#!/usr/bin/env bash
# Cheap gate for the fix-advisories loop: is there anything for a model to do?
#
# Run by scripts/loop.sh in the job's worktree before it wakes an agent. The
# contract is the exit code: 0 = nothing to do (skip the agent), 10 = there is
# work (run the agent), anything else = error. `cargo audit` and the excuse
# check are free and take seconds; a model told "the tree is clean" is not.
set -uo pipefail

audit_clean=0
cargo audit --deny warnings || audit_clean=$?

excuses_clean=0
python3 scripts/check_audit_excuses.py --no-fetch || excuses_clean=$?

# cargo-audit and the excuse check both exit 1 when they have something to
# report and 0 when clean; any other code is a broken run, not a verdict.
for code in "$audit_clean" "$excuses_clean"; do
  case "$code" in
    0|1) ;;
    *) echo "precheck: a check exited $code — treating as error, not a verdict" >&2; exit "$code" ;;
  esac
done

if [ "$audit_clean" -eq 0 ] && [ "$excuses_clean" -eq 0 ]; then
  echo "precheck: no advisories, no stale excuses — nothing to do"
  exit 0
fi
echo "precheck: work found — waking the agent"
exit 10
