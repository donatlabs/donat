Add a durable process to this donat application. Arguments: $ARGUMENTS

Read first:
- `~/.codex/donat/skills/donat-processes/SKILL.md`
- `~/.codex/donat/skills/donat-commands/SKILL.md`
- `~/.codex/donat/skills/donat-connectors/SKILL.md`

Then:

1. List every state, every branch and every terminal before writing YAML. A
   branch with no terminal means the flow is not finished being designed.
2. Identify or write the single command that declares the `start_process`
   effect — it is the public entry point. Everything after is a signal.
3. Header: `kind: process`, `version: 1`, `owner.capture` of the caller's
   session, typed `input`/`output`, `idempotency`, and each `signal` with its
   `correlation` and `payload`.
4. States:
   - `command` — `run_as: caller`, or a named worker role with its own explicit
     table permissions. No privileged role exists.
   - `request` — `timeout`, `retry`, and an `on_error` routing **every** error
     class with a real `fallback`.
   - `wait` — `correlate` on a domain key, `verification: required`,
     `persist_before_match: true` where the signalling command can commit
     first, and always a `deadline` with `on_timeout`.
   - `when` — rule cases with a `default` that goes to `fail`.
   - `for_each` — `max_items` and `max_concurrency` are required.
5. For every provider mutation that can time out, add a read-only `lookup_*`
   state and route on found / proven-absent / unproven. A timeout is not "it
   did not happen"; an unproven outcome is a bounded manual reconciliation.
6. Terminals: an `output` per success branch, a `fail` with a stable `code` per
   violated invariant.
7. Register it in `flows.yaml`.
8. Run `donat validate`. Drive the happy path end to end; then script a
   provider stub to fail and show the intended `fail` code is reached; then run
   `donat process inspect --source <src> --instance <uuid>`. Paste real output.
