# Engineering — how the engine is developed

> Not the engine: the process that produces it. How agents and people work in
> this repository, what the repository tells them before they act, and what
> it refuses to accept afterwards. The premise of the domain is that the
> model does not learn between sessions and the repository does — so every
> lesson is only as strong as the rung it lives on.

**Status: started August 2026.** The first decision came out of evaluating
"loop engineering" and "graph engineering" against the actual goal, which is
that agents write better code here over time, and found that neither is about
that.

## Decisions

- [[decisions/001-the-repository-learns-on-the-rungs-that-refuse]] — a review
  finding lands on the lowest rung that can hold it (test → gate → skill →
  ADR); the rules that can be read from a diff are read from it by
  `scripts/check_change_gate.py` (`make gate`), with `gate:<kind> <reason>`
  in the pull request for the five short cuts to a green build; `cargo audit`
  and the excuse list are checked nightly; a red build is classified before it
  is touched; nothing from the loop-engineering toolkit is adopted, and why
- [[decisions/002-an-applications-tests-are-declared-beside-the-thing-they-test]]
  — a `*_test.yaml` sits beside the metadata file it exercises and `donat
  test` runs it on a fresh stand per case; the runner lives in
  `crates/testkit`, shared with the conformance harness; `expect` is a subset
  match where a fixture's `response` stays exact; a table that grants a role
  something has a test beside it (`scripts/check_app_tests.py`).

## Related

- `CLAUDE.md` / `AGENTS.md` — the rules as the agent reads them: the
  feature-completion review, "When CI Is Red", the change gate
- `scripts/check_change_gate.py`, `scripts/check_audit_excuses.py` — the
  mechanical twins
- `.github/workflows/advisories.yml` — the nightly advisory check
- DonatBench (`evals/`) — the paired measurement a skill edit is asked to name
