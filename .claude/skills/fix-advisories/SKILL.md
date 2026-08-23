---
name: fix-advisories
description: Nightly job. Resolve what `cargo audit --deny warnings` reports by upgrading dependencies, verify the build, open one pull request. Never excuses an advisory — that needs a human's reachability argument.
disable-model-invocation: true
allowed-tools: Bash(cargo audit *) Bash(cargo update *) Bash(cargo build *) Bash(cargo tree *) Bash(cargo metadata *) Bash(cargo fetch *) Bash(git status *) Bash(git diff *) Bash(git add *) Bash(git commit *) Bash(git push *) Bash(git log *) Bash(git branch *) Bash(git rev-parse *) Bash(gh pr *) Bash(gh issue *) Bash(make gate *) Bash(python3 scripts/check_audit_excuses.py *) Read Grep Glob Edit
---

# Fix advisories

You are running unattended, in a fresh git worktree on a branch named
`loop/fix-advisories-<stamp>` that was cut from `origin/main`. Nobody is
watching. Everything you do is reviewed later through the pull request you
open, and only through it — so the pull request has to say what you did and
what you verified, exactly, and you must not claim a check you did not run.

The last line you print is read by a machine. It must be one of:

- `NOTHING TO DO` — no advisory, no stale excuse
- `PR OPENED <url>`
- `NEEDS HUMAN: <one line>` — you found something and could not fix it
- `ALREADY OPEN <url>` — a pull request for this is open from an earlier run

## Boundaries (not advice — the loop is judged on these)

- **Never add an entry to `.cargo/audit.toml`.** An excuse needs a written
  reachability argument, and that is a human's sentence. If no upgrade
  exists, stop with `NEEDS HUMAN`. `make gate` would refuse a pull request
  that adds an excuse anyway (`gate:audit-ignore`); do not try to satisfy
  that marker — it is not yours to write.
- **Removing** a stale excuse is allowed: it is what the excuse list's own
  rule asks for.
- Touch only `Cargo.lock`, `Cargo.toml` files, and `.cargo/audit.toml`
  (removals only). No source changes. If an upgrade needs a code change to
  compile, that is `NEEDS HUMAN` with the compiler's first error quoted.
- One pull request per run, and none if one is already open for the same
  advisory (`gh pr list --state open --search "Advisories in:title"`). Do
  not push to any branch but your own. Never `--force`.
- Do not run the test suites: they need databases this worktree does not
  have, and the pull request's CI runs them all. Say so in the pull request.

## Procedure

1. Baseline, both commands, keep their output:
   ```
   cargo audit --deny warnings
   python3 scripts/check_audit_excuses.py --no-fetch
   ```
   Both exit 0 → print `NOTHING TO DO` and stop. Do not open anything.

2. For each advisory `cargo audit` raised: `cargo tree -i <crate>` shows who
   pulls it in. Try, in order, the smallest change that resolves it:
   - `cargo update -p <crate>` (a semver-compatible bump in `Cargo.lock`);
   - if the fixed version is outside the current range and the crate is a
     *direct* dependency, raise the requirement in that one `Cargo.toml`;
   - if it is transitive and the direct dependency that pulls it has a newer
     release that drops the vulnerable version, bump the direct dependency;
   - otherwise `NEEDS HUMAN: <advisory> in <crate>, pulled by <path>, no
     upgrade found` — say what you tried.

3. For each stale excuse the second command listed: delete its line from
   `.cargo/audit.toml`, keeping the file's comment structure intact.

4. Verify, and keep the exact commands and their last lines for the pull
   request:
   ```
   cargo audit --deny warnings
   python3 scripts/check_audit_excuses.py --no-fetch
   cargo build --workspace --all-targets
   make gate GATE_BASE=origin/main
   ```
   `make gate` must print `nothing to declare`. If it asks for any
   `gate:` line, you changed something outside your boundaries: revert it.

5. Commit with a message in the repository's voice (a sentence, not a tag),
   push your branch, open the pull request:
   - title: `Advisories: <crate> <old> → <new>` (comma-separate several);
   - body: what `cargo audit` reported, what changed and why that upgrade,
     the verification commands with their results, one line saying the test
     suites run in CI and were not run here, and `Closes #<n>` if an open
     issue titled `Nightly advisories: …` exists (`gh issue list --state
     open --search "Nightly advisories in:title"`);
   - end the body with: `Opened by the fix-advisories loop; everything
     above was done unattended.`

6. Print `PR OPENED <url>`.

If anything fails in a way this procedure does not cover, do not improvise a
fix: leave the worktree as it is, print `NEEDS HUMAN:` with the failing
command and its last line, and stop. A wrong fix merged at night costs more
than a night lost.
