---
name: add-command
description: Use when the user runs /donat:add-command, or asks to add a domain command, transactional operation or guarded multi-write to a donat application — produces the steps, rule guards, idempotency key and any process effect.
argument-hint: <command name> [what it does]
allowed-tools: [Read, Write, Edit, Glob, Grep, Bash]
---

Add the command `$1` to this donat application. What it should do: $ARGUMENTS

Load `donat-commands` and `donat-rules`.

1. **Check it should be a command at all.** One write with no guard is a
   permission on a table, not a command. A step that waits on a person, a timer
   or a provider belongs in a process — a command is one synchronous
   transaction and never calls a connector.

2. **Read a neighbour** in `metadata/commands/<domain>/` and match it.

3. **Write the guards as rules first**, in `rules.yaml`, with names that say
   what they decide (`can_reserve_stock`, `approval_was_rejected`). Steps have
   no inline expressions, so anything computed is a rule or a `project` step.

4. **Steps, in order.** Re-assert ownership and state in a `select_one`'s `by`
   rather than looking up by id alone — that is what makes the step a lock on
   the state machine. Prefer a per-row `check:` on the write over a
   read-then-assert pair, which is a race. Bound every batch with
   `maximum_rows` / `maximum_items`.

5. **Ask what two concurrent callers do.** If both can pass the guard
   legitimately, the database must be the arbiter — add the unique constraint
   in a migration **in this same change**, and say in the command's comment
   which constraint it relies on.

6. **`result`** projecting what the caller and any consuming process need.

7. **`idempotency`** with a client-supplied key and a scope that is either the
   caller's session or the domain key. Every state-changing command gets one.

8. **`effects`** only if this command starts or signals a process. Exactly one
   command declares `start_process` for a given process.

9. Register it in `commands.yaml`.

10. **Verify**: `donat validate`, then call it; then call it again with the same
    idempotency key and show that nothing was written twice; then make a guard
    fail and show that every step rolled back. Report real output.
