---
type: decision
status: accepted
date: 2026-07-30
features:
  - "[[declarative-saas]]"
  - "[[005-durable-processes]]"
---

# Deterministic Process transitions use closed context and one journal token

## Context

A Process command may execute as its caller, but the original HTTP request is
gone when a durable worker resumes. Persisting every request header would turn
the journal into an ambient capability and secret store. Reconstructing a
session from current metadata would make an old instance change behavior after
deployment.

Command, `when`, `output`, and `fail` states must also survive concurrent
workers and crashes without duplicating domain writes or advancing a state
twice. A valid Command business rejection is recoverable through its
savepoint; permission, constraint, malformed reserved-error, and driver
failures are not.

## Decision

Process compilation derives the exact session-variable set for each declared
caller role. A start effect persists only that role and closed set. Each caller
Command state filters the persisted set again to the exact variables required
by that Command. Fixed roles have no ambient session and are rejected at
deployment if their Command requires one. Runtime execution requires exact set
equality and never accepts an additional persisted key.

Each new instance owns one pending `start` token. Every successfully advanced
nonterminal deterministic state consumes one `start` or `continue` token and
appends exactly one version-qualified `continue` token in the same source-local
transaction. The worker evaluates immutable values, Rules, and decision tables
from an optimistic snapshot, then locks and rechecks the exact event, instance
version, state, Process revision, and event kind before committing. Signal,
timer, and activity events cannot accidentally execute a deterministic state.

The compiled `when` state retains ordered cases, exact Rule or decision-table
names, and the exact ancestor state selected for literal matching. Runtime does
not repeat graph-resolution heuristics. A terminal `output` stores its validated
closed value both in state history and `terminal_output_json`; an explicit
`fail` stores only its deploy-time safe code and message.

The compiler performs path-sensitive dataflow over the acyclic transition
graph. It distinguishes a state that is an ancestor on some path, a state that
must execute on every reaching path, and a state whose output must have been
persisted on every reaching path. A single request result is unavailable on
`on_error`, and a signal result is unavailable on `on_timeout`; a downstream
join may reference either only when every incoming path provides it. A
`for_each` with `completion: collect` is different: its typed
`successful_items`/`failed_items` collection is the result of both success and
item-error completion routes, so it remains available on those routes.
`activity_key_for_state` similarly requires that its state executed on every
incoming path. These checks happen at deployment rather than becoming a
worker-time missing-value retry loop.

A finalized Command executes as one statement inside
`SAVEPOINT donat_process_command`. Only an exact `P0D01`
`donat.graphql-error.v1` envelope rolls back to that savepoint and commits a
`command_rejected` Process outcome. Every other database or decode error aborts
the outer transition, leaving its event, version, state, domain DML, Command
journal, and Process outboxes unchanged.

## Alternatives

| Option | Why Not |
| --- | --- |
| Persist all request headers | It stores unrelated secrets and creates ambient runtime authority. |
| Rebuild caller variables from current metadata | Old instances would not execute their pinned revision semantics. |
| Advance deterministic states without an event token | Concurrent polling can execute the same version without a durable cause or dedupe identity. |
| Let every event kind wake every state | A late signal or timer could execute an unrelated Command after the instance moved. |
| Recompute the literal-match ancestor at runtime | Compiler and worker graph heuristics can diverge across binary versions. |
| Catch every SQL error through the Command savepoint | Constraint and permission failures would become business branches and could commit corrupted assumptions. |

## Consequences

Caller execution remains useful without granting a Process access to an
arbitrary request. Deterministic states are linearizable, source-local,
revision-pinned, and auditable, while ordinary infrastructure failures remain
retryable because the durable token is unchanged.

The journal stores a bounded caller context and one internal token per running
state. Branch-sensitive output mistakes fail deployment. Runtime preparation
may do pure work more than once under contention, but domain writes and state
advancement occur once after the locked version check.
