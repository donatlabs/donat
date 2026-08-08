Add a declarative domain command to this donat application. Arguments: $ARGUMENTS

Read first:
- `~/.codex/donat/skills/donat-commands/SKILL.md`
- `~/.codex/donat/skills/donat-rules/SKILL.md`

Then:

1. Check it should be a command. One unguarded write is a table permission. A
   step that waits on a person, a timer or a provider belongs in a process — a
   command is one synchronous transaction and never calls a connector.
2. Read a neighbouring file in `metadata/commands/<domain>/` and match it.
3. Write the guards as named rules in `rules.yaml` first. Steps carry no inline
   expressions; anything computed is a rule or a `project` step.
4. Write the steps. Re-assert ownership and state in a `select_one`'s `by`, not
   just identity. Prefer a per-row `check:` on the write over a
   read-then-assert pair, which is a race. Bound every batch.
5. Ask what two concurrent callers do. If both can pass the guard legitimately,
   add the unique constraint in a migration **in this same change** and name it
   in a comment on the command.
6. Add `result`, then `idempotency` with a client-supplied key scoped by the
   caller's session or by the domain key. Every state-changing command gets one.
7. Add `effects` only if this command starts or signals a process. Exactly one
   command declares `start_process` per process.
8. Register it in `commands.yaml`.
9. Run `donat validate`. Then call the command; call it again with the same
   idempotency key and show nothing was written twice; then force a guard to
   fail and show every step rolled back. Paste real output.
