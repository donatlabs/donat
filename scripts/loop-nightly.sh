#!/usr/bin/env bash
# What the nightly timer runs: every loop job, in order, each in its own
# worktree. A job that fails does not stop the next one; the exit code is the
# count of jobs that did not end with a verdict.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Add a job here when its skill exists under .claude/skills/<job>/ and has
# been run by hand at least once (`make loop JOB=<job>`).
JOBS=(fix-advisories)

failed=0
for job in "${JOBS[@]}"; do
  "$here/loop.sh" "$job" || failed=$((failed + 1))
done
exit "$failed"
