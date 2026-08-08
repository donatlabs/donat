---
name: add-process
description: Use when the user runs /donat:add-process, or asks to add a durable process, long-running flow, saga or workflow to a donat application — produces states, connector requests, waits, timers, branching and terminals.
argument-hint: <process name> [the flow it models]
allowed-tools: [Read, Write, Edit, Glob, Grep, Bash]
---

Add the durable process `$1` to this donat application. The flow: $ARGUMENTS

Load `donat-processes`, then `donat-commands` and `donat-connectors`.

1. **Draw the states before writing YAML.** List every state, every branch out
   of it, and every terminal. If a branch has no terminal, the flow is not
   finished being designed.

2. **Entry point.** Exactly one command declares the `start_process` effect and
   is the public entry; everything after is a signal. Write or identify that
   command first.

3. **Header**: `kind: process`, `version: 1`, `owner.capture` of the caller's
   session, typed `input`/`output`, `idempotency`, and each `signal` with its
   `correlation` and `payload`.

4. **States.** For each one:
   - `command` — `run_as: caller` for the owner's own work, or a named worker
     role that has its own explicit table permissions. Never a privileged role;
     there isn't one.
   - `request` — a connector call with `timeout`, `retry` and an `on_error`
     that routes **every** error class plus a real `fallback`.
   - `wait` — `correlate` on a domain key, `verification: required`,
     `persist_before_match: true` where the signalling command can commit
     first, and always a `deadline` with an `on_timeout` state.
   - `when` — rule cases with a `default` that goes to `fail`, not to a happy
     path.
   - `for_each` — `max_items` and `max_concurrency` are required.

5. **Handle ambiguity explicitly.** For every provider mutation that can time
   out, add a read-only `lookup_*` state and route on
   found / proven-absent / unproven. A timeout is not "it did not happen".
   Money and inventory stay claimed until something proves otherwise, and an
   unproven outcome becomes a bounded manual reconciliation — never a silent
   write-off.

6. **Terminals**: an `output` per successful branch, a `fail` with a stable
   `code` per violated invariant.

7. Register it in `flows.yaml`.

8. **Verify**: `donat validate`; drive the happy path end to end; then script a
   provider stub to fail and show the process reaching the intended `fail`
   code; then inspect the journal with
   `donat process inspect --source <src> --instance <uuid>`. Report real output.
