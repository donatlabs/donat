---
type: decision
status: accepted
date: 2026-08-04
features:
  - "[[embedded-sdk]]"
  - "[[003-declarative-domain-commands]]"
  - "[[004-wasm-core-host-split]]"
---

# The embedded core compiles its own snapshot, and a command's hooks come from its steps

## Context

The wasm core built its planner with `Planner::new`, which hardcodes
`commands: None`. A command declared in YAML was therefore not a field in the
schema the core compiled, and an embedded Go host answered a request for one as
an unknown field. Commands are reachable only through the multi-source planner,
which is what `donat-server` uses; the constructor that carries a command
catalog was `pub(crate)`, and the rules adapter that feeds it lived in
`crates/server`, which cannot reach `wasm32`.

Post-commit hooks had a second, quieter version of the same gap. `hooks_for_root`
resolved one `(schema, table, op)` per mutation root and returned nothing for a
command, on the reasoning that a command carries no single table for trigger
metadata to match. An embedded host's event handlers therefore never ran for any
row a command wrote — silently, because a plan with no hooks is indistinguishable
from a plan whose hooks all declined.

## Decision

The core compiles a full serving snapshot at `core_init`, in the same order the
server does: rules, then commands, then the schema. The rules adapter moved to
`donat-schema` so both compile the same catalog from the same code — a rule that
compiled differently in wasm than in the engine would make the two disagree
about what a command may do, which is the one thing this whole split exists to
prevent. Compiling per request would also repay the cost on every call and, worse,
let a deployment serve traffic before discovering its metadata does not compile.

A command's hooks come from its **resolved steps**, not from its root. The
premise that a command has no table was simply wrong: it has as many as its
steps write, and the execution IR names each one. Reading the resolved IR rather
than the metadata declaration means a step the planner dropped cannot leave
behind a hook that fires for a write which did not happen.

Process effects are an empty contract catalog. A durable Process needs a journal
and a transition queue, which live host-side and have no counterpart here, so
`finalize_command_effects` refuses a command whose effect targets a Process this
core cannot run. Refusing at boot is the honest outcome; the alternative is
accepting the metadata and dropping the effect at runtime.

## Alternatives

| Option | Why Not |
|--------|---------|
| Keep `Planner::new` and add commands to it | The single-source constructor has no command catalog to be given, and inventing a second path to build one would be the divergence this decision exists to avoid. |
| Compile the snapshot per request | Pays the cost on every call, and moves a metadata error from boot to whichever request first touches it. |
| Duplicate the rules adapter in `wasm-core` | Two compilers for one YAML dialect. They would agree until they did not, and the failure would be a command permitted in one host and refused in the other. |
| Derive a command's hooks from its metadata declaration | The declaration is not what ran. A step the planner dropped would still emit a hook, so a handler would fire for a write that never happened. |
| Emit one hook per command rather than per table | A handler is registered against a table's trigger name; a command-shaped hook would not match any of them, and the host would have to invent a second dispatch rule. |
| Accept commands with Process effects and ignore the effect | The declaration would be accepted and silently not honoured — the exact defect declarative-saas [[034-a-declaration-the-runtime-ignores-is-a-defect]] names. |

## Consequences

An embedded host serves declarative commands with the engine's own SQL, and its
in-process handlers fire for command-written rows. `examples/lending-golang` is
the worked example, and `tests-system-lending` runs every case against both the
standalone engine and the embedded host, so a divergence between them fails a
build rather than reaching a deployment.

What is paid for it: the core now does real work at `core_init` and can fail
there, so `core_init` grew a distinct exit code for "metadata did not compile"
and a `core_last_error` export to carry the planner's message — an operator with
a directory of command files cannot act on an exit code alone. And a deployment
whose commands declare Process effects cannot be embedded at all until the host
grows a journal; it is refused at boot rather than degraded.
